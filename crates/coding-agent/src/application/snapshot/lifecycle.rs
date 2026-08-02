use super::*;

impl SnapshotCoordinator {
    pub(super) fn lock_state(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, SnapshotState>, CodingSessionError> {
        self.state.lock_resource("runtime snapshot state")
    }

    pub(crate) fn register_operation_event_context(
        &self,
        operation_id: String,
        kind: OperationKind,
        generation: CapabilityGeneration,
        parent_operation_id: Option<String>,
        root_operation_id: String,
    ) -> Result<(), CodingSessionError> {
        let mut state = self.lock_state()?;
        let context = OperationEventContext {
            kind,
            capability_generation: generation,
            parent_operation_id,
            root_operation_id,
        };
        let previous = state
            .operation_event_contexts
            .insert(operation_id, context.clone());
        debug_assert!(previous.is_none() || previous == Some(context));
        Ok(())
    }

    pub(crate) fn clear_operation_event_context_if(
        &self,
        operation_id: &str,
        generation: CapabilityGeneration,
    ) {
        // Operation guards invoke this from Drop; recover only to release
        // terminal bookkeeping and report poison once.
        let mut state = self.state.lock_or_recover("runtime snapshot state");
        if state
            .operation_event_contexts
            .get(operation_id)
            .is_some_and(|current| current.capability_generation == generation)
        {
            state.operation_event_contexts.remove(operation_id);
        }
    }

    pub(crate) fn ensure_runtime_running(&self) -> Result<(), CodingSessionError> {
        let state = self.lock_state()?;
        Self::validate_runtime(&state).map_err(|error| match error {
            ClientRegistryError::Lifecycle(reason) => CodingSessionError::Lifecycle { reason },
            other => CodingSessionError::Input {
                message: other.to_string(),
            },
        })
    }

    pub(crate) fn request_shutdown(&self) -> Result<RuntimeLifecycle, CodingSessionError> {
        let mut state = self.lock_state()?;
        let previous = state.runtime_lifecycle;
        if previous != RuntimeLifecycle::Running {
            return Ok(previous);
        }
        state.runtime_lifecycle = RuntimeLifecycle::ShuttingDown;
        state.shutdown_drain_eligibility.clear();
        let mut eligible = Vec::new();
        for (id, record) in &mut state.clients {
            if record.connection == ConnectionLifecycle::Attached {
                record.connection = ConnectionLifecycle::ShuttingDown;
                eligible.push((id.clone(), record.generation));
            }
        }
        state.shutdown_drain_eligibility.extend(eligible);
        state.lifecycle_epoch = state.lifecycle_epoch.saturating_add(1);
        let lifecycle_epoch = state.lifecycle_epoch;
        drop(state);
        *self
            .prompt_control
            .lock_resource("prompt control binding")? = None;
        self.lifecycle_sender.send_replace(lifecycle_epoch);
        Ok(previous)
    }

    pub(crate) async fn wait_for_active_operation_to_drain(
        &self,
    ) -> Result<(), CodingSessionError> {
        let mut receiver = self.subscribe_lifecycle();
        loop {
            let active = self
                .lock_state()?
                .projection
                .as_ref()
                .and_then(|projection| projection.active_operation);
            if active.is_none() {
                return Ok(());
            }
            if receiver.changed().await.is_err() {
                return Ok(());
            }
        }
    }

    pub(crate) fn finish_shutdown(&self) -> Result<(), CodingSessionError> {
        let mut state = self.lock_state()?;
        if state.runtime_lifecycle == RuntimeLifecycle::ShutDown {
            return Ok(());
        }
        debug_assert_eq!(state.runtime_lifecycle, RuntimeLifecycle::ShuttingDown);
        state.runtime_lifecycle = RuntimeLifecycle::ShutDown;
        for record in state.clients.values_mut() {
            if record.connection == ConnectionLifecycle::ShuttingDown {
                record.connection = ConnectionLifecycle::Detached;
            }
        }
        state.lifecycle_epoch = state.lifecycle_epoch.saturating_add(1);
        let lifecycle_epoch = state.lifecycle_epoch;
        drop(state);
        self.lifecycle_sender.send_replace(lifecycle_epoch);
        Ok(())
    }

    pub(crate) fn is_shut_down(&self) -> Result<bool, CodingSessionError> {
        Ok(self.lock_state()?.runtime_lifecycle == RuntimeLifecycle::ShutDown)
    }

    pub(crate) fn enqueue_prompt_control_draft(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        draft_id: crate::runtime::client::connection::CodingAgentDraftId,
        kind: crate::runtime::client::connection::CodingAgentControlKind,
    ) -> Result<
        crate::runtime::client::connection::CodingAgentControlReceipt,
        crate::runtime::client::connection::CodingAgentControlRejection,
    > {
        let text = {
            let mut state = self.lock_state().map_err(|_| {
                crate::runtime::client::connection::CodingAgentControlRejection {
                    control_id: crate::runtime::client::connection::CodingAgentControlId(
                        draft_id.0.clone(),
                    ),
                    operation_id: operation_id.into(),
                    kind,
                    reason: crate::runtime::client::connection::CodingAgentControlRejectionReason::ResourceUnavailable,
                }
            })?;
            let record = Self::record(&mut state, handle).map_err(|error| {
                crate::runtime::client::connection::CodingAgentControlRejection {
                    control_id: crate::runtime::client::connection::CodingAgentControlId(
                        draft_id.0.clone(),
                    ),
                    operation_id: operation_id.into(),
                    kind,
                    reason: control_rejection_reason(&error),
                }
            })?;
            let queue = match kind {
                crate::runtime::client::connection::CodingAgentControlKind::Steer => {
                    &record.steer_drafts
                }
                crate::runtime::client::connection::CodingAgentControlKind::FollowUp => {
                    &record.follow_up_drafts
                }
                crate::runtime::client::connection::CodingAgentControlKind::Abort => {
                    return Err(crate::runtime::client::connection::CodingAgentControlRejection {
                        control_id: crate::runtime::client::connection::CodingAgentControlId(draft_id.0),
                        operation_id: operation_id.into(), kind,
                        reason: crate::runtime::client::connection::CodingAgentControlRejectionReason::InvalidInput,
                    });
                }
            };
            queue
                .iter()
                .find(|draft| draft.id == draft_id.0)
                .map(|draft| draft.text.clone())
                .ok_or_else(|| crate::runtime::client::connection::CodingAgentControlRejection {
                    control_id: crate::runtime::client::connection::CodingAgentControlId(draft_id.0.clone()),
                    operation_id: operation_id.into(),
                    kind,
                    reason:
                        crate::runtime::client::connection::CodingAgentControlRejectionReason::InvalidInput,
                })?
        };
        self.enqueue_control(
            handle,
            operation_id,
            crate::runtime::client::connection::CodingAgentControlId(draft_id.0),
            kind,
            text,
        )
    }

    pub(crate) fn enqueue_control(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        control_id: crate::runtime::client::connection::CodingAgentControlId,
        kind: crate::runtime::client::connection::CodingAgentControlKind,
        text: String,
    ) -> Result<
        crate::runtime::client::connection::CodingAgentControlReceipt,
        crate::runtime::client::connection::CodingAgentControlRejection,
    > {
        self.enqueue_control_payload(
            handle,
            operation_id,
            control_id,
            kind,
            PromptControlPayload::Text(text),
        )
    }

    pub(crate) fn enqueue_content_control(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        control_id: crate::runtime::client::connection::CodingAgentControlId,
        kind: crate::runtime::client::connection::CodingAgentControlKind,
        content: Vec<ai::api::conversation::ContentBlock>,
    ) -> Result<
        crate::runtime::client::connection::CodingAgentControlReceipt,
        crate::runtime::client::connection::CodingAgentControlRejection,
    > {
        self.enqueue_control_payload(
            handle,
            operation_id,
            control_id,
            kind,
            PromptControlPayload::Content(content),
        )
    }

    pub(super) fn enqueue_control_payload(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        control_id: crate::runtime::client::connection::CodingAgentControlId,
        kind: crate::runtime::client::connection::CodingAgentControlKind,
        payload: PromptControlPayload,
    ) -> Result<
        crate::runtime::client::connection::CodingAgentControlReceipt,
        crate::runtime::client::connection::CodingAgentControlRejection,
    > {
        if control_id.0.trim().is_empty() || payload.is_empty() {
            return Err(crate::runtime::client::connection::CodingAgentControlRejection {
                control_id,
                operation_id: operation_id.into(),
                kind,
                reason: crate::runtime::client::connection::CodingAgentControlRejectionReason::InvalidInput,
            });
        }
        let mut state = self.lock_state().map_err(|_| {
            crate::runtime::client::connection::CodingAgentControlRejection {
                control_id: control_id.clone(),
                operation_id: operation_id.into(),
                kind,
                reason: crate::runtime::client::connection::CodingAgentControlRejectionReason::ResourceUnavailable,
            }
        })?;
        let record = match Self::record(&mut state, handle) {
            Ok(record) => record,
            Err(error @ ClientRegistryError::Lifecycle(_)) => {
                return Err(
                    crate::runtime::client::connection::CodingAgentControlRejection {
                        control_id,
                        operation_id: operation_id.into(),
                        kind,
                        reason: control_rejection_reason(&error),
                    },
                );
            }
            Err(_) => {
                return Err(crate::runtime::client::connection::CodingAgentControlRejection {
                    control_id,
                    operation_id: operation_id.into(),
                    kind,
                    reason:
                        crate::runtime::client::connection::CodingAgentControlRejectionReason::InvalidInput,
                });
            }
        };
        let key = format!("{}:{}", operation_id, control_id.0);
        let signature = format!("{:?}:{}", kind, payload.signature());
        if let Some(stored) = record.control_receipts.get(&key) {
            if stored != &signature {
                return Err(crate::runtime::client::connection::CodingAgentControlRejection {
                    control_id,
                    operation_id: operation_id.into(),
                    kind,
                    reason:
                        crate::runtime::client::connection::CodingAgentControlRejectionReason::PayloadConflict,
                });
            }
            return Ok(
                crate::runtime::client::connection::CodingAgentControlReceipt {
                    control_id,
                    operation_id: operation_id.into(),
                    kind,
                },
            );
        }
        if record.control_receipts.len() >= MAX_RECEIPTS {
            return Err(crate::runtime::client::connection::CodingAgentControlRejection { control_id, operation_id: operation_id.into(), kind, reason: crate::runtime::client::connection::CodingAgentControlRejectionReason::QueueCapacityExceeded });
        }
        let queued_prepared_abort = if kind
            == crate::runtime::client::connection::CodingAgentControlKind::Abort
        {
            match record.prepared_operation.as_ref() {
                Some(prepared) if prepared.operation_id == operation_id => {
                    record.pending_abort_operation_id = Some(operation_id.to_owned());
                    true
                }
                Some(_) => {
                    return Err(
                            crate::runtime::client::connection::CodingAgentControlRejection {
                                control_id,
                                operation_id: operation_id.into(),
                                kind,
                                reason: crate::runtime::client::connection::CodingAgentControlRejectionReason::TargetMismatch,
                            },
                        );
                }
                None => false,
            }
        } else {
            false
        };
        if !queued_prepared_abort {
            let dispatch = self.dispatch_control(handle, operation_id, kind, payload);
            if let Err(reason) = dispatch {
                let queued_running_abort = kind
                    == crate::runtime::client::connection::CodingAgentControlKind::Abort
                    && reason
                        == crate::runtime::client::connection::CodingAgentControlRejectionReason::TargetNotRunning
                    && matches!(
                        record.submitted_operation.as_ref(),
                        Some(SubmittedOperationStatus::Running {
                            operation_id: running_operation_id,
                            ..
                        }) if running_operation_id == operation_id
                    );
                if queued_running_abort {
                    record.pending_abort_operation_id = Some(operation_id.to_owned());
                } else {
                    return Err(
                        crate::runtime::client::connection::CodingAgentControlRejection {
                            control_id,
                            operation_id: operation_id.into(),
                            kind,
                            reason,
                        },
                    );
                }
            }
        }
        record.control_receipts.insert(key.clone(), signature);
        let queue = match kind {
            crate::runtime::client::connection::CodingAgentControlKind::Steer => {
                Some(&mut record.steer_drafts)
            }
            crate::runtime::client::connection::CodingAgentControlKind::FollowUp => {
                Some(&mut record.follow_up_drafts)
            }
            crate::runtime::client::connection::CodingAgentControlKind::Abort => None,
        };
        if let Some(queue) = queue
            && let Some(position) = queue.iter().position(|draft| draft.id == control_id.0)
        {
            queue.remove(position);
        }
        Ok(
            crate::runtime::client::connection::CodingAgentControlReceipt {
                control_id,
                operation_id: operation_id.into(),
                kind,
            },
        )
    }

    pub(super) fn dispatch_control(
        &self,
        handle: &ClientHandle,
        operation_id: &str,
        kind: crate::runtime::client::connection::CodingAgentControlKind,
        payload: PromptControlPayload,
    ) -> Result<(), crate::runtime::client::connection::CodingAgentControlRejectionReason> {
        let mut prompt_binding = self
            .prompt_control
            .lock_resource("prompt control binding")
            .map_err(|_| {
                crate::runtime::client::connection::CodingAgentControlRejectionReason::ResourceUnavailable
            })?;
        if let Some(active) = prompt_binding.as_mut() {
            if active.owner.id != handle.id {
                return Err(
                    crate::runtime::client::connection::CodingAgentControlRejectionReason::NotOwner,
                );
            }
            if active.operation_id != operation_id {
                return Err(
                    crate::runtime::client::connection::CodingAgentControlRejectionReason::TargetMismatch,
                );
            }
            return match (kind, payload) {
                (
                    crate::runtime::client::connection::CodingAgentControlKind::Abort,
                    PromptControlPayload::Text(reason),
                ) => active.sender.abort(reason),
                (
                    crate::runtime::client::connection::CodingAgentControlKind::Steer,
                    PromptControlPayload::Text(text),
                ) => active.sender.steer(text),
                (
                    crate::runtime::client::connection::CodingAgentControlKind::Steer,
                    PromptControlPayload::Content(content),
                ) => active.sender.steer_content(content),
                (
                    crate::runtime::client::connection::CodingAgentControlKind::FollowUp,
                    PromptControlPayload::Text(text),
                ) => active.sender.follow_up(text),
                (
                    crate::runtime::client::connection::CodingAgentControlKind::FollowUp,
                    PromptControlPayload::Content(content),
                ) => active.sender.follow_up_content(content),
                (
                    crate::runtime::client::connection::CodingAgentControlKind::Abort,
                    PromptControlPayload::Content(_),
                ) => Err(CodingSessionError::Input {
                    message: "abort control does not accept structured content".into(),
                }),
            }
            .map_err(|error| match error {
                CodingSessionError::Busy { .. } => {
                    crate::runtime::client::connection::CodingAgentControlRejectionReason::QueueCapacityExceeded
                }
                _ => crate::runtime::client::connection::CodingAgentControlRejectionReason::ControlChannelClosed,
            });
        }
        drop(prompt_binding);

        if kind != crate::runtime::client::connection::CodingAgentControlKind::Abort {
            return Err(
                crate::runtime::client::connection::CodingAgentControlRejectionReason::TargetNotRunning,
            );
        }
        let cancellation_bindings = self
            .operation_cancellations
            .lock_resource("operation cancellation bindings")
            .map_err(|_| {
                crate::runtime::client::connection::CodingAgentControlRejectionReason::ResourceUnavailable
            })?;
        let Some(active) = cancellation_bindings.get(operation_id) else {
            if cancellation_bindings
                .values()
                .any(|active| active.owner.id == handle.id)
            {
                return Err(
                    crate::runtime::client::connection::CodingAgentControlRejectionReason::TargetMismatch,
                );
            }
            return Err(
                crate::runtime::client::connection::CodingAgentControlRejectionReason::TargetNotRunning,
            );
        };
        if active.owner.id != handle.id {
            return Err(
                crate::runtime::client::connection::CodingAgentControlRejectionReason::NotOwner,
            );
        }
        active.cancellation.request().map(|_| ()).map_err(|rejection| {
            match rejection {
                crate::application::operation::control::OperationIdentityRejection::CancellationClosed { .. } => {
                    crate::runtime::client::connection::CodingAgentControlRejectionReason::NoLongerCancellable
                }
                _ => crate::runtime::client::connection::CodingAgentControlRejectionReason::TargetNotRunning,
            }
        })
    }

    pub(crate) fn bind_prompt_control(
        &self,
        owner: ClientHandle,
        operation_id: String,
        channel_generation: crate::application::operation::control::PromptControlGeneration,
        sender: PromptControlHandle,
    ) -> Result<(), CodingSessionError> {
        *self
            .prompt_control
            .lock_resource("prompt control binding")? = Some(PromptControlBinding {
            owner,
            operation_id,
            channel_generation,
            sender,
        });
        Ok(())
    }

    pub(crate) fn clear_prompt_control_if(
        &self,
        operation_id: &str,
        channel_generation: crate::application::operation::control::PromptControlGeneration,
    ) {
        // PromptControlCleanupGuard invokes this from Drop.
        let mut binding = self
            .prompt_control
            .lock_or_recover("prompt control binding");
        if binding.as_ref().is_some_and(|active| {
            active.operation_id == operation_id && active.channel_generation == channel_generation
        }) {
            *binding = None;
        }
    }

    pub(crate) fn bind_operation_cancellation(
        &self,
        owner: ClientHandle,
        operation_id: String,
        cancellation: OperationCancellationHandle,
    ) -> Result<(), CodingSessionError> {
        self.operation_cancellations
            .lock_resource("operation cancellation bindings")?
            .insert(
                operation_id.clone(),
                OperationCancellationBinding {
                    owner: owner.clone(),
                    cancellation,
                },
            );
        let pending_abort = {
            let mut state = self.lock_state()?;
            state.clients.get_mut(&owner.id).and_then(|record| {
                (record.generation == owner.generation
                    && record.pending_abort_operation_id.as_deref() == Some(&operation_id))
                .then(|| record.pending_abort_operation_id.take())
                .flatten()
            })
        };
        if pending_abort.is_some()
            && let Some(active) = self
                .operation_cancellations
                .lock_resource("operation cancellation bindings")?
                .get(&operation_id)
        {
            let _ = active.cancellation.request();
        }
        Ok(())
    }

    pub(crate) fn clear_operation_cancellation_if(&self, operation_id: &str) {
        // Operation guards invoke this from Drop.
        self.operation_cancellations
            .lock_or_recover("operation cancellation bindings")
            .remove(operation_id);
        let mut state = self.state.lock_or_recover("runtime snapshot state");
        for record in state.clients.values_mut() {
            if record.pending_abort_operation_id.as_deref() == Some(operation_id) {
                record.pending_abort_operation_id = None;
            }
        }
    }

    pub(crate) fn subscribe_lifecycle(&self) -> watch::Receiver<u64> {
        self.lifecycle_sender.subscribe()
    }
}
