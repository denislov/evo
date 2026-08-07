//! ProductEvent pump, reconnect, and update publication for active prompts.

use std::collections::VecDeque;

use coding_agent::api::client::{
    CodingAgentClientConnection, CodingAgentFreshSnapshotRecovery, CodingAgentReconnect,
    CodingAgentReconnectDelivery, CodingAgentReconnectReceiver, CodingAgentRecoveryReason,
    CodingAgentSnapshot,
};
use coding_agent::api::event::{
    CodingAgentProductEvent, CodingAgentProductEventDeliveryClass, CodingAgentProductEventFamily,
};
use tokio::sync::mpsc;

use super::ActivePrompt;
use crate::runtime::protocol::{
    DESKTOP_UPDATE_QUEUE_CAPACITY, DesktopBridgeError, DesktopRuntimeError,
    DesktopRuntimeErrorSource, DesktopRuntimeMetadataSnapshot, DesktopRuntimeUpdate, runtime_error,
};

pub(in crate::runtime) async fn recv_product_event(
    receiver: &mut DesktopProductEventSource,
) -> Result<CodingAgentReconnectDelivery, DesktopBridgeError> {
    receiver.recv().await
}

pub(in crate::runtime) struct DesktopProductEventSource {
    pub(in crate::runtime) replay: VecDeque<CodingAgentProductEvent>,
    pub(in crate::runtime) receiver: DesktopProductEventReceiver,
}

pub(in crate::runtime) enum DesktopProductEventReceiver {
    Product(CodingAgentReconnectReceiver),
    #[cfg(test)]
    Injected(mpsc::Receiver<Result<CodingAgentReconnectDelivery, DesktopBridgeError>>),
}

impl DesktopProductEventReceiver {
    async fn recv(&mut self) -> Result<CodingAgentReconnectDelivery, DesktopBridgeError> {
        match self {
            Self::Product(receiver) => receiver.recv().await.map_err(DesktopBridgeError::from),
            #[cfg(test)]
            Self::Injected(receiver) => receiver
                .recv()
                .await
                .unwrap_or_else(|| Err(DesktopBridgeError::cancelled_for_tests())),
        }
    }

    fn try_recv(&mut self) -> Result<Option<CodingAgentReconnectDelivery>, DesktopBridgeError> {
        match self {
            Self::Product(receiver) => receiver.try_recv().map_err(DesktopBridgeError::from),
            #[cfg(test)]
            Self::Injected(receiver) => match receiver.try_recv() {
                Ok(delivery) => delivery.map(Some),
                Err(mpsc::error::TryRecvError::Empty) => Ok(None),
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    Err(DesktopBridgeError::cancelled_for_tests())
                }
            },
        }
    }
}

impl DesktopProductEventSource {
    pub(in crate::runtime) async fn recv(
        &mut self,
    ) -> Result<CodingAgentReconnectDelivery, DesktopBridgeError> {
        if let Some(event) = self.replay.pop_front() {
            return Ok(CodingAgentReconnectDelivery::Event(event));
        }
        self.receiver.recv().await
    }

    fn try_recv(&mut self) -> Result<Option<CodingAgentReconnectDelivery>, DesktopBridgeError> {
        if let Some(event) = self.replay.pop_front() {
            return Ok(Some(CodingAgentReconnectDelivery::Event(event)));
        }
        self.receiver.try_recv()
    }
}

pub(in crate::runtime) enum DesktopReconnectAttempt<R> {
    Replayed {
        events: Vec<CodingAgentProductEvent>,
        receiver: R,
    },
    FreshSnapshotRequired(CodingAgentFreshSnapshotRecovery),
}

pub(in crate::runtime) fn establish_reconnect<R>(
    requested_after: u64,
    mut reconnect: impl FnMut(u64) -> Result<DesktopReconnectAttempt<R>, DesktopBridgeError>,
) -> Result<
    (
        Vec<CodingAgentProductEvent>,
        R,
        Option<CodingAgentFreshSnapshotRecovery>,
    ),
    DesktopBridgeError,
> {
    match reconnect(requested_after)? {
        DesktopReconnectAttempt::Replayed { events, receiver } => Ok((events, receiver, None)),
        DesktopReconnectAttempt::FreshSnapshotRequired(recovery) => {
            let fresh_sequence = recovery.fresh_cursor.last_event_sequence;
            match reconnect(fresh_sequence)? {
                DesktopReconnectAttempt::Replayed { events, receiver } => {
                    Ok((events, receiver, Some(recovery)))
                }
                DesktopReconnectAttempt::FreshSnapshotRequired(second) => {
                    Err(DesktopBridgeError::Input {
                        message: format!(
                            "desktop ProductEvent reconnect exhausted after fresh cursor {} \
                             (oldest retained sequence {})",
                            second.requested_sequence, second.oldest_available_sequence
                        ),
                    })
                }
            }
        }
    }
}

pub(in crate::runtime) fn reconnect_event_source(
    connection: &CodingAgentClientConnection,
    requested_after: u64,
) -> Result<
    (
        DesktopProductEventSource,
        Option<CodingAgentFreshSnapshotRecovery>,
    ),
    DesktopBridgeError,
> {
    let (events, receiver, recovery) = establish_reconnect(requested_after, |sequence| {
        connection
            .reconnect(sequence)
            .map(|reconnect| match reconnect {
                CodingAgentReconnect::Replayed {
                    events, receiver, ..
                } => DesktopReconnectAttempt::Replayed { events, receiver },
                CodingAgentReconnect::FreshSnapshotRequired(recovery) => {
                    DesktopReconnectAttempt::FreshSnapshotRequired(recovery)
                }
            })
            .map_err(DesktopBridgeError::from)
    })?;
    Ok((
        DesktopProductEventSource {
            replay: events.into(),
            receiver: DesktopProductEventReceiver::Product(receiver),
        },
        recovery,
    ))
}

pub(in crate::runtime) async fn recover_product_event_source(
    active: &mut ActivePrompt,
    receiver_error: DesktopBridgeError,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    match reconnect_event_source(&active.connection, active.last_forwarded_sequence) {
        Ok((events, recovery)) => {
            active.events = events;
            if let Some(recovery) = recovery {
                active.last_forwarded_sequence = recovery.fresh_cursor.last_event_sequence;
                priority_updates
                    .send(recovery_update(recovery))
                    .await
                    .is_ok()
            } else {
                true
            }
        }
        Err(reconnect_error) => priority_updates
            .send(DesktopRuntimeUpdate::RuntimeFailed {
                error: DesktopRuntimeError {
                    code: "product_event_reconnect_failed".into(),
                    message: format!(
                        "ProductEvent receiver failed ({}); reconnect from sequence {} failed: {}",
                        receiver_error, active.last_forwarded_sequence, reconnect_error
                    ),
                },
            })
            .await
            .is_ok(),
    }
}

pub(in crate::runtime) fn recovery_update(
    recovery: CodingAgentFreshSnapshotRecovery,
) -> DesktopRuntimeUpdate {
    let reason = match recovery.reason {
        CodingAgentRecoveryReason::RetainedHistoryGap => DesktopRuntimeError {
            code: "product_event_retained_history_gap".into(),
            message: format!(
                "ProductEvent replay after sequence {} is unavailable; oldest retained sequence is {}",
                recovery.requested_sequence, recovery.oldest_available_sequence
            ),
        },
        CodingAgentRecoveryReason::LiveReceiverLag => DesktopRuntimeError {
            code: "product_event_live_receiver_lag".into(),
            message: format!(
                "ProductEvent receiver lagged after sequence {}; recovered at fresh sequence {}",
                recovery.requested_sequence, recovery.fresh_cursor.last_event_sequence
            ),
        },
    };
    DesktopRuntimeUpdate::ResyncRequired {
        reason,
        snapshot: *recovery.snapshot,
    }
}

pub(in crate::runtime) async fn publish_product_event(
    event: CodingAgentProductEvent,
    active: &ActivePrompt,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    if event.family() == CodingAgentProductEventFamily::Capability {
        let snapshot = match active.connection.state() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return priority_updates
                    .send(DesktopRuntimeUpdate::RuntimeFailed {
                        error: runtime_error(&error),
                    })
                    .await
                    .is_ok();
            }
        };
        return priority_updates
            .send(DesktopRuntimeUpdate::ResyncRequired {
                reason: DesktopRuntimeError {
                    code: "capability_generation_changed".into(),
                    message: format!(
                        "capability generation changed at ProductEvent sequence {}; replacing the desktop projection atomically",
                        event.sequence()
                    ),
                },
                snapshot,
            })
            .await
            .is_ok();
    }
    if is_priority_event(&event) {
        return priority_updates
            .send(DesktopRuntimeUpdate::ProductEvent {
                session_id: active.session_id.clone(),
                event,
            })
            .await
            .is_ok();
    }
    publish_data_update(
        DesktopRuntimeUpdate::ProductEvent {
            session_id: active.session_id.clone(),
            event,
        },
        || active.connection.state(),
        priority_updates,
        data_updates,
    )
    .await
}

pub(in crate::runtime) async fn acknowledge_product_event(
    active: &ActivePrompt,
    sequence: u64,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    match active.connection.acknowledge(sequence) {
        Ok(_) => true,
        Err(error) => priority_updates
            .send(DesktopRuntimeUpdate::RuntimeFailed {
                error: runtime_error(&error),
            })
            .await
            .is_ok(),
    }
}

pub(in crate::runtime) async fn publish_data_update<E>(
    update: DesktopRuntimeUpdate,
    snapshot: impl FnOnce() -> Result<CodingAgentSnapshot, E>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool
where
    E: DesktopRuntimeErrorSource,
{
    match data_updates.try_send(update) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Closed(_)) => false,
        Err(mpsc::error::TrySendError::Full(_)) => {
            let snapshot = match snapshot() {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    return priority_updates
                        .send(DesktopRuntimeUpdate::RuntimeFailed {
                            error: runtime_error(&error),
                        })
                        .await
                        .is_ok();
                }
            };
            priority_updates
                .send(DesktopRuntimeUpdate::ResyncRequired {
                    reason: DesktopRuntimeError {
                        code: "desktop_data_queue_full".into(),
                        message: format!(
                            "desktop message update queue reached its {}-event bound",
                            DESKTOP_UPDATE_QUEUE_CAPACITY
                        ),
                    },
                    snapshot,
                })
                .await
                .is_ok()
        }
    }
}

pub(in crate::runtime) async fn ensure_operation_started(
    active: &mut ActivePrompt,
    candidate_operation_id: Option<&str>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    if active.operation_id.is_some() {
        return true;
    }
    let snapshot = match active.connection.state() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = priority_updates
                .send(DesktopRuntimeUpdate::RuntimeFailed {
                    error: runtime_error(&error),
                })
                .await;
            return false;
        }
    };
    let operation_id = snapshot
        .submitted_operation
        .as_ref()
        .map(|operation| operation.operation_id.clone())
        .or_else(|| candidate_operation_id.map(str::to_owned));
    let Some(operation_id) = operation_id else {
        return true;
    };
    active.operation_id = Some(operation_id.clone());
    priority_updates
        .send(DesktopRuntimeUpdate::PromptStarted {
            command_id: active.command_id,
            operation_id,
            metadata: DesktopRuntimeMetadataSnapshot {
                project: active.context.snapshot().clone(),
                session: Some(snapshot),
            },
        })
        .await
        .is_ok()
}

pub(in crate::runtime) async fn drain_product_events(
    active: &mut ActivePrompt,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> bool {
    loop {
        let received = active.events.try_recv();
        match received {
            Ok(Some(CodingAgentReconnectDelivery::Event(event))) => {
                let sequence = event.sequence();
                let candidate_operation_id = event.operation_id().map(str::to_owned);
                if !ensure_operation_started(
                    active,
                    candidate_operation_id.as_deref(),
                    priority_updates,
                )
                .await
                {
                    return false;
                }
                if !publish_product_event(event, active, priority_updates, data_updates).await {
                    return false;
                }
                if !acknowledge_product_event(active, sequence, priority_updates).await {
                    return false;
                }
                active.last_forwarded_sequence = sequence;
            }
            Ok(Some(CodingAgentReconnectDelivery::FreshSnapshotRequired(recovery))) => {
                active.last_forwarded_sequence = recovery.fresh_cursor.last_event_sequence;
                if priority_updates
                    .send(recovery_update(recovery))
                    .await
                    .is_err()
                {
                    return false;
                }
            }
            Ok(None) => return true,
            Err(error) => {
                return recover_product_event_source(active, error, priority_updates).await;
            }
        }
    }
}

pub(in crate::runtime) fn is_priority_event(event: &CodingAgentProductEvent) -> bool {
    !matches!(
        (event.delivery_class(), event.family()),
        (
            CodingAgentProductEventDeliveryClass::Data,
            CodingAgentProductEventFamily::Message
        )
    )
}
