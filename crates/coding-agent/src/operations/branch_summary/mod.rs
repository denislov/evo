use self::runner::{
    BranchSummaryContext, BranchSummaryOptions, BranchSummaryRunner, branch_summary_failed_outcome,
    branch_summary_outcome_text, branch_summary_success_outcome,
};
use crate::operations::prompt::context::{
    InternalPromptTurnOutcome, PromptTurnOptions, RuntimeSnapshot,
};
use crate::runtime::capability::{
    OperationCapabilitySnapshot, SessionReadCapability, SessionWriteCapability,
};
use crate::runtime::control::OperationCancellationHandle;
use crate::runtime::facade::CodingSessionError;
use crate::services::event::EventService;
use crate::services::session::apply_finalized_session_write;
use crate::session::id::{IdGenerator, SystemIdGenerator};
use crate::session::service::{SessionPersistence, SessionService};
use tokio_util::sync::CancellationToken;

pub(crate) mod runner;

pub(crate) fn reused_outcome(
    persistence: &SessionPersistence,
    options: &PromptTurnOptions,
    source_leaf_id: &str,
    target_leaf_id: &str,
    snapshot: &OperationCapabilitySnapshot,
) -> Result<Option<InternalPromptTurnOutcome>, CodingSessionError> {
    SessionReadCapability::require(snapshot.session_read.as_ref())?;
    let runtime = branch_summary_runtime(options)?;
    let SessionPersistence::Persistent(session_service) = persistence else {
        return Err(CodingSessionError::UnsupportedCapability {
            capability: "branch summary without persistent session".into(),
        });
    };
    let Some(summary) = session_service.branch_summary_for(source_leaf_id, target_leaf_id)? else {
        return Ok(None);
    };
    let mut ids = SystemIdGenerator;
    let turn_id = ids.next_turn_id();
    Ok(Some(branch_summary_success_outcome(
        snapshot.operation_id.clone(),
        turn_id,
        session_service.session_id().to_owned(),
        session_service.current_active_leaf_id(),
        &runtime,
        summary,
    )))
}

#[allow(
    clippy::too_many_arguments,
    reason = "typed runner entry keeps source, target, runtime, and cancellation facts explicit"
)]
pub(crate) async fn run(
    session_service: &mut SessionService,
    event_service: &EventService,
    options: PromptTurnOptions,
    source_leaf_id: String,
    target_leaf_id: String,
    custom_instructions: Option<String>,
    snapshot: &OperationCapabilitySnapshot,
    cancellation: Option<CancellationToken>,
    cancellation_handle: Option<OperationCancellationHandle>,
) -> Result<InternalPromptTurnOutcome, CodingSessionError> {
    SessionReadCapability::require(snapshot.session_read.as_ref())?;
    SessionWriteCapability::require(snapshot.session_write.as_ref())?;
    let runtime = branch_summary_runtime(&options)?;
    let mut branch_options = BranchSummaryOptions::new()
        .with_source_leaf_id(source_leaf_id)
        .with_target_leaf_id(target_leaf_id)
        .with_runtime(runtime.clone());
    if let Some(custom_instructions) = custom_instructions {
        branch_options = branch_options.with_custom_instructions(custom_instructions);
    }
    let replay = session_service.replay()?;
    let transaction = session_service.begin_branch_summary_transaction(snapshot);
    let mut context =
        BranchSummaryContext::new(branch_options, replay, transaction, snapshot.clone());
    let operation_id = context.operation_id().to_owned();
    let turn_id = context.turn_id().to_owned();

    let result = match BranchSummaryRunner::new()?
        .run_typed(&mut context, cancellation.clone())
        .await
    {
        Ok(_) => context.finish_success(),
        Err(error) => Err(error),
    };
    match result {
        Ok(branch_summary) => {
            let commit_gate = match cancellation_handle {
                Some(cancellation_handle) => cancellation_handle.close(),
                None if cancellation
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled) =>
                {
                    Err(CodingSessionError::Cancelled)
                }
                None => Ok(()),
            };
            if let Err(error) = commit_gate {
                let reason = error.to_string();
                let mut outcome = InternalPromptTurnOutcome::Aborted {
                    operation_id: operation_id.clone(),
                    turn_id: Some(turn_id),
                    reason: reason.clone(),
                    session_id: Some(session_service.session_id().to_owned()),
                };
                let finalized = session_service.abort_prompt_transaction(
                    context.take_transaction(),
                    operation_id,
                    reason,
                )?;
                apply_finalized_session_write(&mut outcome, &finalized);
                event_service.emit_session_write_events(&finalized);
                return Ok(outcome);
            }
            let final_text = branch_summary_outcome_text(&branch_summary);
            let mut outcome = branch_summary_success_outcome(
                operation_id.clone(),
                turn_id,
                session_service.session_id().to_owned(),
                session_service.current_active_leaf_id(),
                &runtime,
                final_text,
            );
            let finalized = session_service
                .commit_branch_summary_transaction(context.take_transaction(), operation_id)?;
            apply_finalized_session_write(&mut outcome, &finalized);
            event_service.emit_session_write_events(&finalized);
            Ok(outcome)
        }
        Err(error) => {
            if error == CodingSessionError::Cancelled {
                let reason = "branch summary cancelled".to_owned();
                let mut outcome = InternalPromptTurnOutcome::Aborted {
                    operation_id: operation_id.clone(),
                    turn_id: Some(turn_id),
                    reason: reason.clone(),
                    session_id: Some(session_service.session_id().to_owned()),
                };
                let finalized = session_service.abort_prompt_transaction(
                    context.take_transaction(),
                    operation_id,
                    reason,
                )?;
                apply_finalized_session_write(&mut outcome, &finalized);
                event_service.emit_session_write_events(&finalized);
                return Ok(outcome);
            }
            let mut outcome =
                branch_summary_failed_outcome(operation_id.clone(), turn_id, error.clone());
            let finalized = session_service.fail_prompt_transaction(
                context.take_transaction(),
                operation_id,
                error.code(),
                error.to_string(),
            )?;
            apply_finalized_session_write(&mut outcome, &finalized);
            event_service.emit_session_write_events(&finalized);
            Ok(outcome)
        }
    }
}

fn branch_summary_runtime(
    options: &PromptTurnOptions,
) -> Result<RuntimeSnapshot, CodingSessionError> {
    options
        .runtime()
        .cloned()
        .ok_or_else(|| CodingSessionError::Config {
            message: "branch summary options do not include a runtime snapshot".into(),
        })
}

#[cfg(test)]
mod tests {
    use agent_core::api::agent::AgentResources;

    use super::*;
    use crate::app::bootstrap::PromptInvocation;
    use crate::app::prompt_runtime::PromptRuntimeOptions;
    use crate::profiles::ProfileId;
    use crate::runtime::capability::OperationCapabilitySnapshot;
    use crate::session::service::{SessionPersistence, TransientSessionState};

    fn prompt_options() -> PromptTurnOptions {
        PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
            model: ai::api::model::lookup_model("claude-haiku-4-5").unwrap(),
            api_key: None,
            auth_diagnostics: Vec::new(),
            system_prompt: None,
            max_turns: Some(1),
            tools: Vec::new(),
            register_builtins: false,
            ai_client: None,
            session: None,
            session_target: None,
            session_name: None,
            thinking_level: None,
            tool_execution: None,
            resources: AgentResources::default(),
            settings: None,
            invocation: PromptInvocation::Text("summarize".into()),
        })
    }

    #[test]
    fn reused_outcome_requires_session_read_capability() {
        let persistence = SessionPersistence::NonPersistent(TransientSessionState::new(
            ProfileId::from("default"),
        ));
        let mut snapshot = OperationCapabilitySnapshot::permissive("op_branch_reuse");
        snapshot.session_read = None;

        let error = reused_outcome(
            &persistence,
            &prompt_options(),
            "leaf_source",
            "leaf_target",
            &snapshot,
        )
        .unwrap_err();

        assert_eq!(error.code(), "unsupported_capability");
        assert!(
            error
                .to_string()
                .contains("session read capability is not granted")
        );
    }
}
