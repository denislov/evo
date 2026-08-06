use super::*;

impl SnapshotCoordinator {
    pub(crate) fn reset_after_rewind(
        &self,
        restored_session_sequence: u64,
    ) -> Result<CapabilityGeneration, CodingSessionError> {
        let _transition = self.capability_transition_guard()?;
        let mut prompt_control = self
            .prompt_control
            .lock_resource("prompt control binding")?;
        let mut cancellations = self
            .operation_cancellations
            .lock_resource("operation cancellation bindings")?;
        let mut state = self.lock_state()?;
        let next = state.capability_generation.next()?;
        state.capability_generation = next;
        state.event_stream_id = crate::platform::time::new_product_event_stream_id();
        state.next_event_sequence = 1;
        state.committed_session_sequence = restored_session_sequence;
        state.retained_product_events.clear();
        state.dropped_before = None;
        state.operation_event_contexts.clear();
        state.pending_authorizations.clear();
        state.published_outbox_record_ids.clear();
        state.shutdown_drain_boundary = None;
        state.shutdown_drain_eligibility.clear();
        state.context_projection = UiContextProjection::default();
        state.clients.clear();
        state.recovery_revision = state.recovery_revision.saturating_add(1);
        state.lifecycle_epoch = state.lifecycle_epoch.saturating_add(1);
        let lifecycle_epoch = state.lifecycle_epoch;
        if let Some(projection) = state.projection.as_mut() {
            projection.revision = projection.revision.checked_add(1).ok_or_else(|| {
                CodingSessionError::UnsupportedCapability {
                    capability: "snapshot projection revision is exhausted".into(),
                }
            })?;
            projection.capability_generation = next;
        }
        *prompt_control = None;
        cancellations.clear();
        drop(state);
        drop(cancellations);
        drop(prompt_control);
        self.lifecycle_sender.send_replace(lifecycle_epoch);
        Ok(next)
    }

    pub(crate) fn current_capability_generation(
        &self,
    ) -> Result<CapabilityGeneration, CodingSessionError> {
        Ok(self.lock_state()?.capability_generation)
    }

    pub(crate) fn capability_transition_guard(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, ()>, CodingSessionError> {
        self.capability_transition
            .lock_resource("capability transition")
    }

    pub(crate) fn install_next_capability_generation(
        &self,
    ) -> Result<CapabilityGeneration, CodingSessionError> {
        let _transition = self.capability_transition_guard()?;
        let mut state = self.lock_state()?;
        let next = state.capability_generation.next()?;
        state.capability_generation = next;
        if let Some(projection) = state.projection.as_mut() {
            projection.revision = projection.revision.checked_add(1).ok_or_else(|| {
                CodingSessionError::UnsupportedCapability {
                    capability: "snapshot projection revision is exhausted".into(),
                }
            })?;
            projection.capability_generation = next;
        }
        Ok(next)
    }

    pub(crate) fn set_active_operation(
        &self,
        active_operation: Option<OperationKind>,
    ) -> Result<(), CodingSessionError> {
        let mut state = self.lock_state()?;
        if let Some(projection) = state.projection.as_mut() {
            projection.revision += 1;
            projection.active_operation = active_operation;
        }
        if active_operation.is_none() {
            state.lifecycle_epoch = state.lifecycle_epoch.saturating_add(1);
            let lifecycle_epoch = state.lifecycle_epoch;
            drop(state);
            self.lifecycle_sender.send_replace(lifecycle_epoch);
        }
        Ok(())
    }

    pub(crate) fn set_active_operation_from_drop(&self, active_operation: Option<OperationKind>) {
        // Operation guards cannot return an error from Drop. Recover solely to
        // publish the released activity and report poison once.
        let mut state = self.state.lock_or_recover("runtime snapshot state");
        if let Some(projection) = state.projection.as_mut() {
            projection.revision += 1;
            projection.active_operation = active_operation;
        }
        if active_operation.is_none() {
            state.lifecycle_epoch = state.lifecycle_epoch.saturating_add(1);
            let lifecycle_epoch = state.lifecycle_epoch;
            drop(state);
            self.lifecycle_sender.send_replace(lifecycle_epoch);
        }
    }

    pub(crate) fn mark_recovery_projected(&self) -> Result<(), CodingSessionError> {
        self.lock_state()?.recovery_revision += 1;
        Ok(())
    }

    pub(crate) fn validate_submission_slot(
        &self,
        handle: &ClientHandle,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.lock_state()?;
        let record = Self::record(&mut state, handle)?;
        if record.prepared_operation.is_some()
            || matches!(
                record.submitted_operation,
                Some(SubmittedOperationStatus::Running { .. })
                    | Some(SubmittedOperationStatus::RecoveryPending { .. })
            )
        {
            Err(ClientRegistryError::SubmittedOperationPending)
        } else {
            Ok(())
        }
    }

    pub(crate) fn register_prepared_submission(
        &self,
        handle: &ClientHandle,
        operation_id: String,
        descriptor: OperationDescriptor,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.lock_state()?;
        let record = Self::record(&mut state, handle)?;
        if record.prepared_operation.is_some()
            || matches!(
                record.submitted_operation,
                Some(SubmittedOperationStatus::Running { .. })
                    | Some(SubmittedOperationStatus::RecoveryPending { .. })
            )
        {
            return Err(ClientRegistryError::SubmittedOperationPending);
        }
        // A terminal submission (acknowledged or not) no longer blocks a
        // fresh prepare: this is the escape hatch for clients whose previous
        // operation ended in `TerminalUncertain` (e.g. a failed prompt that
        // never produced a terminal product event), which previously locked
        // them out of submitting anything until the runtime restarted.
        if record.submitted_operation.is_some() {
            record.submitted_operation = None;
        }
        record.prepared_operation = Some(PreparedOperation {
            operation_id,
            descriptor,
        });
        Ok(())
    }

    /// Drop control receipts belonging to an operation that reached a
    /// terminal state. Receipts only exist to deduplicate control requests
    /// against a live operation; without this cleanup long-lived sessions
    /// exhaust MAX_RECEIPTS and lose steering/abort for every later
    /// operation.
    pub(super) fn clear_control_receipts_for(record: &mut ClientRecord, operation_id: &str) {
        let prefix = format!("{operation_id}:");
        record
            .control_receipts
            .retain(|key, _| !key.starts_with(&prefix));
    }

    pub(crate) fn abandon_prepared_submission(&self, handle: &ClientHandle, operation_id: &str) {
        // Called by lease Drop; recovery is limited to releasing stale
        // submission ownership and reports poison once.
        let mut state = self.state.lock_or_recover("runtime snapshot state");
        let Some(record) = state.clients.get_mut(&handle.id) else {
            return;
        };
        if record.generation != handle.generation {
            return;
        }
        if record
            .prepared_operation
            .as_ref()
            .is_some_and(|prepared| prepared.operation_id == operation_id)
        {
            record.prepared_operation = None;
        }
        if record.pending_abort_operation_id.as_deref() == Some(operation_id) {
            record.pending_abort_operation_id = None;
        }
    }

    pub(crate) fn set_prompt_draft(
        &self,
        handle: &ClientHandle,
        draft: Option<DraftRecord>,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.lock_state()?;
        Self::record(&mut state, handle)?.prompt_draft = draft;
        Ok(())
    }

    pub(crate) fn enqueue_draft(
        &self,
        handle: &ClientHandle,
        draft: DraftRecord,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.lock_state()?;
        let record = Self::record(&mut state, handle)?;
        let queue = match draft.kind {
            crate::runtime::client::state::ClientDraftKind::Steer => &mut record.steer_drafts,
            crate::runtime::client::state::ClientDraftKind::FollowUp => {
                &mut record.follow_up_drafts
            }
            crate::runtime::client::state::ClientDraftKind::Prompt => {
                return Err(ClientRegistryError::InvalidInput);
            }
        };
        if queue.iter().any(|item| item.id == draft.id) {
            if let Some(item) = queue.iter_mut().find(|item| item.id == draft.id) {
                *item = draft;
            }
            return Ok(());
        }
        if queue.len() >= MAX_DRAFTS {
            return Err(ClientRegistryError::QueueCapacityExceeded { limit: MAX_DRAFTS });
        }
        queue.push_back(draft);
        Ok(())
    }

    pub(crate) fn clear_control_drafts(
        &self,
        handle: &ClientHandle,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.lock_state()?;
        let record = Self::record(&mut state, handle)?;
        record.steer_drafts.clear();
        record.follow_up_drafts.clear();
        Ok(())
    }

    pub(crate) fn commit_submission_running(
        &self,
        handle: &ClientHandle,
        operation_id: String,
        descriptor: OperationDescriptor,
        expected_prompt_draft: Option<&DraftRecord>,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.lock_state()?;
        #[cfg(test)]
        if let Some(probe) = self
            .submission_transition_probe
            .lock_or_recover("test submission transition probe")
            .take()
        {
            probe.entered.send(()).unwrap();
            probe.release.recv().unwrap();
        }
        let record = Self::record(&mut state, handle)?;
        if matches!(
            record.submitted_operation,
            Some(SubmittedOperationStatus::Running { .. })
                | Some(SubmittedOperationStatus::RecoveryPending { .. })
        ) {
            return Err(ClientRegistryError::SubmittedRegression);
        }
        // Same escape hatch as `register_prepared_submission`: a stale
        // terminal submission (in particular `TerminalUncertain`) is
        // replaced by the new running operation instead of blocking it.
        record.submitted_operation = None;
        match record.prepared_operation.as_ref() {
            Some(prepared)
                if prepared.operation_id == operation_id && prepared.descriptor == descriptor => {}
            _ => return Err(ClientRegistryError::SubmittedRegression),
        }
        match (descriptor.submitted_kind, expected_prompt_draft) {
            (OperationKind::Prompt, Some(expected))
                if record.prompt_draft.as_ref() == Some(expected) => {}
            (OperationKind::Prompt, _) => {
                return Err(ClientRegistryError::SubmissionDraftMismatch);
            }
            (_, None) => {}
            (_, Some(_)) => return Err(ClientRegistryError::InvalidInput),
        }
        record.submitted_operation = Some(SubmittedOperationStatus::Running {
            operation_id,
            kind: descriptor.submitted_kind,
            descriptor,
        });
        record.prepared_operation = None;
        if descriptor.submitted_kind == OperationKind::Prompt {
            record.prompt_draft = None;
        }
        Ok(())
    }

    pub(crate) fn abort_running_submission_if_matches(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        descriptor: OperationDescriptor,
    ) {
        // Called by operation cleanup; recovery is limited to removing a
        // stale running submission and reports poison once.
        let mut state = self.state.lock_or_recover("runtime snapshot state");
        let Some(record) = state.clients.get_mut(&handle.id) else {
            return;
        };
        if !matches!(
            record.submitted_operation.as_ref(),
            Some(SubmittedOperationStatus::Running {
                operation_id: stored_id,
                kind: stored_kind,
                descriptor: stored_descriptor,
            }) if stored_id == operation_id
                && *stored_kind == descriptor.submitted_kind
                && *stored_descriptor == descriptor
        ) {
            return;
        }
        Self::clear_control_receipts_for(record, operation_id);
        record.submitted_operation = Some(SubmittedOperationStatus::Terminal {
            operation_id: operation_id.to_owned(),
            kind: descriptor.submitted_kind,
            descriptor,
            anchor: SubmittedTerminalAnchor::TerminalUncertain {
                operation_id: operation_id.to_owned(),
            },
            status: ProductEventTerminalStatus::Aborted,
            root_count: 0,
        });
    }

    pub(crate) fn mark_recovery_pending(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        descriptor: OperationDescriptor,
        recovery_id: String,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.lock_state()?;
        Self::validate_terminal_runtime(&state)?;
        let record = state
            .clients
            .get_mut(&handle.id)
            .ok_or(ClientRegistryError::SubmittedRegression)?;
        match record.submitted_operation.as_ref() {
            Some(SubmittedOperationStatus::Running {
                operation_id: stored_id,
                descriptor: stored_descriptor,
                ..
            }) if stored_id == operation_id && *stored_descriptor == descriptor => {
                record.submitted_operation = Some(SubmittedOperationStatus::RecoveryPending {
                    operation_id: operation_id.to_owned(),
                    kind: descriptor.submitted_kind,
                    descriptor,
                    recovery_id,
                });
                Ok(())
            }
            Some(SubmittedOperationStatus::RecoveryPending {
                operation_id: stored_id,
                descriptor: stored_descriptor,
                recovery_id: stored_recovery_id,
                ..
            }) if stored_id == operation_id
                && *stored_descriptor == descriptor
                && stored_recovery_id == &recovery_id =>
            {
                Ok(())
            }
            _ => Err(ClientRegistryError::SubmittedRegression),
        }
    }

    pub(crate) fn mark_terminal(
        &self,
        handle: &ClientHandle,
        operation_id: String,
        kind: OperationKind,
        descriptor: OperationDescriptor,
        anchor: SubmittedTerminalAnchor,
        status: ProductEventTerminalStatus,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.lock_state()?;
        Self::validate_terminal_runtime(&state)?;
        let record = state
            .clients
            .get_mut(&handle.id)
            .ok_or(ClientRegistryError::Lifecycle(
                CodingAgentLifecycleRejection::StaleGeneration,
            ))?;
        if !matches!(
            &record.submitted_operation,
            Some(SubmittedOperationStatus::Running {
                operation_id: stored_id,
                kind: stored_kind,
                ..
            }) if stored_id == &operation_id && *stored_kind == kind
        ) {
            return Err(ClientRegistryError::SubmittedRegression);
        }
        Self::clear_control_receipts_for(record, &operation_id);
        record.submitted_operation = Some(SubmittedOperationStatus::Terminal {
            operation_id: operation_id.clone(),
            kind,
            descriptor,
            anchor,
            status,
            root_count: 0,
        });
        Ok(())
    }

    pub(crate) fn observe_root_terminal_in_state(state: &mut SnapshotState, event: &ProductEvent) {
        let Some(operation_id) = event.operation_id() else {
            return;
        };
        let Some(terminal_operation) = event.terminal_operation() else {
            return;
        };
        let status = terminal_operation.status;
        for record in state.clients.values_mut() {
            let (stored_id, descriptor) = match record.submitted_operation.as_ref() {
                Some(SubmittedOperationStatus::Running {
                    operation_id,
                    descriptor,
                    ..
                })
                | Some(SubmittedOperationStatus::RecoveryPending {
                    operation_id,
                    descriptor,
                    ..
                })
                | Some(SubmittedOperationStatus::Terminal {
                    operation_id,
                    descriptor,
                    ..
                }) => (operation_id, *descriptor),
                None => continue,
            };
            if stored_id != operation_id {
                continue;
            }
            if crate::application::operation::contract::terminal_operation_kind(
                descriptor.submitted_kind,
            ) != Some(terminal_operation.kind)
            {
                continue;
            }
            match record.submitted_operation.as_mut() {
                Some(SubmittedOperationStatus::Terminal { root_count, .. }) => {
                    *root_count = root_count.saturating_add(1);
                }
                Some(
                    SubmittedOperationStatus::Running { .. }
                    | SubmittedOperationStatus::RecoveryPending { .. },
                ) => {
                    let durability = match event.durability() {
                        crate::events::CodingAgentProductEventDurability::PersistenceUncertain {
                            ..
                        } => SubmittedEventDurability::Uncertain,
                        _ => SubmittedEventDurability::Durable,
                    };
                    Self::clear_control_receipts_for(record, operation_id);
                    record.submitted_operation = Some(SubmittedOperationStatus::Terminal {
                        operation_id: operation_id.to_owned(),
                        kind: descriptor.submitted_kind,
                        descriptor,
                        anchor: SubmittedTerminalAnchor::ProductEvent {
                            sequence: event.sequence(),
                            durability,
                        },
                        status,
                        root_count: 1,
                    });
                }
                None => {}
            }
        }
    }

    pub(crate) fn finalize_terminal_association(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        descriptor: OperationDescriptor,
        fallback_status: ProductEventTerminalStatus,
    ) -> Result<(), ClientRegistryError> {
        let mut state = self.lock_state()?;
        Self::validate_terminal_runtime(&state)?;
        let record = state
            .clients
            .get_mut(&handle.id)
            .ok_or(ClientRegistryError::SubmittedRegression)?;
        match record.submitted_operation.as_mut() {
            Some(SubmittedOperationStatus::Terminal {
                operation_id: stored_id,
                descriptor: stored_descriptor,
                root_count,
                ..
            }) if stored_id == operation_id && *stored_descriptor == descriptor => {
                if *root_count == 1 {
                    Ok(())
                } else {
                    Err(ClientRegistryError::TerminalCardinality { count: *root_count })
                }
            }
            Some(SubmittedOperationStatus::Running {
                operation_id: stored_id,
                descriptor: stored_descriptor,
                ..
            }) if stored_id == operation_id && *stored_descriptor == descriptor => {
                Self::clear_control_receipts_for(record, operation_id);
                record.submitted_operation = Some(SubmittedOperationStatus::Terminal {
                    operation_id: operation_id.to_owned(),
                    kind: descriptor.submitted_kind,
                    descriptor,
                    anchor: SubmittedTerminalAnchor::TerminalUncertain {
                        operation_id: operation_id.to_owned(),
                    },
                    status: fallback_status,
                    root_count: 0,
                });
                Ok(())
            }
            _ => Err(ClientRegistryError::SubmittedRegression),
        }
    }
}
