use std::sync::Arc;

use self::runner::{
    ModelSelfHealingEditRepairStrategy, PlannedSelfHealingEditRepairStrategy,
    SelfHealingEditContext, SelfHealingEditOptions, SelfHealingEditOutcome,
    SelfHealingEditRepairStrategy, SelfHealingEditReplacement, SelfHealingEditRunner,
};
use crate::operations::prompt::context::PromptTurnOptions;
use crate::runtime::capability::{
    ModelCapability, OperationCapabilitySnapshot, SessionWriteCapability,
};
use crate::runtime::control::OperationCancellationHandle;
use crate::runtime::facade::CodingSessionError;
use crate::services::event::{EventService, SelfHealingEditEventObserver};
use crate::services::session::{default_cwd, session_cwd};
use crate::session::service::{FinalizedSessionWrite, SessionService};
use tokio_util::sync::CancellationToken;

pub(crate) mod runner;

pub(crate) struct SelfHealingEditExecution {
    pub(crate) result: Result<SelfHealingEditOutcome, CodingSessionError>,
    pub(crate) finalized: FinalizedSessionWrite,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    session_service: &mut SessionService,
    event_service: EventService,
    path: String,
    replacements: Vec<SelfHealingEditReplacement>,
    check_command: Option<String>,
    repair_attempts: Vec<Vec<SelfHealingEditReplacement>>,
    model_repair_policy: Option<(Arc<dyn SelfHealingEditRepairStrategy>, usize)>,
    snapshot: &OperationCapabilitySnapshot,
    cancellation: Option<CancellationToken>,
    cancellation_handle: Option<OperationCancellationHandle>,
) -> Result<SelfHealingEditExecution, CodingSessionError> {
    SessionWriteCapability::require(snapshot.session_write.as_ref())?;
    let replacement_count = replacements.len();
    let event_path = path.clone();
    let cwd = session_cwd(session_service).unwrap_or_else(default_cwd);
    let mut transaction = session_service.begin_self_healing_edit_transaction(snapshot);
    let operation_id = transaction.operation_id().to_owned();
    event_service.emit_self_healing_edit_started(
        operation_id.clone(),
        event_path.clone(),
        replacement_count,
    );
    SessionService::record_self_healing_edit_started(
        &mut transaction,
        path.clone(),
        replacement_count,
    )?;
    let mut options =
        SelfHealingEditOptions::new(cwd, path, replacements)?.with_repair_observer(Arc::new(
            SelfHealingEditEventObserver::new(event_service.clone(), operation_id.clone()),
        ));
    if let Some(command) = check_command {
        options = options.with_check_command(command).with_real_check_runner();
    }
    let repair_attempt_count = repair_attempts.len();
    if repair_attempt_count > 0 {
        options = options
            .with_repair_strategy(Arc::new(PlannedSelfHealingEditRepairStrategy::new(
                repair_attempts,
            )))
            .with_max_repair_attempts(repair_attempt_count);
    } else if let Some((strategy, max_attempts)) = model_repair_policy {
        options = options
            .with_repair_strategy(strategy)
            .with_max_repair_attempts(max_attempts);
    }
    let mut context = SelfHealingEditContext::new(options);
    if let Some(cancellation_handle) = cancellation_handle {
        context.set_cancellation_handle(cancellation_handle);
    }

    let result = match SelfHealingEditRunner::new()?
        .run_typed(&mut context, cancellation.clone())
        .await
    {
        Ok(_) => context.finish_success(),
        Err(error) => Err(error),
    };
    match result {
        Ok(outcome) => {
            if cancellation
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                let error = CodingSessionError::Cancelled;
                for repair in outcome.repair_attempts.iter() {
                    SessionService::record_self_healing_edit_repair_attempted(
                        &mut transaction,
                        &outcome.path,
                        repair,
                    )?;
                }
                let finalized = session_service.fail_self_healing_edit_transaction(
                    Some(transaction),
                    operation_id.clone(),
                    error.code(),
                    error.to_string(),
                )?;
                event_service.defer_terminal_draft(
                    operation_id.clone(),
                    EventService::self_healing_edit_error_draft(operation_id, event_path, &error),
                );
                return Ok(SelfHealingEditExecution {
                    result: Err(error),
                    finalized,
                });
            }
            for repair in outcome.repair_attempts.iter() {
                SessionService::record_self_healing_edit_repair_attempted(
                    &mut transaction,
                    &outcome.path,
                    repair,
                )?;
            }
            SessionService::record_self_healing_edit_completed(&mut transaction, &outcome)?;
            let finalized = session_service
                .commit_self_healing_edit_transaction(Some(transaction), operation_id.clone())?;
            event_service.defer_terminal_draft(
                operation_id.clone(),
                EventService::self_healing_edit_completed_draft(operation_id, &outcome),
            );
            Ok(SelfHealingEditExecution {
                result: Ok(outcome),
                finalized,
            })
        }
        Err(error) => {
            for repair in context.repair_attempts() {
                SessionService::record_self_healing_edit_repair_attempted(
                    &mut transaction,
                    &event_path,
                    repair,
                )?;
            }
            let finalized = session_service.fail_self_healing_edit_transaction(
                Some(transaction),
                operation_id.clone(),
                error.code(),
                error.to_string(),
            )?;
            event_service.defer_terminal_draft(
                operation_id.clone(),
                EventService::self_healing_edit_error_draft(operation_id, event_path, &error),
            );
            Ok(SelfHealingEditExecution {
                result: Err(error),
                finalized,
            })
        }
    }
}

#[allow(clippy::type_complexity)]
pub(crate) fn model_repair_policy(
    prompt_options: PromptTurnOptions,
    max_attempts: usize,
    snapshot: &OperationCapabilitySnapshot,
) -> Result<(Arc<dyn SelfHealingEditRepairStrategy>, usize), CodingSessionError> {
    let runtime = prompt_options
        .runtime()
        .cloned()
        .ok_or_else(|| CodingSessionError::Config {
            message: "self-healing edit model repair options do not include a runtime snapshot"
                .into(),
        })?;
    let model_capability = ModelCapability::require(snapshot.model.as_ref(), runtime.profile_id())?;
    Ok((
        Arc::new(ModelSelfHealingEditRepairStrategy::new(
            runtime,
            model_capability.clone(),
        )),
        max_attempts,
    ))
}
