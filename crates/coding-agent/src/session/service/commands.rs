use super::*;
use crate::session::rewind::{self, RewindCheckpoint};

impl SessionService {
    pub(crate) async fn set_tree_label(
        &mut self,
        entry_id: &str,
        label: Option<String>,
        operation_id: &str,
    ) -> Result<SessionTreeLabelUpdate, CodingSessionError> {
        let entry_id = normalize_tree_entry_id(entry_id)?;
        let label = normalize_tree_label(label);
        let source_events = self.store.read_events(&self.handle)?;
        if committed_leaf_cutoff(&source_events, &entry_id).is_none() {
            return Err(CodingSessionError::Session {
                message: format!("tree entry id not found in session: {entry_id}"),
            });
        }

        let session_id = self.session_id().to_owned();
        let mut ids = SystemIdGenerator;
        let updated_at = SystemClock.now_rfc3339();
        let events = vec![
            SessionEventEnvelope::new(
                session_id.clone(),
                ids.next_event_id(),
                updated_at.clone(),
                SessionEventData::OperationStarted {
                    operation: OperationKind::SessionTreeLabel,
                    runtime_generation: Default::default(),
                },
            )
            .with_operation_id(operation_id),
            SessionEventEnvelope::new(
                session_id.clone(),
                ids.next_event_id(),
                updated_at.clone(),
                SessionEventData::SessionTreeLabelUpdated {
                    entry_id: entry_id.clone(),
                    label: label.clone(),
                },
            )
            .with_operation_id(operation_id),
            SessionEventEnvelope::new(
                session_id.clone(),
                ids.next_event_id(),
                updated_at.clone(),
                SessionEventData::OperationCommitted { new_leaf_id: None },
            )
            .with_operation_id(operation_id),
        ];
        self.commit_writer_mutation(
            events,
            ManifestPatch::new().updated_at(updated_at.clone()),
            Some(operation_id.to_owned()),
        )
        .await?;
        Ok(SessionTreeLabelUpdate {
            entry_id,
            label,
            updated_at,
        })
    }

    pub(crate) async fn set_session_name(
        &mut self,
        name: Option<String>,
        operation_id: &str,
    ) -> Result<SessionNameUpdate, CodingSessionError> {
        let name = normalize_session_name(name);
        let updated_at = SystemClock.now_rfc3339();
        self.commit_writer_mutation(
            Vec::new(),
            ManifestPatch::new()
                .updated_at(updated_at.clone())
                .name(name.clone()),
            Some(operation_id.to_owned()),
        )
        .await?;
        let update = SessionNameUpdate { name, updated_at };
        self.session_name_updates.send_replace(update.clone());
        Ok(update)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "durable delegation confirmation records retain every typed request fact"
    )]
    pub(crate) async fn record_delegation_confirmation_requested(
        &mut self,
        source_operation_id: String,
        turn_id: String,
        tool_call_id: String,
        requesting_profile_id: ProfileId,
        target_kind: ProfileKind,
        target_id: ProfileId,
        task: String,
        reason: String,
        runtime_seed: PersistedDelegationRuntimeSeed,
    ) -> Result<(), CodingSessionError> {
        self.append_durable_session_event(
            Some(source_operation_id.clone()),
            Some(turn_id.clone()),
            SessionEventData::DelegationConfirmationRequested {
                source_operation_id,
                turn_id,
                tool_call_id,
                requesting_profile_id,
                target_kind,
                target_id,
                task,
                reason,
                runtime_seed,
            },
        )
        .await
    }

    pub(crate) async fn record_delegation_confirmation_approved(
        &mut self,
        source_operation_id: String,
        tool_call_id: String,
        approval_operation_id: String,
    ) -> Result<(), CodingSessionError> {
        self.append_durable_session_event(
            Some(source_operation_id.clone()),
            None,
            SessionEventData::DelegationConfirmationApproved {
                source_operation_id,
                tool_call_id,
                approval_operation_id,
            },
        )
        .await
    }

    pub(crate) async fn record_delegation_confirmation_rejected(
        &mut self,
        source_operation_id: String,
        tool_call_id: String,
        reason: String,
    ) -> Result<(), CodingSessionError> {
        self.append_durable_session_event(
            Some(source_operation_id.clone()),
            None,
            SessionEventData::DelegationConfirmationRejected {
                source_operation_id,
                tool_call_id,
                reason,
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn record_delegation_folded_update(
        &mut self,
        tool_call_id: String,
        requesting_profile_id: ProfileId,
        target_kind: ProfileKind,
        target_id: ProfileId,
        task: String,
        status: PersistedDelegationStatus,
        child_operation_id: Option<String>,
        summary: Option<String>,
    ) -> Result<(), CodingSessionError> {
        self.append_durable_session_event(
            None,
            None,
            SessionEventData::DelegationFoldedUpdated {
                tool_call_id,
                requesting_profile_id,
                target_kind,
                target_id,
                task,
                status,
                child_operation_id,
                summary,
            },
        )
        .await
    }

    pub(crate) async fn switch_active_leaf(
        &mut self,
        target_leaf_id: &str,
        operation_id: &str,
    ) -> Result<(), CodingSessionError> {
        let target_leaf_id = normalize_leaf_id(target_leaf_id)?;
        let events = self.store.read_events(&self.handle)?;
        if committed_leaf_cutoff(&events, &target_leaf_id).is_none() {
            return Err(CodingSessionError::Session {
                message: format!("leaf id not found in session: {target_leaf_id}"),
            });
        }

        let session_id = self.session_id().to_owned();
        let mut ids = SystemIdGenerator;
        let clock = SystemClock;
        let updated_at = clock.now_rfc3339();
        let event = SessionEventEnvelope::new(
            session_id.clone(),
            ids.next_event_id(),
            updated_at.clone(),
            SessionEventData::ActiveLeafChanged {
                leaf_id: target_leaf_id.clone(),
            },
        );
        self.commit_writer_mutation(
            vec![event],
            ManifestPatch::new()
                .updated_at(updated_at)
                .active_leaf_id(Some(target_leaf_id)),
            Some(operation_id.to_owned()),
        )
        .await?;
        Ok(())
    }

    pub(crate) fn begin_prompt_transaction_with_snapshot(
        &self,
        snapshot: &OperationCapabilitySnapshot,
    ) -> PromptTurnTransaction {
        TurnTransaction::begin_admitted_with_runtime_generation(
            self.transaction_writer(),
            self.session_id().to_owned(),
            SystemIdGenerator,
            SystemClock,
            OperationKind::Prompt,
            snapshot.persisted_runtime_generation_ref(),
            snapshot.operation_id.clone(),
            self.active_branch_id(),
        )
    }

    pub(crate) fn begin_manual_compaction_transaction(
        &self,
        snapshot: &OperationCapabilitySnapshot,
    ) -> PromptTurnTransaction {
        TurnTransaction::begin_admitted_with_runtime_generation(
            self.transaction_writer(),
            self.session_id().to_owned(),
            SystemIdGenerator,
            SystemClock,
            OperationKind::ManualCompaction,
            snapshot.persisted_runtime_generation_ref(),
            snapshot.operation_id.clone(),
            self.active_branch_id(),
        )
    }

    pub(crate) fn begin_branch_summary_transaction(
        &self,
        snapshot: &OperationCapabilitySnapshot,
    ) -> PromptTurnTransaction {
        TurnTransaction::begin_admitted_with_runtime_generation(
            self.transaction_writer(),
            self.session_id().to_owned(),
            SystemIdGenerator,
            SystemClock,
            OperationKind::BranchSummary,
            snapshot.persisted_runtime_generation_ref(),
            snapshot.operation_id.clone(),
            self.active_branch_id(),
        )
    }

    pub(crate) fn begin_self_healing_edit_transaction(
        &self,
        snapshot: &OperationCapabilitySnapshot,
    ) -> PromptTurnTransaction {
        TurnTransaction::begin_admitted_with_runtime_generation(
            self.transaction_writer(),
            self.session_id().to_owned(),
            SystemIdGenerator,
            SystemClock,
            OperationKind::SelfHealingEdit,
            snapshot.persisted_runtime_generation_ref(),
            snapshot.operation_id.clone(),
            self.active_branch_id(),
        )
    }

    pub(crate) fn record_self_healing_edit_started(
        transaction: &mut PromptTurnTransaction,
        path: String,
        replacements: usize,
    ) -> Result<(), CodingSessionError> {
        transaction.record_self_healing_edit_started(path, replacements)
    }

    pub(crate) fn record_self_healing_edit_repair_attempted(
        transaction: &mut PromptTurnTransaction,
        path: &str,
        repair: &SelfHealingEditRepairAttempt,
    ) -> Result<(), CodingSessionError> {
        transaction.record_self_healing_edit_repair_attempted(path, repair)
    }

    pub(crate) fn record_self_healing_edit_completed(
        transaction: &mut PromptTurnTransaction,
        outcome: &SelfHealingEditOutcome,
    ) -> Result<(), CodingSessionError> {
        transaction.record_self_healing_edit_completed(outcome)
    }

    pub(crate) fn event_writer(&self) -> SessionEventWriter {
        SessionEventWriter {
            session_id: self.handle.manifest().session_id.clone(),
            writer: self.transaction_writer(),
            committed_session_sequence: self.committed_session_sequence.clone(),
        }
    }

    pub(crate) fn arm_auto_name_for_prompt(
        &mut self,
        replay: &SessionReplay,
    ) -> Result<(), CodingSessionError> {
        let has_conversation = replay.transcript.iter().any(|item| {
            matches!(
                item,
                TranscriptItem::UserInput { .. }
                    | TranscriptItem::AssistantMessage {
                        status: MessageStatus::Completed,
                        ..
                    }
            )
        });
        self.auto_name_eligible_for_active_prompt =
            !has_conversation && self.transaction_writer.manifest_snapshot()?.name.is_none();
        Ok(())
    }

    pub(crate) fn take_auto_name_writer_after_prompt(
        &mut self,
    ) -> Result<Option<SessionAutoNameWriter>, CodingSessionError> {
        if !std::mem::take(&mut self.auto_name_eligible_for_active_prompt)
            || self.transaction_writer.manifest_snapshot()?.name.is_some()
        {
            return Ok(None);
        }
        Ok(Some(SessionAutoNameWriter {
            session_id: self.handle.manifest().session_id.clone(),
            writer: self.transaction_writer(),
            committed_session_sequence: self.committed_session_sequence.clone(),
            session_name_updates: self.session_name_updates.clone(),
        }))
    }

    pub(crate) fn subscribe_session_name_updates(&self) -> watch::Receiver<SessionNameUpdate> {
        self.session_name_updates.subscribe()
    }

    pub(super) async fn append_durable_session_event(
        &mut self,
        operation_id: Option<String>,
        turn_id: Option<String>,
        data: SessionEventData,
    ) -> Result<(), CodingSessionError> {
        let session_id = self.session_id().to_owned();
        let mut ids = SystemIdGenerator;
        let clock = SystemClock;
        let updated_at = clock.now_rfc3339();
        let mut event = SessionEventEnvelope::new(
            session_id.clone(),
            ids.next_event_id(),
            updated_at.clone(),
            data,
        );
        if let Some(branch_id) = self.active_branch_id() {
            event = event.with_branch_id(branch_id);
        }
        event.operation_id = operation_id.clone();
        event.turn_id = turn_id;
        self.commit_writer_mutation(
            vec![event],
            ManifestPatch::new().updated_at(updated_at),
            operation_id.clone(),
        )
        .await?;
        Ok(())
    }

    pub(crate) fn active_branch_id(&self) -> Option<String> {
        self.transaction_writer
            .manifest_snapshot()
            .ok()
            .and_then(|manifest| manifest.active_branch_id)
    }

    pub(crate) fn load_rewind_checkpoint(
        &self,
        checkpoint_id: &str,
    ) -> Result<RewindCheckpoint, CodingSessionError> {
        rewind::load(&self.handle, checkpoint_id)
    }

    pub(crate) async fn create_rewind_checkpoint(
        &mut self,
        tracker: change_tracker::HunkTrackerCheckpoint,
        workspace: workspace_runtime::api::WorkspaceSnapshot,
        operation_id: &str,
    ) -> Result<RewindCheckpoint, CodingSessionError> {
        let branch_id = self
            .active_branch_id()
            .ok_or_else(|| CodingSessionError::Session {
                message: "session has no active branch".into(),
            })?;
        let leaf_id = self
            .transaction_writer
            .manifest_snapshot()?
            .active_leaf_id
            .ok_or_else(|| CodingSessionError::Session {
                message: "session has no committed active leaf for rewind checkpoint".into(),
            })?;
        let mut ids = SystemIdGenerator;
        let active_branch_sequence = self.active_branch_session_sequence()?;
        let checkpoint = RewindCheckpoint::create(
            format!("cp_{}", ids.next_branch_id()),
            self.session_id().to_owned(),
            branch_id.clone(),
            leaf_id.clone(),
            active_branch_sequence,
            tracker,
            workspace,
        )?;
        rewind::save(&self.handle, &checkpoint)?;
        let event = SessionEventEnvelope::new(
            self.session_id().to_owned(),
            ids.next_event_id(),
            SystemClock.now_rfc3339(),
            SessionEventData::RewindCheckpointCreated {
                checkpoint_id: checkpoint.checkpoint_id.clone(),
                leaf_id,
                workspace_identity: self.session_id().to_owned(),
                digest: checkpoint.digest.clone(),
            },
        )
        .with_operation_id(operation_id.to_owned())
        .with_branch_id(branch_id);
        let result = self
            .commit_writer_mutation(
                vec![event],
                ManifestPatch::new().updated_at(SystemClock.now_rfc3339()),
                Some(operation_id.to_owned()),
            )
            .await;
        if result.is_err() {
            let _ = rewind::remove(&self.handle, &checkpoint.checkpoint_id);
        }
        result.map(|()| checkpoint)
    }

    pub(crate) async fn commit_rewind(
        &mut self,
        checkpoint: &RewindCheckpoint,
        operation_id: &str,
    ) -> Result<String, CodingSessionError> {
        checkpoint.validate(self.session_id())?;
        let source_branch_id =
            self.active_branch_id()
                .ok_or_else(|| CodingSessionError::Session {
                    message: "session has no active branch".into(),
                })?;
        if checkpoint.branch_id != source_branch_id {
            return Err(CodingSessionError::Input {
                message: format!(
                    "rewind checkpoint {} belongs to branch {}, active branch is {}",
                    checkpoint.checkpoint_id, checkpoint.branch_id, source_branch_id
                ),
            });
        }
        if checkpoint.session_sequence > self.committed_session_sequence() {
            return Err(CodingSessionError::Input {
                message: "rewind checkpoint is ahead of the current session cursor".into(),
            });
        }
        let mut ids = SystemIdGenerator;
        let new_branch_id = ids.next_branch_id();
        let updated_at = SystemClock.now_rfc3339();
        let event = SessionEventEnvelope::new(
            self.session_id().to_owned(),
            ids.next_event_id(),
            updated_at.clone(),
            SessionEventData::SessionRewound {
                source_branch_id: source_branch_id.clone(),
                new_branch_id: new_branch_id.clone(),
                checkpoint_id: checkpoint.checkpoint_id.clone(),
                target_leaf_id: checkpoint.leaf_id.clone(),
                restored_session_sequence: checkpoint.session_sequence,
            },
        )
        .with_operation_id(operation_id.to_owned())
        .with_branch_id(source_branch_id);
        self.commit_writer_mutation(
            vec![event],
            ManifestPatch::new()
                .updated_at(updated_at)
                .active_branch_id(Some(new_branch_id.clone()))
                .active_leaf_id(Some(checkpoint.leaf_id.clone())),
            Some(operation_id.to_owned()),
        )
        .await?;
        Ok(new_branch_id)
    }
}

impl SessionAutoNameWriter {
    pub(crate) fn is_unnamed(&self) -> Result<bool, CodingSessionError> {
        Ok(self.writer.manifest_snapshot()?.name.is_none())
    }

    pub(crate) async fn commit_generated_name(
        &self,
        operation_id: &str,
        name: String,
        model_id: String,
        usage: Usage,
    ) -> Result<(), CodingSessionError> {
        let mut ids = SystemIdGenerator;
        let updated_at = SystemClock.now_rfc3339();
        let events = vec![
            SessionEventEnvelope::new(
                self.session_id.clone(),
                ids.next_event_id(),
                updated_at.clone(),
                SessionEventData::OperationStarted {
                    operation: OperationKind::Other {
                        name: "session_naming".into(),
                    },
                    runtime_generation: Default::default(),
                },
            )
            .with_operation_id(operation_id),
            SessionEventEnvelope::new(
                self.session_id.clone(),
                ids.next_event_id(),
                updated_at.clone(),
                SessionEventData::ModelUsageRecorded {
                    purpose: "session_naming".into(),
                    model_id,
                    usage,
                },
            )
            .with_operation_id(operation_id),
            SessionEventEnvelope::new(
                self.session_id.clone(),
                ids.next_event_id(),
                updated_at.clone(),
                SessionEventData::OperationCommitted { new_leaf_id: None },
            )
            .with_operation_id(operation_id),
        ];
        let receipt = self
            .writer
            .commit_session_name_if_unset(
                events,
                ManifestPatch::new().updated_at(updated_at).name(Some(name)),
                operation_id.to_owned(),
            )
            .await?;
        observe_commit_receipt(&self.committed_session_sequence, receipt);
        let manifest = self.writer.manifest_snapshot()?;
        self.session_name_updates.send_replace(SessionNameUpdate {
            name: manifest.name,
            updated_at: manifest.updated_at,
        });
        Ok(())
    }

    pub(crate) async fn commit_failure_diagnostic(
        &self,
        operation_id: &str,
        message: String,
        model_usage: Option<(String, Usage)>,
    ) -> Result<(), CodingSessionError> {
        let mut ids = SystemIdGenerator;
        let created_at = SystemClock.now_rfc3339();
        let mut events = Vec::with_capacity(4);
        events.push(
            SessionEventEnvelope::new(
                self.session_id.clone(),
                ids.next_event_id(),
                created_at.clone(),
                SessionEventData::OperationStarted {
                    operation: OperationKind::Other {
                        name: "session_naming".into(),
                    },
                    runtime_generation: Default::default(),
                },
            )
            .with_operation_id(operation_id),
        );
        if let Some((model_id, usage)) = model_usage {
            events.push(
                SessionEventEnvelope::new(
                    self.session_id.clone(),
                    ids.next_event_id(),
                    created_at.clone(),
                    SessionEventData::ModelUsageRecorded {
                        purpose: "session_naming".into(),
                        model_id,
                        usage,
                    },
                )
                .with_operation_id(operation_id),
            );
        }
        events.push(
            SessionEventEnvelope::new(
                self.session_id.clone(),
                ids.next_event_id(),
                created_at,
                SessionEventData::DiagnosticEmitted {
                    level: crate::session::event::DiagnosticLevel::Warn,
                    message,
                },
            )
            .with_operation_id(operation_id),
        );
        events.push(
            SessionEventEnvelope::new(
                self.session_id.clone(),
                ids.next_event_id(),
                SystemClock.now_rfc3339(),
                SessionEventData::OperationFailed {
                    error_code: "session_naming".into(),
                    message: "automatic session naming failed".into(),
                },
            )
            .with_operation_id(operation_id),
        );
        let receipt = self
            .writer
            .append_checkpoint_events_with_receipt(events)
            .await?;
        observe_commit_receipt(&self.committed_session_sequence, receipt);
        let manifest = self.writer.manifest_snapshot()?;
        self.session_name_updates.send_replace(SessionNameUpdate {
            name: manifest.name,
            updated_at: manifest.updated_at,
        });
        Ok(())
    }
}

impl SessionEventWriter {
    pub(crate) async fn append(
        &self,
        operation_id: &str,
        turn_id: &str,
        data: Vec<SessionEventData>,
    ) -> Result<(), CodingSessionError> {
        let events = self.events(operation_id, turn_id, data);
        if events.is_empty() {
            return Ok(());
        }
        let receipt = self
            .writer
            .append_checkpoint_events_with_receipt(events)
            .await?;
        observe_commit_receipt(&self.committed_session_sequence, receipt);
        Ok(())
    }

    pub(crate) fn append_blocking(
        &self,
        operation_id: &str,
        turn_id: &str,
        data: Vec<SessionEventData>,
    ) -> Result<(), CodingSessionError> {
        let events = self.events(operation_id, turn_id, data);
        if events.is_empty() {
            return Ok(());
        }
        let receipt = self
            .writer
            .append_checkpoint_events_with_receipt_blocking(events)?;
        observe_commit_receipt(&self.committed_session_sequence, receipt);
        Ok(())
    }

    fn events(
        &self,
        operation_id: &str,
        turn_id: &str,
        data: Vec<SessionEventData>,
    ) -> Vec<SessionEventEnvelope> {
        let mut ids = SystemIdGenerator;
        let updated_at = SystemClock.now_rfc3339();
        data.into_iter()
            .map(|data| {
                SessionEventEnvelope::new(
                    self.session_id.clone(),
                    ids.next_event_id(),
                    updated_at.clone(),
                    data,
                )
                .with_operation_id(operation_id)
                .with_turn_id(turn_id)
            })
            .collect()
    }
}
