use self::runner::{
    ManualCompactionContext, ManualCompactionOptions, ManualCompactionRunner,
    manual_compaction_failed_outcome, manual_compaction_operation_id,
    manual_compaction_success_outcome,
};
use crate::application::capability::OperationCapabilitySnapshot;
use crate::application::operation::control::OperationCancellationHandle;
use crate::kernel::capability::{SessionReadCapability, SessionWriteCapability};
use crate::kernel::error::CodingSessionError;
use crate::operations::prompt::context::InternalPromptTurnOutcome;
use crate::services::event::EventService;
use crate::session::service::SessionService;

pub(crate) mod runner;

pub(crate) async fn run(
    session_service: &mut SessionService,
    event_service: &EventService,
    options: ManualCompactionOptions,
    snapshot: &OperationCapabilitySnapshot,
    cancellation: Option<OperationCancellationHandle>,
) -> Result<InternalPromptTurnOutcome, CodingSessionError> {
    SessionReadCapability::require(snapshot.session_read.as_ref())?;
    SessionWriteCapability::require(snapshot.session_write.as_ref())?;
    let replay = session_service.replay()?;
    let transaction = session_service.begin_manual_compaction_transaction(snapshot);
    let mut context = ManualCompactionContext::new(options, replay, transaction, snapshot.clone());
    let operation_id = context.operation_id().to_owned();
    let turn_id = context.turn_id().to_owned();

    let compaction_result = ManualCompactionRunner::new()?.run_typed(&mut context).await;
    match compaction_result {
        Ok(compaction) => {
            if let Some(cancellation) = cancellation
                && let Err(error) = cancellation.close()
            {
                let mut outcome =
                    manual_compaction_failed_outcome(operation_id.clone(), turn_id, error.clone());
                let finalized = session_service
                    .fail_prompt_transaction(
                        context.take_transaction(),
                        operation_id,
                        error.code(),
                        error.to_string(),
                    )
                    .await?;
                outcome.apply_success_session_write_metadata(
                    finalized.session_id.clone(),
                    finalized.leaf_id.clone(),
                );
                event_service.emit_session_write_events(&finalized)?;
                defer_compact_terminal(event_service, &outcome)?;
                return Ok(outcome);
            }
            let mut outcome = manual_compaction_success_outcome(
                operation_id.clone(),
                turn_id.clone(),
                session_service.session_id().to_owned(),
                session_service.current_active_leaf_id()?,
                &compaction,
            );
            let finalized = session_service
                .commit_manual_compaction_transaction(
                    context.take_transaction(),
                    operation_id.clone(),
                )
                .await?;
            outcome.apply_success_session_write_metadata(
                finalized.session_id.clone(),
                finalized.leaf_id.clone(),
            );

            event_service.emit_session_write_pending(&finalized)?;
            event_service.defer_terminal_draft(
                operation_id.clone(),
                crate::events::session::SessionCompactionEvent {
                    operation_id: operation_id.clone(),
                    turn_id,
                    summary: compaction.summary.clone(),
                    first_kept_message_id: compaction.first_kept_message_id.clone(),
                    tokens_before: compaction.tokens_before,
                }
                .into_product_draft(),
            )?;
            event_service.emit_session_write_committed(&finalized)?;
            Ok(outcome)
        }
        Err(error) => {
            let mut outcome =
                manual_compaction_failed_outcome(operation_id.clone(), turn_id, error.clone());
            let finalized = session_service
                .fail_prompt_transaction(
                    context.take_transaction(),
                    operation_id,
                    error.code(),
                    error.to_string(),
                )
                .await?;
            outcome.apply_success_session_write_metadata(
                finalized.session_id.clone(),
                finalized.leaf_id.clone(),
            );
            event_service.emit_session_write_events(&finalized)?;
            defer_compact_terminal(event_service, &outcome)?;
            Ok(outcome)
        }
    }
}

fn defer_compact_terminal(
    event_service: &EventService,
    outcome: &InternalPromptTurnOutcome,
) -> Result<(), CodingSessionError> {
    event_service.emit_prompt_diagnostics(outcome)?;
    if let Some(draft) = EventService::prompt_terminal_draft(outcome) {
        let operation_id = manual_compaction_operation_id(outcome);
        event_service.defer_terminal_draft(operation_id.to_owned(), draft)?;
    }
    Ok(())
}
