use super::*;

impl SnapshotCoordinator {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn connect_or_takeover(
        &self,
        id: ClientConnectionId,
    ) -> Result<ClientHandle, ClientRegistryError> {
        let mut state = self.lock_state()?;
        Self::validate_runtime(&state)?;
        state.shutdown_drain_eligibility.remove(&id);
        if let Some(record) = state.clients.get_mut(&id) {
            record.generation.0 += 1;
            record.connection = ConnectionLifecycle::Attached;
            let generation = record.generation;
            state.lifecycle_epoch = state.lifecycle_epoch.saturating_add(1);
            let lifecycle_epoch = state.lifecycle_epoch;
            let handle = ClientHandle { id, generation };
            drop(state);
            self.rebind_controls(&handle)?;
            self.lifecycle_sender.send_replace(lifecycle_epoch);
            return Ok(handle);
        }
        if state.clients.len() >= MAX_CLIENTS {
            return Err(ClientRegistryError::ClientCapacityExceeded { limit: MAX_CLIENTS });
        }
        let generation = ClientGeneration(1);
        state
            .clients
            .insert(id.clone(), ClientRecord::new(generation));
        Ok(ClientHandle { id, generation })
    }

    pub(crate) fn validate_receiver(
        &self,
        handle: &ClientHandle,
    ) -> Result<(), ClientRegistryError> {
        let state = self.lock_state()?;
        Self::validate_receiver_in_state(&state, handle, None)
    }

    pub(crate) fn validate_receiver_event(
        &self,
        handle: &ClientHandle,
        sequence: ProductEventSequence,
    ) -> Result<(), ClientRegistryError> {
        let state = self.lock_state()?;
        Self::validate_receiver_in_state(&state, handle, Some(sequence))
    }

    pub(super) fn validate_receiver_in_state(
        state: &SnapshotState,
        handle: &ClientHandle,
        sequence: Option<ProductEventSequence>,
    ) -> Result<(), ClientRegistryError> {
        if state.runtime_lifecycle == RuntimeLifecycle::ShutDown {
            if sequence.is_some_and(|sequence| {
                state
                    .shutdown_drain_boundary
                    .is_some_and(|boundary| sequence <= boundary)
            }) && state
                .shutdown_drain_eligibility
                .get(&handle.id)
                .is_some_and(|generation| *generation == handle.generation)
            {
                let record =
                    state
                        .clients
                        .get(&handle.id)
                        .ok_or(ClientRegistryError::Lifecycle(
                            CodingAgentLifecycleRejection::StaleGeneration,
                        ))?;
                if record.generation == handle.generation {
                    return Ok(());
                }
            }
            if state.clients.get(&handle.id).is_some_and(|record| {
                record.generation == handle.generation
                    && record.connection == ConnectionLifecycle::Detached
                    && state
                        .shutdown_drain_eligibility
                        .get(&handle.id)
                        .is_none_or(|generation| *generation != handle.generation)
            }) {
                return Err(ClientRegistryError::Lifecycle(
                    CodingAgentLifecycleRejection::Detached,
                ));
            }
            return Err(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::RuntimeShutDown,
            ));
        }
        let record = state
            .clients
            .get(&handle.id)
            .ok_or(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ))?;
        if record.generation != handle.generation {
            return Err(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ));
        }
        match record.connection {
            ConnectionLifecycle::Attached | ConnectionLifecycle::ShuttingDown => Ok(()),
            ConnectionLifecycle::Detached => Err(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::Detached,
            )),
        }
    }

    pub(super) fn rebind_controls(&self, handle: &ClientHandle) -> Result<(), ClientRegistryError> {
        let mut binding = self
            .prompt_control
            .lock_resource("prompt control binding")?;
        if let Some(active) = binding.as_mut()
            && active.owner.id == handle.id
        {
            active.owner.generation = handle.generation;
        }
        drop(binding);
        let mut cancellations = self
            .operation_cancellations
            .lock_resource("operation cancellation bindings")?;
        for active in cancellations.values_mut() {
            if active.owner.id == handle.id {
                active.owner.generation = handle.generation;
            }
        }
        Ok(())
    }

    pub(crate) fn detach(
        &self,
        handle: &ClientHandle,
    ) -> Result<ClientDetachOutcome, ClientRegistryError> {
        let mut state = self.lock_state()?;
        Self::validate_runtime(&state)?;
        let Some(record) = state.clients.get_mut(&handle.id) else {
            return Ok(ClientDetachOutcome::StaleGeneration);
        };
        if record.generation != handle.generation {
            return Ok(ClientDetachOutcome::StaleGeneration);
        }
        match record.connection {
            ConnectionLifecycle::Attached => {
                record.connection = ConnectionLifecycle::Detached;
                state.shutdown_drain_eligibility.remove(&handle.id);
                state.lifecycle_epoch = state.lifecycle_epoch.saturating_add(1);
                let lifecycle_epoch = state.lifecycle_epoch;
                drop(state);
                self.lifecycle_sender.send_replace(lifecycle_epoch);
                Ok(ClientDetachOutcome::Detached)
            }
            ConnectionLifecycle::Detached => Ok(ClientDetachOutcome::AlreadyDetached),
            ConnectionLifecycle::ShuttingDown => {
                unreachable!("runtime validation rejects detach while the runtime is shutting down")
            }
        }
    }

    pub(crate) fn is_current(&self, handle: &ClientHandle) -> Result<bool, CodingSessionError> {
        let state = self.lock_state()?;
        Ok(state
            .clients
            .get(&handle.id)
            .is_some_and(|record| record.generation == handle.generation))
    }

    pub(crate) fn client_snapshot(
        &self,
        handle: &ClientHandle,
    ) -> Result<UiSnapshot, ClientRegistryError> {
        self.snapshot_for_client(Some(handle))
    }

    pub(crate) fn client_state(
        &self,
        handle: &ClientHandle,
    ) -> Result<ClientSnapshotState, ClientRegistryError> {
        let snapshot = self.client_snapshot(handle)?;
        let mut state = self.lock_state()?;
        let record = Self::record(&mut state, handle)?;
        let drafts = record
            .prompt_draft
            .iter()
            .chain(record.steer_drafts.iter())
            .chain(record.follow_up_drafts.iter())
            .cloned()
            .collect();
        Ok(ClientSnapshotState {
            snapshot,
            drafts,
            submitted_operation: record.submitted_operation.clone(),
        })
    }

    pub(super) fn record<'a>(
        state: &'a mut SnapshotState,
        handle: &ClientHandle,
    ) -> Result<&'a mut ClientRecord, ClientRegistryError> {
        Self::validate_runtime(state)?;
        let record = state
            .clients
            .get_mut(&handle.id)
            .ok_or(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ))?;
        if record.generation != handle.generation {
            return Err(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ));
        }
        match record.connection {
            ConnectionLifecycle::Attached => {}
            ConnectionLifecycle::ShuttingDown => {
                return Err(ClientRegistryError::Lifecycle(
                    CodingAgentLifecycleRejection::RuntimeShutDown,
                ));
            }
            ConnectionLifecycle::Detached => {
                return Err(ClientRegistryError::Lifecycle(
                    CodingAgentLifecycleRejection::Detached,
                ));
            }
        }
        Ok(record)
    }

    pub(super) fn validate_runtime(state: &SnapshotState) -> Result<(), ClientRegistryError> {
        match state.runtime_lifecycle {
            RuntimeLifecycle::Running => Ok(()),
            RuntimeLifecycle::ShuttingDown | RuntimeLifecycle::ShutDown => Err(
                ClientRegistryError::Lifecycle(CodingAgentLifecycleRejection::RuntimeShutDown),
            ),
        }
    }

    pub(super) fn validate_terminal_runtime(
        state: &SnapshotState,
    ) -> Result<(), ClientRegistryError> {
        match state.runtime_lifecycle {
            RuntimeLifecycle::Running | RuntimeLifecycle::ShuttingDown => Ok(()),
            RuntimeLifecycle::ShutDown => Err(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::RuntimeShutDown,
            )),
        }
    }

    pub(crate) fn validate_client(
        state: &SnapshotState,
        handle: &ClientHandle,
    ) -> Result<(), ClientRegistryError> {
        Self::validate_runtime(state)?;
        let record = state
            .clients
            .get(&handle.id)
            .ok_or(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ))?;
        if record.generation != handle.generation {
            return Err(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ));
        }
        match record.connection {
            ConnectionLifecycle::Attached => {}
            ConnectionLifecycle::ShuttingDown => {
                return Err(ClientRegistryError::Lifecycle(
                    CodingAgentLifecycleRejection::RuntimeShutDown,
                ));
            }
            ConnectionLifecycle::Detached => {
                return Err(ClientRegistryError::Lifecycle(
                    CodingAgentLifecycleRejection::Detached,
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn live_lag_recovery(
        &self,
        handle: &ClientHandle,
    ) -> Result<(ClientSnapshotState, u64), ClientRegistryError> {
        let state = self.lock_state()?;
        match state.runtime_lifecycle {
            RuntimeLifecycle::ShutDown => {
                let boundary =
                    state
                        .shutdown_drain_boundary
                        .ok_or(ClientRegistryError::Lifecycle(
                            CodingAgentLifecycleRejection::RuntimeShutDown,
                        ))?;
                Self::shutdown_lag_recovery_from_state(&state, handle, boundary)
            }
            RuntimeLifecycle::Running | RuntimeLifecycle::ShuttingDown => {
                Self::validate_receiver_in_state(&state, handle, None)?;
                Self::lag_recovery_from_state(&state, handle)
            }
        }
    }

    pub(super) fn shutdown_lag_recovery_from_state(
        state: &SnapshotState,
        handle: &ClientHandle,
        boundary: ProductEventSequence,
    ) -> Result<(ClientSnapshotState, u64), ClientRegistryError> {
        Self::validate_receiver_in_state(state, handle, Some(boundary))?;
        Self::lag_recovery_from_state(state, handle)
    }

    pub(super) fn lag_recovery_from_state(
        state: &SnapshotState,
        handle: &ClientHandle,
    ) -> Result<(ClientSnapshotState, u64), ClientRegistryError> {
        let record = state
            .clients
            .get(&handle.id)
            .filter(|record| record.generation == handle.generation)
            .ok_or(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ))?;
        let projection = state
            .projection
            .clone()
            .expect("snapshot projection must be installed by session construction");
        let snapshot = UiSnapshot::new(
            UiSnapshotCursor {
                stream_id: state.event_stream_id.clone(),
                last_event_sequence: ProductEventSequence::new(
                    state.next_event_sequence.saturating_sub(1),
                ),
                last_session_sequence: state.committed_session_sequence,
                capability_generation: projection.capability_generation,
            },
            UI_SNAPSHOT_PROTOCOL_VERSION,
            projection.session,
            projection.capabilities,
            projection.active_operation,
            record
                .prompt_draft
                .iter()
                .chain(record.steer_drafts.iter())
                .chain(record.follow_up_drafts.iter())
                .map(|draft| ClientDraft::new(draft.kind, draft.text.clone()))
                .collect(),
            Vec::new(),
        )
        .with_context(state.context_projection.clone());
        let client_state = ClientSnapshotState {
            snapshot,
            drafts: record
                .prompt_draft
                .iter()
                .chain(record.steer_drafts.iter())
                .chain(record.follow_up_drafts.iter())
                .cloned()
                .collect(),
            submitted_operation: record.submitted_operation.clone(),
        };
        let oldest_available = state
            .retained_product_events
            .front()
            .map(ProductEvent::sequence)
            .unwrap_or_else(|| {
                client_state
                    .snapshot
                    .cursor
                    .last_event_sequence
                    .get()
                    .saturating_add(1)
            });
        Ok((client_state, oldest_available))
    }

    pub(crate) fn acknowledge(
        &self,
        handle: &ClientHandle,
        sequence: u64,
    ) -> Result<u64, ClientRegistryError> {
        let mut state = self.lock_state()?;
        let record = Self::record(&mut state, handle)?;
        if sequence < record.acknowledged_sequence {
            return Ok(record.acknowledged_sequence);
        }
        record.acknowledged_sequence = sequence;
        if let Some(SubmittedOperationStatus::Terminal {
            anchor:
                SubmittedTerminalAnchor::ProductEvent {
                    sequence: terminal_sequence,
                    ..
                },
            ..
        }) = &record.submitted_operation
            && sequence >= *terminal_sequence
        {
            record.submitted_operation = None;
        }
        Ok(record.acknowledged_sequence)
    }

    pub(crate) fn acknowledge_outcome(
        &self,
        handle: &ClientHandle,
        acknowledgement: &crate::runtime::client::connection::CodingAgentOutcomeAcknowledgementId,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.lock_state()?;
        let record = Self::record(&mut state, handle)?;
        match &record.submitted_operation {
            Some(SubmittedOperationStatus::Terminal {
                anchor:
                    SubmittedTerminalAnchor::OutcomeOnly {
                        acknowledgement: stored,
                    },
                ..
            }) if stored == acknowledgement => {
                record.submitted_operation = None;
                Ok(())
            }
            _ => Err(ClientRegistryError::InvalidInput),
        }
    }
}
