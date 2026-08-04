use super::*;

impl CodingAgentClientConnection {
    pub(crate) fn handle(&self) -> ClientHandle {
        ClientHandle {
            id: internal_client_id(&self.client_id),
            generation: ClientGeneration(self.generation.0),
        }
    }

    pub fn state(&self) -> Result<CodingAgentSnapshot, CodingAgentPublicError> {
        self.state_internal().map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn state_internal(&self) -> Result<CodingAgentSnapshot, CodingSessionError> {
        self.coordinator
            .client_state(&self.handle())
            .map(public_client_snapshot)
            .map_err(|error| registry_error(&self.client_id, error))
    }

    pub fn pending_tool_authorizations(
        &self,
    ) -> Result<Vec<crate::authorization::ToolAuthorizationRequest>, CodingAgentPublicError> {
        self.pending_tool_authorizations_internal()
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn pending_tool_authorizations_internal(
        &self,
    ) -> Result<Vec<crate::authorization::ToolAuthorizationRequest>, CodingSessionError> {
        Ok(self.state_internal()?.pending_authorizations)
    }

    pub async fn decide_tool_authorization(
        &self,
        identity: &crate::authorization::ToolAuthorizationIdentity,
        decision: crate::authorization::ToolAuthorizationDecision,
    ) -> Result<(), CodingAgentPublicError> {
        self.decide_tool_authorization_internal(identity, decision)
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn decide_tool_authorization_internal(
        &self,
        identity: &crate::authorization::ToolAuthorizationIdentity,
        decision: crate::authorization::ToolAuthorizationDecision,
    ) -> Result<(), CodingSessionError> {
        self.coordinator
            .client_state(&self.handle())
            .map_err(|error| registry_error(&self.client_id, error))?;
        self.authorization_service.decide(identity, decision).await
    }

    /// Switch the interactive permission policy (Plan / Ask / Yolo) for the
    /// runtime session. Takes effect for subsequent tool invocations.
    pub fn set_tool_authorization_mode(
        &self,
        mode: crate::authorization::ToolAuthorizationMode,
    ) -> Result<(), CodingAgentPublicError> {
        self.set_tool_authorization_mode_internal(mode)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn set_tool_authorization_mode_internal(
        &self,
        mode: crate::authorization::ToolAuthorizationMode,
    ) -> Result<(), CodingSessionError> {
        self.coordinator
            .client_state(&self.handle())
            .map_err(|error| registry_error(&self.client_id, error))?;
        self.authorization_service.set_mode(mode)
    }

    pub fn prompt_control(&self, operation_id: impl Into<String>) -> CodingAgentPromptControl {
        CodingAgentPromptControl {
            client_id: self.client_id.clone(),
            generation: self.generation,
            operation_id: operation_id.into(),
            coordinator: self.coordinator.clone(),
        }
    }

    pub fn operation_control(
        &self,
        operation_id: impl Into<String>,
    ) -> CodingAgentOperationControl {
        CodingAgentOperationControl {
            client_id: self.client_id.clone(),
            generation: self.generation,
            operation_id: operation_id.into(),
            coordinator: self.coordinator.clone(),
        }
    }

    pub(crate) fn bind_operation_cancellation(
        &self,
        operation_id: String,
        cancellation: crate::application::operation::control::OperationCancellationHandle,
    ) -> Result<(), CodingSessionError> {
        self.coordinator
            .bind_operation_cancellation(self.handle(), operation_id, cancellation)
    }

    pub fn acknowledge(&self, sequence: u64) -> Result<u64, CodingAgentPublicError> {
        self.acknowledge_internal(sequence)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn acknowledge_internal(&self, sequence: u64) -> Result<u64, CodingSessionError> {
        self.coordinator
            .acknowledge(&self.handle(), sequence)
            .map_err(|error| registry_error(&self.client_id, error))
    }

    pub fn acknowledge_outcome(
        &self,
        acknowledgement: CodingAgentOutcomeAcknowledgementId,
    ) -> Result<(), CodingAgentPublicError> {
        self.acknowledge_outcome_internal(acknowledgement)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn acknowledge_outcome_internal(
        &self,
        acknowledgement: CodingAgentOutcomeAcknowledgementId,
    ) -> Result<(), CodingSessionError> {
        self.coordinator
            .acknowledge_outcome(&self.handle(), &acknowledgement)
            .map_err(|error| registry_error(&self.client_id, error))
    }

    pub fn detach(&self) -> Result<CodingAgentDetachOutcome, CodingAgentPublicError> {
        self.detach_internal().map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn detach_internal(&self) -> Result<CodingAgentDetachOutcome, CodingSessionError> {
        self.coordinator
            .detach(&self.handle())
            .map(|outcome| match outcome {
                ClientDetachOutcome::Detached => CodingAgentDetachOutcome::Detached,
                ClientDetachOutcome::AlreadyDetached => CodingAgentDetachOutcome::AlreadyDetached,
                ClientDetachOutcome::StaleGeneration => CodingAgentDetachOutcome::StaleGeneration,
            })
            .map_err(|error| registry_error(&self.client_id, error))
    }

    pub fn reconnect(
        &self,
        requested_after: u64,
    ) -> Result<CodingAgentReconnect, CodingAgentPublicError> {
        self.reconnect_internal(requested_after)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn reconnect_internal(
        &self,
        requested_after: u64,
    ) -> Result<CodingAgentReconnect, CodingSessionError> {
        match self
            .event_service
            .recovery_boundary_after_for_client(
                &self.handle(),
                ProductEventSequence::new(requested_after),
            )
            .map_err(|error| registry_error(&self.client_id, error))?
        {
            ProductEventRecovery::Ready(boundary) => Ok(CodingAgentReconnect::Replayed {
                events: boundary.replay.into_iter().collect(),
                cursor: CodingAgentSnapshotCursor {
                    stream_id: self.snapshot.cursor.stream_id.clone(),
                    snapshot_protocol_major: UI_SNAPSHOT_PROTOCOL_VERSION.major,
                    last_event_sequence: boundary.replayed_through.get(),
                    last_session_sequence: self.snapshot.cursor.last_session_sequence,
                    capability_generation: boundary.capability_generation,
                },
                receiver: CodingAgentReconnectReceiver {
                    inner: boundary.receiver,
                    lifecycle_receiver: boundary.lifecycle_receiver,
                    lifecycle_epoch: boundary.lifecycle_epoch,
                    coordinator: self.coordinator.clone(),
                    client_id: self.client_id.clone(),
                    handle: self.handle(),
                    last_sequence: boundary.replayed_through.get(),
                    shutdown_delivered: false,
                },
            }),
            ProductEventRecovery::RetainedGap {
                requested_after,
                oldest_available,
            } => {
                let snapshot = self.state_internal()?;
                Ok(CodingAgentReconnect::FreshSnapshotRequired(
                    CodingAgentFreshSnapshotRecovery {
                        requested_sequence: requested_after.get(),
                        oldest_available_sequence: oldest_available.get(),
                        fresh_cursor: snapshot.cursor.clone(),
                        reason: CodingAgentRecoveryReason::RetainedHistoryGap,
                        snapshot: Box::new(snapshot),
                    },
                ))
            }
        }
    }

    pub fn reconnect_from_cursor(
        &self,
        cursor: &CodingAgentSnapshotCursor,
    ) -> Result<CodingAgentReconnect, CodingAgentPublicError> {
        self.reconnect_from_cursor_internal(cursor)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn reconnect_from_cursor_internal(
        &self,
        cursor: &CodingAgentSnapshotCursor,
    ) -> Result<CodingAgentReconnect, CodingSessionError> {
        if cursor.stream_id != self.snapshot.cursor.stream_id {
            return Err(CodingSessionError::Input {
                message: format!(
                    "snapshot cursor belongs to stream {}, expected {}",
                    cursor.stream_id, self.snapshot.cursor.stream_id
                ),
            });
        }
        if cursor.snapshot_protocol_major != UI_SNAPSHOT_PROTOCOL_VERSION.major {
            return Err(CodingSessionError::UnsupportedProtocolVersion {
                family: UI_SNAPSHOT_PROTOCOL_VERSION.family.into(),
                requested: format!(
                    "{}.{}.{}",
                    UI_SNAPSHOT_PROTOCOL_VERSION.family, cursor.snapshot_protocol_major, 0
                ),
                supported: UI_SNAPSHOT_PROTOCOL_VERSION.to_string(),
            });
        }
        self.reconnect_internal(cursor.last_event_sequence)
    }

    pub fn set_prompt_draft(
        &self,
        id: CodingAgentDraftId,
        text: impl Into<String>,
    ) -> Result<(), CodingAgentPublicError> {
        self.set_prompt_draft_internal(id, text)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn set_prompt_draft_internal(
        &self,
        id: CodingAgentDraftId,
        text: impl Into<String>,
    ) -> Result<(), CodingSessionError> {
        let text = text.into();
        validate_submission_draft(&id, &text)?;
        self.coordinator
            .set_prompt_draft(
                &self.handle(),
                Some(DraftRecord {
                    id: id.0,
                    kind: ClientDraftKind::Prompt,
                    fingerprint:
                        crate::application::operation::contract::prompt_text_submission_fingerprint(
                            &text,
                        ),
                    text,
                }),
            )
            .map_err(|error| registry_error(&self.client_id, error))
    }

    pub fn enqueue_control_draft(
        &self,
        draft: CodingAgentDraft,
    ) -> Result<(), CodingAgentMutationRejection> {
        let kind = match draft.kind {
            CodingAgentDraftKind::Steer => ClientDraftKind::Steer,
            CodingAgentDraftKind::FollowUp => ClientDraftKind::FollowUp,
            CodingAgentDraftKind::Prompt => return Err(CodingAgentMutationRejection::InvalidInput),
        };
        self.coordinator
            .enqueue_draft(
                &self.handle(),
                DraftRecord {
                    id: draft.id.0,
                    kind,
                    fingerprint: draft.text.clone(),
                    text: draft.text,
                },
            )
            .map_err(|error| match error {
                ClientRegistryError::QueueCapacityExceeded { .. } => {
                    CodingAgentMutationRejection::QueueCapacity
                }
                ClientRegistryError::Lifecycle(
                    crate::kernel::error::CodingAgentLifecycleRejection::Detached,
                ) => CodingAgentMutationRejection::Detached,
                ClientRegistryError::Lifecycle(
                    crate::kernel::error::CodingAgentLifecycleRejection::StaleGeneration,
                ) => CodingAgentMutationRejection::StaleGeneration,
                ClientRegistryError::Lifecycle(
                    crate::kernel::error::CodingAgentLifecycleRejection::RuntimeShutDown,
                ) => CodingAgentMutationRejection::RuntimeShutDown,
                _ => CodingAgentMutationRejection::InvalidInput,
            })
    }

    pub fn clear_control_drafts(&self) -> Result<(), CodingAgentPublicError> {
        self.clear_control_drafts_internal()
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn clear_control_drafts_internal(&self) -> Result<(), CodingSessionError> {
        self.coordinator
            .clear_control_drafts(&self.handle())
            .map_err(|error| registry_error(&self.client_id, error))
    }

    /// Prepare one client-owned operation and its optional prompt draft for canonical admission.
    pub fn prepare_client_submission(
        &self,
        session: &mut crate::runtime::facade::CodingAgentSession,
        draft: Option<CodingAgentSubmissionDraft>,
        operation: crate::runtime::facade::CodingAgentOperation,
    ) -> Result<CodingAgentPreparedSubmission, CodingAgentPublicError> {
        self.prepare_client_submission_internal(session, draft, operation)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn prepare_client_submission_internal(
        &self,
        session: &mut crate::runtime::facade::CodingAgentSession,
        draft: Option<CodingAgentSubmissionDraft>,
        operation: crate::runtime::facade::CodingAgentOperation,
    ) -> Result<CodingAgentPreparedSubmission, CodingSessionError> {
        let descriptor = operation.descriptor();
        let prompt_fingerprint = operation.submission_fingerprint();
        let expected_prompt_draft = match (
            descriptor.submitted_kind,
            draft,
            prompt_fingerprint.as_ref(),
        ) {
            (
                crate::kernel::operation::OperationKind::Prompt,
                Some(draft),
                Some((_, fingerprint)),
            ) => {
                validate_submission_draft(&draft.id, &draft.display_text)?;
                Some(DraftRecord {
                    id: draft.id.0,
                    kind: ClientDraftKind::Prompt,
                    text: draft.display_text,
                    fingerprint: fingerprint.clone(),
                })
            }
            (crate::kernel::operation::OperationKind::Prompt, None, _) => {
                return Err(CodingSessionError::Input {
                    message: "prompt submission requires a client draft".into(),
                });
            }
            (crate::kernel::operation::OperationKind::Prompt, Some(_), None) => {
                return Err(CodingSessionError::Input {
                    message: "prompt submission requires a fingerprintable invocation".into(),
                });
            }
            (_, None, _) => None,
            (_, Some(_), _) => {
                return Err(CodingSessionError::Input {
                    message: "only prompt submissions accept a client draft".into(),
                });
            }
        };
        let handle = self.handle();
        self.coordinator
            .validate_submission_slot(&handle)
            .map_err(|error| submission_preparation_error(&self.client_id, error))?;
        let (shared, operation_id) = session.install_submission_lease(
            handle.clone(),
            descriptor,
            prompt_fingerprint,
            expected_prompt_draft.clone(),
        )?;
        let lease = CodingAgentSubmissionLease { shared };
        if let Err(error) =
            self.coordinator
                .register_prepared_submission(&handle, operation_id.clone(), descriptor)
        {
            lease.abandon_if_prepared();
            session.discard_submission_lease(&lease.shared);
            return Err(registry_error(&self.client_id, error));
        }
        if let Some(draft) = expected_prompt_draft
            && let Err(error) = self.coordinator.set_prompt_draft(&handle, Some(draft))
        {
            lease.abandon_if_prepared();
            session.discard_submission_lease(&lease.shared);
            self.coordinator
                .abandon_prepared_submission(&handle, &operation_id);
            return Err(registry_error(&self.client_id, error));
        }
        Ok(CodingAgentPreparedSubmission {
            operation_id,
            operation: Some(operation),
            lease,
            owner: self.coordinator.clone(),
            owner_handle: handle,
        })
    }
}
