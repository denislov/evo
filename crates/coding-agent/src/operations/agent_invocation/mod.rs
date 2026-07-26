pub(crate) mod runner;

use crate::profiles::ProfileRegistry;
use crate::runtime::capability::OperationCapabilitySnapshot;
use crate::runtime::control::{OperationControl, PromptControlReceiver};
use crate::runtime::facade::CodingSessionError;
use crate::services::event::EventService;
use runner::{
    AgentInvocationContext, AgentInvocationOptions, AgentInvocationOutcome, AgentInvocationRunner,
};
use tokio_util::sync::CancellationToken;

#[allow(
    clippy::too_many_arguments,
    reason = "typed runner entry keeps scheduler, profile, event, and cancellation owners explicit"
)]
pub(crate) async fn run(
    options: AgentInvocationOptions,
    scheduler_parent_operation_id: String,
    prompt_control_receiver: Option<PromptControlReceiver>,
    profile_registry: &ProfileRegistry,
    event_service: &EventService,
    operation_control: &OperationControl,
    parent_capability_snapshot: OperationCapabilitySnapshot,
    cancellation: Option<CancellationToken>,
) -> Result<AgentInvocationOutcome, CodingSessionError> {
    let mut context = AgentInvocationContext::new(
        options,
        profile_registry.clone(),
        event_service.clone(),
        operation_control.clone(),
        scheduler_parent_operation_id,
    )
    .with_deferred_terminal_publication()
    .with_parent_capability_snapshot(parent_capability_snapshot);
    if let Some(receiver) = prompt_control_receiver {
        context.set_prompt_control_receiver(receiver);
    }
    let result = match AgentInvocationRunner::new()?
        .run_typed(&mut context, cancellation)
        .await
    {
        Ok(_) => context.finish_success(),
        Err(error) => Err(error),
    };
    if let Err(error) = &result {
        context.ensure_failure_terminal_draft(error);
    }
    result
}
