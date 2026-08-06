use tokio::task::JoinHandle;

use super::OperationOutcome;
use super::admission::OperationScheduler;
use super::contract::{CodingAgentOperation, CodingAgentOperationOutcome};
use super::control::OperationCancellationHandle;
use crate::kernel::error::CodingSessionError;
use crate::public_error::CodingAgentPublicError;
use crate::runtime::client::connection::CodingAgentClientConnection;
use crate::runtime::facade::CodingAgentSession;

enum RuntimeOwnedOperation {
    Agent(crate::operations::agent_invocation::runner::AgentInvocationOptions),
    Team(crate::operations::team_invocation::runner::AgentTeamOptions),
}

#[derive(Debug)]
#[must_use = "dropping the handle detaches the runtime-owned operation task"]
pub struct CodingAgentOperationTask {
    operation_id: String,
    cancellation: OperationCancellationHandle,
    task: JoinHandle<Result<CodingAgentOperationOutcome, CodingSessionError>>,
}

impl CodingAgentOperationTask {
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub async fn join(self) -> Result<CodingAgentOperationOutcome, CodingAgentPublicError> {
        self.join_internal()
            .await
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) async fn join_internal(
        self,
    ) -> Result<CodingAgentOperationOutcome, CodingSessionError> {
        self.task
            .await
            .map_err(|error| CodingSessionError::Session {
                message: format!("runtime-owned operation task failed: {error}"),
            })?
    }

    pub fn bind_control_owner(
        &self,
        connection: &CodingAgentClientConnection,
    ) -> Result<(), CodingAgentPublicError> {
        connection
            .bind_operation_cancellation(self.operation_id.clone(), self.cancellation.clone())
            .map_err(CodingAgentPublicError::from)
    }
}

impl CodingAgentSession {
    pub fn submit(
        &mut self,
        operation: CodingAgentOperation,
    ) -> Result<CodingAgentOperationTask, CodingAgentPublicError> {
        self.submit_internal(operation)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn submit_internal(
        &mut self,
        mut operation: CodingAgentOperation,
    ) -> Result<CodingAgentOperationTask, CodingSessionError> {
        self.runtime_host
            .client_projection
            .snapshots
            .ensure_runtime_running()?;
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            CodingSessionError::UnsupportedCapability {
                capability: "runtime operation submission requires an active Tokio runtime".into(),
            }
        })?;
        let fingerprint = operation.submission_fingerprint();
        let descriptor = operation.descriptor();
        let prompt_options = match &mut operation {
            CodingAgentOperation::InvokeAgent(options) => options.prompt_options_mut(),
            CodingAgentOperation::InvokeTeam(options) => options.prompt_options_mut(),
            _ => {
                return Err(CodingSessionError::UnsupportedCapability {
                    capability: "runtime-owned execution accepts supported async non-session roots"
                        .into(),
                });
            }
        };
        if let Some(provider_runtime) = prompt_options.runtime_mut() {
            self.runtime_host
                .runtime_service
                .install_provider_runtime(provider_runtime);
            self.runtime_host
                .runtime_service
                .install_background_tasks(provider_runtime);
        }

        let mut submission = self.consume_submission_lease(descriptor, fingerprint.as_ref())?;
        let admission = self.resolve_operation_admission_with_id(
            &operation,
            submission
                .as_ref()
                .map(|submission| submission.operation_id.as_str()),
        )?;
        let operation_permit = OperationScheduler::admit(
            &self.runtime_host.operation_supervisor.control,
            &admission,
            descriptor.dispatch_mode,
        )
        .map_err(|rejection| rejection.into_error())?;
        if let Some(guard) = submission.as_mut() {
            guard.commit_execution(operation_permit.execution())?;
        }
        let operation = match operation {
            CodingAgentOperation::InvokeAgent(options) => RuntimeOwnedOperation::Agent(options),
            CodingAgentOperation::InvokeTeam(options) => RuntimeOwnedOperation::Team(options),
            _ => unreachable!("runtime-owned operation was narrowed before admission"),
        };

        let execution = operation_permit.execution().clone();
        let snapshot = execution.capability_snapshot.clone();
        let operation_id = execution.operation_id.clone();
        let operation_cancellation = operation_permit.cancellation_token();
        let cancellation_handle = operation_permit
            .cancellation_handle()
            .expect("runtime-owned roots must have cancellation authority");
        if let (Some(submission), Some(cancellation)) =
            (submission.as_ref(), Some(cancellation_handle.clone()))
        {
            self.runtime_host
                .client_projection
                .snapshots
                .bind_operation_cancellation(
                    submission.handle.clone(),
                    operation_id.clone(),
                    cancellation,
                )?;
        }
        let prompt_control_receiver = if matches!(operation, RuntimeOwnedOperation::Agent(_)) {
            let receiver = self
                .runtime_host
                .operation_supervisor
                .control
                .take_prompt_control_receiver()?;
            self.runtime_host
                .operation_supervisor
                .control
                .clear_prompt_control_receiver()?;
            receiver
        } else {
            None
        };
        let profile_registry = self.runtime_host.profile_registry.clone();
        let event_service = self.runtime_host.events.clone();
        let operation_control = self.runtime_host.operation_supervisor.control.clone();
        let (extension_session_id, extension_workspace_root) = self.runtime_host.session_identity();
        let extension_events = crate::services::ports::ExtensionEventDispatch::from_parts(
            Some(self.runtime_host.extension_host.sink()),
            extension_session_id,
            extension_workspace_root,
        );

        let task = runtime.spawn(async move {
            let result = match operation {
                RuntimeOwnedOperation::Agent(options) => {
                    let result = crate::operations::agent_invocation::run(
                        options,
                        snapshot.operation_id.clone(),
                        prompt_control_receiver,
                        &profile_registry,
                        &event_service,
                        &operation_control,
                        snapshot.clone(),
                        operation_cancellation.clone(),
                        extension_events.clone(),
                    )
                    .await;
                    result.map(OperationOutcome::AgentInvocation)
                }
                RuntimeOwnedOperation::Team(options) => crate::operations::team_invocation::run(
                    options,
                    snapshot.operation_id.clone(),
                    &profile_registry,
                    &event_service,
                    &operation_control,
                    snapshot.clone(),
                    operation_cancellation.clone(),
                    extension_events,
                )
                .await
                .map(OperationOutcome::AgentTeam),
            };
            let decision = super::finalize::FinalizationDecision::freeze(&execution, &result);
            let commit_result = decision.resolve_non_session()?;
            if let Some(draft) =
                event_service.take_deferred_terminal_draft(&decision.operation_id)?
            {
                event_service.emit_committed_terminal_draft(draft, execution.kind)?;
            }
            if let Some(guard) = submission.as_mut() {
                guard.finish(&decision, &commit_result)?;
            }
            drop(operation_permit);
            result.map(CodingAgentOperationOutcome::from_internal)
        });

        Ok(CodingAgentOperationTask {
            operation_id,
            cancellation: cancellation_handle,
            task,
        })
    }
}
