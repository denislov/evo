use super::*;

impl SessionService {
    pub(crate) async fn finalize_prompt_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
        outcome: &InternalPromptTurnOutcome,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        let operation_id = operation_id.into();
        match outcome {
            InternalPromptTurnOutcome::Success { .. } => {
                self.commit_prompt_transaction(transaction, operation_id)
                    .await
            }
            InternalPromptTurnOutcome::Aborted { reason, .. } => {
                self.abort_prompt_transaction(transaction, operation_id, reason.clone())
                    .await
            }
            InternalPromptTurnOutcome::Failed { error, .. } => {
                self.fail_prompt_transaction(
                    transaction,
                    operation_id,
                    error.code(),
                    error.to_string(),
                )
                .await
            }
        }
    }

    pub(crate) async fn commit_prompt_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        let fallback_operation_id = operation_id.into();
        let Some(mut transaction) = transaction else {
            return Ok(Self::skipped_write(
                fallback_operation_id,
                "no active prompt transaction",
            ));
        };

        let operation_id = transaction.operation_id().to_owned();
        let session_id = self.session_id().to_owned();
        let new_leaf_id = Some(Self::next_leaf_id());
        let mut events = vec![EventService::session_write_pending_event(
            operation_id.clone(),
        )];
        let (committed, outbox_intent) = session_write_outbox_intent(&session_id, &operation_id);
        transaction
            .commit_with_outbox(new_leaf_id.clone(), outbox_intent)
            .await?;
        self.observe_committed_sequence(transaction.committed_session_sequence());
        events.push(committed);
        Ok(FinalizedSessionWrite {
            events,
            session_id: Some(session_id),
            leaf_id: new_leaf_id,
            committed_session_sequence: transaction.committed_session_sequence(),
        })
    }

    pub(crate) async fn fail_prompt_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
        error_code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        self.fail_non_leaf_transaction(
            transaction,
            operation_id,
            error_code,
            message,
            "no active prompt transaction",
        )
        .await
    }

    pub(crate) async fn commit_manual_compaction_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        self.commit_non_leaf_transaction(
            transaction,
            operation_id,
            "no active manual compaction transaction",
        )
        .await
    }

    pub(crate) async fn commit_branch_summary_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        self.commit_non_leaf_transaction(
            transaction,
            operation_id,
            "no active branch summary transaction",
        )
        .await
    }

    pub(crate) async fn commit_self_healing_edit_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        self.commit_non_leaf_transaction(
            transaction,
            operation_id,
            "no active self-healing edit transaction",
        )
        .await
    }

    pub(crate) async fn fail_self_healing_edit_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
        error_code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        self.fail_non_leaf_transaction(
            transaction,
            operation_id,
            error_code,
            message,
            "no active self-healing edit transaction",
        )
        .await
    }

    pub(super) async fn commit_non_leaf_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
        missing_transaction_reason: &'static str,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        let fallback_operation_id = operation_id.into();
        let Some(mut transaction) = transaction else {
            return Ok(Self::skipped_write(
                fallback_operation_id,
                missing_transaction_reason,
            ));
        };

        let operation_id = transaction.operation_id().to_owned();
        let session_id = self.session_id().to_owned();
        let mut events = vec![EventService::session_write_pending_event(
            operation_id.clone(),
        )];
        let (committed, outbox_intent) = session_write_outbox_intent(&session_id, &operation_id);
        transaction.commit_with_outbox(None, outbox_intent).await?;
        self.observe_committed_sequence(transaction.committed_session_sequence());
        self.commit_writer_mutation(
            Vec::new(),
            ManifestPatch::new().updated_at(SystemClock.now_rfc3339()),
            Some(operation_id.clone()),
        )
        .await?;
        events.push(committed);
        Ok(FinalizedSessionWrite {
            events,
            session_id: Some(session_id),
            leaf_id: self.current_active_leaf_id()?,
            committed_session_sequence: transaction.committed_session_sequence(),
        })
    }

    pub(super) async fn fail_non_leaf_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
        error_code: impl Into<String>,
        message: impl Into<String>,
        missing_transaction_reason: &'static str,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        let fallback_operation_id = operation_id.into();
        let Some(mut transaction) = transaction else {
            return Ok(Self::skipped_write(
                fallback_operation_id,
                missing_transaction_reason,
            ));
        };

        let operation_id = transaction.operation_id().to_owned();
        let session_id = self.session_id().to_owned();
        let mut events = vec![EventService::session_write_pending_event(
            operation_id.clone(),
        )];
        let (committed, outbox_intent) = session_write_outbox_intent(&session_id, &operation_id);
        transaction
            .fail_with_outbox(error_code, message, outbox_intent)
            .await?;
        self.observe_committed_sequence(transaction.committed_session_sequence());
        self.commit_writer_mutation(
            Vec::new(),
            ManifestPatch::new().updated_at(SystemClock.now_rfc3339()),
            Some(operation_id.clone()),
        )
        .await?;
        events.push(committed);
        Ok(FinalizedSessionWrite {
            events,
            session_id: Some(session_id),
            leaf_id: self.current_active_leaf_id()?,
            committed_session_sequence: transaction.committed_session_sequence(),
        })
    }

    pub(crate) async fn abort_prompt_transaction(
        &mut self,
        transaction: Option<PromptTurnTransaction>,
        operation_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<FinalizedSessionWrite, CodingSessionError> {
        let fallback_operation_id = operation_id.into();
        let Some(mut transaction) = transaction else {
            return Ok(Self::skipped_write(
                fallback_operation_id,
                "no active prompt transaction",
            ));
        };

        let operation_id = transaction.operation_id().to_owned();
        let session_id = self.session_id().to_owned();
        let mut events = vec![EventService::session_write_pending_event(
            operation_id.clone(),
        )];
        let (committed, outbox_intent) = session_write_outbox_intent(&session_id, &operation_id);
        transaction.abort_with_outbox(reason, outbox_intent).await?;
        self.observe_committed_sequence(transaction.committed_session_sequence());
        events.push(committed);
        Ok(FinalizedSessionWrite {
            events,
            session_id: Some(session_id),
            leaf_id: None,
            committed_session_sequence: transaction.committed_session_sequence(),
        })
    }

    pub(crate) fn skip_prompt_transaction(
        operation_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> FinalizedSessionWrite {
        Self::skipped_write(operation_id, reason)
    }

    pub(crate) fn failed_prompt_transaction(
        operation_id: impl Into<String>,
        error: &CodingSessionError,
    ) -> FinalizedSessionWrite {
        let operation_id = operation_id.into();
        let status = if matches!(error, CodingSessionError::PartialCommit { .. }) {
            CodingAgentSessionWriteFailureStatus::Uncertain
        } else {
            CodingAgentSessionWriteFailureStatus::Definite
        };
        let failure_reason = matches!(
            error,
            CodingSessionError::SessionWriteFailure {
                reason: SessionWriteFailureReason::QueueSaturated,
                ..
            }
        )
        .then_some(CodingAgentSessionWriteFailureReason::QueueSaturated);
        FinalizedSessionWrite {
            events: vec![
                EventService::session_write_pending_event(operation_id.clone()),
                EventService::session_write_failed_event(
                    operation_id,
                    error.to_string(),
                    status,
                    failure_reason,
                ),
            ],
            session_id: None,
            leaf_id: None,
            committed_session_sequence: None,
        }
    }

    pub(crate) fn recovery_id_for_uncertain_operation(
        &self,
        operation_id: &str,
    ) -> Result<String, CodingSessionError> {
        let outbox = self.store.read_outbox(&self.handle)?;
        if let Some(recovery_id) = outbox.iter().find_map(|record| {
            if record.operation_id.as_deref() != Some(operation_id)
                || record.kind != DurableOutboxRecordKind::Recovery
            {
                return None;
            }
            match &record.draft.event {
                crate::events::CodingAgentProductEventKind::Workflow(
                    crate::events::CodingAgentWorkflowProductEvent::OperationRecoveryPending {
                        recovery_id,
                        ..
                    },
                ) => Some(recovery_id.clone()),
                _ => None,
            }
        }) {
            return Ok(recovery_id);
        }
        if let Some(record) = outbox.into_iter().find(|record| {
            record.operation_id.as_deref() == Some(operation_id)
                && record.kind == DurableOutboxRecordKind::SessionWrite
        }) {
            return Ok(format!("recovery_pending:{}", record.record_id));
        }
        let has_durable_fact = self
            .store
            .read_events(&self.handle)?
            .into_iter()
            .any(|event| event.operation_id.as_deref() == Some(operation_id));
        if has_durable_fact {
            return Ok(format!(
                "recovery_pending:{}/{}",
                self.session_id(),
                operation_id
            ));
        }
        Err(CodingSessionError::PartialCommit {
            operation_id: operation_id.to_owned(),
            message: "partial commit has no durable fact or outbox evidence".into(),
        })
    }

    pub(crate) async fn persist_terminal_decision(
        &self,
        decision: &FinalizationDecision,
        draft: ProductEventDraft,
    ) -> Result<(), CodingSessionError> {
        let mut ids = SystemIdGenerator;
        let event = SessionEventEnvelope::new(
            self.session_id(),
            ids.next_event_id(),
            SystemClock.now_rfc3339(),
            SessionEventData::OperationTerminalRecorded {
                status: decision.terminal_status.as_str().into(),
                semantic_event_id: decision.semantic_event_id.clone(),
            },
        )
        .with_operation_id(decision.operation_id.clone());
        let intent = DurableOutboxRecordCandidate::new(
            decision.semantic_event_id.clone(),
            self.session_id().to_owned(),
            Some(decision.operation_id.clone()),
            vec![event.event_id.clone()],
            DurableOutboxRecordKind::OperationTerminal,
            draft.with_durable_session(self.session_id()),
        )
        .map_err(|message| CodingSessionError::Session {
            message: message.into(),
        })?
        .with_operation_kind(decision.operation_kind.as_str());
        let receipt = self
            .transaction_writer
            .commit_session_mutation_with_outbox(
                vec![event],
                vec![intent],
                ManifestPatch::new().updated_at(SystemClock.now_rfc3339()),
                Some(decision.operation_id.clone()),
            )
            .await?;
        observe_commit_receipt(&self.committed_session_sequence, receipt);
        Ok(())
    }

    pub(super) fn observe_committed_sequence(&self, sequence: Option<u64>) {
        if let Some(sequence) = sequence {
            self.committed_session_sequence
                .fetch_max(sequence, Ordering::AcqRel);
        }
    }

    pub(crate) fn inspect_recovery_pending(
        &self,
    ) -> Result<Vec<RecoveryPendingInspection>, CodingSessionError> {
        let replay = self.replay()?;
        let outbox = self.store.read_outbox(&self.handle)?;
        let mut pending_operation_ids = replay
            .recovery_summary()
            .in_doubt_operations
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        for record in outbox.iter().filter(|record| {
            record.kind == DurableOutboxRecordKind::SessionWrite
                && matches!(
                    record.draft.durability,
                    CodingAgentProductEventDurability::PersistenceUncertain { .. }
                )
        }) {
            if replay
                .operation_statuses
                .get(record.operation_id.as_deref().unwrap_or_default())
                .is_none_or(|status| {
                    !matches!(
                        status,
                        crate::session::replay::OperationReplayStatus::Recovered
                            | crate::session::replay::OperationReplayStatus::Committed
                            | crate::session::replay::OperationReplayStatus::Failed
                            | crate::session::replay::OperationReplayStatus::Aborted
                    )
                })
                && let Some(operation_id) = &record.operation_id
            {
                pending_operation_ids.insert(operation_id.clone());
            }
        }
        let durable_events = self.store.read_events(&self.handle)?;
        let operation_facts = durable_events
            .iter()
            .filter_map(|event| match &event.data {
                SessionEventData::OperationStarted {
                    operation,
                    runtime_generation,
                } => event.operation_id.clone().map(|id| {
                    (
                        id,
                        (operation.clone(), runtime_generation.capability_generation),
                    )
                }),
                _ => None,
            })
            .collect::<std::collections::HashMap<_, _>>();
        let pending_facts = durable_events
            .into_iter()
            .filter_map(|event| match event.data {
                SessionEventData::OperationRecoveryPending {
                    recovery_id,
                    record_version,
                    descriptor_revision,
                    capability_generation,
                    attempt_count,
                    last_attempt_at,
                    next_attempt_at,
                    ..
                } => event.operation_id.map(|id| {
                    (
                        id,
                        (
                            recovery_id,
                            record_version,
                            descriptor_revision,
                            capability_generation,
                            attempt_count,
                            last_attempt_at,
                            next_attempt_at,
                        ),
                    )
                }),
                _ => None,
            })
            .collect::<std::collections::HashMap<_, _>>();
        pending_operation_ids
            .into_iter()
            .map(|operation_id| {
                let operation_kind = operation_facts
                    .get(&operation_id)
                    .map(|(kind, _)| persisted_operation_kind_name(kind));
                let operation_capability_generation = operation_facts
                    .get(&operation_id)
                    .and_then(|(_, generation)| *generation);
                let (
                    recovery_id,
                    record_version,
                    descriptor_revision,
                    capability_generation,
                    attempt_count,
                    last_attempt_at,
                    next_attempt_at,
                ) = pending_facts.get(&operation_id).cloned().unwrap_or((
                    self.recovery_id_for_uncertain_operation(&operation_id)?,
                    RECOVERY_RECORD_VERSION,
                    crate::kernel::operation::OPERATION_DESCRIPTOR_REVISION,
                    operation_capability_generation,
                    0,
                    None,
                    None,
                ));
                Ok(RecoveryPendingInspection {
                    operation_id,
                    recovery_id,
                    operation_kind,
                    record_version,
                    descriptor_revision,
                    capability_generation,
                    attempt_count,
                    last_attempt_at,
                    next_attempt_at,
                })
            })
            .collect()
    }

    pub(crate) async fn resolve_recovery_as(
        &self,
        request: &CodingAgentRecoveryResolutionRequest,
        authorization_subject: &str,
    ) -> Result<RecoveryResolutionCommit, CodingSessionError> {
        let pending = self
            .inspect_recovery_pending()?
            .into_iter()
            .find(|pending| pending.recovery_id == request.recovery_id)
            .ok_or_else(|| CodingSessionError::Input {
                message: format!(
                    "unknown or already resolved recovery: {}",
                    request.recovery_id
                ),
            })?;
        if pending.operation_id != request.operation_id {
            return Err(CodingSessionError::Input {
                message: "recovery operation identity mismatch".into(),
            });
        }
        if pending.record_version != request.expected_record_version {
            return Err(CodingSessionError::Input {
                message: "recovery record version is stale".into(),
            });
        }
        if pending.descriptor_revision != request.expected_descriptor_revision {
            return Err(CodingSessionError::Input {
                message: "recovery descriptor revision is stale".into(),
            });
        }
        if pending.capability_generation != request.expected_capability_generation {
            return Err(CodingSessionError::Input {
                message: "recovery capability generation is stale".into(),
            });
        }
        if pending.attempt_count != request.expected_attempt_count {
            return Err(CodingSessionError::Input {
                message: "recovery attempt count is stale".into(),
            });
        }
        let reason = request.reason.trim();
        if reason.is_empty() {
            return Err(CodingSessionError::Input {
                message: "recovery resolution reason must not be empty".into(),
            });
        }
        if reason.chars().count() > 1_200 {
            return Err(CodingSessionError::Input {
                message: "recovery resolution reason exceeds 1200 characters".into(),
            });
        }
        let reason = observability::scrub_sensitive_text(reason);
        let operation_kind = self
            .store
            .read_events(&self.handle)?
            .into_iter()
            .find_map(|event| match event.data {
                SessionEventData::OperationStarted { operation, .. }
                    if event.operation_id.as_deref() == Some(request.operation_id.as_str()) =>
                {
                    Some(operation)
                }
                _ => None,
            })
            .ok_or_else(|| CodingSessionError::Session {
                message: "recovery resolution requires the original operation kind".into(),
            })?;
        if matches!(
            operation_kind,
            crate::session::event::OperationKind::Other { .. }
                | crate::session::event::OperationKind::SessionTreeLabel
        ) {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: "recovery resolution requires a durable root operation family".into(),
            });
        }
        let persisted_resolution = match request.resolution {
            crate::events::CodingAgentRecoveryResolution::Failed => {
                crate::session::event::PersistedRecoveryResolution::Failed
            }
            crate::events::CodingAgentRecoveryResolution::Aborted => {
                crate::session::event::PersistedRecoveryResolution::Aborted
            }
        };
        let session_id = self.session_id().to_owned();
        let observed_at = SystemClock.now_rfc3339();
        let semantic_event_id = format!(
            "{}/{}/recovery_resolution/v{}",
            session_id, request.operation_id, pending.record_version
        );
        let mut ids = SystemIdGenerator;
        let audit_event = SessionEventEnvelope::new(
            session_id.clone(),
            ids.next_event_id(),
            observed_at.clone(),
            SessionEventData::OperationRecoveryResolved {
                recovery_id: pending.recovery_id.clone(),
                record_version: pending.record_version,
                descriptor_revision: pending.descriptor_revision,
                capability_generation: pending.capability_generation,
                resolution: persisted_resolution,
                reason: reason.clone(),
                authorization_subject: authorization_subject.to_owned(),
            },
        )
        .with_operation_id(request.operation_id.clone());
        let status_event = SessionEventEnvelope::new(
            session_id.clone(),
            ids.next_event_id(),
            observed_at.clone(),
            match request.resolution {
                crate::events::CodingAgentRecoveryResolution::Failed => {
                    SessionEventData::OperationFailed {
                        error_code: "recovery_resolved".into(),
                        message: reason.clone(),
                    }
                }
                crate::events::CodingAgentRecoveryResolution::Aborted => {
                    SessionEventData::OperationAborted {
                        reason: reason.clone(),
                    }
                }
            },
        )
        .with_operation_id(request.operation_id.clone());
        let terminal_event = SessionEventEnvelope::new(
            session_id.clone(),
            ids.next_event_id(),
            observed_at.clone(),
            SessionEventData::OperationTerminalRecorded {
                status: match request.resolution {
                    crate::events::CodingAgentRecoveryResolution::Failed => "failed",
                    crate::events::CodingAgentRecoveryResolution::Aborted => "aborted",
                }
                .into(),
                semantic_event_id: semantic_event_id.clone(),
            },
        )
        .with_operation_id(request.operation_id.clone());
        let draft = crate::events::recovery::RecoveryResolvedEvent {
            operation_id: request.operation_id.clone(),
            recovery_id: pending.recovery_id.clone(),
            resolution: request.resolution,
            reason,
            session_id: session_id.clone(),
            record_version: pending.record_version,
            descriptor_revision: pending.descriptor_revision,
            capability_generation: pending.capability_generation,
        }
        .into_product_draft();
        let source_event_ids = vec![
            audit_event.event_id.clone(),
            status_event.event_id.clone(),
            terminal_event.event_id.clone(),
        ];
        let outbox = DurableOutboxRecordCandidate::new(
            semantic_event_id,
            session_id,
            Some(request.operation_id.clone()),
            source_event_ids,
            DurableOutboxRecordKind::OperationTerminal,
            draft.clone(),
        )
        .map_err(|message| CodingSessionError::Session {
            message: message.into(),
        })?
        .with_operation_kind(persisted_operation_kind_name(&operation_kind));
        self.commit_writer_mutation_with_outbox(
            vec![audit_event, status_event, terminal_event],
            vec![outbox],
            ManifestPatch::new().updated_at(observed_at),
            Some(request.operation_id.clone()),
        )
        .await?;
        Ok(RecoveryResolutionCommit {
            operation_id: request.operation_id.clone(),
            recovery_id: pending.recovery_id,
            resolution: request.resolution,
            operation_kind,
            draft,
        })
    }

    pub(crate) fn take_startup_recovery_markers(&mut self) -> Vec<StartupRecoveryMarker> {
        std::mem::take(&mut self.startup_recovery_markers)
    }

    pub(super) fn skipped_write(
        operation_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> FinalizedSessionWrite {
        FinalizedSessionWrite {
            events: vec![EventService::session_write_skipped_event(
                operation_id,
                reason,
            )],
            session_id: None,
            leaf_id: None,
            committed_session_sequence: None,
        }
    }
}

impl TransientSessionState {
    pub(crate) fn new(default_agent_profile_id: ProfileId) -> Self {
        let mut ids = SystemIdGenerator;
        Self {
            runtime_id: format!("runtime_{}", ids.next_session_id()),
            transcript: Vec::new(),
            default_agent_profile_id,
        }
    }

    pub(crate) fn finalize_prompt_transaction(
        &mut self,
        context: &PromptTurnContext,
        outcome: &InternalPromptTurnOutcome,
    ) -> FinalizedSessionWrite {
        if outcome.is_success() {
            self.transcript.extend(context.completed_transcript_items());
        }
        SessionService::skip_prompt_transaction(
            context.operation_id().to_owned(),
            "session persistence disabled",
        )
    }
}

fn persisted_operation_kind_name(kind: &OperationKind) -> String {
    match kind {
        OperationKind::Prompt => "prompt".into(),
        OperationKind::ManualCompaction => "compact".into(),
        OperationKind::BranchSummary => "branch_summary".into(),
        OperationKind::Export => "export".into(),
        OperationKind::SelfHealingEdit => "self_healing_edit".into(),
        OperationKind::SessionTreeLabel => "session_tree_label".into(),
        OperationKind::Other { name } => name.clone(),
    }
}

fn session_write_outbox_intent(
    session_id: &str,
    operation_id: &str,
) -> (SessionWriteEvent, DurableOutboxIntent) {
    let committed =
        EventService::session_write_committed_event(operation_id.to_owned(), session_id.to_owned());
    let intent = DurableOutboxIntent::new(
        format!("{session_id}/{operation_id}/session_write_committed"),
        DurableOutboxRecordKind::SessionWrite,
        committed.clone().into_product_draft(),
    );
    (committed, intent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_prompt_transaction_transition_table() {
        let cases = [
            (
                "definite input failure",
                CodingSessionError::Input {
                    message: "invalid request".into(),
                },
                CodingAgentSessionWriteFailureStatus::Definite,
                None,
            ),
            (
                "definite queue saturation",
                CodingSessionError::SessionWriteFailure {
                    reason: SessionWriteFailureReason::QueueSaturated,
                    message: "writer queue is full".into(),
                },
                CodingAgentSessionWriteFailureStatus::Definite,
                Some(CodingAgentSessionWriteFailureReason::QueueSaturated),
            ),
            (
                "uncertain partial commit",
                CodingSessionError::PartialCommit {
                    operation_id: "operation-3".into(),
                    message: "commit acknowledgement was lost".into(),
                },
                CodingAgentSessionWriteFailureStatus::Uncertain,
                None,
            ),
        ];

        for (name, error, expected_status, expected_reason) in cases {
            let write = SessionService::failed_prompt_transaction("operation", &error);
            assert_eq!(write.events.len(), 2, "{name}");
            assert!(
                matches!(
                    &write.events[0],
                    SessionWriteEvent::Pending { operation_id } if operation_id == "operation"
                ),
                "{name}"
            );
            assert!(
                matches!(
                    &write.events[1],
                    SessionWriteEvent::Failed {
                        operation_id,
                        status,
                        failure_reason,
                        ..
                    } if operation_id == "operation"
                        && *status == expected_status
                        && *failure_reason == expected_reason
                ),
                "{name}: {:?}",
                write.events[1]
            );
            assert_eq!(write.session_id, None, "{name}");
            assert_eq!(write.leaf_id, None, "{name}");
            assert_eq!(write.committed_session_sequence, None, "{name}");
        }
    }

    #[test]
    fn skipped_prompt_transaction_transition_table() {
        let cases = [
            ("missing transaction", "no active prompt transaction"),
            ("persistence disabled", "session persistence disabled"),
            ("explicit policy", "write intentionally skipped"),
        ];

        for (operation_id, reason) in cases {
            let write = SessionService::skip_prompt_transaction(operation_id, reason);
            assert_eq!(
                write.events,
                vec![SessionWriteEvent::Skipped {
                    operation_id: operation_id.into(),
                    reason: reason.into(),
                }]
            );
            assert_eq!(write.session_id, None);
            assert_eq!(write.leaf_id, None);
            assert_eq!(write.committed_session_sequence, None);
        }
    }
}
