#[derive(Clone)]
pub(crate) struct RuntimeService {
    ai_client: Arc<AiClient>,
}

impl std::fmt::Debug for RuntimeService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeService")
            .field(
                "registered_apis",
                &self.ai_client.provider_registry().registered_apis(),
            )
            .finish()
    }
}

use std::collections::BTreeSet;
use std::sync::Arc;

use agent_core::api::agent::{Agent, AgentMessage, AgentResources, ProviderStreamer};
use agent_core::api::tool::AgentTool;
use ai::api::client::AiClient;
use ai::api::conversation::{AssistantMessage, ContentBlock, Context, StopReason};
use ai::api::stream::{EventStream, StreamOptions};

use crate::app::bootstrap::{SessionMode, build_agent_config_with_auth_diagnostics};

use crate::operations::delegation::delegation_tools;
use crate::operations::prompt::context::{
    CodingDiagnostic, DelegationToolExecutor, RuntimeSnapshot,
};
use crate::runtime::capability::{ModelCapability, OperationCapabilitySnapshot};
use crate::runtime::facade::CodingSessionError;
use crate::services::authorization::{AuthorizationHookContext, ToolAuthorizationInventory};
use crate::session::event::PersistedContentBlock;
use crate::session::replay::{MessageStatus, SessionReplay, ToolCallStatus, TranscriptItem};

pub(crate) struct AgentRuntimeBuild {
    pub(crate) agent: Agent,
    pub(crate) diagnostics: Vec<CodingDiagnostic>,
}

pub(crate) fn stream_model_for_scoped_runtime(
    runtime: &RuntimeSnapshot,
    model_capability: &ModelCapability,
    context: Context,
    opts: Option<StreamOptions>,
) -> Result<EventStream, CodingSessionError> {
    let provider_streamer = scoped_provider_streamer_for_runtime(runtime, model_capability)?;
    Ok(provider_streamer(runtime.model(), context, opts))
}

pub(crate) fn scoped_provider_streamer_for_runtime(
    runtime: &RuntimeSnapshot,
    model_capability: &ModelCapability,
) -> Result<ProviderStreamer, CodingSessionError> {
    ModelCapability::require(Some(model_capability), runtime.profile_id())?;
    if let Some(provider_streamer) = runtime.provider_streamer() {
        return Ok(provider_streamer.clone());
    }
    let ai_client = scoped_ai_client_for_runtime(runtime);
    Ok(Arc::new(move |model, context, opts| {
        ai_client.stream_model(model, context, opts)
    }))
}

fn scoped_ai_client_for_runtime(runtime: &RuntimeSnapshot) -> Arc<AiClient> {
    let ai_client = AiClient::new();
    if runtime.register_builtins() {
        ai_client.register_builtins();
    }
    Arc::new(ai_client)
}

impl RuntimeService {
    pub(crate) fn new() -> Self {
        Self::with_ai_client(AiClient::new())
    }

    pub(crate) fn with_ai_client(ai_client: AiClient) -> Self {
        Self {
            ai_client: Arc::new(ai_client),
        }
    }

    pub(crate) fn install_provider_runtime(&self, runtime: &mut RuntimeSnapshot) {
        if runtime.register_builtins() {
            self.ai_client.register_builtins();
        }
        if self
            .ai_client
            .lookup_provider(&runtime.model().api)
            .is_none()
        {
            return;
        }
        let ai_client = self.ai_client.clone();
        runtime.set_provider_streamer(Arc::new(move |model, context, opts| {
            ai_client.stream_model(model, context, opts)
        }));
    }

    pub(crate) fn build_agent_runtime_with_capabilities(
        &self,
        runtime: &RuntimeSnapshot,
        snapshot: &OperationCapabilitySnapshot,
    ) -> Result<AgentRuntimeBuild, CodingSessionError> {
        self.build_agent_runtime_with_authorization(runtime, snapshot, None, None)
    }

    pub(crate) fn build_agent_runtime_with_authorization(
        &self,
        runtime: &RuntimeSnapshot,
        snapshot: &OperationCapabilitySnapshot,
        authorization: Option<AuthorizationHookContext>,
        delegation_executor: Option<DelegationToolExecutor>,
    ) -> Result<AgentRuntimeBuild, CodingSessionError> {
        let model_capability =
            ModelCapability::require(snapshot.model.as_ref(), runtime.profile_id())?;
        let provider_streamer = scoped_provider_streamer_for_runtime(runtime, model_capability)?;

        let mut diagnostics = runtime.profile_diagnostics().to_vec();
        let resources = apply_skill_policy(runtime, &mut diagnostics);        let mut policy_tools = delegation_tools(
            runtime.profile_id(),
            runtime.profile_delegation_policy(),
            runtime.delegation_target_inventory(),
            delegation_executor,
        );
        let uses_interactive_waiters = authorization
            .as_ref()
            .is_some_and(|context| context.service.uses_interactive_waiters());
        if !uses_interactive_waiters {
            for tool in &mut policy_tools {
                if let Some(schema) = tool.parameters.as_object_mut() {
                    schema.remove("x-evo-authorization-risk");
                }
            }
        }
        let authorization_tools = runtime
            .tools()
            .iter()
            .chain(policy_tools.iter())
            .cloned()
            .collect::<Vec<_>>();
        let authorization_inventory = ToolAuthorizationInventory::new(&authorization_tools);
        let tools = apply_tool_policy(runtime, policy_tools, &mut diagnostics)?
            .into_iter()
            .filter(|tool| snapshot.tools.allows(&tool.name))
            .filter_map(|tool| {
                crate::tools::bind_builtin_tool_to_capabilities(
                    tool,
                    snapshot.filesystem.as_ref(),
                    snapshot.shell.as_ref(),
                )
            })
            .collect::<Vec<_>>();

        let mut config = build_agent_config_with_auth_diagnostics(
            runtime.model().clone(),
            runtime.system_prompt().map(str::to_owned),
            runtime.max_turns(),
            runtime.api_key().map(str::to_owned),
            runtime.auth_diagnostics().to_vec(),
            runtime.thinking_level(),
            runtime.tool_execution(),
            resources,
            runtime.settings(),
        );
        let persistent_session = matches!(
            runtime
                .session_run_options()
                .map(|session_options| &session_options.mode),
            Some(SessionMode::Enabled)
        );
        if persistent_session && config.compaction.take().is_some() {
            diagnostics.push(CodingDiagnostic::info(
                "automatic runtime compaction is disabled for persistent sessions; use durable manual compaction",
            ));
        }
        config.provider_streamer = Some(provider_streamer);
        config.tool_execution_scope = Some(snapshot.operation_id.clone());
        if let Some(authorization) = authorization {
            let service = authorization.service;
            let turn_id = authorization.turn_id;
            let capability_snapshot = authorization.capability_snapshot;
            let event_writer = authorization.event_writer;
            config.hooks.before_tool_call = Some(Arc::new(move |context| {
                let service = service.clone();
                let turn_id = turn_id.clone();
                let capability_snapshot = capability_snapshot.clone();
                let inventory = authorization_inventory.clone();
                let event_writer = event_writer.clone();
                Box::pin(async move {
                    service
                        .authorize_with_event_writer(
                            context,
                            turn_id,
                            capability_snapshot,
                            inventory,
                            event_writer,
                        )
                        .await
                })
            }));
        }

        let agent = Agent::new(config);
        for tool in tools {
            agent
                .add_tool(tool)
                .map_err(|error| CodingSessionError::Tool {
                    message: error.to_string(),
                })?;
        }
        Ok(AgentRuntimeBuild { agent, diagnostics })
    }

    pub(crate) fn hydrate_agent_runtime(
        &self,
        agent: &Agent,
        runtime: &RuntimeSnapshot,
        replay: &SessionReplay,
    ) {
        let mut pending_assistant: Option<(String, AssistantMessage)> = None;
        let mut pending_tool_results = Vec::new();

        for (index, item) in replay.transcript.iter().enumerate() {
            match item {
                TranscriptItem::UserInput { text, .. } if !text.is_empty() => {
                    flush_replay_hydration_group(
                        agent,
                        &mut pending_assistant,
                        &mut pending_tool_results,
                    );
                    agent.add_message(AgentMessage::UserText {
                        message_id: format!("replay_user_{index}"),
                        text: text.clone(),
                    });
                }
                TranscriptItem::UserInput { .. } => {}
                TranscriptItem::AssistantMessage {
                    message_id,
                    content,
                    status: MessageStatus::Completed,
                    reasoning_duration_millis: None,
                    ..
                } => {
                    flush_replay_hydration_group(
                        agent,
                        &mut pending_assistant,
                        &mut pending_tool_results,
                    );
                    let mut message = replay_assistant_message(runtime);
                    message.content = replay_content_blocks(content);
                    if replay.usage.last_context_message_id.as_deref() == Some(message_id.as_str())
                        && let Some(context_tokens) = replay.usage.last_context_tokens
                    {
                        message.usage.total_tokens = context_tokens;
                    }
                    pending_assistant = Some((message_id.clone(), message));
                }
                TranscriptItem::ToolCall {
                    tool_call_id,
                    name,
                    arguments,
                    status: status @ (ToolCallStatus::Completed | ToolCallStatus::Failed),
                    summary,
                    ..
                } => {
                    pending_replay_assistant_message(&mut pending_assistant, runtime, index)
                        .content
                        .push(ContentBlock::ToolCall {
                            id: tool_call_id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                            thought_signature: None,
                        });
                    pending_tool_results.push(AgentMessage::ToolResult {
                        message_id: format!("replay_tool_result_{index}"),
                        tool_call_id: tool_call_id.clone(),
                        tool_name: name.clone(),
                        is_error: matches!(status, ToolCallStatus::Failed),
                        content: vec![ContentBlock::Text {
                            text: summary.clone(),
                            text_signature: None,
                        }],
                    });
                }
                TranscriptItem::ToolCall { .. } => {}
                TranscriptItem::CompactionSummary {
                    summary,
                    tokens_before,
                    ..
                } => {
                    flush_replay_hydration_group(
                        agent,
                        &mut pending_assistant,
                        &mut pending_tool_results,
                    );
                    agent.add_message(AgentMessage::CompactionSummary {
                        message_id: format!("replay_compaction_{index}"),
                        summary: summary.clone(),
                        tokens_before: *tokens_before,
                    });
                }
                TranscriptItem::BranchSummary {
                    summary,
                    source_leaf_id,
                    ..
                } => {
                    flush_replay_hydration_group(
                        agent,
                        &mut pending_assistant,
                        &mut pending_tool_results,
                    );
                    agent.add_message(AgentMessage::BranchSummary {
                        message_id: format!("replay_branch_summary_{index}"),
                        summary: summary.clone(),
                        from_id: source_leaf_id.clone(),
                        timestamp: 0,
                    });
                }
                TranscriptItem::Diagnostic { .. } | TranscriptItem::DelegationBlock { .. } => {
                    flush_replay_hydration_group(
                        agent,
                        &mut pending_assistant,
                        &mut pending_tool_results,
                    );
                }
                TranscriptItem::AssistantMessage { .. } => {}
            }
        }

        flush_replay_hydration_group(agent, &mut pending_assistant, &mut pending_tool_results);
    }
}

fn apply_tool_policy(
    runtime: &RuntimeSnapshot,
    policy_tools: Vec<AgentTool>,
    diagnostics: &mut Vec<CodingDiagnostic>,
) -> Result<Vec<AgentTool>, CodingSessionError> {
    let mut tools = Vec::new();
    let mut owners = std::collections::BTreeMap::new();
    let policy_tool_names = policy_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    for (owner, candidates) in [
        ("runtime", runtime.tools().to_vec()),
        ("delegation", policy_tools),
    ] {
        for tool in candidates {
            if let Some(first) = owners.insert(tool.name.clone(), owner) {
                return Err(CodingSessionError::Tool {
                    message: format!(
                        "tool_name_conflict: tool `{}` is declared by both {first} and {owner} inventory",
                        tool.name
                    ),
                });
            }
            tools.push(tool);
        }
    }
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    let Some(allowlist) = runtime.profile_tool_allowlist() else {
        return Ok(tools);
    };

    let available = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    let allowed = allowlist
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for requested in &allowed {
        if !available.contains(requested) {
            diagnostics.push(CodingDiagnostic::warning(format!(
                "agent profile requested unavailable tool: {requested}"
            )));
        }
    }
    Ok(tools
        .into_iter()
        .filter(|tool| {
            policy_tool_names.contains(tool.name.as_str()) || allowed.contains(tool.name.as_str())
        })
        .collect())
}

fn apply_skill_policy(
    runtime: &RuntimeSnapshot,
    diagnostics: &mut Vec<CodingDiagnostic>,
) -> AgentResources {
    let mut resources = runtime.resources().clone();
    let Some(allowlist) = runtime.profile_skill_allowlist() else {
        return resources;
    };

    let available = resources
        .skills
        .iter()
        .map(|skill| skill.name.as_str())
        .collect::<BTreeSet<_>>();
    let allowed = allowlist
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for requested in &allowed {
        if !available.contains(requested) {
            diagnostics.push(CodingDiagnostic::warning(format!(
                "agent profile requested unavailable skill: {requested}"
            )));
        }
    }
    resources
        .skills
        .retain(|skill| allowed.contains(skill.name.as_str()));
    resources
}

fn replay_assistant_message(runtime: &RuntimeSnapshot) -> AssistantMessage {
    let mut message = AssistantMessage::empty(&runtime.model().api, &runtime.model().id);
    message.provider = Some(runtime.model().provider.clone());
    message.stop_reason = StopReason::Stop;
    message
}

fn replay_content_blocks(content: &[PersistedContentBlock]) -> Vec<ContentBlock> {
    content
        .iter()
        .map(|block| match block {
            PersistedContentBlock::Text { text } => ContentBlock::Text {
                text: text.clone(),
                text_signature: None,
            },
            PersistedContentBlock::Thinking {
                thinking,
                thinking_signature,
                redacted,
            } => ContentBlock::Thinking {
                thinking: thinking.clone(),
                thinking_signature: thinking_signature.clone(),
                redacted: *redacted,
            },
            PersistedContentBlock::Image { mime_type, data } => ContentBlock::Image {
                mime_type: mime_type.clone(),
                data: data.clone(),
            },
        })
        .collect()
}

fn pending_replay_assistant_message<'a>(
    pending_assistant: &'a mut Option<(String, AssistantMessage)>,
    runtime: &RuntimeSnapshot,
    index: usize,
) -> &'a mut AssistantMessage {
    if pending_assistant.is_none() {
        *pending_assistant = Some((
            format!("replay_assistant_tool_{index}"),
            replay_assistant_message(runtime),
        ));
    }
    &mut pending_assistant.as_mut().expect("pending assistant set").1
}

fn flush_replay_hydration_group(
    agent: &Agent,
    pending_assistant: &mut Option<(String, AssistantMessage)>,
    pending_tool_results: &mut Vec<AgentMessage>,
) {
    if let Some((message_id, message)) = pending_assistant.take() {
        agent.add_message(AgentMessage::Assistant {
            message_id,
            message,
        });
    }
    for message in pending_tool_results.drain(..) {
        agent.add_message(message);
    }
}
