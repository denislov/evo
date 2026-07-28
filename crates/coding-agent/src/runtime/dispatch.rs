use super::capability::CapabilityRevocationPolicy;
use super::control::PromptControlRegistration;
use super::facade::{CodingAgentSession, CodingSessionError, PromptControlCleanupGuard};
use super::intent::IntentRouter;
use super::operation::{Operation, OperationDispatchMode, OperationOutcome};
use super::scheduler::OperationScheduler;
use super::submission::SubmissionCommitGuard;
use crate::operations::compaction::runner::ManualCompactionOptions;
use crate::runtime::capability::SessionWriteCapability;
use crate::session::id::{Clock, SystemClock};
use crate::session::service::SessionPersistence;

impl CodingAgentSession {
    pub(super) fn run_sync_operation(
        &self,
        operation: Operation,
        mut submission: Option<SubmissionCommitGuard>,
    ) -> Result<OperationOutcome, CodingSessionError> {
        let admission = self.resolve_operation_admission_with_id(
            &operation,
            submission
                .as_ref()
                .map(|submission| submission.operation_id.as_str()),
        )?;
        let operation_permit = OperationScheduler::admit(
            &self.runtime_host.operation_supervisor.control,
            &admission,
            OperationDispatchMode::SyncReadOnly,
        )
        .map_err(|rejection| rejection.into_error())?;
        if let Some(guard) = submission.as_mut() {
            guard.commit_execution(operation_permit.execution())?;
        }
        let execution = operation_permit.execution().clone();

        let result = match operation {
            Operation::Export(options) => crate::operations::export::run(
                options,
                operation_permit.capability_snapshot(),
                &self.runtime_host.session_coordinator.persistence,
            )
            .map(OperationOutcome::Export),
            Operation::RejectDelegationConfirmation { .. } => {
                Err(IntentRouter::unsupported_dispatch(&admission))
            }
            Operation::Prompt(_)
            | Operation::ManualCompaction(_)
            | Operation::ApproveDelegationConfirmation { .. }
            | Operation::BranchSummary { .. }
            | Operation::SelfHealingEdit(_)
            | Operation::AgentInvocation(_)
            | Operation::AgentTeam(_)
            | Operation::ForkSession { .. }
            | Operation::SwitchActiveLeaf { .. }
            | Operation::SetSessionTreeLabel { .. }
            | Operation::SetSessionName { .. }
            | Operation::SetDefaultAgentProfile { .. } => {
                Err(IntentRouter::unsupported_dispatch(&admission))
            }
        };
        let decision = self
            .runtime_host
            .operation_supervisor
            .finalizer
            .freeze(&execution, &result);
        let commit_result = self
            .runtime_host
            .session_coordinator
            .resolve_finalization(&decision)?;
        self.runtime_host
            .event_hub
            .service
            .emit_recovery_pending(&decision, &commit_result);
        self.persist_operation_terminal_outbox(&decision, &result, &commit_result)?;
        if let Some(guard) = submission.as_mut() {
            guard.finish(&decision, &commit_result)?;
        }
        result
    }

    pub(super) fn run_sync_mut_operation(
        &mut self,
        operation: Operation,
        mut submission: Option<SubmissionCommitGuard>,
    ) -> Result<OperationOutcome, CodingSessionError> {
        let admission = self.resolve_operation_admission_with_id(
            &operation,
            submission
                .as_ref()
                .map(|submission| submission.operation_id.as_str()),
        )?;
        self.runtime_host
            .session_coordinator
            .ensure_write_admission(admission.descriptor.admission_class())?;
        let operation_permit = OperationScheduler::admit(
            &self.runtime_host.operation_supervisor.control,
            &admission,
            OperationDispatchMode::SyncMutable,
        )
        .map_err(|rejection| rejection.into_error())?;
        if let Some(guard) = submission.as_mut() {
            guard.commit_execution(operation_permit.execution())?;
        }
        let execution = operation_permit.execution().clone();

        let result = (|| match operation {
            Operation::RejectDelegationConfirmation {
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
                    crate::runtime::session_coordinator::SessionWriterCommand::reject_delegation(
                        operation_permit.execution().operation_id.clone(),
                        operation_permit.execution().capability_generation,
                        operation_id,
                        tool_call_id,
                        now,
                        reason,
                    ),
                )?;
                let crate::runtime::session_coordinator::SessionWriterReply::DelegationRejected {
                    request,
                    reason,
                } = reply
                else {
                    unreachable!("delegation rejection writer command returns its typed reply")
                };
                self.runtime_host
                    .event_hub
                    .service
                    .emit_delegation_rejected(&request, &reason);
                Ok(OperationOutcome::DelegationRejection)
            }
            Operation::ForkSession { target_leaf_id } => {
                SessionWriteCapability::require(
                    operation_permit
                        .capability_snapshot()
                        .session_write
                        .as_ref(),
                )?;
                let command =
                    crate::runtime::session_coordinator::SessionWriterCommand::fork_session(
                        operation_permit.execution().operation_id.clone(),
                        operation_permit.execution().capability_generation,
                        target_leaf_id,
                    );
                drop(operation_permit);
                let reply = self
                    .runtime_host
                    .session_coordinator
                    .execute_writer_command(command)?;
                let crate::runtime::session_coordinator::SessionWriterReply::ForkedSession {
                    session_id,
                } = reply
                else {
                    unreachable!("fork writer command returns its typed reply")
                };
                self.refresh_snapshot_projection();
                self.runtime_host
                    .event_hub
                    .service
                    .emit_session_opened(session_id);
                Ok(OperationOutcome::ForkSession)
            }
            Operation::SwitchActiveLeaf { target_leaf_id } => {
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
                    crate::runtime::session_coordinator::SessionWriterCommand::switch_active_leaf(
                        operation_permit.execution().operation_id.clone(),
                        operation_permit.execution().capability_generation,
                        target_leaf_id,
                    ),
                )?;
                if !matches!(
                    reply,
                    crate::runtime::session_coordinator::SessionWriterReply::ActiveLeaf
                ) {
                    return Err(CodingSessionError::Session {
                        message: "session writer returned an unexpected switch-leaf reply".into(),
                    });
                }
                self.refresh_snapshot_projection();
                Ok(OperationOutcome::SwitchActiveLeaf)
            }
            Operation::SetSessionTreeLabel { entry_id, label } => {
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
                        crate::runtime::session_coordinator::SessionWriterCommand::
                            set_session_tree_label(
                                operation_permit.execution().operation_id.clone(),
                                operation_permit.execution().capability_generation,
                                entry_id,
                                label,
                            ),
                    )?;
                self.refresh_snapshot_projection();
                let crate::runtime::session_coordinator::SessionWriterReply::SessionTreeLabel {
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
            Operation::SetSessionName { name } => {
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
                        crate::runtime::session_coordinator::SessionWriterCommand::set_session_name(
                            operation_permit.execution().operation_id.clone(),
                            operation_permit.execution().capability_generation,
                            name,
                        ),
                    )?;
                self.refresh_snapshot_projection();
                let crate::runtime::session_coordinator::SessionWriterReply::SessionName {
                    name,
                    updated_at,
                } = reply
                else {
                    unreachable!("session-name writer command returns its typed reply")
                };
                Ok(OperationOutcome::SessionNameChanged { name, updated_at })
            }
            Operation::SetDefaultAgentProfile { profile_id } => {
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
                        crate::runtime::session_coordinator::SessionWriterCommand::
                            set_default_agent_profile(
                                operation_permit.execution().operation_id.clone(),
                                operation_permit
                                .execution()
                                .capability_generation,
                                profile_id.clone(),
                            ),
                    )?;
                match reply {
                    crate::runtime::session_coordinator::SessionWriterReply::DefaultAgentProfile {
                        profile_id: changed_profile_id,
                    } if changed_profile_id == profile_id => {}
                    _ => {
                        return Err(CodingSessionError::Session {
                            message:
                                "session writer returned an unexpected default-profile reply"
                                    .into(),
                        });
                    }
                }
                self.runtime_host
                    .event_hub
                    .service
                    .emit_default_agent_profile_changed(profile_id);
                let installed = self
                    .runtime_host
                    .operation_supervisor
                    .capabilities
                    .install_next_generation(CapabilityRevocationPolicy::FutureOnly)?;
                self.refresh_snapshot_projection();
                self.runtime_host
                    .event_hub
                    .service
                    .emit_capability_changed(installed);
                Ok(OperationOutcome::SetDefaultAgentProfile)
            }
            Operation::Export(_) => Err(IntentRouter::unsupported_dispatch(&admission)),
            Operation::Prompt(_)
            | Operation::ManualCompaction(_)
            | Operation::ApproveDelegationConfirmation { .. }
            | Operation::BranchSummary { .. }
            | Operation::SelfHealingEdit(_)
            | Operation::AgentInvocation(_)
            | Operation::AgentTeam(_) => Err(IntentRouter::unsupported_dispatch(&admission)),
        })();
        let decision = self
            .runtime_host
            .operation_supervisor
            .finalizer
            .freeze(&execution, &result);
        let commit_result = self
            .runtime_host
            .session_coordinator
            .resolve_finalization(&decision)?;
        self.runtime_host
            .event_hub
            .service
            .emit_recovery_pending(&decision, &commit_result);
        self.persist_operation_terminal_outbox(&decision, &result, &commit_result)?;
        if let Some(guard) = submission.as_mut() {
            guard.finish(&decision, &commit_result)?;
        }
        result
    }

    pub(super) async fn run_operation(
        &mut self,
        mut operation: Operation,
        mut submission: Option<SubmissionCommitGuard>,
    ) -> Result<OperationOutcome, CodingSessionError> {
        self.prepare_operation_for_admission(&mut operation)?;
        if let Some(options) = operation.prompt_options_mut()
            && let Some(runtime) = options.runtime_mut()
        {
            self.runtime_host
                .runtime_service
                .install_provider_runtime(runtime);
        }
        let admission = self.resolve_operation_admission_with_id(
            &operation,
            submission
                .as_ref()
                .map(|submission| submission.operation_id.as_str()),
        )?;
        self.runtime_host
            .session_coordinator
            .ensure_write_admission(admission.descriptor.admission_class())?;
        let operation_permit = OperationScheduler::admit(
            &self.runtime_host.operation_supervisor.control,
            &admission,
            OperationDispatchMode::Async,
        )
        .map_err(|rejection| rejection.into_error())?;
        if let Some(guard) = submission.as_mut() {
            guard.commit_execution(operation_permit.execution())?;
        }
        let execution = operation_permit.execution().clone();
        let snapshot = execution.capability_snapshot.clone();
        let operation_cancellation = operation_permit.cancellation_token();
        let operation_cancellation_handle = operation_permit.cancellation_handle();
        if let (Some(submission), Some(cancellation)) =
            (submission.as_ref(), operation_cancellation_handle.clone())
        {
            self.runtime_host
                .client_projection
                .coordinator
                .bind_operation_cancellation(
                    submission.handle.clone(),
                    snapshot.operation_id.clone(),
                    cancellation,
                );
        }

        let result =
            Box::pin(async {
                match operation {
                    Operation::Prompt(options) => {
                        let has_existing_prompt_control = self
                            .runtime_host
                            .operation_supervisor
                            .control
                            .current_prompt_control_registration()
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
                            (submission.as_ref(), prompt_control.as_ref())
                        {
                            self.runtime_host
                                .client_projection
                                .coordinator
                                .bind_prompt_control(
                                    submission.handle.clone(),
                                    snapshot.operation_id.clone(),
                                    prompt_control.generation,
                                    prompt_control.handle.clone(),
                                );
                        }
                        let mut prompt_control_cleanup = prompt_control.map(
                            |PromptControlRegistration { generation, .. }| {
                                PromptControlCleanupGuard::new(
                                    self.runtime_host
                                        .operation_supervisor
                                        .control
                                        .prompt_control_cleanup(),
                                    self.runtime_host.client_projection.coordinator.clone(),
                                    snapshot.operation_id.clone(),
                                    generation,
                                )
                            },
                        );
                        let result = crate::operations::prompt::run(
                            &mut self.runtime_host.session_coordinator.persistence,
                            &mut self.runtime_host.operation_supervisor.control,
                            &self.runtime_host.profile_registry,
                            &self.runtime_host.event_hub.service,
                            &mut self
                                .runtime_host
                                .session_coordinator
                                .pending_delegation_confirmations,
                            &self.runtime_host.authorization_service,
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
                    Operation::ManualCompaction(options) => {
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
                            &self.runtime_host.event_hub.service,
                            options,
                            &snapshot,
                            operation_cancellation_handle.clone(),
                        )
                        .await
                        .map(OperationOutcome::ManualCompaction)
                    }
                    Operation::BranchSummary {
                        options,
                        source_leaf_id,
                        target_leaf_id,
                        custom_instructions,
                        reuse_existing,
                    } => {
                        if reuse_existing
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
                            &self.runtime_host.event_hub.service,
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
                    Operation::SelfHealingEdit(request) => {
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
                            self.runtime_host.event_hub.service.clone(),
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
                            .event_hub
                            .service
                            .emit_session_write_events(&outcome.finalized);
                        outcome.result.map(OperationOutcome::SelfHealingEdit)
                    }
                    Operation::AgentInvocation(options) => {
                        let prompt_control_receiver = self
                            .runtime_host
                            .operation_supervisor
                            .control
                            .take_prompt_control_receiver();
                        self.runtime_host
                            .operation_supervisor
                            .control
                            .clear_prompt_control_receiver();
                        let result = crate::operations::agent_invocation::run(
                            options,
                            snapshot.operation_id.clone(),
                            prompt_control_receiver,
                            &self.runtime_host.profile_registry,
                            &self.runtime_host.event_hub.service,
                            &self.runtime_host.operation_supervisor.control,
                            snapshot.clone(),
                            operation_cancellation.clone(),
                        )
                        .await;
                        result.map(OperationOutcome::AgentInvocation)
                    }
                    Operation::AgentTeam(options) => crate::operations::team_invocation::run(
                        options,
                        snapshot.operation_id.clone(),
                        &self.runtime_host.profile_registry,
                        &self.runtime_host.event_hub.service,
                        &self.runtime_host.operation_supervisor.control,
                        snapshot.clone(),
                        operation_cancellation.clone(),
                    )
                    .await
                    .map(OperationOutcome::AgentTeam),
                    Operation::Export(_)
                    | Operation::RejectDelegationConfirmation { .. }
                    | Operation::ForkSession { .. }
                    | Operation::SwitchActiveLeaf { .. }
                    | Operation::SetSessionTreeLabel { .. }
                    | Operation::SetSessionName { .. }
                    | Operation::SetDefaultAgentProfile { .. } => {
                        Err(IntentRouter::unsupported_dispatch(&admission))
                    }
                    Operation::ApproveDelegationConfirmation {
                        operation_id,
                        tool_call_id,
                    } => crate::operations::delegation::execution::approve(
                        &mut self.runtime_host.session_coordinator,
                        &self.runtime_host.runtime_service,
                        &self.runtime_host.profile_registry,
                        &self.runtime_host.event_hub.service,
                        &self.runtime_host.operation_supervisor.control,
                        operation_id,
                        tool_call_id,
                        admission
                            .admitted_at
                            .expect("delegation approval admission time is resolved"),
                        snapshot.clone(),
                    )
                    .await
                    .map(|_| OperationOutcome::DelegationApproval),
                }
            })
            .await;
        let decision = self
            .runtime_host
            .operation_supervisor
            .finalizer
            .freeze(&execution, &result);
        let commit_result = self
            .runtime_host
            .session_coordinator
            .resolve_finalization(&decision)?;
        self.runtime_host
            .event_hub
            .service
            .emit_recovery_pending(&decision, &commit_result);
        self.persist_operation_terminal_outbox(&decision, &result, &commit_result)?;
        if let Some(guard) = submission.as_mut() {
            guard.finish(&decision, &commit_result)?;
        }
        result
    }

    fn persist_operation_terminal_outbox(
        &self,
        decision: &super::finalization::FinalizationDecision,
        result: &Result<OperationOutcome, CodingSessionError>,
        commit_result: &super::finalization::FinalizationCommitResult,
    ) -> Result<(), CodingSessionError> {
        if !matches!(
            decision.operation_kind,
            crate::runtime::control::OperationKind::Prompt
                | crate::runtime::control::OperationKind::Compact
                | crate::runtime::control::OperationKind::SelfHealingEdit
                | crate::runtime::control::OperationKind::AgentInvocation
                | crate::runtime::control::OperationKind::AgentTeam
        ) || !matches!(
            commit_result,
            super::finalization::FinalizationCommitResult::Committed
                | super::finalization::FinalizationCommitResult::DefinitelyFailed { .. }
        ) {
            return Ok(());
        }
        let (draft, prompt_outcome) = match decision.operation_kind {
            crate::runtime::control::OperationKind::Prompt => {
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
            crate::runtime::control::OperationKind::Compact => {
                let Some(OperationOutcome::ManualCompaction(outcome)) = result.as_ref().ok() else {
                    return Ok(());
                };
                let Some(draft) = self
                    .runtime_host
                    .event_hub
                    .service
                    .take_deferred_terminal_draft(&decision.operation_id)
                else {
                    return Ok(());
                };
                (draft, Some(outcome))
            }
            crate::runtime::control::OperationKind::SelfHealingEdit => {
                let Some(draft) = self
                    .runtime_host
                    .event_hub
                    .service
                    .take_deferred_terminal_draft(&decision.operation_id)
                else {
                    return Ok(());
                };
                (draft, None)
            }
            crate::runtime::control::OperationKind::AgentInvocation
            | crate::runtime::control::OperationKind::AgentTeam => {
                let Some(draft) = self
                    .runtime_host
                    .event_hub
                    .service
                    .take_deferred_terminal_draft(&decision.operation_id)
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
            crate::runtime::control::OperationKind::AgentInvocation
                | crate::runtime::control::OperationKind::AgentTeam
        ) {
            self.runtime_host
                .event_hub
                .service
                .emit_committed_terminal_draft(live_draft, decision.operation_kind);
            return Ok(());
        }
        self.runtime_host
            .session_coordinator
            .persist_terminal_decision(decision, draft)
            .map(|_| {
                if matches!(
                    decision.operation_kind,
                    crate::runtime::control::OperationKind::Compact
                        | crate::runtime::control::OperationKind::SelfHealingEdit
                ) {
                    self.runtime_host
                        .event_hub
                        .service
                        .emit_committed_terminal_draft(live_draft, decision.operation_kind);
                }
                if let Some(outcome) = prompt_outcome
                    && (decision.operation_kind == crate::runtime::control::OperationKind::Prompt
                        || compact_terminal_is_session_event)
                {
                    self.runtime_host
                        .event_hub
                        .service
                        .emit_prompt_terminal(outcome);
                }
            })
            .map(|_| ())
    }
}
