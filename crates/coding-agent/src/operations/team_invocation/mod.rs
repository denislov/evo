pub(crate) mod runner;

use crate::application::capability::OperationCapabilitySnapshot;
use crate::application::operation::control::OperationControl;
use crate::kernel::error::CodingSessionError;
use crate::profiles::ProfileRegistry;
use crate::services::event::EventService;
use crate::services::ports::ExtensionEventDispatch;
use runner::{AgentTeamContext, AgentTeamOptions, AgentTeamOutcome, AgentTeamRunner};
use tokio_util::sync::CancellationToken;

#[allow(
    clippy::too_many_arguments,
    reason = "typed runner entry keeps scheduler, profile, event, and extension owners explicit"
)]
pub(crate) async fn run(
    options: AgentTeamOptions,
    scheduler_parent_operation_id: String,
    profile_registry: &ProfileRegistry,
    event_service: &EventService,
    operation_control: &OperationControl,
    parent_capability_snapshot: OperationCapabilitySnapshot,
    cancellation: Option<CancellationToken>,
    extension_events: ExtensionEventDispatch,
) -> Result<AgentTeamOutcome, CodingSessionError> {
    let mut context = AgentTeamContext::new(
        options,
        profile_registry.clone(),
        event_service.clone(),
        operation_control.clone(),
        scheduler_parent_operation_id,
    )
    .with_deferred_terminal_publication()
    .with_parent_capability_snapshot(parent_capability_snapshot)
    .with_extension_events(extension_events);
    let result = match AgentTeamRunner::new()?
        .run_typed(&mut context, cancellation)
        .await
    {
        Ok(_) => context.finish_success(),
        Err(error) => Err(error),
    };
    if let Err(error) = &result {
        context.ensure_failure_terminal_draft(error)?;
    }
    result
}
