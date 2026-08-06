use agent_core::api::agent::{AgentEvent, AgentMessage, AgentStream};
use futures::StreamExt;

use super::CodingSessionError;
use super::context::{CodingDiagnostic, PromptTurnContext, QueuedPromptInput};
use crate::app::bootstrap::PromptInvocation;
use crate::kernel::control::PromptControlCommand;
use crate::services::runtime::RuntimeService;

pub(crate) struct PromptTurnRunner;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptTurnStep {
    Start,
    ResolveRequest,
    PrepareInput,
    ResolveRuntime,
    LoadResources,
    OpenSession,
    BuildAgentRuntime,
    RecordUserInput,
    RunAgentTurn,
    FinalizeTurn,
    EmitCompletion,
}

impl PromptTurnRunner {
    pub(crate) fn new() -> Result<Self, CodingSessionError> {
        Ok(Self)
    }

    pub(crate) async fn run_typed(
        &self,
        ctx: &mut PromptTurnContext,
    ) -> Result<(), CodingSessionError> {
        let mut step = PromptTurnStep::Start;
        loop {
            let result = match step {
                PromptTurnStep::Start => step_complete(),
                PromptTurnStep::ResolveRequest => resolve_request(ctx),
                PromptTurnStep::PrepareInput => prepare_input(ctx),
                PromptTurnStep::ResolveRuntime => resolve_runtime(ctx),
                PromptTurnStep::LoadResources => load_resources(ctx),
                PromptTurnStep::OpenSession => open_session(ctx),
                PromptTurnStep::BuildAgentRuntime => build_agent_runtime(ctx).await,
                PromptTurnStep::RecordUserInput => record_user_input(ctx).await,
                PromptTurnStep::RunAgentTurn => run_agent_turn(ctx).await,
                PromptTurnStep::FinalizeTurn => finalize_turn(ctx),
                PromptTurnStep::EmitCompletion => return emit_completion(ctx),
            };
            result?;
            step = match step {
                PromptTurnStep::Start => PromptTurnStep::ResolveRequest,
                PromptTurnStep::ResolveRequest => PromptTurnStep::PrepareInput,
                PromptTurnStep::PrepareInput => PromptTurnStep::ResolveRuntime,
                PromptTurnStep::ResolveRuntime => PromptTurnStep::LoadResources,
                PromptTurnStep::LoadResources => PromptTurnStep::OpenSession,
                PromptTurnStep::OpenSession => PromptTurnStep::BuildAgentRuntime,
                PromptTurnStep::BuildAgentRuntime => PromptTurnStep::RecordUserInput,
                PromptTurnStep::RecordUserInput => PromptTurnStep::RunAgentTurn,
                PromptTurnStep::RunAgentTurn => PromptTurnStep::FinalizeTurn,
                PromptTurnStep::FinalizeTurn => PromptTurnStep::EmitCompletion,
                PromptTurnStep::EmitCompletion => unreachable!(),
            };
        }
    }
}

fn resolve_request(ctx: &mut PromptTurnContext) -> Result<(), CodingSessionError> {
    ctx.resolve_request()?;
    step_complete()
}

fn prepare_input(ctx: &mut PromptTurnContext) -> Result<(), CodingSessionError> {
    ctx.prepare_input()?;
    step_complete()
}

fn resolve_runtime(ctx: &mut PromptTurnContext) -> Result<(), CodingSessionError> {
    ctx.resolve_runtime_from_options()?;
    step_complete()
}

fn load_resources(ctx: &mut PromptTurnContext) -> Result<(), CodingSessionError> {
    ctx.load_resources_from_runtime()?;
    step_complete()
}

fn open_session(ctx: &mut PromptTurnContext) -> Result<(), CodingSessionError> {
    if ctx.session_id().is_some() {
        if ctx.replay().is_none() {
            return Err(CodingSessionError::Session {
                message: "prompt turn cannot continue before session replay is loaded".into(),
            });
        }
        if !ctx.has_active_transaction() {
            return Err(CodingSessionError::Session {
                message: "prompt turn cannot continue before a turn transaction is active".into(),
            });
        }
        return step_complete();
    }

    if ctx.non_persistent_runtime_id().is_some() {
        if ctx.replay().is_none() {
            return Err(CodingSessionError::Session {
                message: "prompt turn cannot continue before non-persistent replay is loaded"
                    .into(),
            });
        }
        return step_complete();
    }

    if ctx.session_id().is_none() {
        return Err(CodingSessionError::Session {
            message: "prompt turn cannot continue before a session is opened".into(),
        });
    }
    step_complete()
}

async fn build_agent_runtime(ctx: &mut PromptTurnContext) -> Result<(), CodingSessionError> {
    if ctx.agent().is_some() {
        return step_complete();
    }

    if ctx.loaded_resources().is_none() {
        return Err(CodingSessionError::Config {
            message: "prompt turn cannot build agent runtime before resources are loaded".into(),
        });
    }

    let runtime = ctx
        .runtime()
        .cloned()
        .ok_or_else(|| CodingSessionError::Config {
            message: "prompt turn cannot build agent runtime without a runtime snapshot".into(),
        })?;
    let snapshot =
        ctx.capability_snapshot()
            .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                capability: "prompt runtime build requires operation capability snapshot".into(),
            })?;
    let service = RuntimeService::new();
    let authorization = ctx.authorization_hook_context();
    let delegation_executor = ctx.delegation_executor();
    let build = service
        .build_agent_runtime_with_authorization(
            &runtime,
            snapshot,
            authorization,
            delegation_executor,
            ctx.mutation_tracking(),
        )
        .await?;
    for diagnostic in build.diagnostics {
        ctx.record_diagnostic(diagnostic);
    }
    if let Some(replay) = ctx.replay() {
        service.hydrate_agent_runtime(&build.agent, &runtime, replay);
    }
    for input in ctx.options().queued_steering() {
        match input {
            QueuedPromptInput::Text(text) => build.agent.steer(text.clone()),
            QueuedPromptInput::Content(content) => build.agent.steer_content(content.clone()),
        }
        .map_err(|error| CodingSessionError::Workflow {
            message: error.to_string(),
        })?;
    }
    for input in ctx.options().queued_follow_up() {
        match input {
            QueuedPromptInput::Text(text) => build.agent.follow_up(text.clone()),
            QueuedPromptInput::Content(content) => build.agent.follow_up_content(content.clone()),
        }
        .map_err(|error| CodingSessionError::Workflow {
            message: error.to_string(),
        })?;
    }
    ctx.set_agent(build.agent);
    step_complete()
}

async fn record_user_input(ctx: &mut PromptTurnContext) -> Result<(), CodingSessionError> {
    ctx.record_user_input().await?;
    step_complete()
}

async fn run_agent_turn(ctx: &mut PromptTurnContext) -> Result<(), CodingSessionError> {
    let agent = ctx
        .agent()
        .cloned()
        .ok_or_else(|| CodingSessionError::Session {
            message: "prompt turn has no agent runtime".into(),
        })?;
    let mut controls = ctx.take_prompt_control_receiver();
    let mut cancellation = ctx.operation_cancellation();
    let mut stream = start_agent_turn(ctx).await?;
    loop {
        let next = match (controls.as_mut(), cancellation.as_ref()) {
            (Some(receiver), Some(cancellation)) => {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => AgentTurnInput::Cancellation,
                    command = receiver.recv() => AgentTurnInput::Control(command),
                    event = stream.next() => AgentTurnInput::Event(event),
                }
            }
            (Some(receiver), None) => {
                tokio::select! {
                    biased;
                    command = receiver.recv() => AgentTurnInput::Control(command),
                    event = stream.next() => AgentTurnInput::Event(event),
                }
            }
            (None, Some(cancellation)) => {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => AgentTurnInput::Cancellation,
                    event = stream.next() => AgentTurnInput::Event(event),
                }
            }
            (None, None) => AgentTurnInput::Event(stream.next().await),
        };

        let event = match next {
            AgentTurnInput::Cancellation => {
                ctx.request_abort("parent operation ended");
                agent.abort();
                cancellation = None;
                continue;
            }
            AgentTurnInput::Control(Some(command)) => {
                apply_prompt_control_command(ctx, &agent, command).map_err(|error| {
                    CodingSessionError::Workflow {
                        message: error.to_string(),
                    }
                })?;
                continue;
            }
            AgentTurnInput::Control(None) => {
                controls = None;
                continue;
            }
            AgentTurnInput::Event(Some(event)) => event,
            AgentTurnInput::Event(None) => break,
        };

        match &event {
            AgentEvent::AgentDone { message } => {
                ctx.record_final_message(message.clone());
            }
            AgentEvent::AgentError { error } => {
                let message = error.clone();
                ctx.record_diagnostic(CodingDiagnostic::error(message.clone()));
                ctx.record_agent_event(event)?;
                return Err(CodingSessionError::Provider { message });
            }
            _ => {}
        }
        ctx.record_agent_event(event)?;
    }

    if ctx.final_message().is_none() {
        return Err(CodingSessionError::Provider {
            message: "agent turn ended without a final assistant message".into(),
        });
    }
    step_complete()
}

#[allow(
    clippy::large_enum_variant,
    reason = "select-loop input owns one event at a time and avoids per-delta boxing"
)]
enum AgentTurnInput {
    Cancellation,
    Control(Option<PromptControlCommand>),
    Event(Option<AgentEvent>),
}

fn apply_prompt_control_command(
    ctx: &mut PromptTurnContext,
    agent: &agent_core::api::agent::Agent,
    command: PromptControlCommand,
) -> Result<(), agent_core::api::agent::AgentQueueError> {
    match command {
        PromptControlCommand::Abort { reason } => {
            ctx.request_abort(reason);
            agent.abort();
            Ok(())
        }
        PromptControlCommand::Steer { text } => agent.steer(text),
        PromptControlCommand::SteerContent { content } => agent.steer_content(content),
        PromptControlCommand::FollowUp { text } => agent.follow_up(text),
        PromptControlCommand::FollowUpContent { content } => agent.follow_up_content(content),
    }
}

fn finalize_turn(ctx: &mut PromptTurnContext) -> Result<(), CodingSessionError> {
    if ctx.final_message().is_none() {
        return Err(CodingSessionError::Session {
            message: "prompt turn cannot finalize without a final assistant message".into(),
        });
    }

    if ctx.session_id().is_some() {
        if ctx.replay().is_none() {
            return Err(CodingSessionError::Session {
                message: "prompt turn cannot finalize before session replay is loaded".into(),
            });
        }
        if !ctx.has_active_transaction() {
            return Err(CodingSessionError::Session {
                message: "prompt turn cannot finalize before a turn transaction is active".into(),
            });
        }
        return step_complete();
    }

    if ctx.non_persistent_runtime_id().is_some() {
        if ctx.replay().is_none() {
            return Err(CodingSessionError::Session {
                message: "prompt turn cannot finalize before non-persistent replay is loaded"
                    .into(),
            });
        }
        return step_complete();
    }

    Err(CodingSessionError::Session {
        message: "prompt turn cannot finalize before a session is opened".into(),
    })
}

fn emit_completion(ctx: &mut PromptTurnContext) -> Result<(), CodingSessionError> {
    ctx.record_prompt_completed()?;
    step_complete()
}

async fn start_agent_turn(ctx: &mut PromptTurnContext) -> Result<AgentStream, CodingSessionError> {
    let agent = ctx
        .agent()
        .cloned()
        .ok_or_else(|| CodingSessionError::Session {
            message: "prompt turn has no agent runtime".into(),
        })?;

    match ctx.options().invocation() {
        PromptInvocation::Text(text) if !text.is_empty() => Ok(agent.prompt(text)),
        PromptInvocation::Text(_) => Err(CodingSessionError::Input {
            message: "prompt turn requires non-empty text input".into(),
        }),
        PromptInvocation::Content(content) if !content.is_empty() => {
            let message_id = format!("user_{}", agent.messages().await.len());
            agent.add_message(AgentMessage::Custom {
                message_id,
                custom_type: "input".into(),
                content: content.clone(),
                display: true,
                details: None,
                timestamp: 0,
            });
            agent
                .run()
                .map_err(|message| CodingSessionError::Provider { message })
        }
        PromptInvocation::Content(_) => Err(CodingSessionError::Input {
            message: "prompt turn requires non-empty content input".into(),
        }),
        PromptInvocation::Compact { .. } => Err(CodingSessionError::UnsupportedCapability {
            capability: "manual compaction in PromptTurnRunner".into(),
        }),
        PromptInvocation::Skill {
            name,
            additional_instructions,
        } => agent
            .skill(name, additional_instructions.as_deref())
            .await
            .map_err(|message| CodingSessionError::Resource { message }),
        PromptInvocation::PromptTemplate { name, args } => agent
            .prompt_from_template(name, args)
            .await
            .map_err(|message| CodingSessionError::Resource { message }),
    }
}

fn step_complete() -> Result<(), CodingSessionError> {
    Ok(())
}
