use super::*;

#[derive(Debug)]
pub struct CodingAgentReconnectReceiver {
    pub(super) inner: ProductEventReceiver,
    pub(super) lifecycle_receiver: tokio::sync::watch::Receiver<u64>,
    pub(super) lifecycle_epoch: u64,
    pub(super) coordinator: Arc<SnapshotCoordinator>,
    pub(super) client_id: CodingAgentClientId,
    pub(super) handle: ClientHandle,
    pub(super) last_sequence: u64,
    pub(super) shutdown_delivered: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "public reconnect delivery retains the exact ProductEvent payload contract"
)]
pub enum CodingAgentReconnectDelivery {
    Event(CodingAgentProductEvent),
    FreshSnapshotRequired(CodingAgentFreshSnapshotRecovery),
}

impl CodingAgentReconnectReceiver {
    pub async fn recv(&mut self) -> Result<CodingAgentReconnectDelivery, CodingAgentPublicError> {
        self.recv_internal()
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn recv_internal(
        &mut self,
    ) -> Result<CodingAgentReconnectDelivery, CodingSessionError> {
        if self.shutdown_delivered {
            return Err(CodingSessionError::Cancelled);
        }
        if let Err(error) = self.ensure_live()
            && !matches!(
                error,
                CodingSessionError::Lifecycle {
                    reason: crate::kernel::error::CodingAgentLifecycleRejection::RuntimeShutDown
                }
            )
        {
            return Err(error);
        }
        loop {
            tokio::select! {
                biased;
                event = self.inner.recv() => {
                    let delivery = match event {
                        Ok(event) => self.project_event(event),
                        Err(CodingSessionError::EventStreamLag { .. }) => {
                            return self.project_live_lag().and_then(|delivery| self.finish_delivery(delivery));
                        }
                        Err(error) => return Err(error),
                    };
                    return self.finish_delivery(delivery);
                }
                changed = self.lifecycle_receiver.changed() => {
                    changed.map_err(|_| CodingSessionError::Cancelled)?;
                    self.lifecycle_epoch = *self.lifecycle_receiver.borrow_and_update();
                    if let Err(error) = self.ensure_live() {
                        if matches!(
                            error,
                            CodingSessionError::Lifecycle {
                                reason: crate::kernel::error::CodingAgentLifecycleRejection::RuntimeShutDown
                            }
                        ) {
                            return Err(CodingSessionError::Cancelled);
                        }
                        return Err(error);
                    }
                }
            }
        }
    }

    pub fn try_recv(
        &mut self,
    ) -> Result<Option<CodingAgentReconnectDelivery>, CodingAgentPublicError> {
        self.try_recv_internal()
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn try_recv_internal(
        &mut self,
    ) -> Result<Option<CodingAgentReconnectDelivery>, CodingSessionError> {
        if self.shutdown_delivered {
            return Err(CodingSessionError::Cancelled);
        }
        let delivery = match self.inner.try_recv() {
            Ok(Some(event)) => {
                let delivery = self.project_event(event);
                self.finish_delivery(delivery).map(Some)
            }
            Ok(None) => {
                self.observe_lifecycle()?;
                Ok(None)
            }
            Err(CodingSessionError::EventStreamLag { .. }) => self
                .project_live_lag()
                .and_then(|delivery| self.finish_delivery(delivery))
                .map(Some),
            Err(error) => Err(error),
        }?;
        Ok(delivery)
    }

    fn finish_delivery(
        &mut self,
        delivery: CodingAgentReconnectDelivery,
    ) -> Result<CodingAgentReconnectDelivery, CodingSessionError> {
        self.ensure_delivery_live(&delivery)?;
        if matches!(
            delivery,
            CodingAgentReconnectDelivery::Event(ref event)
                if matches!(
                    event.event(),
                    crate::events::CodingAgentProductEventKind::Runtime(
                        crate::events::CodingAgentRuntimeProductEvent::ShutDown
                    )
                )
        ) {
            self.shutdown_delivered = true;
        }
        Ok(delivery)
    }

    fn observe_lifecycle(&mut self) -> Result<(), CodingSessionError> {
        if self.lifecycle_receiver.has_changed().unwrap_or(true) {
            self.lifecycle_epoch = *self.lifecycle_receiver.borrow_and_update();
        }
        self.ensure_live()
    }

    fn ensure_live(&self) -> Result<(), CodingSessionError> {
        let _ = self.lifecycle_epoch;
        self.coordinator
            .validate_receiver(&self.handle)
            .map_err(|error| registry_error(&self.client_id, error))
    }

    fn ensure_delivery_live(
        &self,
        delivery: &CodingAgentReconnectDelivery,
    ) -> Result<(), CodingSessionError> {
        let delivery_sequence = match delivery {
            CodingAgentReconnectDelivery::Event(event) => Some(event.sequence()),
            CodingAgentReconnectDelivery::FreshSnapshotRequired(recovery)
                if recovery.reason == CodingAgentRecoveryReason::LiveReceiverLag =>
            {
                Some(recovery.fresh_cursor.last_event_sequence)
            }
            CodingAgentReconnectDelivery::FreshSnapshotRequired(_) => None,
        };
        if delivery_sequence.is_some_and(|sequence| {
            self.coordinator
                .validate_receiver_event(
                    &self.handle,
                    crate::events::ProductEventSequence::new(sequence),
                )
                .is_ok()
        }) {
            return Ok(());
        }
        match self.ensure_live() {
            Ok(()) => Ok(()),
            Err(CodingSessionError::Lifecycle {
                reason: crate::kernel::error::CodingAgentLifecycleRejection::RuntimeShutDown,
            }) if matches!(
                delivery,
                CodingAgentReconnectDelivery::Event(event)
                    if matches!(
                        event.event(),
                        crate::events::CodingAgentProductEventKind::Runtime(
                            crate::events::CodingAgentRuntimeProductEvent::ShutDown
                        )
                    )
            ) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn project_event(
        &mut self,
        event: crate::events::ProductEvent,
    ) -> CodingAgentReconnectDelivery {
        self.last_sequence = event.sequence();
        CodingAgentReconnectDelivery::Event(event)
    }

    fn project_live_lag(&self) -> Result<CodingAgentReconnectDelivery, CodingSessionError> {
        let (state, oldest_available_sequence) =
            self.coordinator
                .live_lag_recovery(&self.handle)
                .map_err(|error| registry_error(&self.client_id, error))?;
        let snapshot = public_client_snapshot(state);
        Ok(CodingAgentReconnectDelivery::FreshSnapshotRequired(
            CodingAgentFreshSnapshotRecovery {
                requested_sequence: self.last_sequence,
                oldest_available_sequence,
                fresh_cursor: snapshot.cursor.clone(),
                reason: CodingAgentRecoveryReason::LiveReceiverLag,
                snapshot: Box::new(snapshot),
            },
        ))
    }
}

#[derive(Debug)]
pub struct CodingAgentProductEventReceiver {
    inner: ProductEventReceiver,
}

impl CodingAgentProductEventReceiver {
    pub(crate) fn new(inner: ProductEventReceiver) -> Self {
        Self { inner }
    }

    pub async fn recv(&mut self) -> Result<CodingAgentProductEvent, CodingAgentPublicError> {
        self.recv_internal()
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn recv_internal(
        &mut self,
    ) -> Result<CodingAgentProductEvent, CodingSessionError> {
        self.inner.recv().await
    }

    pub fn try_recv(&mut self) -> Result<Option<CodingAgentProductEvent>, CodingAgentPublicError> {
        self.try_recv_internal()
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn try_recv_internal(
        &mut self,
    ) -> Result<Option<CodingAgentProductEvent>, CodingSessionError> {
        self.inner.try_recv()
    }
}
