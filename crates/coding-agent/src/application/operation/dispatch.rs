use super::admission::OperationScheduler;
use super::contract::{BranchSummaryReusePolicy, CodingAgentOperation};
use super::control::PromptControlRegistration;
use super::permit::OperationPermit;
use super::submission::SubmissionCommitGuard;
use super::{OperationDispatchMode, OperationExecution, OperationOutcome};
use crate::kernel::capability::SessionWriteCapability;
use crate::kernel::error::CodingSessionError;
use crate::kernel::operation::OperationKind;
use crate::mutex::report_infallible_resource_error;
use crate::operations::compaction::runner::ManualCompactionOptions;
use crate::platform::time::{Clock, SystemClock};
use crate::runtime::facade::{CodingAgentSession, PromptControlCleanupGuard};
use crate::session::service::SessionPersistence;

impl CodingAgentSession {
    pub(super) async fn execute_operation_envelope(
        &mut self,
        mut operation: CodingAgentOperation,
        mut submission: Option<SubmissionCommitGuard>,
    ) -> Result<OperationOutcome, CodingSessionError> {
        let dispatch_mode = operation.descriptor().dispatch_mode;
        if dispatch_mode == OperationDispatchMode::Async {
            self.prepare_operation_for_admission(&mut operation)?;
            if let Some(options) = operation.prompt_options_mut()
                && let Some(runtime) = options.runtime_mut()
            {
                self.runtime_host
                    .runtime_service
                    .install_provider_runtime(runtime);
                self.runtime_host
                    .runtime_service
                    .install_background_tasks(runtime);
            }
        }

        let admission = self.resolve_operation_admission_with_id(
            &operation,
            submission
                .as_ref()
                .map(|submission| submission.operation_id.as_str()),
        )?;
        if dispatch_mode != OperationDispatchMode::SyncReadOnly {
            self.runtime_host
                .session_coordinator
                .ensure_write_admission(admission.descriptor.admission_class())?;
        }
        let mut operation_permit = OperationScheduler::admit(
            &self.runtime_host.operation_supervisor.control,
            &admission,
            dispatch_mode,
        )
        .map_err(|rejection| rejection.into_error())?;
        if let Some(guard) = submission.as_mut() {
            guard.commit_execution(operation_permit.execution())?;
        }

        let execution = operation_permit.execution().clone();
        let session_naming_seed = match &operation {
            CodingAgentOperation::Prompt(options) => {
                crate::operations::session_naming::SessionNamingSeed::from_prompt(
                    options,
                    &execution.capability_snapshot,
                )
            }
            _ => None,
        };
        let result = match dispatch_mode {
            OperationDispatchMode::SyncReadOnly => {
                self.dispatch_sync_read_only(operation, &operation_permit)
            }
            OperationDispatchMode::SyncMutable => {
                self.dispatch_sync_mutable(operation, &mut operation_permit)
                    .await
            }
            OperationDispatchMode::Async => {
                self.dispatch_async(
                    operation,
                    &admission,
                    &operation_permit,
                    submission.as_ref(),
                )
                .await
            }
        };

        let decision = super::finalize::FinalizationDecision::freeze(&execution, &result);
        let commit_result = self
            .runtime_host
            .session_coordinator
            .resolve_finalization(&decision)?;
        self.runtime_host
            .events
            .emit_recovery_pending(&decision, &commit_result)?;
        self.persist_operation_terminal_outbox(&decision, &result, &commit_result)
            .await?;
        if let Some(guard) = submission.as_mut() {
            guard.finish(&decision, &commit_result)?;
        }
        if let Ok(OperationOutcome::Rewound {
            restored_session_sequence,
            ..
        }) = &result
        {
            self.runtime_host
                .authorization_service
                .cancel_all("tool authorization invalidated by session rewind")?;
            self.runtime_host
                .client_projection
                .snapshots
                .reset_after_rewind(*restored_session_sequence)?;
            self.refresh_snapshot_projection()?;
        }
        self.schedule_session_naming_after_prompt(session_naming_seed, &result);
        result
    }

    fn dispatch_sync_read_only(
        &self,
        operation: CodingAgentOperation,
        operation_permit: &OperationPermit,
    ) -> Result<OperationOutcome, CodingSessionError> {
        match operation {
            CodingAgentOperation::ExportCurrent | CodingAgentOperation::ExportCurrentHtml(_) => {
                let options = operation
                    .export_options()
                    .expect("export variants always normalize to export options");
                crate::operations::export::run(
                    options,
                    operation_permit.capability_snapshot(),
                    &self.runtime_host.session_coordinator.persistence,
                )
                .map(OperationOutcome::Export)
            }
            _ => {
                unreachable!("descriptor routed a non-read-only operation to the read-only handler")
            }
        }
    }

    async fn dispatch_sync_mutable(
        &mut self,
        operation: CodingAgentOperation,
        operation_permit: &mut OperationPermit,
    ) -> Result<OperationOutcome, CodingSessionError> {
        match operation {
            CodingAgentOperation::RejectDelegation {
                operation_id,
                tool_call_id,
                reason,
            } => {
                SessionWriteCapability::require(
                    operation_permit
                        .capability_snapshot()
                        .session_write
                        .as_ref(),
                )?;
                let now = SystemClock.now_rfc3339();
                let reply = self
                    .runtime_host
                    .session_coordinator
                    .execute_writer_command(
                    crate::application::session_coordinator::SessionWriterCommand::reject_delegation(
                        operation_permit.execution().operation_id.clone(),
                        operation_permit.execution().capability_generation,
                        operation_id,
                        tool_call_id,
                        now,
                        reason,
                    ),
                )
                .await?;
                let crate::application::session_coordinator::SessionWriterReply::DelegationRejected {
                    request,
                    reason,
                } = reply
                else {
                    unreachable!("delegation rejection writer command returns its typed reply")
                };
                self.runtime_host
                    .events
                    .emit_delegation_rejected(&request, &reason)?;
                Ok(OperationOutcome::DelegationRejection)
            }
            CodingAgentOperation::ForkSession { target_leaf_id } => {
                SessionWriteCapability::require(
                    operation_permit
                        .capability_snapshot()
                        .session_write
                        .as_ref(),
                )?;
                let command =
                    crate::application::session_coordinator::SessionWriterCommand::fork_session(
                        operation_permit.execution().operation_id.clone(),
                        operation_permit.execution().capability_generation,
                        target_leaf_id,
                    );
                operation_permit.release();
                let reply = self
                    .runtime_host
                    .session_coordinator
                    .execute_writer_command(command)
                    .await?;
                let crate::application::session_coordinator::SessionWriterReply::ForkedSession {
                    session_id,
                } = reply
                else {
                    unreachable!("fork writer command returns its typed reply")
                };
                self.refresh_snapshot_projection()?;
                self.runtime_host.events.emit_session_opened(session_id)?;
                Ok(OperationOutcome::ForkSession)
            }
            CodingAgentOperation::SwitchActiveLeaf { target_leaf_id } => {
                SessionWriteCapability::require(
                    operation_permit
                        .capability_snapshot()
                        .session_write
                        .as_ref(),
                )?;
                let reply = self
                    .runtime_host
                    .session_coordinator
                    .execute_writer_command(
                    crate::application::session_coordinator::SessionWriterCommand::switch_active_leaf(
                        operation_permit.execution().operation_id.clone(),
                        operation_permit.execution().capability_generation,
                        target_leaf_id,
                    ),
                )
                .await?;
                if !matches!(
                    reply,
                    crate::application::session_coordinator::SessionWriterReply::ActiveLeaf
                ) {
                    return Err(CodingSessionError::Session {
                        message: "session writer returned an unexpected switch-leaf reply".into(),
                    });
                }
                self.refresh_snapshot_projection()?;
                Ok(OperationOutcome::SwitchActiveLeaf)
            }
            CodingAgentOperation::SetSessionTreeLabel { entry_id, label } => {
                SessionWriteCapability::require(
                    operation_permit
                        .capability_snapshot()
                        .session_write
                        .as_ref(),
                )?;
                let reply = self
                    .runtime_host
                    .session_coordinator
                    .execute_writer_command(
                        crate::application::session_coordinator::SessionWriterCommand::
                            set_session_tree_label(
                                operation_permit.execution().operation_id.clone(),
                                operation_permit.execution().capability_generation,
                                entry_id,
                                label,
                            ),
                    )
                    .await?;
                self.refresh_snapshot_projection()?;
                let crate::application::session_coordinator::SessionWriterReply::SessionTreeLabel {
                    entry_id,
                    label,
                    updated_at,
                } = reply
                else {
                    unreachable!("tree-label writer command returns its typed reply")
                };
                Ok(OperationOutcome::SessionTreeLabelChanged {
                    entry_id,
                    label,
                    updated_at,
                })
            }
            CodingAgentOperation::SetSessionName { name } => {
                SessionWriteCapability::require(
                    operation_permit
                        .capability_snapshot()
                        .session_write
                        .as_ref(),
                )?;
                let reply = self
                    .runtime_host
                    .session_coordinator
                    .execute_writer_command(
                        crate::application::session_coordinator::SessionWriterCommand::set_session_name(
                            operation_permit.execution().operation_id.clone(),
                            operation_permit.execution().capability_generation,
                            name,
                        ),
                    )
                    .await?;
                self.refresh_snapshot_projection()?;
                let crate::application::session_coordinator::SessionWriterReply::SessionName {
                    name,
                    updated_at,
                } = reply
                else {
                    unreachable!("session-name writer command returns its typed reply")
                };
                Ok(OperationOutcome::SessionNameChanged { name, updated_at })
            }
            CodingAgentOperation::CreateRewindCheckpoint => {
                SessionWriteCapability::require(
                    operation_permit
                        .capability_snapshot()
                        .session_write
                        .as_ref(),
                )?;
                let review = self.runtime_host.review_service.checkpoint().await?;
                let reply = self
                    .runtime_host
                    .session_coordinator
                    .execute_writer_command(
                        crate::application::session_coordinator::SessionWriterCommand::create_rewind_checkpoint(
                            operation_permit.execution().operation_id.clone(),
                            operation_permit.execution().capability_generation,
                            review.tracker,
                            review.workspace,
                        ),
                    )
                    .await?;
                let crate::application::session_coordinator::SessionWriterReply::RewindCheckpointCreated {
                    checkpoint,
                } = reply
                else {
                    unreachable!("rewind checkpoint writer command returns its typed reply")
                };
                Ok(OperationOutcome::RewindCheckpointCreated {
                    checkpoint_id: checkpoint.checkpoint_id,
                    branch_id: checkpoint.branch_id,
                    leaf_id: checkpoint.leaf_id,
                    session_sequence: checkpoint.session_sequence,
                })
            }
            CodingAgentOperation::Rewind { checkpoint_id } => {
                SessionWriteCapability::require(
                    operation_permit
                        .capability_snapshot()
                        .session_write
                        .as_ref(),
                )?;
                let checkpoint = match &self.runtime_host.session_coordinator.persistence {
                    SessionPersistence::Persistent(session_service) => {
                        session_service.load_rewind_checkpoint(&checkpoint_id)?
                    }
                    SessionPersistence::NonPersistent(_) => {
                        return Err(CodingSessionError::UnsupportedCapability {
                            capability: "rewind requires a persistent Rust-native session".into(),
                        });
                    }
                };
                if checkpoint.checkpoint_id != checkpoint_id {
                    return Err(CodingSessionError::Input {
                        message: "rewind checkpoint identity does not match the request".into(),
                    });
                }
                let current = self.runtime_host.review_service.checkpoint().await?;
                let target = crate::services::review::ReviewCheckpoint {
                    tracker: checkpoint.tracker.clone(),
                    workspace: checkpoint.workspace.clone(),
                };
                let rewind_operation_id = operation_permit.execution().operation_id.clone();
                self.runtime_host
                    .review_service
                    .restore_workspace_and_tracker(&current, &target, &rewind_operation_id)
                    .await?;
                let restored_session_sequence = checkpoint.session_sequence;
                let command =
                    crate::application::session_coordinator::SessionWriterCommand::commit_rewind(
                        operation_permit.execution().operation_id.clone(),
                        operation_permit.execution().capability_generation,
                        checkpoint.clone(),
                    );
                let reply = match self
                    .runtime_host
                    .session_coordinator
                    .execute_writer_command(command)
                    .await
                {
                    Ok(reply) => reply,
                    Err(commit_error) => {
                        if let Err(rollback_error) = self
                            .runtime_host
                            .review_service
                            .restore_workspace_and_tracker(&target, &current, &rewind_operation_id)
                            .await
                        {
                            return Err(CodingSessionError::PartialCommit {
                                operation_id: operation_permit.execution().operation_id.clone(),
                                message: format!(
                                    "rewind session commit failed: {commit_error}; workspace rollback failed: {rollback_error}"
                                ),
                            });
                        }
                        return Err(commit_error);
                    }
                };
                let crate::application::session_coordinator::SessionWriterReply::Rewound {
                    new_branch_id,
                } = reply
                else {
                    unreachable!("rewind writer command returns its typed reply")
                };
                Ok(OperationOutcome::Rewound {
                    checkpoint_id,
                    new_branch_id,
                    restored_session_sequence,
                })
            }
            _ => unreachable!("descriptor routed a non-mutable operation to the mutable handler"),
        }
    }

    async fn dispatch_async(
        &mut self,
        operation: CodingAgentOperation,
        admission: &OperationExecution,
        operation_permit: &OperationPermit,
        submission: Option<&SubmissionCommitGuard>,
    ) -> Result<OperationOutcome, CodingSessionError> {
        let snapshot = operation_permit.execution().capability_snapshot.clone();
        let operation_cancellation = operation_permit.cancellation_token();
        let operation_cancellation_handle = operation_permit.cancellation_handle();
        if let (Some(submission), Some(cancellation)) =
            (submission, operation_cancellation_handle.clone())
        {
            self.runtime_host
                .client_projection
                .snapshots
                .bind_operation_cancellation(
                    submission.handle.clone(),
                    snapshot.operation_id.clone(),
                    cancellation,
                )?;
        }

        Box::pin(async {
                match operation {
                    CodingAgentOperation::Prompt(options) => {
                        let has_existing_prompt_control = self
                            .runtime_host
                            .operation_supervisor
                            .control
                            .current_prompt_control_registration()?
                            .is_some();
                        let prompt_control = if submission.is_some() || has_existing_prompt_control
                        {
                            Some(
                                self.runtime_host
                                    .operation_supervisor
                                    .control
                                    .prompt_control_registration_for(&snapshot.operation_id)?,
                            )
                        } else {
                            None
                        };
                        if let (Some(submission), Some(prompt_control)) =
                            (submission, prompt_control.as_ref())
                        {
                            self.runtime_host
                                .client_projection.snapshots
                                .bind_prompt_control(
                                    submission.handle.clone(),
                                    snapshot.operation_id.clone(),
                                    prompt_control.generation,
                                    prompt_control.handle.clone(),
                                )?;
                        }
                        let mut prompt_control_cleanup = prompt_control.map(
                            |PromptControlRegistration { generation, .. }| {
                                PromptControlCleanupGuard::new(
                                    self.runtime_host
                                        .operation_supervisor
                                        .control
                                        .prompt_control_cleanup(),
                                    self.runtime_host.client_projection.snapshots.clone(),
                                    snapshot.operation_id.clone(),
                                    generation,
                                )
                            },
                        );
                        let (_session_id, workspace_root) =
                            self.runtime_host.session_identity();
                        let result = crate::operations::prompt::run(
                            &mut self.runtime_host.session_coordinator.persistence,
                            &mut self.runtime_host.operation_supervisor.control,
                            &self.runtime_host.profile_registry,
                            &self.runtime_host.events,
                            &mut self
                                .runtime_host
                                .session_coordinator
                                .pending_delegation_confirmations,
                            &self.runtime_host.authorization_service,
                            &self.runtime_host.review_service,
                            Some(self.runtime_host.extension_host.sink()),
                            workspace_root,
                            options,
                            &snapshot,
                            operation_cancellation.clone(),
                        )
                        .await;
                        if let Some(cleanup) = prompt_control_cleanup.as_mut() {
                            cleanup.cleanup();
                        }
                        result.map(OperationOutcome::Prompt)
                    }
                    CodingAgentOperation::Compact(options) => {
                        let mut options =
                            ManualCompactionOptions::from_prompt_turn_options(&options)?;
                        if let Some(cancellation) = operation_cancellation.clone() {
                            options = options.with_cancellation(cancellation);
                        }
                        let SessionPersistence::Persistent(session_service) =
                            &mut self.runtime_host.session_coordinator.persistence
                        else {
                            return Err(CodingSessionError::UnsupportedCapability {
                                capability: "manual compaction without persistent session".into(),
                            });
                        };
                        crate::operations::compaction::run(
                            session_service,
                            &self.runtime_host.events,
                            options,
                            &snapshot,
                            operation_cancellation_handle.clone(),
                        )
                        .await
                        .map(OperationOutcome::ManualCompaction)
                    }
                    CodingAgentOperation::BranchSummary {
                        options,
                        source_leaf_id,
                        target_leaf_id,
                        custom_instructions,
                        reuse,
                    } => {
                        if matches!(reuse, BranchSummaryReusePolicy::ReuseExisting)
                            && let Some(outcome) =
                                crate::operations::branch_summary::reused_outcome(
                                    &self.runtime_host.session_coordinator.persistence,
                                    &options,
                                    source_leaf_id.as_str(),
                                    target_leaf_id.as_str(),
                                    operation_permit.capability_snapshot(),
                                )?
                        {
                            return Ok(OperationOutcome::BranchSummary(outcome));
                        }
                        let SessionPersistence::Persistent(session_service) =
                            &mut self.runtime_host.session_coordinator.persistence
                        else {
                            return Err(CodingSessionError::UnsupportedCapability {
                                capability: "branch summary without persistent session".into(),
                            });
                        };
                        crate::operations::branch_summary::run(
                            session_service,
                            &self.runtime_host.events,
                            options,
                            source_leaf_id,
                            target_leaf_id,
                            custom_instructions,
                            &snapshot,
                            operation_cancellation.clone(),
                            operation_cancellation_handle.clone(),
                        )
                        .await
                        .map(OperationOutcome::BranchSummary)
                    }
                    CodingAgentOperation::SelfHealingEdit(request) => {
                        let (path, replacements, check_command, repair_attempts, model_repair) =
                            request.into_parts();
                        if !repair_attempts.is_empty() && model_repair.is_some() {
                            return Err(CodingSessionError::Input {
                            message:
                                "configure either planned repair attempts or model repair, not both"
                                    .into(),
                        });
                        }
                        let model_repair_policy = match model_repair {
                            Some(model_repair) => {
                                let (prompt_options, max_attempts) = model_repair.into_parts();
                                Some(crate::operations::self_healing_edit::model_repair_policy(
                                    prompt_options,
                                    max_attempts,
                                    &snapshot,
                                )?)
                            }
                            None => None,
                        };
                        let SessionPersistence::Persistent(session_service) =
                            &mut self.runtime_host.session_coordinator.persistence
                        else {
                            return Err(CodingSessionError::UnsupportedCapability {
                                capability:
                                    "self-healing edit requires a persistent Rust-native session"
                                        .into(),
                            });
                        };
                        let outcome = crate::operations::self_healing_edit::run(
                            session_service,
                            self.runtime_host.events.clone(),
                            path,
                            replacements,
                            check_command,
                            repair_attempts,
                            model_repair_policy,
                            &snapshot,
                            operation_cancellation.clone(),
                            operation_cancellation_handle.clone(),
                        )
                        .await?;
                        self.runtime_host
                            .events
                            .emit_session_write_events(&outcome.finalized)?;
                        outcome.result.map(OperationOutcome::SelfHealingEdit)
                    }
                    CodingAgentOperation::InvokeAgent(options) => {
                        let prompt_control_receiver = self
                            .runtime_host
                            .operation_supervisor
                            .control
                            .take_prompt_control_receiver()?;
                        self.runtime_host
                            .operation_supervisor
                            .control
                            .clear_prompt_control_receiver()?;
                        let result = crate::operations::agent_invocation::run(
                            options,
                            snapshot.operation_id.clone(),
                            prompt_control_receiver,
                            &self.runtime_host.profile_registry,
                            &self.runtime_host.events,
                            &self.runtime_host.operation_supervisor.control,
                            snapshot.clone(),
                            operation_cancellation.clone(),
                        )
                        .await;
                        result.map(OperationOutcome::AgentInvocation)
                    }
                    CodingAgentOperation::InvokeTeam(options) => crate::operations::team_invocation::run(
                        options,
                        snapshot.operation_id.clone(),
                        &self.runtime_host.profile_registry,
                        &self.runtime_host.events,
                        &self.runtime_host.operation_supervisor.control,
                        snapshot.clone(),
                        operation_cancellation.clone(),
                    )
                    .await
                    .map(OperationOutcome::AgentTeam),
                    CodingAgentOperation::ListMergeProposals => {
                        let workspace_root = snapshot
                            .workspace
                            .as_ref()
                            .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                                capability: "proposal review requires the session workspace authority"
                                    .into(),
                            })?
                            .cwd()
                            .to_path_buf();
                        let registry = self
                            .runtime_host
                            .operation_supervisor
                            .control
                            .worktree_registry()
                            .cloned()
                            .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                                capability: "proposal review requires a managed worktree registry"
                                    .into(),
                            })?;
                        crate::operations::merge::runner::list_proposals(
                            &registry,
                            &workspace_root,
                            operation_cancellation
                                .clone()
                            .unwrap_or_default(),
                        )
                        .await
                        .map(OperationOutcome::MergeProposals)
                    }
                    CodingAgentOperation::MergeChildWorktree { worktree_id } => {
                        let workspace_root = snapshot
                            .workspace
                            .as_ref()
                            .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                                capability: "merge requires the session workspace authority"
                                    .into(),
                            })?
                            .cwd()
                            .to_path_buf();
                        let registry = self
                            .runtime_host
                            .operation_supervisor
                            .control
                            .worktree_registry()
                            .cloned()
                            .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                                capability: "merge requires a managed worktree registry".into(),
                            })?;
                        let (session_id, _) = self.runtime_host.session_identity();
                        crate::operations::merge::runner::merge_worktree(
                            &self.runtime_host.events,
                            &self.runtime_host.extension_host,
                            &registry,
                            &workspace_root,
                            &snapshot.operation_id,
                            &session_id,
                            &worktree_id,
                            operation_cancellation
                                .clone()
                            .unwrap_or_default(),
                        )
                        .await
                        .map(|outcome| {
                            OperationOutcome::MergeApplied {
                                worktree_id: outcome.worktree_id,
                                applied: outcome.applied,
                            }
                        })
                    }
                    CodingAgentOperation::DiscardChildWorktree { worktree_id } => {
                        let workspace_root = snapshot
                            .workspace
                            .as_ref()
                            .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                                capability: "discard requires the session workspace authority"
                                    .into(),
                            })?
                            .cwd()
                            .to_path_buf();
                        let registry = self
                            .runtime_host
                            .operation_supervisor
                            .control
                            .worktree_registry()
                            .cloned()
                            .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                                capability: "discard requires a managed worktree registry".into(),
                            })?;
                        crate::operations::merge::runner::discard_worktree(
                            &self.runtime_host.events,
                            &registry,
                            &workspace_root,
                            &snapshot.operation_id,
                            &worktree_id,
                        )?;
                        Ok(OperationOutcome::WorktreeDiscarded { worktree_id })
                    }
                    CodingAgentOperation::ApproveDelegation {
                        operation_id,
                        tool_call_id,
                    } => crate::operations::delegation::execution::approve(
                        &mut self.runtime_host.session_coordinator,
                        &self.runtime_host.runtime_service,
                        &self.runtime_host.profile_registry,
                        &self.runtime_host.events,
                        &self.runtime_host.operation_supervisor.control,
                        operation_id,
                        tool_call_id,
                        admission
                            .admitted_at
                            .clone()
                            .expect("delegation approval admission time is resolved"),
                        snapshot.clone(),
                    )
                    .await
                    .map(|_| OperationOutcome::DelegationApproval),
                    _ => unreachable!("descriptor routed a non-async operation to the async handler"),
                }
        })
        .await
    }

    fn schedule_session_naming_after_prompt(
        &mut self,
        seed: Option<crate::operations::session_naming::SessionNamingSeed>,
        result: &Result<OperationOutcome, CodingSessionError>,
    ) {
        let Some(seed) = seed else {
            return;
        };
        let Ok(OperationOutcome::Prompt(
            crate::operations::prompt::context::InternalPromptTurnOutcome::Success {
                final_text,
                ..
            },
        )) = result
        else {
            return;
        };
        let crate::session::service::SessionPersistence::Persistent(session_service) =
            &mut self.runtime_host.session_coordinator.persistence
        else {
            return;
        };
        match session_service.take_auto_name_writer_after_prompt() {
            Ok(Some(writer)) => seed.spawn_after_first_exchange(
                writer,
                final_text.clone(),
                self.runtime_host.events.clone(),
            ),
            Ok(None) => {}
            Err(error) => {
                report_infallible_resource_error(
                    "automatic session naming inspection diagnostic",
                    self.runtime_host.events.emit_diagnostic(
                        None::<String>,
                        format!(
                            "automatic session naming could not inspect session state: {error}"
                        ),
                    ),
                );
            }
        }
    }

    async fn persist_operation_terminal_outbox(
        &self,
        decision: &super::finalize::FinalizationDecision,
        result: &Result<OperationOutcome, CodingSessionError>,
        commit_result: &super::finalize::FinalizationCommitResult,
    ) -> Result<(), CodingSessionError> {
        if !matches!(
            decision.operation_kind,
            OperationKind::Prompt
                | OperationKind::Compact
                | OperationKind::SelfHealingEdit
                | OperationKind::AgentInvocation
                | OperationKind::AgentTeam
        ) || !matches!(
            commit_result,
            super::finalize::FinalizationCommitResult::Committed
                | super::finalize::FinalizationCommitResult::DefinitelyFailed { .. }
        ) {
            return Ok(());
        }
        let (draft, prompt_outcome) = match decision.operation_kind {
            OperationKind::Prompt => {
                let Some(OperationOutcome::Prompt(outcome)) = result.as_ref().ok() else {
                    return Ok(());
                };
                let Some(draft) =
                    crate::services::event::EventService::prompt_terminal_draft(outcome)
                else {
                    return Ok(());
                };
                (draft, Some(outcome))
            }
            OperationKind::Compact => {
                let Some(OperationOutcome::ManualCompaction(outcome)) = result.as_ref().ok() else {
                    return Ok(());
                };
                let Some(draft) = self
                    .runtime_host
                    .events
                    .take_deferred_terminal_draft(&decision.operation_id)?
                else {
                    return Ok(());
                };
                (draft, Some(outcome))
            }
            OperationKind::SelfHealingEdit => {
                let Some(draft) = self
                    .runtime_host
                    .events
                    .take_deferred_terminal_draft(&decision.operation_id)?
                else {
                    return Ok(());
                };
                (draft, None)
            }
            OperationKind::AgentInvocation | OperationKind::AgentTeam => {
                let Some(draft) = self
                    .runtime_host
                    .events
                    .take_deferred_terminal_draft(&decision.operation_id)?
                else {
                    return Ok(());
                };
                (draft, None)
            }
            _ => return Ok(()),
        };
        let compact_terminal_is_session_event = matches!(
            &draft.event,
            crate::events::CodingAgentProductEventKind::Session(
                crate::events::CodingAgentSessionProductEvent::CompactionCompleted { .. }
            )
        );
        let live_draft = draft.clone();
        if matches!(
            decision.operation_kind,
            OperationKind::AgentInvocation | OperationKind::AgentTeam
        ) {
            self.runtime_host
                .events
                .emit_committed_terminal_draft(live_draft, decision.operation_kind)?;
            return Ok(());
        }
        self.runtime_host
            .session_coordinator
            .persist_terminal_decision(decision, draft)
            .await?;
        if matches!(
            decision.operation_kind,
            OperationKind::Compact | OperationKind::SelfHealingEdit
        ) {
            self.runtime_host
                .events
                .emit_committed_terminal_draft(live_draft, decision.operation_kind)?;
        }
        if let Some(outcome) = prompt_outcome
            && (decision.operation_kind == OperationKind::Prompt
                || compact_terminal_is_session_event)
        {
            self.runtime_host.events.emit_prompt_terminal(outcome)?;
        }
        Ok(())
    }
}
