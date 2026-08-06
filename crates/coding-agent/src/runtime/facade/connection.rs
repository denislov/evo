use super::*;
use crate::mutex::MutexExt;

impl CodingAgentSession {
    /// Returns read-only storage authority for legacy adapter protocols.
    pub fn session_storage(&self) -> Result<Option<SessionStorageHandle>, CodingAgentPublicError> {
        self.hydrate_current()
            .map(|hydration| hydration.map(|value| value.summary.storage))
            .map_err(CodingAgentPublicError::from)
    }

    /// Hydrate the complete read-only transcript for the current active leaf.
    ///
    /// Persistent and non-persistent sessions share this presentation
    /// contract. Callers receive owned DTOs and no repository or writer
    /// authority.
    pub fn transcript_snapshot(
        &self,
    ) -> Result<CodingAgentTranscriptSnapshot, CodingAgentPublicError> {
        self.transcript_snapshot_internal()
            .map_err(CodingAgentPublicError::from)
    }

    /// Subscribe to durable session-name changes without polling the session
    /// catalog. The receiver is available only for persistent sessions.
    pub fn subscribe_session_name_updates(&self) -> Option<CodingAgentSessionNameUpdateReceiver> {
        match &self.runtime_host.session_coordinator.persistence {
            SessionPersistence::Persistent(session_service) => {
                Some(CodingAgentSessionNameUpdateReceiver {
                    inner: session_service.subscribe_session_name_updates(),
                })
            }
            SessionPersistence::NonPersistent(_) => None,
        }
    }

    pub(crate) fn transcript_snapshot_internal(
        &self,
    ) -> Result<CodingAgentTranscriptSnapshot, CodingSessionError> {
        IntentRouter::admit_query(
            &self.runtime_host.operation_supervisor.control,
            QueryIntent::SessionView,
        );
        match &self.runtime_host.session_coordinator.persistence {
            SessionPersistence::Persistent(session_service) => {
                let hydration = session_service.hydrated_view()?;
                Ok(CodingAgentTranscriptSnapshot::new_bounded(
                    hydration.summary.session_id,
                    hydration.summary.active_leaf_id,
                    hydration.transcript,
                    hydration.omitted_items,
                    hydration.continuation,
                ))
            }
            SessionPersistence::NonPersistent(state) => Ok(CodingAgentTranscriptSnapshot::new(
                state.runtime_id.clone(),
                None,
                crate::session::service::coding_transcript_from_replay(state.transcript.clone()),
            )),
        }
    }

    pub(crate) fn hydrate_current(
        &self,
    ) -> Result<Option<CodingAgentSessionHydration>, CodingSessionError> {
        match &self.runtime_host.session_coordinator.persistence {
            SessionPersistence::Persistent(session_service) => {
                Ok(Some(session_service.hydrated_view()?))
            }
            SessionPersistence::NonPersistent(_) => Ok(None),
        }
    }

    pub(crate) fn subscribe_product_events(
        &self,
    ) -> Result<ProductEventReceiver, CodingSessionError> {
        let receiver = self.runtime_host.events.subscribe_product_events();
        self.emit_pending_startup_recovery_markers()?;
        Ok(receiver)
    }

    pub fn subscribe_product_events_public(
        &self,
    ) -> Result<CodingAgentProductEventReceiver, CodingAgentPublicError> {
        self.subscribe_product_events()
            .map(CodingAgentProductEventReceiver::new)
            .map_err(CodingAgentPublicError::from)
    }

    pub fn runtime_shutdown_handle(&self) -> CodingAgentRuntimeShutdownHandle {
        CodingAgentRuntimeShutdownHandle {
            coordinator: self.runtime_host.client_projection.snapshots.clone(),
            authorization_service: self.runtime_host.authorization_service.clone(),
        }
    }

    pub fn capability_control(&self) -> CodingAgentCapabilityControl {
        CodingAgentCapabilityControl {
            coordinator: self.runtime_host.client_projection.snapshots.clone(),
            operation_control: self.runtime_host.operation_supervisor.control.clone(),
            event_service: self.runtime_host.events.clone(),
            authorization_service: self.runtime_host.authorization_service.clone(),
        }
    }

    pub async fn shutdown(&mut self) -> Result<CodingAgentShutdownOutcome, CodingAgentPublicError> {
        self.shutdown_internal()
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn shutdown_internal(
        &mut self,
    ) -> Result<CodingAgentShutdownOutcome, CodingSessionError> {
        if self
            .runtime_host
            .client_projection
            .snapshots
            .request_shutdown()?
            == snapshot_coordinator::RuntimeLifecycle::ShutDown
        {
            return Ok(CodingAgentShutdownOutcome::AlreadyShutDown);
        }
        self.runtime_host
            .authorization_service
            .cancel_all("tool authorization cancelled by runtime shutdown")?;
        self.runtime_host
            .operation_supervisor
            .control
            .cancel_open_operations_for_shutdown()?;
        // Session-close ownership policy: every background task of this
        // session is terminated and its driver joined before the session
        // commits its terminal state.
        self.runtime_host.background_tasks.shutdown().await;
        // Extension host lifecycle: session close notifies the host so it can
        // shut down in deterministic order (ARC-710 wires the real host;
        // the Noop port makes this a no-op today).
        self.runtime_host
            .extension_host
            .notify_shutdown("session shutdown");
        self.runtime_host
            .client_projection
            .snapshots
            .wait_for_active_operation_to_drain()
            .await?;
        self.runtime_host.events.emit_runtime_shutdown()?;
        self.runtime_host.session_coordinator.shutdown_writer()?;
        self.runtime_host
            .client_projection
            .snapshots
            .finish_shutdown()?;
        Ok(CodingAgentShutdownOutcome::ShutDown)
    }

    fn emit_pending_startup_recovery_markers(&self) -> Result<(), CodingSessionError> {
        let markers = {
            let mut markers = self
                .runtime_host
                .session_coordinator
                .startup_recovery_markers
                .lock_resource("startup recovery markers")?;
            std::mem::take(&mut *markers)
        };
        if !markers.is_empty() {
            self.runtime_host
                .client_projection
                .snapshots
                .mark_recovery_projected()?;
        }
        for marker in markers {
            self.runtime_host.events.emit_startup_recovery_pending(
                marker.operation_id,
                marker.recovery_id,
                marker.reason,
                marker.session_id,
                marker
                    .operation_kind
                    .and_then(persisted_runtime_operation_kind),
                marker.capability_generation,
                marker.attempt_count,
                marker.last_attempt_at,
                marker.next_attempt_at,
            )?;
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Result<CodingAgentSnapshot, CodingAgentPublicError> {
        self.emit_pending_startup_recovery_markers()
            .map_err(CodingAgentPublicError::from)?;
        self.runtime_host
            .client_projection
            .snapshots
            .snapshot()
            .map(Into::into)
            .map_err(CodingAgentPublicError::from)
    }

    pub fn connect(
        &self,
        id: CodingAgentClientId,
    ) -> Result<CodingAgentClientConnection, CodingAgentPublicError> {
        self.connect_internal(id)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn connect_internal(
        &self,
        id: CodingAgentClientId,
    ) -> Result<CodingAgentClientConnection, CodingSessionError> {
        let internal_id = public_connection::internal_client_id(&id);
        let handle = self
            .runtime_host
            .client_projection
            .clients
            .connect_or_takeover(internal_id)
            .map_err(|error| match error {
                snapshot_coordinator::ClientRegistryError::ClientCapacityExceeded { limit } => {
                    CodingSessionError::ClientCapacityExceeded { limit }
                }
                snapshot_coordinator::ClientRegistryError::Lifecycle(reason) => {
                    CodingSessionError::Lifecycle { reason }
                }
                other => CodingSessionError::Input {
                    message: other.to_string(),
                },
            })?;
        let state = self
            .runtime_host
            .client_projection
            .snapshots
            .client_state(&handle)
            .map_err(|error| CodingSessionError::Input {
                message: error.to_string(),
            })?;
        Ok(public_connection::public_client_connection(
            id,
            self.runtime_host.client_projection.snapshots.clone(),
            self.runtime_host.events.clone(),
            self.runtime_host.authorization_service.clone(),
            handle,
            state,
        ))
    }

    pub(crate) fn refresh_snapshot_projection(&self) -> Result<(), CodingSessionError> {
        let session = match &self.runtime_host.session_coordinator.persistence {
            SessionPersistence::Persistent(session_service) => session_service.view()?,
            SessionPersistence::NonPersistent(state) => CodingAgentSessionView {
                session_id: state.runtime_id.clone(),
                name: None,
                default_agent_profile_id: state.default_agent_profile_id.clone(),
            },
        };
        let capabilities = self.capabilities_internal()?;
        let generation = self
            .runtime_host
            .operation_supervisor
            .capabilities
            .current_generation()?;
        let committed_session_sequence = match &self.runtime_host.session_coordinator.persistence {
            SessionPersistence::Persistent(session_service) => {
                session_service.active_branch_session_sequence()?
            }
            SessionPersistence::NonPersistent(_) => 0,
        };
        self.runtime_host
            .client_projection
            .snapshots
            .install_projection(
                session,
                capabilities,
                generation,
                committed_session_sequence,
            )?;
        Ok(())
    }
}

impl CodingAgentSessionNameUpdateReceiver {
    /// Return the current durable name state at the subscription cursor.
    pub fn current(&self) -> CodingAgentSessionNameUpdate {
        let update = self.inner.borrow();
        CodingAgentSessionNameUpdate {
            name: update.name.clone(),
            updated_at: update.updated_at.clone(),
        }
    }

    /// Wait for the next committed durable name change.
    pub async fn changed(&mut self) -> Option<CodingAgentSessionNameUpdate> {
        self.inner.changed().await.ok()?;
        Some(self.current())
    }
}

pub(super) fn persisted_runtime_operation_kind(
    kind: crate::session::event::OperationKind,
) -> Option<crate::kernel::operation::OperationKind> {
    use crate::kernel::operation::OperationKind as RuntimeKind;
    use crate::session::event::OperationKind as SessionKind;
    match kind {
        SessionKind::Prompt => Some(RuntimeKind::Prompt),
        SessionKind::ManualCompaction => Some(RuntimeKind::Compact),
        SessionKind::BranchSummary => Some(RuntimeKind::BranchSummary),
        SessionKind::Export => Some(RuntimeKind::Export),
        SessionKind::SelfHealingEdit => Some(RuntimeKind::SelfHealingEdit),
        SessionKind::SessionTreeLabel => Some(RuntimeKind::SetSessionTreeLabel),
        SessionKind::Other { .. } => None,
    }
}
