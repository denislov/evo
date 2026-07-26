use super::{
    PendingDelegationConfirmation, PendingDelegationConfirmationQueue,
    PendingDelegationConfirmationState, delegation_runtime_seed_from_prompt_options,
};
use crate::runtime::facade::CodingSessionError;
use crate::services::event::EventService;
use crate::session::service::SessionPersistence;

pub(crate) fn active_views(
    queue: &PendingDelegationConfirmationQueue,
    now: &str,
) -> Vec<PendingDelegationConfirmation> {
    queue.active_views(now)
}

pub(crate) fn active_pending(
    queue: &PendingDelegationConfirmationQueue,
    operation_id: &str,
    tool_call_id: &str,
    now: &str,
) -> Result<PendingDelegationConfirmationState, CodingSessionError> {
    queue
        .active_pending(operation_id, tool_call_id, now)
        .cloned()
        .ok_or_else(|| pending_delegation_confirmation_not_found(operation_id, tool_call_id))
}

pub(crate) fn adopt_pending(
    persistence: &mut SessionPersistence,
    queue: &mut PendingDelegationConfirmationQueue,
    event_service: &EventService,
    pending_confirmations: Vec<PendingDelegationConfirmationState>,
) -> Result<(), CodingSessionError> {
    for pending in pending_confirmations {
        queue_pending(persistence, queue, event_service, pending, false)?;
    }
    Ok(())
}

pub(crate) fn queue_pending(
    persistence: &mut SessionPersistence,
    queue: &mut PendingDelegationConfirmationQueue,
    event_service: &EventService,
    pending: PendingDelegationConfirmationState,
    emit_confirmation_required: bool,
) -> Result<(), CodingSessionError> {
    if queue.is_duplicate(&pending) {
        event_service.emit_diagnostic(
                Some(pending.request.operation_id.clone()),
                format!(
                    "duplicate pending delegation confirmation ignored: operation_id={}, tool_call_id={}",
                    pending.request.operation_id, pending.request.tool_call_id
                ),
            );
        return Ok(());
    }
    record_delegation_confirmation_requested(persistence, &pending)?;
    if emit_confirmation_required {
        event_service.emit_delegation_confirmation_required(&pending.request, &pending.reason);
    }
    queue.push(pending);
    Ok(())
}

fn pending_delegation_confirmation_not_found(
    operation_id: &str,
    tool_call_id: &str,
) -> CodingSessionError {
    CodingSessionError::Input {
        message: format!(
            "pending delegation confirmation not found: operation_id={operation_id}, tool_call_id={tool_call_id}"
        ),
    }
}

fn record_delegation_confirmation_requested(
    persistence: &mut SessionPersistence,
    pending: &PendingDelegationConfirmationState,
) -> Result<(), CodingSessionError> {
    let runtime_seed = delegation_runtime_seed_from_prompt_options(
        &pending.prompt_options,
        pending.child_delegation_depth,
        &pending.delegation_lineage,
    )?;
    if let SessionPersistence::Persistent(session_service) = persistence {
        session_service.record_delegation_confirmation_requested(
            pending.request.operation_id.clone(),
            pending.request.turn_id.clone(),
            pending.request.tool_call_id.clone(),
            pending.request.requesting_profile_id.clone(),
            pending.request.target_kind,
            pending.request.target_id.clone(),
            pending.request.task.clone(),
            pending.reason.clone(),
            runtime_seed,
        )?;
    }
    Ok(())
}
