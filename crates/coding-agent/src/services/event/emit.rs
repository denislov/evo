use super::*;

impl EventService {
    pub(crate) fn emit_tool_authorization_required(
        &self,
        request: crate::authorization::ToolAuthorizationRequest,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_without_root_terminal(
            ToolEvent::AuthorizationRequired { request }.into_product_draft(),
        )
    }

    pub(crate) fn emit_tool_authorization_approved(
        &self,
        request: crate::authorization::ToolAuthorizationRequest,
        decision: crate::authorization::ToolAuthorizationDecision,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_without_root_terminal(
            ToolEvent::AuthorizationApproved { request, decision }.into_product_draft(),
        )
    }

    pub(crate) fn emit_tool_authorization_denied(
        &self,
        request: crate::authorization::ToolAuthorizationRequest,
        reason: impl Into<String>,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_without_root_terminal(
            ToolEvent::AuthorizationDenied {
                request,
                reason: reason.into(),
            }
            .into_product_draft(),
        )
    }

    pub(crate) fn emit_tool_authorization_cancelled(
        &self,
        request: crate::authorization::ToolAuthorizationRequest,
        reason: impl Into<String>,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_without_root_terminal(
            ToolEvent::AuthorizationCancelled {
                request,
                reason: reason.into(),
            }
            .into_product_draft(),
        )
    }

    pub(crate) fn emit_session_opened(
        &self,
        session_id: impl Into<String>,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_without_root_terminal(
            SessionLifecycleEvent::Opened {
                session_id: session_id.into(),
            }
            .into_product_draft(),
        )
    }

    pub(crate) fn emit_diagnostic(
        &self,
        operation_id: Option<impl Into<String>>,
        message: impl Into<String>,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_without_root_terminal(
            DiagnosticEvent::Diagnostic {
                operation_id: operation_id.map(Into::into),
                message: message.into(),
            }
            .into_product_draft(),
        )
    }

    pub(crate) fn emit_capability_changed(
        &self,
        installed: InstalledCapabilityGeneration,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_without_root_terminal(
            CapabilityEvent::Changed {
                generation: installed.generation.get(),
                revocation: installed.revocation,
                cancellation_requested_operation_ids: installed
                    .cancellation_requested_operation_ids,
            }
            .into_product_draft(),
        )
    }

    pub(crate) fn emit_runtime_shutdown(&self) -> Result<ProductEvent, CodingSessionError> {
        self.publish_without_root_terminal(RuntimeEvent::ShutDown.into_product_draft())
    }

    pub(crate) fn emit_prompt_started(
        &self,
        operation_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_prompt_event(PromptEvent::Started {
            operation_id: operation_id.into(),
            turn_id: turn_id.into(),
        })
    }

    pub(crate) fn emit_events_before_prompt_outcome(
        &self,
        events: &[PromptStreamEvent],
    ) -> Result<(), CodingSessionError> {
        for event in events {
            self.publish_prompt_stream_event(event.clone())?;
        }
        Ok(())
    }

    pub(crate) fn session_write_pending_event(
        operation_id: impl Into<String>,
    ) -> SessionWriteEvent {
        SessionWriteEvent::Pending {
            operation_id: operation_id.into(),
        }
    }

    pub(crate) fn session_write_committed_event(
        operation_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> SessionWriteEvent {
        SessionWriteEvent::Committed {
            operation_id: operation_id.into(),
            session_id: session_id.into(),
        }
    }

    pub(crate) fn session_write_skipped_event(
        operation_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> SessionWriteEvent {
        SessionWriteEvent::Skipped {
            operation_id: operation_id.into(),
            reason: reason.into(),
        }
    }

    pub(crate) fn session_write_failed_event(
        operation_id: impl Into<String>,
        reason: impl Into<String>,
        status: CodingAgentSessionWriteFailureStatus,
        failure_reason: Option<CodingAgentSessionWriteFailureReason>,
    ) -> SessionWriteEvent {
        SessionWriteEvent::Failed {
            operation_id: operation_id.into(),
            reason: reason.into(),
            status,
            failure_reason,
        }
    }

    pub(crate) fn emit_prompt_completed(
        &self,
        operation_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_prompt_event(PromptEvent::Completed {
            operation_id: operation_id.into(),
            turn_id: turn_id.into(),
        })
    }

    pub(crate) fn emit_prompt_aborted(
        &self,
        operation_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_prompt_event(PromptEvent::Aborted {
            operation_id: operation_id.into(),
            reason: reason.into(),
        })
    }

    pub(crate) fn emit_prompt_failed(
        &self,
        operation_id: impl Into<String>,
        error: CodingSessionError,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_prompt_event(PromptEvent::Failed {
            operation_id: operation_id.into(),
            error,
        })
    }

    pub(crate) fn emit_session_write_events(
        &self,
        finalized: &FinalizedSessionWrite,
    ) -> Result<(), CodingSessionError> {
        for event in &finalized.events {
            self.publish_without_root_terminal(event.clone().into_product_draft())?;
        }
        Ok(())
    }

    pub(crate) fn emit_session_write_pending(
        &self,
        finalized: &FinalizedSessionWrite,
    ) -> Result<(), CodingSessionError> {
        for event in &finalized.events {
            if event.is_pending() {
                self.publish_without_root_terminal(event.clone().into_product_draft())?;
            }
        }
        Ok(())
    }

    pub(crate) fn emit_session_write_committed(
        &self,
        finalized: &FinalizedSessionWrite,
    ) -> Result<(), CodingSessionError> {
        for event in &finalized.events {
            if event.is_final() {
                self.publish_without_root_terminal(event.clone().into_product_draft())?;
            }
        }
        Ok(())
    }

    pub(crate) fn emit_prompt_terminal(
        &self,
        outcome: &InternalPromptTurnOutcome,
    ) -> Result<(), CodingSessionError> {
        match outcome {
            InternalPromptTurnOutcome::Success {
                operation_id,
                turn_id,
                ..
            } => {
                self.emit_prompt_completed(operation_id.clone(), turn_id.clone())?;
            }
            InternalPromptTurnOutcome::Aborted {
                operation_id,
                reason,
                ..
            } => {
                self.emit_prompt_aborted(operation_id.clone(), reason.clone())?;
            }
            InternalPromptTurnOutcome::Failed {
                operation_id,
                error,
                ..
            } => {
                if !matches!(error, CodingSessionError::PartialCommit { .. }) {
                    self.emit_prompt_failed(operation_id.clone(), error.clone())?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn prompt_terminal_draft(
        outcome: &InternalPromptTurnOutcome,
    ) -> Option<ProductEventDraft> {
        let draft = match outcome {
            InternalPromptTurnOutcome::Success {
                operation_id,
                turn_id,
                ..
            } => PromptEvent::Completed {
                operation_id: operation_id.clone(),
                turn_id: turn_id.clone(),
            }
            .into_product_draft(),
            InternalPromptTurnOutcome::Aborted {
                operation_id,
                reason,
                ..
            } => PromptEvent::Aborted {
                operation_id: operation_id.clone(),
                reason: reason.clone(),
            }
            .into_product_draft(),
            InternalPromptTurnOutcome::Failed {
                operation_id,
                error,
                ..
            } if !matches!(error, CodingSessionError::PartialCommit { .. }) => {
                PromptEvent::Failed {
                    operation_id: operation_id.clone(),
                    error: error.clone(),
                }
                .into_product_draft()
            }
            InternalPromptTurnOutcome::Failed { .. } => return None,
        };
        Some(draft)
    }

    pub(crate) fn emit_agent_invocation_started(
        &self,
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        profile_id: impl Into<ProfileId>,
        task: impl Into<String>,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_agent_invocation_event(AgentInvocationEvent::Started {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            profile_id: profile_id.into(),
            task: task.into(),
        })
    }

    pub(crate) fn agent_invocation_completed_draft(
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        profile_id: impl Into<ProfileId>,
        final_text: impl Into<String>,
    ) -> ProductEventDraft {
        AgentInvocationEvent::Completed {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            profile_id: profile_id.into(),
            final_text: final_text.into(),
        }
        .into_product_draft()
    }

    pub(crate) fn agent_invocation_failed_draft(
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        profile_id: impl Into<ProfileId>,
        error: &CodingSessionError,
    ) -> ProductEventDraft {
        AgentInvocationEvent::Failed {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            profile_id: profile_id.into(),
            error: error.clone(),
        }
        .into_product_draft()
    }

    pub(crate) fn agent_invocation_aborted_draft(
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        profile_id: impl Into<ProfileId>,
        reason: impl Into<String>,
    ) -> ProductEventDraft {
        AgentInvocationEvent::Aborted {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            profile_id: profile_id.into(),
            reason: reason.into(),
        }
        .into_product_draft()
    }

    pub(crate) fn emit_agent_team_started(
        &self,
        operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        task: impl Into<String>,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_team_event(TeamEvent::Started {
            operation_id: operation_id.into(),
            team_id: team_id.into(),
            task: task.into(),
        })
    }

    pub(crate) fn agent_team_completed_draft(
        operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        final_text: impl Into<String>,
    ) -> ProductEventDraft {
        TeamEvent::Completed {
            operation_id: operation_id.into(),
            team_id: team_id.into(),
            final_text: final_text.into(),
        }
        .into_product_draft()
    }

    pub(crate) fn agent_team_failed_draft(
        operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        error: &CodingSessionError,
    ) -> ProductEventDraft {
        TeamEvent::Failed {
            operation_id: operation_id.into(),
            team_id: team_id.into(),
            error: error.clone(),
        }
        .into_product_draft()
    }

    pub(crate) fn agent_team_aborted_draft(
        operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        reason: impl Into<String>,
    ) -> ProductEventDraft {
        TeamEvent::Aborted {
            operation_id: operation_id.into(),
            team_id: team_id.into(),
            reason: reason.into(),
        }
        .into_product_draft()
    }

    pub(crate) fn emit_agent_team_member_started(
        &self,
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        profile_id: impl Into<ProfileId>,
        task: impl Into<String>,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_team_event(TeamEvent::MemberStarted {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            team_id: team_id.into(),
            profile_id: profile_id.into(),
            task: task.into(),
        })
    }

    pub(crate) fn emit_agent_team_member_completed(
        &self,
        operation_id: impl Into<String>,
        child_operation_id: impl Into<String>,
        team_id: impl Into<ProfileId>,
        profile_id: impl Into<ProfileId>,
        final_text: impl Into<String>,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_team_event(TeamEvent::MemberCompleted {
            operation_id: operation_id.into(),
            child_operation_id: child_operation_id.into(),
            team_id: team_id.into(),
            profile_id: profile_id.into(),
            final_text: final_text.into(),
        })
    }

    pub(crate) fn emit_prompt_diagnostics(
        &self,
        outcome: &InternalPromptTurnOutcome,
    ) -> Result<(), CodingSessionError> {
        let (operation_id, diagnostics) = match outcome {
            InternalPromptTurnOutcome::Success {
                operation_id,
                diagnostics,
                ..
            }
            | InternalPromptTurnOutcome::Failed {
                operation_id,
                diagnostics,
                ..
            } => (operation_id, diagnostics),
            InternalPromptTurnOutcome::Aborted { .. } => return Ok(()),
        };
        for diagnostic in diagnostics {
            self.emit_diagnostic(Some(operation_id.clone()), diagnostic.message.clone())?;
        }
        Ok(())
    }

    pub(crate) fn emit_delegation_approved(
        &self,
        request: &DelegationRequest,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_prompt_stream_event(PromptStreamEvent::Delegation(DelegationEvent::Approved {
            context: delegation_event_context(request),
        }))
    }

    pub(crate) fn emit_delegation_rejected(
        &self,
        request: &DelegationRequest,
        reason: &str,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_prompt_stream_event(PromptStreamEvent::Delegation(DelegationEvent::Rejected {
            context: delegation_event_context(request),
            reason: reason.to_owned(),
        }))
    }

    pub(crate) fn emit_delegation_confirmation_required(
        &self,
        request: &DelegationRequest,
        reason: &str,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_prompt_stream_event(PromptStreamEvent::Delegation(
            DelegationEvent::ConfirmationRequired {
                context: delegation_event_context(request),
                reason: reason.to_owned(),
            },
        ))
    }

    pub(crate) fn emit_delegation_started(
        &self,
        request: &DelegationRequest,
        child_operation_id: impl Into<String>,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_prompt_stream_event(PromptStreamEvent::Delegation(DelegationEvent::Started {
            context: delegation_event_context(request),
            child_operation_id: child_operation_id.into(),
        }))
    }

    pub(crate) fn emit_delegation_completed(
        &self,
        request: &DelegationRequest,
        child_operation_id: impl Into<String>,
        final_text: impl Into<String>,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_prompt_stream_event(PromptStreamEvent::Delegation(
            DelegationEvent::Completed {
                context: delegation_event_context(request),
                child_operation_id: child_operation_id.into(),
                final_text: final_text.into(),
            },
        ))
    }

    pub(crate) fn emit_delegation_failed(
        &self,
        request: &DelegationRequest,
        child_operation_id: impl Into<String>,
        error: CodingSessionError,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish_prompt_stream_event(PromptStreamEvent::Delegation(DelegationEvent::Failed {
            context: delegation_event_context(request),
            child_operation_id: child_operation_id.into(),
            error,
        }))
    }

    pub(crate) fn emit_merge_proposal_created(
        &self,
        operation_id: impl Into<String>,
        worktree_id: impl Into<String>,
        child_operation_id: impl Into<String>,
    ) -> Result<(), CodingSessionError> {
        self.publish_merge_event(MergeEvent::ProposalCreated {
            operation_id: operation_id.into(),
            worktree_id: worktree_id.into(),
            child_operation_id: child_operation_id.into(),
        })?;
        Ok(())
    }

    pub(crate) fn emit_merge_applied(
        &self,
        operation_id: impl Into<String>,
        worktree_id: impl Into<String>,
        applied: usize,
    ) -> Result<(), CodingSessionError> {
        self.publish_merge_event(MergeEvent::Applied {
            operation_id: operation_id.into(),
            worktree_id: worktree_id.into(),
            applied,
        })?;
        Ok(())
    }

    pub(crate) fn emit_merge_conflicted(
        &self,
        operation_id: impl Into<String>,
        worktree_id: impl Into<String>,
        paths: Vec<String>,
    ) -> Result<(), CodingSessionError> {
        self.publish_merge_event(MergeEvent::Conflicted {
            operation_id: operation_id.into(),
            worktree_id: worktree_id.into(),
            paths,
        })?;
        Ok(())
    }

    pub(crate) fn emit_merge_stale_parent(
        &self,
        operation_id: impl Into<String>,
        worktree_id: impl Into<String>,
        expected: Option<String>,
        actual: Option<String>,
    ) -> Result<(), CodingSessionError> {
        self.publish_merge_event(MergeEvent::StaleParent {
            operation_id: operation_id.into(),
            worktree_id: worktree_id.into(),
            expected,
            actual,
        })?;
        Ok(())
    }

    pub(crate) fn emit_merge_discarded(
        &self,
        operation_id: impl Into<String>,
        worktree_id: impl Into<String>,
    ) -> Result<(), CodingSessionError> {
        self.publish_merge_event(MergeEvent::Discarded {
            operation_id: operation_id.into(),
            worktree_id: worktree_id.into(),
        })?;
        Ok(())
    }

    pub(crate) fn emit_merge_failed(
        &self,
        operation_id: impl Into<String>,
        worktree_id: impl Into<String>,
        error: &CodingSessionError,
    ) -> Result<(), CodingSessionError> {
        self.publish_merge_event(MergeEvent::Failed {
            operation_id: operation_id.into(),
            worktree_id: worktree_id.into(),
            error: error.clone(),
        })?;
        Ok(())
    }

    pub(crate) fn emit_self_healing_edit_started(
        &self,
        operation_id: impl Into<String>,
        path: impl Into<String>,
        replacements: usize,
    ) -> Result<(), CodingSessionError> {
        self.publish_self_healing_edit_event(SelfHealingEditEvent::Started {
            operation_id: operation_id.into(),
            path: path.into(),
            replacements,
        })?;
        Ok(())
    }

    pub(crate) fn emit_self_healing_edit_repair_attempted(
        &self,
        operation_id: impl Into<String>,
        path: impl Into<String>,
        repair: &SelfHealingEditRepairAttempt,
    ) -> Result<(), CodingSessionError> {
        self.publish_self_healing_edit_event(SelfHealingEditEvent::RepairAttempted {
            operation_id: operation_id.into(),
            path: path.into(),
            attempt: repair.attempt,
            replacements: repair.replacements.clone(),
            diagnostics: repair.diagnostics.clone(),
            check_output: repair.check_output.clone(),
        })?;
        Ok(())
    }

    pub(crate) fn self_healing_edit_completed_draft(
        operation_id: impl Into<String>,
        outcome: &SelfHealingEditOutcome,
    ) -> ProductEventDraft {
        SelfHealingEditEvent::Completed {
            operation_id: operation_id.into(),
            path: outcome.path.clone(),
            attempts: outcome.attempts,
            first_changed_line: outcome.first_changed_line,
            check_output: outcome.check_output.clone(),
        }
        .into_product_draft()
    }

    pub(crate) fn self_healing_edit_error_draft(
        operation_id: impl Into<String>,
        path: impl Into<String>,
        error: &CodingSessionError,
    ) -> ProductEventDraft {
        if error == &CodingSessionError::Cancelled {
            SelfHealingEditEvent::Aborted {
                operation_id: operation_id.into(),
                path: path.into(),
                reason: error.to_string(),
            }
        } else {
            SelfHealingEditEvent::Failed {
                operation_id: operation_id.into(),
                path: path.into(),
                error: error.clone(),
            }
        }
        .into_product_draft()
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "recovery event emission keeps durable association metadata explicit"
    )]
    pub(crate) fn emit_startup_recovery_pending(
        &self,
        operation_id: impl Into<String>,
        recovery_id: impl Into<String>,
        reason: impl Into<String>,
        session_id: impl Into<String>,
        operation_kind: Option<OperationKind>,
        capability_generation: Option<u64>,
        attempt_count: u32,
        last_attempt_at: Option<String>,
        next_attempt_at: Option<String>,
    ) -> Result<ProductEvent, CodingSessionError> {
        let operation_id = operation_id.into();
        self.publish(
            RecoveryPendingEvent {
                operation_id: operation_id.clone(),
                recovery_id: recovery_id.into(),
                reason: reason.into(),
                session_id: session_id.into(),
                record_version: crate::events::recovery::RECOVERY_RECORD_VERSION,
                descriptor_revision: crate::kernel::operation::OPERATION_DESCRIPTOR_REVISION,
                capability_generation,
                attempt_count,
                last_attempt_at,
                next_attempt_at,
            }
            .into_product_draft(),
            ProductEventEmissionContext {
                capability_generation: capability_generation.map(CapabilityGeneration::new),
                operation_kind,
                root_operation_id: Some(operation_id),
            },
            |_, _| None,
        )
    }

    pub(crate) fn emit_recovery_pending(
        &self,
        decision: &FinalizationDecision,
        commit_result: &FinalizationCommitResult,
    ) -> Result<Option<ProductEvent>, CodingSessionError> {
        let FinalizationCommitResult::InDoubt { recovery_id } = commit_result else {
            return Ok(None);
        };
        let Some(session_id) = decision.session_identity.clone() else {
            return Ok(None);
        };
        Ok(Some(
            self.publish_without_root_terminal(
                RecoveryPendingEvent {
                    operation_id: decision.operation_id.clone(),
                    recovery_id: recovery_id.clone(),
                    reason: "session commit outcome requires recovery inspection".into(),
                    session_id,
                    record_version: crate::events::recovery::RECOVERY_RECORD_VERSION,
                    descriptor_revision: decision.descriptor.revision,
                    capability_generation: Some(decision.capability_generation.get()),
                    attempt_count: 0,
                    last_attempt_at: None,
                    next_attempt_at: None,
                }
                .into_product_draft(),
            )?,
        ))
    }

    pub(crate) fn emit_committed_recovery_pending_draft(
        &self,
        draft: ProductEventDraft,
        operation_kind: Option<OperationKind>,
        capability_generation: Option<u64>,
    ) -> Result<ProductEvent, CodingSessionError> {
        self.publish(
            draft,
            ProductEventEmissionContext {
                operation_kind,
                capability_generation: capability_generation.map(CapabilityGeneration::new),
                ..ProductEventEmissionContext::default()
            },
            |_, _| None,
        )
    }
}
