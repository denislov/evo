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
    #[cfg(test)]
    tool_names: Vec<String>,
    #[cfg(test)]
    compaction_enabled: bool,
}

#[cfg(test)]
impl AgentRuntimeBuild {
    pub(crate) fn tool_names_for_tests(&self) -> Vec<String> {
        self.tool_names.clone()
    }

    pub(crate) fn compaction_enabled_for_tests(&self) -> bool {
        self.compaction_enabled
    }
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

    #[cfg(test)]
    pub(crate) fn build_agent_runtime(
        &self,
        runtime: &RuntimeSnapshot,
    ) -> Result<Agent, CodingSessionError> {
        Ok(self.build_agent_runtime_with_diagnostics(runtime)?.agent)
    }

    #[cfg(test)]
    pub(crate) fn build_agent_runtime_with_diagnostics(
        &self,
        runtime: &RuntimeSnapshot,
    ) -> Result<AgentRuntimeBuild, CodingSessionError> {
        let snapshot = OperationCapabilitySnapshot::permissive("op_test_runtime");
        self.build_agent_runtime_with_capabilities(runtime, &snapshot)
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
        let resources = apply_skill_policy(runtime, &mut diagnostics);
        let mut policy_tools = delegation_tools(
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
        #[cfg(test)]
        let tool_names = tools
            .iter()
            .map(|tool| tool.name.clone())
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

        #[cfg(test)]
        let compaction_enabled = config.compaction.is_some();
        let agent = Agent::new(config);
        for tool in tools {
            agent
                .add_tool(tool)
                .map_err(|error| CodingSessionError::Tool {
                    message: error.to_string(),
                })?;
        }
        Ok(AgentRuntimeBuild {
            agent,
            diagnostics,
            #[cfg(test)]
            tool_names,
            #[cfg(test)]
            compaction_enabled,
        })
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

#[cfg(test)]
mod tests {
    use agent_core::api::agent::AgentResources;
    use agent_core::api::tool::{AgentTool, ToolExecutionMode};
    use ai::api::auth::ProviderAuthDiagnostic;
    use ai::api::model::{Model, ModelCost, ModelInput};
    use std::collections::HashSet;

    use super::*;
    use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
    use crate::app::prompt_runtime::PromptRuntimeOptions;
    use crate::config::settings::{PartialCompaction, PartialSettings};
    use crate::session::event::DiagnosticLevel;
    use crate::session::replay::{
        MessageStatus, ReplayDiagnostic, SessionReplay, ToolCallStatus, TranscriptItem,
    };

    fn model(api: &str) -> Model {
        Model {
            id: "test-model".into(),
            name: "Test Model".into(),
            api: api.into(),
            provider: "test".into(),
            base_url: String::new(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![ModelInput::Text],
            cost: ModelCost::default(),
            context_window: 0,
            max_tokens: 0,
            headers: None,
            compat: None,
        }
    }

    fn runtime_snapshot(api: &str) -> RuntimeSnapshot {
        runtime_snapshot_with_auth_diagnostics(api, Vec::new())
    }

    fn runtime_snapshot_with_compaction(api: &str, persistent: bool) -> RuntimeSnapshot {
        let settings = PartialSettings {
            compaction: Some(PartialCompaction {
                enabled: Some(true),
                reserve_tokens: Some(1024),
                keep_recent_tokens: Some(2048),
            }),
            ..Default::default()
        }
        .resolve();
        RuntimeSnapshot::from_prompt_runtime_options(PromptRuntimeOptions {
            model: model(api),
            api_key: Some("key".into()),
            auth_diagnostics: Vec::new(),
            system_prompt: Some("system".into()),
            max_turns: Some(2),
            tools: Vec::new(),
            register_builtins: false,
            ai_client: None,
            session: Some(if persistent {
                SessionRunOptions::enabled(".".into())
            } else {
                SessionRunOptions::disabled(".".into())
            }),
            session_target: None,
            session_name: None,
            thinking_level: None,
            tool_execution: Some(ToolExecutionMode::Sequential),
            resources: AgentResources::default(),
            settings: Some(settings),
            invocation: PromptInvocation::Text("hello".into()),
        })
    }

    fn runtime_snapshot_with_auth_diagnostics(
        api: &str,
        auth_diagnostics: Vec<ProviderAuthDiagnostic>,
    ) -> RuntimeSnapshot {
        RuntimeSnapshot::from_prompt_runtime_options(PromptRuntimeOptions {
            model: model(api),
            api_key: Some("key".into()),
            auth_diagnostics,
            system_prompt: Some("system".into()),
            max_turns: Some(2),
            tools: Vec::new(),
            register_builtins: false,
            ai_client: None,
            session: Some(SessionRunOptions::disabled(".".into())),
            session_target: None,
            session_name: None,
            thinking_level: None,
            tool_execution: Some(ToolExecutionMode::Sequential),
            resources: AgentResources::default(),
            settings: None,
            invocation: PromptInvocation::Text("hello".into()),
        })
    }

    fn runtime_snapshot_with_tools(
        names: impl IntoIterator<Item = &'static str>,
    ) -> RuntimeSnapshot {
        let tools = names
            .into_iter()
            .map(|name| {
                AgentTool::new_text(
                    name,
                    name,
                    serde_json::json!({"type": "object"}),
                    |_, _| async { Ok(String::new()) },
                )
            })
            .collect::<Vec<_>>();
        RuntimeSnapshot::from_prompt_runtime_options(PromptRuntimeOptions {
            model: model("runtime-service-capability-tools"),
            api_key: Some("key".into()),
            auth_diagnostics: Vec::new(),
            system_prompt: Some("system".into()),
            max_turns: Some(2),
            tools,
            register_builtins: false,
            ai_client: None,
            session: Some(SessionRunOptions::disabled(".".into())),
            session_target: None,
            session_name: None,
            thinking_level: None,
            tool_execution: Some(ToolExecutionMode::Sequential),
            resources: AgentResources::default(),
            settings: None,
            invocation: PromptInvocation::Text("hello".into()),
        })
    }

    #[test]
    fn persistent_runtime_disables_ephemeral_automatic_compaction() {
        let build = RuntimeService::new()
            .build_agent_runtime_with_capabilities(
                &runtime_snapshot_with_compaction("runtime-persistent-compaction", true),
                &OperationCapabilitySnapshot::permissive("op_persistent_compaction"),
            )
            .unwrap();

        assert!(!build.compaction_enabled_for_tests());
        assert!(
            build
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.message.contains("use durable manual compaction") })
        );
    }

    #[test]
    fn non_persistent_runtime_retains_ephemeral_automatic_compaction() {
        let build = RuntimeService::new()
            .build_agent_runtime_with_capabilities(
                &runtime_snapshot_with_compaction("runtime-ephemeral-compaction", false),
                &OperationCapabilitySnapshot::permissive("op_ephemeral_compaction"),
            )
            .unwrap();

        assert!(build.compaction_enabled_for_tests());
        assert!(
            build.diagnostics.iter().all(|diagnostic| {
                !diagnostic.message.contains("use durable manual compaction")
            })
        );
    }

    #[test]
    fn builds_agent_from_runtime_snapshot() {
        let service = RuntimeService::new();

        let agent = service
            .build_agent_runtime(&runtime_snapshot("runtime-service-build"))
            .unwrap();

        let (context, stream_options) = agent.provider_request_snapshot();
        assert_eq!(context.system_prompt.as_deref(), Some("system"));
        assert_eq!(
            stream_options.and_then(|options| options.api_key),
            Some("key".into())
        );
    }

    #[test]
    fn build_agent_runtime_preserves_prompt_auth_diagnostics() {
        let service = RuntimeService::new();
        let snapshot = runtime_snapshot_with_auth_diagnostics(
            "runtime-service-auth-diagnostics",
            vec![ProviderAuthDiagnostic {
                field: "api_key".into(),
                source: "auth.toml:oauth".into(),
            }],
        );

        let agent = service.build_agent_runtime(&snapshot).unwrap();

        let (_context, stream_options) = agent.provider_request_snapshot();
        let diagnostics = stream_options
            .expect("auth diagnostics should create stream options")
            .auth_diagnostics
            .into_iter()
            .map(|diagnostic| (diagnostic.field, diagnostic.source))
            .collect::<Vec<_>>();
        assert_eq!(
            diagnostics,
            vec![("api_key".into(), "auth.toml:oauth".into())]
        );
    }

    #[test]
    fn runtime_build_rejects_missing_model_capability() {
        use crate::runtime::capability::{
            ActorId, CapabilityGeneration, OperationCapabilitySnapshot, ToolCapabilitySet,
        };
        let runtime = runtime_snapshot("test-api");
        let snapshot = OperationCapabilitySnapshot {
            generation: CapabilityGeneration::new(1),
            operation_id: "op_runtime".into(),
            actor: ActorId::Client,
            model: None,
            tools: ToolCapabilitySet::default(),
            commands: Default::default(),
            filesystem: None,
            shell: None,
            session_read: None,
            session_write: None,
            ui: None,
        };

        let error = match RuntimeService::new()
            .build_agent_runtime_with_capabilities(&runtime, &snapshot)
        {
            Ok(_) => panic!("expected missing model capability to be rejected"),
            Err(error) => error,
        };

        assert_eq!(error.code(), "unsupported_capability");
        assert!(error.to_string().contains("model capability"));
    }

    #[test]
    fn runtime_build_filters_tools_through_capability_snapshot() {
        use crate::runtime::capability::OperationCapabilitySnapshot;
        let runtime = runtime_snapshot_with_tools(["read", "bash"]);
        let snapshot = OperationCapabilitySnapshot::test_with_tools("op_runtime", ["read"]);

        let build = RuntimeService::new()
            .build_agent_runtime_with_authorization(&runtime, &snapshot, None, None)
            .unwrap();

        assert_eq!(build.tool_names_for_tests(), vec!["read".to_string()]);
    }

    #[test]
    fn hydrates_agent_runtime_from_completed_replay_items() {
        let service = RuntimeService::new();
        let runtime = runtime_snapshot("runtime-service-hydrate");
        let agent = service.build_agent_runtime(&runtime).unwrap();
        let replay = SessionReplay {
            session_id: "sess_replay".into(),
            committed_through_session_sequence: 0,
            cwd: None,
            active_leaf_id: None,
            leaves: Vec::new(),
            tree_labels: Default::default(),
            transcript: vec![
                TranscriptItem::CompactionSummary {
                    summary: "summary of earlier work".into(),
                    first_kept_message_id: "turn_1".into(),
                    tokens_before: 1200,
                },
                TranscriptItem::UserInput {
                    turn_id: "turn_1".into(),
                    text: "previous question".into(),
                },
                TranscriptItem::AssistantMessage {
                    message_id: "msg_1".into(),
                    content: vec![
                        PersistedContentBlock::Thinking {
                            thinking: "previous thought".into(),
                            thinking_signature: None,
                            redacted: None,
                        },
                        PersistedContentBlock::Text {
                            text: "previous answer".into(),
                        },
                    ],
                    status: MessageStatus::Completed,
                },
                TranscriptItem::ToolCall {
                    tool_call_id: "tool_1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "src/lib.rs"}),
                    status: ToolCallStatus::Completed,
                    summary: "tool output".into(),
                    started_at: "2026-07-27T00:00:00Z".into(),
                    duration_millis: Some(120),
                },
                TranscriptItem::AssistantMessage {
                    message_id: "msg_cancelled".into(),
                    content: vec![PersistedContentBlock::Text {
                        text: "cancelled answer".into(),
                    }],
                    status: MessageStatus::Cancelled,
                },
                TranscriptItem::Diagnostic {
                    level: DiagnosticLevel::Warn,
                    message: "ignored".into(),
                },
            ],
            diagnostics: vec![ReplayDiagnostic {
                level: DiagnosticLevel::Warn,
                message: "ignored".into(),
            }],
            pending_delegation_confirmations: Vec::new(),
            pending_tool_authorizations: Vec::new(),
            usage: crate::session::replay::ReplayUsageSummary {
                last_context_tokens: Some(95_000),
                last_context_message_id: Some("msg_1".into()),
                ..Default::default()
            },
            operation_statuses: Default::default(),
        };

        service.hydrate_agent_runtime(&agent, &runtime, &replay);

        let messages = agent.messages();
        assert_eq!(messages.len(), 4);
        let message_ids: HashSet<_> = messages
            .iter()
            .map(|message| match message {
                AgentMessage::UserText { message_id, .. }
                | AgentMessage::Assistant { message_id, .. }
                | AgentMessage::ToolResult { message_id, .. }
                | AgentMessage::CompactionSummary { message_id, .. }
                | AgentMessage::BranchSummary { message_id, .. }
                | AgentMessage::SystemPrompt { message_id, .. }
                | AgentMessage::BashExecution { message_id, .. }
                | AgentMessage::Custom { message_id, .. } => message_id.as_str(),
            })
            .collect();
        assert_eq!(message_ids.len(), messages.len());
        assert!(matches!(
            &messages[0],
            AgentMessage::CompactionSummary {
                summary,
                tokens_before,
                ..
            } if summary == "summary of earlier work" && *tokens_before == 1200
        ));
        assert!(matches!(
            &messages[1],
            AgentMessage::UserText { text, .. } if text == "previous question"
        ));
        assert!(matches!(
            &messages[2],
            AgentMessage::Assistant { message_id, message }
                if message_id == "msg_1"
                    && message.content == vec![ContentBlock::Thinking {
                        thinking: "previous thought".into(),
                        thinking_signature: None,
                        redacted: None,
                    }, ContentBlock::Text {
                        text: "previous answer".into(),
                        text_signature: None,
                    }, ContentBlock::ToolCall {
                        id: "tool_1".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({"path": "src/lib.rs"}),
                        thought_signature: None,
                }]
                    && message.usage.total_tokens == 95_000
        ));
        assert!(matches!(
            &messages[3],
            AgentMessage::ToolResult {
                message_id,
                tool_call_id,
                tool_name,
                is_error,
                content,
            } if message_id == "replay_tool_result_3"
                && tool_call_id == "tool_1"
                && tool_name == "read"
                && !is_error
                && *content == vec![ContentBlock::Text {
                    text: "tool output".into(),
                    text_signature: None,
                }]
        ));
    }
}
