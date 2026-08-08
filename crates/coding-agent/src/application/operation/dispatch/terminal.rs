use crate::kernel::error::CodingSessionError;
use crate::kernel::operation::OperationKind;
use crate::runtime::facade::CodingAgentSession;

use super::super::OperationOutcome;
use super::super::finalize::{FinalizationCommitResult, FinalizationDecision};

impl CodingAgentSession {
    pub(super) async fn persist_operation_terminal_outbox(
        &self,
        decision: &FinalizationDecision,
        result: &Result<OperationOutcome, CodingSessionError>,
        commit_result: &FinalizationCommitResult,
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
            FinalizationCommitResult::Committed | FinalizationCommitResult::DefinitelyFailed { .. }
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
