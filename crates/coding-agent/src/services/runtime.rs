#[derive(Clone)]
pub(crate) struct RuntimeService {
    ai_client: Arc<AiClient>,
    background_tasks: Option<BackgroundTaskService>,
}

impl std::fmt::Debug for RuntimeService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeService")
            .field(
                "registered_apis",
                &self.ai_client.provider_registry().registered_apis(),
            )
            .field("background_tasks", &self.background_tasks)
            .finish()
    }
}

use std::collections::BTreeSet;
use std::sync::Arc;

use agent_core::api::agent::{Agent, AgentMessage, AgentResources, ProviderStreamer};
use ai::api::client::AiClient;
use ai_protocol::api::conversation::{AssistantMessage, ContentBlock, Context, StopReason};
use ai_protocol::api::stream::{EventStream, StreamOptions};
use tool_runtime::api::{ToolRegistry, ToolRuntime};

use crate::app::bootstrap::{SessionMode, build_agent_config_with_auth_diagnostics};
use crate::platform::time::Clock;

use crate::application::capability::OperationCapabilitySnapshot;
use crate::kernel::capability::ModelCapability;
use crate::kernel::error::CodingSessionError;
use crate::operations::delegation::delegation_tools;
use crate::operations::prompt::context::{
    CodingDiagnostic, DelegationToolExecutor, RuntimeSnapshot,
};
use crate::services::authorization::{AuthorizationHookContext, ToolAuthorizationInventory};
use crate::services::background::BackgroundTaskService;
use crate::services::review::MutationTracking;
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
            background_tasks: None,
        }
    }

    pub(crate) fn with_background_tasks(mut self, background_tasks: BackgroundTaskService) -> Self {
        self.background_tasks = Some(background_tasks);
        self
    }

    /// Inject the session's background task service into a runtime snapshot
    /// at operation submission time, so tools built inside operation runners
    /// (which construct their own `RuntimeService`) still reach the session
    /// registry.
    pub(crate) fn install_background_tasks(&self, runtime: &mut RuntimeSnapshot) {
        runtime.set_background_tasks(self.background_tasks.clone());
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

    pub(crate) async fn build_agent_runtime_with_capabilities(
        &self,
        runtime: &RuntimeSnapshot,
        snapshot: &OperationCapabilitySnapshot,
    ) -> Result<AgentRuntimeBuild, CodingSessionError> {
        self.build_agent_runtime_with_authorization(runtime, snapshot, None, None, None)
            .await
    }

    pub(crate) async fn build_agent_runtime_with_authorization(
        &self,
        runtime: &RuntimeSnapshot,
        snapshot: &OperationCapabilitySnapshot,
        authorization: Option<AuthorizationHookContext>,
        delegation_executor: Option<DelegationToolExecutor>,
        mutation_tracking: Option<MutationTracking>,
    ) -> Result<AgentRuntimeBuild, CodingSessionError> {
        let model_capability =
            ModelCapability::require(snapshot.model.as_ref(), runtime.profile_id())?;
        let provider_streamer = scoped_provider_streamer_for_runtime(runtime, model_capability)?;

        let mut diagnostics = runtime.profile_diagnostics().to_vec();
        let resources = apply_skill_policy(runtime, &mut diagnostics);
        let provider_tools = runtime.provider_tools();
        let mut typed_registry = ToolRegistry::default();
        for id in runtime
            .typed_tool_ids()
            .filter(|id| snapshot.tools.allows(id))
        {
            let tool = match id.as_str() {
                "read" => snapshot
                    .workspace
                    .clone()
                    .map(crate::tools::filesystem::read::read_runtime_tool)
                    .transpose()
                    .map_err(|error| CodingSessionError::Tool {
                        message: error.to_string(),
                    })?,
                "ls" => snapshot
                    .workspace
                    .clone()
                    .map(crate::tools::filesystem::ls::ls_runtime_tool)
                    .transpose()
                    .map_err(|error| CodingSessionError::Tool {
                        message: error.to_string(),
                    })?,
                "find" => snapshot
                    .workspace
                    .clone()
                    .map(crate::tools::filesystem::find::find_runtime_tool)
                    .transpose()
                    .map_err(|error| CodingSessionError::Tool {
                        message: error.to_string(),
                    })?,
                "grep" => snapshot
                    .workspace
                    .clone()
                    .map(crate::tools::filesystem::grep::grep_runtime_tool)
                    .transpose()
                    .map_err(|error| CodingSessionError::Tool {
                        message: error.to_string(),
                    })?,
                "write" => snapshot
                    .workspace
                    .clone()
                    .map(|filesystem| {
                        crate::tools::filesystem::write::write_runtime_tool_with_tracking(
                            filesystem,
                            mutation_tracking.clone(),
                        )
                    })
                    .transpose()
                    .map_err(|error| CodingSessionError::Tool {
                        message: error.to_string(),
                    })?,
                "edit" => snapshot
                    .workspace
                    .clone()
                    .map(|filesystem| {
                        crate::tools::filesystem::edit::edit_runtime_tool_with_tracking(
                            filesystem,
                            mutation_tracking.clone(),
                        )
                    })
                    .transpose()
                    .map_err(|error| CodingSessionError::Tool {
                        message: error.to_string(),
                    })?,
                "hashline_edit" => snapshot
                    .workspace
                    .clone()
                    .map(|filesystem| {
                        crate::tools::filesystem::hashline::hashline_edit_runtime_tool_with_tracking(
                            filesystem,
                            mutation_tracking.clone(),
                        )
                    })
                    .transpose()
                    .map_err(|error| CodingSessionError::Tool {
                        message: error.to_string(),
                    })?,
                "apply_patch" => snapshot
                    .workspace
                    .clone()
                    .map(|filesystem| {
                        crate::tools::filesystem::apply_patch::apply_patch_runtime_tool_with_tracking(
                            filesystem,
                            mutation_tracking.clone(),
                        )
                    })
                    .transpose()
                    .map_err(|error| CodingSessionError::Tool {
                        message: error.to_string(),
                    })?,
                "bash" => snapshot
                    .workspace
                    .clone()
                    .map(|shell| {
                        crate::tools::shell::bash_runtime_tool(
                            shell,
                            runtime.background_tasks().cloned(),
                        )
                    })
                    .transpose()
                    .map_err(|error| CodingSessionError::Tool {
                        message: error.to_string(),
                    })?,
                "web_fetch" => Some(
                    crate::tools::web_fetch::web_fetch_runtime_tool().map_err(|error| {
                        CodingSessionError::Tool {
                            message: error.to_string(),
                        }
                    })?,
                ),
                _ => None,
            };
            if let Some(tool) = tool {
                typed_registry
                    .register(tool)
                    .map_err(|error| CodingSessionError::Tool {
                        message: error.to_string(),
                    })?;
            }
        }
        for tool in runtime.tools() {
            if snapshot.tools.allows(&tool.definition().id) {
                typed_registry.register(tool.clone()).map_err(|error| {
                    CodingSessionError::Tool {
                        message: error.to_string(),
                    }
                })?;
            }
        }
        for tool in delegation_tools(
            runtime.profile_id(),
            runtime.profile_delegation_policy(),
            runtime.delegation_target_inventory(),
            delegation_executor,
        ) {
            if snapshot.tools.allows(&tool.definition().id) {
                typed_registry
                    .register(tool)
                    .map_err(|error| CodingSessionError::Tool {
                        message: error.to_string(),
                    })?;
            }
        }
        let runtime_definitions = typed_registry.definitions();
        let typed_runtime = (!runtime_definitions.is_empty())
            .then(|| ToolRuntime::new(typed_registry))
            .transpose()
            .map_err(|error| CodingSessionError::Tool {
                message: error.to_string(),
            })?;
        record_unavailable_profile_tools(runtime, &runtime_definitions, &mut diagnostics);
        let authorization_inventory = ToolAuthorizationInventory::new(&runtime_definitions);

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
            let extension_events = authorization.extension_events;
            let (extension_session_id, extension_workspace_root) = authorization.extension_identity;
            if let Some(extension_events) = extension_events {
                let service_for_before = service.clone();
                let turn_id_for_before = turn_id.clone();
                let capability_for_before = capability_snapshot.clone();
                let writer_for_before = event_writer.clone();
                let events_for_before = extension_events.clone();
                let session_for_before = extension_session_id.clone();
                let workspace_for_before = extension_workspace_root.clone();
                config.hooks.before_tool_call = Some(Arc::new(move |context| {
                    let service = service_for_before.clone();
                    let turn_id = turn_id_for_before.clone();
                    let capability_snapshot = capability_for_before.clone();
                    let inventory = authorization_inventory.clone();
                    let event_writer = writer_for_before.clone();
                    let extension_events = events_for_before.clone();
                    let extension_session_id = session_for_before.clone();
                    let extension_workspace_root = workspace_for_before.clone();
                    Box::pin(async move {
                        // user hooks Tool gate：deny / sandbox 拒绝 → 阻塞
                        // 工具调用（block 语义直接生效）；allow 与失败
                        // （fail-open）→ 继续走产品 authorization。
                        let sink = &extension_events;
                        let gate = sink.hook_gate();
                        if let Some(event) = pre_tool_event(
                            &context,
                            &extension_session_id,
                            &extension_workspace_root,
                        ) {
                            if let Some(gate) = gate {
                                match gate.evaluate_tool(&event).await {
                                    extension_host::api::ToolGateDecision::Deny { reason }
                                    | extension_host::api::ToolGateDecision::ClosedByEnvironment {
                                        reason,
                                    } => {
                                        return Ok(Some(
                                            agent_core::api::agent::BeforeToolCallResult {
                                                block: true,
                                                reason: Some(reason),
                                            },
                                        ));
                                    }
                                    extension_host::api::ToolGateDecision::Allow => {}
                                }
                            }
                            let extension_host::api::ExtensionEventPayload::PreToolUse {
                                tool_name,
                                tool_input,
                                tool_input_truncated,
                                path,
                            } = event.payload
                            else {
                                unreachable!("pre_tool_event always builds PreToolUse");
                            };
                            sink.submit(
                                extension_host::api::ExtensionEventKind::PreToolUse,
                                &event.session_id,
                                &event.workspace_root,
                                extension_host::api::ExtensionEventPayload::PreToolUse {
                                    tool_name,
                                    tool_input,
                                    tool_input_truncated,
                                    path,
                                },
                            );
                        }
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
                // user hooks 的 after_tool_call：PostToolUse 事件（Observe gate）。
                let events_for_after = extension_events.clone();
                let session_for_after = extension_session_id.clone();
                let workspace_for_after = extension_workspace_root.clone();
                config.hooks.after_tool_call = Some(Arc::new(move |context| {
                    let extension_events = events_for_after.clone();
                    let extension_session_id = session_for_after.clone();
                    let extension_workspace_root = workspace_for_after.clone();
                    Box::pin(async move {
                        let event = post_tool_event(
                            &context,
                            &extension_session_id,
                            &extension_workspace_root,
                        );
                        extension_events.submit(
                            extension_host::api::ExtensionEventKind::PostToolUse,
                            &event.session_id,
                            &event.workspace_root,
                            event.payload,
                        );
                        Ok(None)
                    })
                }));
                // user hooks Stop gate：工具执行后与每个 turn 自然结束时
                // 评估。工具执行后的决策点（模型还会继续总结/推进）只有
                // 用户 hooks 显式 force_stop 才停止；turn 自然结束时
                // block → 继续（false）；force_stop / 无信号 / 失败
                // （fail-open）→ 正常停止（true）。无用户扩展（gate=None，
                // 即没有 Stop gate）时：工具执行后必须继续，自然结束时
                // 正常停止。
                let events_for_stop = extension_events.clone();
                let session_for_stop = extension_session_id.clone();
                let workspace_for_stop = extension_workspace_root.clone();
                config.hooks.should_stop_after_turn = Some(Arc::new(move |context| {
                    let extension_events = events_for_stop.clone();
                    let extension_session_id = session_for_stop.clone();
                    let extension_workspace_root = workspace_for_stop.clone();
                    Box::pin(async move {
                        let gate = extension_events.hook_gate();
                        let event = extension_host::api::ExtensionEvent::new(
                            extension_host::api::ExtensionEventKind::Stop,
                            &extension_session_id,
                            &extension_workspace_root,
                            crate::platform::time::SystemClock.now_rfc3339(),
                            extension_host::api::ExtensionEventPayload::Stop {
                                reason: format!("{:?}", context.assistant_message.stop_reason),
                                last_assistant_message: None,
                            },
                        );
                        extension_events.submit(
                            extension_host::api::ExtensionEventKind::Stop,
                            &event.session_id,
                            &event.workspace_root,
                            event.payload.clone(),
                        );
                        let after_tool_use =
                            context.assistant_message.stop_reason == StopReason::ToolUse;
                        let Some(gate) = gate else {
                            return Ok(if after_tool_use {
                                agent_core::api::agent::ShouldStopAfterTurnResult::continue_with(
                                    Vec::new(),
                                )
                            } else {
                                agent_core::api::agent::ShouldStopAfterTurnResult::stop()
                            });
                        };
                        let decision = gate.evaluate_stop(&event).await;
                        Ok(agent_core::api::agent::ShouldStopAfterTurnResult {
                            should_stop: if after_tool_use {
                                decision.force_stop.is_some()
                            } else {
                                !decision.wants_continuation()
                            },
                            additional_context: decision.additional_context.clone(),
                        })
                    })
                }));
            } else {
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
        }

        let agent = Agent::new(config);
        if let Some(runtime) = typed_runtime {
            agent
                .set_tool_runtime(runtime)
                .await
                .map_err(|error| CodingSessionError::Tool {
                    message: error.to_string(),
                })?;
        }
        for definition in provider_tools
            .into_iter()
            .filter(|definition| snapshot.tools.allows(&definition.id))
        {
            agent.add_provider_tool(definition).await.map_err(|error| {
                CodingSessionError::Tool {
                    message: error.to_string(),
                }
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
                            kind: if name == "apply_patch" && arguments.is_string() {
                                ai_protocol::api::conversation::ToolCallKind::Custom
                            } else {
                                ai_protocol::api::conversation::ToolCallKind::Function
                            },
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

fn record_unavailable_profile_tools(
    runtime: &RuntimeSnapshot,
    runtime_definitions: &[tool_contract::api::definition::ToolDefinition],
    diagnostics: &mut Vec<CodingDiagnostic>,
) {
    let Some(allowlist) = runtime.profile_tool_allowlist() else {
        return;
    };

    let provider_tools = runtime.provider_tools();
    let available = runtime_definitions
        .iter()
        .map(|definition| definition.id.as_str())
        .chain(
            provider_tools
                .iter()
                .map(|definition| definition.id.as_str()),
        )
        .collect::<BTreeSet<_>>();
    let allowed = allowlist
        .iter()
        .map(|id| id.as_str())
        .collect::<BTreeSet<_>>();
    for requested in &allowed {
        if !available.contains(requested) {
            diagnostics.push(CodingDiagnostic::warning(format!(
                "agent profile requested unavailable tool: {requested}"
            )));
        }
    }
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
                provider_metadata,
                redacted,
            } => ContentBlock::Thinking {
                thinking: thinking.clone(),
                thinking_signature: thinking_signature.clone(),
                provider_metadata: provider_metadata.clone(),
                redacted: *redacted,
            },
            PersistedContentBlock::Image { mime_type, data } => ContentBlock::Image {
                mime_type: mime_type.clone(),
                data: data.clone(),
            },
            PersistedContentBlock::ProviderItem { api, item } => ContentBlock::ProviderItem {
                api: api.clone(),
                item: item.clone(),
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

/// 从 `before_tool_call` 上下文构造 `pre_tool_use` 事件信封。
///
/// `path` 取 arguments 的 `path` 字段（matcher 的 path 条件数据源）；
/// 工具名非法（不应发生）时返回 `None`，调用方跳过 hook 层直接走
/// authorization（fail-open）。
fn pre_tool_event(
    context: &agent_core::api::agent::BeforeToolCallContext,
    session_id: &str,
    workspace_root: &str,
) -> Option<extension_host::api::ExtensionEvent> {
    let tool_name = tool_contract::api::definition::ToolId::new(&context.tool_name).ok()?;
    let path = context
        .arguments
        .get("path")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Some(extension_host::api::ExtensionEvent::new(
        extension_host::api::ExtensionEventKind::PreToolUse,
        session_id,
        workspace_root,
        crate::platform::time::SystemClock.now_rfc3339(),
        extension_host::api::ExtensionEventPayload::PreToolUse {
            tool_name,
            tool_input: context.arguments.clone(),
            tool_input_truncated: false,
            path,
        },
    ))
}

/// 从 `after_tool_call` 上下文构造 `post_tool_use` 事件信封。
///
/// 工具结果按摘要 JSON 承载（`isError` / `terminate` / `details`），不把
/// 完整 ContentBlock 转储进 hook 环境（事件不携带输出内容的骨架约定）。
fn post_tool_event(
    context: &agent_core::api::agent::AfterToolCallContext,
    session_id: &str,
    workspace_root: &str,
) -> extension_host::api::ExtensionEvent {
    let tool_name =
        tool_contract::api::definition::ToolId::new(&context.tool_name).unwrap_or_else(|_| {
            tool_contract::api::definition::ToolId::new("unknown").expect("static tool id")
        });
    let path = context
        .arguments
        .get("path")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let tool_result = serde_json::json!({
        "isError": context.result.is_error,
        "terminate": context.result.terminate,
        "details": context.result.details,
    });
    extension_host::api::ExtensionEvent::new(
        extension_host::api::ExtensionEventKind::PostToolUse,
        session_id,
        workspace_root,
        crate::platform::time::SystemClock.now_rfc3339(),
        extension_host::api::ExtensionEventPayload::PostToolUse {
            tool_name,
            tool_input: context.arguments.clone(),
            tool_result,
            tool_input_truncated: false,
            tool_result_truncated: false,
            duration_ms: None,
            path,
        },
    )
}
