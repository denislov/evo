use super::{
    DelegationAuthorizationDecision, DelegationLineageEntry, DelegationToolResult,
    DelegationToolResultStatus, PendingDelegationConfirmationState,
    authorize_delegation_requests_with_lineage, capability_snapshot_for_child_operation,
    required_non_empty_string, required_profile_id,
};
use crate::application::capability::OperationCapabilitySnapshot;
use crate::application::operation::admission::OperationScheduler;
use crate::application::operation::control::OperationControl;
use crate::application::session_coordinator::{
    SessionCoordinator, SessionWriterCommand, SessionWriterReply,
};
use crate::kernel::capability::SessionWriteCapability;
use crate::kernel::error::CodingSessionError;
use crate::kernel::operation::OperationKind;
use crate::mutex::MutexExt;
use crate::operations::agent_invocation::runner::{
    AgentInvocationContext, AgentInvocationOptions, AgentInvocationRunner,
};
use crate::operations::prompt::context::{DelegationRequest, PromptTurnContext, PromptTurnOptions};
use crate::operations::team_invocation::runner::{
    AgentTeamContext, AgentTeamOptions, AgentTeamRunner,
};
use crate::profiles::{DelegationPolicy, ProfileId, ProfileKind, ProfileRegistry};
use crate::services::authorization::AuthorizationService;
use crate::services::event::EventService;
use crate::services::ports::ExtensionEventDispatch;
use crate::services::runtime::RuntimeService;
use crate::session::event::PersistedDelegationStatus;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// How long a cancelled delegation child task gets to unwind through its
/// RAII guards before the parent force-aborts it.
const DELEGATION_CHILD_CANCEL_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApprovedDelegationExecution {
    pub(crate) child_operation_id: String,
    pub(crate) final_text: String,
}

#[derive(Debug)]
pub(crate) struct ApprovedDelegationExecutionOutcome {
    pub(crate) execution: Result<ApprovedDelegationExecution, CodingSessionError>,
    pub(crate) pending_confirmations: Vec<PendingDelegationConfirmationState>,
}

pub(crate) fn install_tool_executor(
    context: &mut PromptTurnContext,
    profile_registry: ProfileRegistry,
    event_service: EventService,
    operation_control: OperationControl,
    authorization_service: AuthorizationService,
    current_depth: usize,
    delegation_lineage: Vec<DelegationLineageEntry>,
) -> Result<(), CodingSessionError> {
    let Some(runtime) = context.options().runtime().cloned() else {
        return Err(CodingSessionError::Config {
            message: "delegation executor requires a runtime snapshot".into(),
        });
    };
    let Some(policy) = runtime.profile_delegation_policy().cloned() else {
        return Ok(());
    };
    let Some(requesting_profile_id) = runtime.profile_id().cloned() else {
        return Ok(());
    };
    let Some(parent_capability_snapshot) = context.capability_snapshot().cloned() else {
        return Err(CodingSessionError::UnsupportedCapability {
            capability: "delegation executor requires a parent capability snapshot".into(),
        });
    };
    let prompt_options = context.options().clone();
    let turn_id = context.turn_id().to_owned();
    let deferred_pending = context.deferred_pending_delegations();
    let confirmation_awaited = authorization_service.uses_interactive_waiters();
    let extension_events = context.extension_events_dispatch();
    context.set_delegation_executor(Arc::new(move |tool_context, args| {
        let profile_registry = profile_registry.clone();
        let event_service = event_service.clone();
        let operation_control = operation_control.clone();
        let policy = policy.clone();
        let prompt_options = prompt_options.clone();
        let parent_capability_snapshot = parent_capability_snapshot.clone();
        let requesting_profile_id = requesting_profile_id.clone();
        let deferred_pending = deferred_pending.clone();
        let authorization_service = authorization_service.clone();
        let delegation_lineage = delegation_lineage.clone();
        let turn_id = turn_id.clone();
        let extension_events = extension_events.clone();
        Box::pin(async move {
            execute_tool_request_with_pending(
                profile_registry,
                event_service,
                operation_control,
                requesting_profile_id,
                policy,
                prompt_options,
                parent_capability_snapshot,
                tool_context,
                args,
                deferred_pending,
                confirmation_awaited,
                authorization_service,
                turn_id,
                current_depth,
                delegation_lineage,
                extension_events,
            )
            .await
        })
    }));
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the delegation tool executor freezes policy, runtime, lineage, and capability inputs"
)]
pub(crate) async fn execute_tool_request_with_pending(
    profile_registry: ProfileRegistry,
    event_service: EventService,
    operation_control: OperationControl,
    requesting_profile_id: ProfileId,
    policy: DelegationPolicy,
    prompt_options: PromptTurnOptions,
    parent_capability_snapshot: OperationCapabilitySnapshot,
    tool_context: tool_runtime::api::ToolCallContext,
    args: serde_json::Value,
    deferred_pending: Arc<Mutex<Vec<PendingDelegationConfirmationState>>>,
    confirmation_awaited: bool,
    authorization_service: AuthorizationService,
    turn_id: String,
    current_depth: usize,
    delegation_lineage: Vec<DelegationLineageEntry>,
    extension_events: ExtensionEventDispatch,
) -> Result<String, String> {
    let cancellation = tool_context.cancel.clone();
    let (target_kind, target_field) = match tool_context.tool_id.as_str() {
        "delegate_agent" => (ProfileKind::Agent, "agent_id"),
        "delegate_team" => (ProfileKind::Team, "team_id"),
        other => return Err(format!("unsupported delegation tool: {other}")),
    };
    let target_id = required_profile_id(&args, target_field)?;
    let task = required_non_empty_string(&args, "task")?;
    let request = DelegationRequest {
        operation_id: tool_context
            .operation_id
            .clone()
            .unwrap_or_else(|| parent_capability_snapshot.operation_id.clone()),
        turn_id,
        tool_call_id: tool_context.call_id.clone(),
        requesting_profile_id: requesting_profile_id.clone(),
        target_kind,
        target_id: target_id.clone(),
        task: task.clone(),
    };
    let decisions = authorize_delegation_requests_with_lineage(
        std::slice::from_ref(&request),
        &policy,
        current_depth,
        &delegation_lineage,
    );
    let Some(decision) = decisions.into_iter().next() else {
        return Err("delegation policy produced no decision".into());
    };
    let decision = match decision {
        DelegationAuthorizationDecision::RequiresConfirmation {
            request,
            child_delegation_depth,
            ..
        } if confirmation_awaited => DelegationAuthorizationDecision::Approved {
            request,
            child_delegation_depth,
        },
        decision => decision,
    };
    match decision {
        DelegationAuthorizationDecision::Approved {
            request,
            child_delegation_depth,
        } => {
            event_service
                .emit_delegation_approved(&request)
                .map_err(|error| error.to_string())?;
            let child_request = request.clone();
            let child_lineage =
                super::delegation_lineage_for_request(&delegation_lineage, &child_request);
            // The child task must unwind through its RAII guards (the
            // admitted child operation's cleanup) on cancellation, so it gets
            // its own cancellation token that the select below cancels
            // instead of aborting the task outright (abort skips destructors
            // and leaks the child operation state).
            let external_cancellation = CancellationToken::new();
            let child_cancel = external_cancellation.clone();
            let mut child_task = match request.target_kind {
                ProfileKind::Agent => tokio::spawn(async move {
                    execute_agent(
                        profile_registry,
                        event_service,
                        operation_control,
                        &child_request,
                        prompt_options,
                        child_delegation_depth,
                        child_lineage,
                        Some(parent_capability_snapshot),
                        Some(authorization_service),
                        Some(child_cancel),
                        extension_events,
                    )
                    .await
                }),
                ProfileKind::Team => tokio::spawn(async move {
                    execute_team(
                        profile_registry,
                        event_service,
                        operation_control,
                        &child_request,
                        prompt_options,
                        child_delegation_depth,
                        child_lineage,
                        Some(parent_capability_snapshot),
                        Some(authorization_service),
                        Some(child_cancel),
                        extension_events,
                    )
                    .await
                }),
            };
            let outcome = tokio::select! {
                joined = &mut child_task => match joined {
                    Ok(outcome) => outcome,
                    Err(error) => failed_execution(CodingSessionError::Workflow {
                        message: format!("delegation child task failed: {error}"),
                    }),
                },
                _ = cancellation.cancelled() => {
                    // Request cancellation through the child token so the
                    // task exits normally and its RAII guards (child
                    // operation cleanup, descendant cancellation) run. Only
                    // force-abort if it fails to unwind within the grace
                    // period.
                    external_cancellation.cancel();
                    tokio::select! {
                        joined = &mut child_task => match joined {
                            Ok(_) => {}
                            Err(error) => {
                                let _ = failed_execution(CodingSessionError::Workflow {
                                    message: format!("delegation child task failed: {error}"),
                                });
                            }
                        },
                        _ = tokio::time::sleep(DELEGATION_CHILD_CANCEL_GRACE) => {
                            child_task.abort();
                            let _ = child_task.await;
                        }
                    }
                    failed_execution(CodingSessionError::Cancelled)
                }
            };
            match outcome.execution {
                Ok(execution) if !outcome.pending_confirmations.is_empty() => {
                    deferred_pending
                        .lock_resource("deferred delegation queue")
                        .map_err(|error| error.to_string())?
                        .extend(outcome.pending_confirmations);
                    let mut result = DelegationToolResult::from_request(
                        &request,
                        DelegationToolResultStatus::Rejected,
                    );
                    result.child_operation_id = Some(execution.child_operation_id);
                    result.error =
                        Some("nested delegation requires interactive authorization".into());
                    Ok(result.to_json())
                }
                Ok(execution) => {
                    let mut result = DelegationToolResult::from_request(
                        &request,
                        DelegationToolResultStatus::Completed,
                    );
                    result.child_operation_id = Some(execution.child_operation_id);
                    result.final_text = Some(execution.final_text);
                    Ok(result.to_json())
                }
                Err(error) => {
                    let status = if error == CodingSessionError::Cancelled {
                        DelegationToolResultStatus::Cancelled
                    } else {
                        DelegationToolResultStatus::Failed
                    };
                    let mut result = DelegationToolResult::from_request(&request, status);
                    result.error = Some(error.to_string());
                    Ok(result.to_json())
                }
            }
        }
        DelegationAuthorizationDecision::RequiresConfirmation {
            request, reason, ..
        } => {
            let mut result =
                DelegationToolResult::from_request(&request, DelegationToolResultStatus::Rejected);
            result.error = Some(reason);
            Ok(result.to_json())
        }
        DelegationAuthorizationDecision::Rejected { request, reason } => {
            let mut result =
                DelegationToolResult::from_request(&request, DelegationToolResultStatus::Rejected);
            result.error = Some(reason);
            Ok(result.to_json())
        }
    }
}

fn failed_execution(error: CodingSessionError) -> ApprovedDelegationExecutionOutcome {
    ApprovedDelegationExecutionOutcome {
        execution: Err(error),
        pending_confirmations: Vec::new(),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "delegation execution keeps lineage and admitted child authorities explicit"
)]
pub(crate) async fn execute_agent(
    profile_registry: ProfileRegistry,
    event_service: EventService,
    operation_control: OperationControl,
    request: &DelegationRequest,
    prompt_options: PromptTurnOptions,
    child_delegation_depth: usize,
    delegation_lineage: Vec<DelegationLineageEntry>,
    parent_capability_snapshot: Option<OperationCapabilitySnapshot>,
    authorization_service: Option<AuthorizationService>,
    external_cancellation: Option<CancellationToken>,
    extension_events: ExtensionEventDispatch,
) -> ApprovedDelegationExecutionOutcome {
    let child_operation_id = OperationScheduler::allocate_child_operation_id();
    let mut context = AgentInvocationContext::new(
        AgentInvocationOptions::new(
            request.target_id.clone(),
            request.task.clone(),
            prompt_options,
        )
        .with_delegation_depth(child_delegation_depth)
        .with_delegation_lineage(delegation_lineage),
        profile_registry,
        event_service.clone(),
        operation_control.clone(),
        child_operation_id.clone(),
    )
    .with_extension_events(extension_events.clone());
    if let Some(service) = authorization_service {
        context = context.with_authorization_service(service);
    }
    let Some(parent_capability_snapshot) = parent_capability_snapshot else {
        return ApprovedDelegationExecutionOutcome {
            execution: Err(CodingSessionError::UnsupportedCapability {
                capability: "delegated agent execution requires a parent capability snapshot"
                    .into(),
            }),
            pending_confirmations: Vec::new(),
        };
    };
    let child_snapshot = capability_snapshot_for_child_operation(
        &parent_capability_snapshot,
        child_operation_id.clone(),
    );
    let child_admission = match OperationScheduler::admit_child(
        &operation_control,
        OperationKind::AgentInvocation,
        child_snapshot.clone(),
    ) {
        Ok(admission) => admission,
        Err(rejection) => {
            return ApprovedDelegationExecutionOutcome {
                execution: Err(rejection.into_error()),
                pending_confirmations: Vec::new(),
            };
        }
    };
    context = context.with_parent_capability_snapshot(child_snapshot);
    if let Err(error) = event_service.emit_delegation_started(request, child_operation_id.clone()) {
        drop(child_admission);
        return failed_execution(error);
    }
    // user hooks subagent 事件（Observe gate：subagent_start 派发 hook；
    // subagent_stop 为 gate 事件，host 通道记账）。ARC-730 产品接线。
    extension_events.submit(
        extension_host::api::ExtensionEventKind::SubagentStart,
        extension_host::api::ExtensionEventPayload::SubagentStart {
            subagent_type: request.target_id.to_string(),
        },
    );
    let subagent_stop = |reason: Option<String>| {
        extension_events.submit(
            extension_host::api::ExtensionEventKind::SubagentStop,
            extension_host::api::ExtensionEventPayload::SubagentStop {
                subagent_type: request.target_id.to_string(),
                phase: extension_host::api::SubagentStopPhase::Gate,
                stop_reason: reason,
            },
        );
    };
    let result = match AgentInvocationRunner::new() {
        Ok(runner) => {
            let runner_fut = runner.run_typed(&mut context, child_admission.cancellation_token());
            let run_result = match external_cancellation {
                Some(external) => tokio::select! {
                    _ = external.cancelled() => Err(CodingSessionError::Cancelled),
                    result = runner_fut => result,
                },
                None => runner_fut.await,
            };
            match run_result {
                Ok(_) => context.finish_success(),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    };
    let pending_confirmations = context.take_pending_delegation_confirmations();
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Err(event_error) =
                event_service.emit_delegation_failed(request, child_operation_id, error.clone())
            {
                subagent_stop(Some(event_error.to_string()));
                drop(child_admission);
                return ApprovedDelegationExecutionOutcome {
                    execution: Err(event_error),
                    pending_confirmations,
                };
            }
            subagent_stop(Some(error.to_string()));
            drop(child_admission);
            return ApprovedDelegationExecutionOutcome {
                execution: Err(error),
                pending_confirmations,
            };
        }
    };
    let final_text = outcome.final_text;
    if pending_confirmations.is_empty()
        && let Err(error) = event_service.emit_delegation_completed(
            request,
            child_operation_id.clone(),
            final_text.clone(),
        )
    {
        subagent_stop(Some(error.to_string()));
        drop(child_admission);
        return ApprovedDelegationExecutionOutcome {
            execution: Err(error),
            pending_confirmations,
        };
    }
    subagent_stop(Some("completed".into()));
    drop(child_admission);
    ApprovedDelegationExecutionOutcome {
        execution: Ok(ApprovedDelegationExecution {
            child_operation_id,
            final_text,
        }),
        pending_confirmations,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "delegation execution keeps lineage and admitted child authorities explicit"
)]
pub(crate) async fn execute_team(
    profile_registry: ProfileRegistry,
    event_service: EventService,
    operation_control: OperationControl,
    request: &DelegationRequest,
    prompt_options: PromptTurnOptions,
    child_delegation_depth: usize,
    delegation_lineage: Vec<DelegationLineageEntry>,
    parent_capability_snapshot: Option<OperationCapabilitySnapshot>,
    authorization_service: Option<AuthorizationService>,
    external_cancellation: Option<CancellationToken>,
    extension_events: ExtensionEventDispatch,
) -> ApprovedDelegationExecutionOutcome {
    let child_operation_id = OperationScheduler::allocate_child_operation_id();
    let mut context = AgentTeamContext::new(
        AgentTeamOptions::new(
            request.target_id.clone(),
            request.task.clone(),
            prompt_options,
        )
        .with_delegation_depth(child_delegation_depth)
        .with_delegation_lineage(delegation_lineage),
        profile_registry,
        event_service.clone(),
        operation_control.clone(),
        child_operation_id.clone(),
    )
    .with_extension_events(extension_events.clone());
    if let Some(service) = authorization_service {
        context = context.with_authorization_service(service);
    }
    let Some(parent_capability_snapshot) = parent_capability_snapshot else {
        return ApprovedDelegationExecutionOutcome {
            execution: Err(CodingSessionError::UnsupportedCapability {
                capability: "delegated team execution requires a parent capability snapshot".into(),
            }),
            pending_confirmations: Vec::new(),
        };
    };
    let child_snapshot = capability_snapshot_for_child_operation(
        &parent_capability_snapshot,
        child_operation_id.clone(),
    );
    let child_admission = match OperationScheduler::admit_child(
        &operation_control,
        OperationKind::AgentTeam,
        child_snapshot.clone(),
    ) {
        Ok(admission) => admission,
        Err(rejection) => {
            return ApprovedDelegationExecutionOutcome {
                execution: Err(rejection.into_error()),
                pending_confirmations: Vec::new(),
            };
        }
    };
    context = context.with_parent_capability_snapshot(child_snapshot);
    if let Err(error) = event_service.emit_delegation_started(request, child_operation_id.clone()) {
        drop(child_admission);
        return failed_execution(error);
    }
    // user hooks subagent 事件（Observe gate：subagent_start 派发 hook；
    // subagent_stop 为 gate 事件，host 通道记账）。ARC-730 产品接线。
    extension_events.submit(
        extension_host::api::ExtensionEventKind::SubagentStart,
        extension_host::api::ExtensionEventPayload::SubagentStart {
            subagent_type: request.target_id.to_string(),
        },
    );
    let subagent_stop = |reason: Option<String>| {
        extension_events.submit(
            extension_host::api::ExtensionEventKind::SubagentStop,
            extension_host::api::ExtensionEventPayload::SubagentStop {
                subagent_type: request.target_id.to_string(),
                phase: extension_host::api::SubagentStopPhase::Gate,
                stop_reason: reason,
            },
        );
    };
    let result = match AgentTeamRunner::new() {
        Ok(runner) => {
            let runner_fut = runner.run_typed(&mut context, child_admission.cancellation_token());
            let run_result = match external_cancellation {
                Some(external) => tokio::select! {
                    _ = external.cancelled() => Err(CodingSessionError::Cancelled),
                    result = runner_fut => result,
                },
                None => runner_fut.await,
            };
            match run_result {
                Ok(_) => context.finish_success(),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    };
    let pending_confirmations = context.take_pending_delegation_confirmations();
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Err(event_error) =
                event_service.emit_delegation_failed(request, child_operation_id, error.clone())
            {
                subagent_stop(Some(event_error.to_string()));
                drop(child_admission);
                return ApprovedDelegationExecutionOutcome {
                    execution: Err(event_error),
                    pending_confirmations,
                };
            }
            subagent_stop(Some(error.to_string()));
            drop(child_admission);
            return ApprovedDelegationExecutionOutcome {
                execution: Err(error),
                pending_confirmations,
            };
        }
    };
    let final_text = outcome.final_text;
    if pending_confirmations.is_empty()
        && let Err(error) = event_service.emit_delegation_completed(
            request,
            child_operation_id.clone(),
            final_text.clone(),
        )
    {
        subagent_stop(Some(error.to_string()));
        drop(child_admission);
        return ApprovedDelegationExecutionOutcome {
            execution: Err(error),
            pending_confirmations,
        };
    }
    subagent_stop(Some("completed".into()));
    drop(child_admission);
    ApprovedDelegationExecutionOutcome {
        execution: Ok(ApprovedDelegationExecution {
            child_operation_id,
            final_text,
        }),
        pending_confirmations,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn approve(
    session_coordinator: &mut SessionCoordinator,
    runtime_service: &RuntimeService,
    profile_registry: &ProfileRegistry,
    event_service: &EventService,
    operation_control: &OperationControl,
    operation_id: String,
    tool_call_id: String,
    now: String,
    parent_capability_snapshot: OperationCapabilitySnapshot,
    extension_events: ExtensionEventDispatch,
) -> Result<(), CodingSessionError> {
    SessionWriteCapability::require(parent_capability_snapshot.session_write.as_ref())?;
    let approval_operation_id = parent_capability_snapshot.operation_id.clone();
    let approval_generation = parent_capability_snapshot.generation;
    let reply = session_coordinator
        .execute_writer_command(SessionWriterCommand::approve_delegation(
            approval_operation_id.clone(),
            approval_generation,
            operation_id,
            tool_call_id,
            now,
        ))
        .await?;
    let SessionWriterReply::DelegationApproved { pending } = reply else {
        unreachable!("delegation approval writer command returns its typed reply")
    };
    let mut pending = *pending;
    event_service.emit_delegation_approved(&pending.request)?;
    if let Some(runtime) = pending.prompt_options.runtime_mut() {
        runtime_service.install_provider_runtime(runtime);
    }
    let outcome = match pending.request.target_kind {
        ProfileKind::Agent => {
            execute_agent(
                profile_registry.clone(),
                event_service.clone(),
                operation_control.clone(),
                &pending.request,
                pending.prompt_options,
                pending.child_delegation_depth,
                pending.delegation_lineage,
                Some(parent_capability_snapshot.clone()),
                None,
                None,
                extension_events.clone(),
            )
            .await
        }
        ProfileKind::Team => {
            execute_team(
                profile_registry.clone(),
                event_service.clone(),
                operation_control.clone(),
                &pending.request,
                pending.prompt_options,
                pending.child_delegation_depth,
                pending.delegation_lineage,
                Some(parent_capability_snapshot),
                None,
                None,
                extension_events,
            )
            .await
        }
    };
    let has_nested_confirmations = !outcome.pending_confirmations.is_empty();
    let (status, child_operation_id, summary) = match &outcome.execution {
        Ok(execution) if has_nested_confirmations => (
            PersistedDelegationStatus::ConfirmationRequired,
            Some(execution.child_operation_id.clone()),
            Some("nested delegation is waiting for permission".to_string()),
        ),
        Ok(execution) => (
            PersistedDelegationStatus::Completed,
            Some(execution.child_operation_id.clone()),
            Some(execution.final_text.clone()),
        ),
        Err(error) => (
            PersistedDelegationStatus::Failed,
            None,
            Some(error.to_string()),
        ),
    };
    let reply = session_coordinator
        .execute_writer_command(SessionWriterCommand::adopt_delegations(
            approval_operation_id.clone(),
            approval_generation,
            outcome.pending_confirmations,
        ))
        .await?;
    let SessionWriterReply::DelegationsAdopted { diagnostics } = reply else {
        unreachable!("delegation adoption writer command returns its typed reply")
    };
    for diagnostic in diagnostics {
        event_service.emit_diagnostic(diagnostic.operation_id, diagnostic.message)?;
    }
    let reply = session_coordinator
        .execute_writer_command(SessionWriterCommand::record_delegation_folded_update(
            approval_operation_id,
            approval_generation,
            pending.request.tool_call_id.clone(),
            pending.request.requesting_profile_id.clone(),
            pending.request.target_kind,
            pending.request.target_id.clone(),
            pending.request.task.clone(),
            status,
            child_operation_id,
            summary,
        ))
        .await?;
    let SessionWriterReply::DelegationFoldedUpdated = reply else {
        unreachable!("delegation folded writer command returns its typed reply")
    };
    outcome.execution.map(|_| ())
}
