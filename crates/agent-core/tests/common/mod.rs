#![allow(
    dead_code,
    reason = "Cargo compiles this shared integration fixture separately for agent, compaction, execution, and hook targets"
)]

use agent_core::api::agent::{AgentConfig, ProviderStreamer};
use ai::api::client::AiClient;
use ai::api::conversation::{AssistantMessage, ContentBlock, Context, StopReason};
use ai::api::model::{Model, ModelCost, ModelInput};
use ai::api::provider::ApiProvider;
use ai::api::stream::{AssistantMessageEvent, EventStream, StreamOptions};
use ai::api::testing::FauxResponse;
use async_stream::stream;
use std::sync::{Arc, Mutex};

/// A scripted LLM response for one turn.
pub struct ScriptedTurn {
    pub events: Vec<AssistantMessageEvent>,
    pub stop_reason: StopReason,
    pub response_id: String,
    pub model_name: String,
}

/// Test ApiProvider that replays scripted turns from a queue.
pub struct TestProvider {
    pub turns: Mutex<Vec<ScriptedTurn>>,
    pub stream_options: Mutex<Vec<Option<StreamOptions>>>,
}

impl TestProvider {
    pub fn new(turns: Vec<ScriptedTurn>) -> Self {
        Self {
            turns: Mutex::new(turns),
            stream_options: Mutex::new(Vec::new()),
        }
    }
}

pub struct ProviderGuard {
    ai_client: AiClient,
}

impl ProviderGuard {
    pub fn register(api: impl Into<String>, provider: Arc<dyn ApiProvider>) -> Self {
        Self::register_many(vec![(api.into(), provider)])
    }

    pub fn register_many(providers: Vec<(String, Arc<dyn ApiProvider>)>) -> Self {
        let ai_client = AiClient::new();
        for (api, provider) in providers {
            ai_client.register_provider(api, provider);
        }
        Self { ai_client }
    }

    pub fn provider_streamer(&self) -> ProviderStreamer {
        let ai_client = self.ai_client.clone();
        Arc::new(move |model, context, options| ai_client.stream_model(model, context, options))
    }

    pub fn ai_client(&self) -> AiClient {
        self.ai_client.clone()
    }

    pub fn agent_config(&self, model: Model) -> AgentConfig {
        let mut config = AgentConfig::new(model);
        self.install(&mut config);
        config
    }

    pub fn install(&self, config: &mut AgentConfig) {
        config.provider_streamer = Some(self.provider_streamer());
    }
}

pub fn agent_config(model: Model) -> AgentConfig {
    AgentConfig::new(model)
}

impl ApiProvider for TestProvider {
    fn stream(&self, _model: &Model, _ctx: Context, opts: Option<StreamOptions>) -> EventStream {
        self.stream_options.lock().unwrap().push(opts);
        let turn = {
            let mut turns = self.turns.lock().unwrap();
            if turns.is_empty() {
                return Box::pin(stream! {
                    let mut msg = AssistantMessage::empty("test", "test-model");
                    msg.error_message = Some("no more scripted turns".into());
                    msg.stop_reason = StopReason::Error;
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        message: msg,
                    };
                });
            }
            turns.remove(0)
        };

        let content = turn
            .events
            .last()
            .map(|e| match e {
                AssistantMessageEvent::Start { partial, .. } => partial.content.clone(),
                AssistantMessageEvent::TextStart { partial, .. } => partial.content.clone(),
                AssistantMessageEvent::TextDelta { partial, .. } => partial.content.clone(),
                AssistantMessageEvent::TextEnd { partial, .. } => partial.content.clone(),
                AssistantMessageEvent::ThinkingStart { partial, .. } => partial.content.clone(),
                AssistantMessageEvent::ThinkingDelta { partial, .. } => partial.content.clone(),
                AssistantMessageEvent::ThinkingEnd { partial, .. } => partial.content.clone(),
                AssistantMessageEvent::ToolcallStart { partial, .. } => partial.content.clone(),
                AssistantMessageEvent::ToolcallDelta { partial, .. } => partial.content.clone(),
                AssistantMessageEvent::ToolcallEnd { partial, .. } => partial.content.clone(),
                AssistantMessageEvent::Done { message, .. } => message.content.clone(),
                AssistantMessageEvent::Error { .. } => vec![],
            })
            .unwrap_or_default();

        Box::pin(stream! {
            for event in &turn.events {
                yield event.clone();
            }

            let msg = AssistantMessage {
                content,
                api: "test".into(),
                provider: Some("test".into()),
                model: turn.model_name.clone(),
                response_model: None,
                response_id: Some(turn.response_id.clone()),
                usage: Default::default(),
                stop_reason: turn.stop_reason.clone(),
                error_message: None,
                diagnostics: None,
                timestamp: 0,
            };
            yield AssistantMessageEvent::Done {
                reason: turn.stop_reason,
                message: msg,
            };
        })
    }
}

/// Helper: create a simple text-response turn.
pub fn text_turn(text: &str) -> ScriptedTurn {
    let text_block = ContentBlock::Text {
        text: text.into(),
        text_signature: None,
    };
    let mut msg = AssistantMessage::empty("test", "test-model");
    msg.content.push(text_block.clone());
    let partial = msg.clone();

    ScriptedTurn {
        events: vec![
            AssistantMessageEvent::Start {
                content_index: None,
                partial: partial.clone(),
            },
            AssistantMessageEvent::TextStart {
                content_index: 0,
                partial: {
                    let mut p = partial.clone();
                    p.content = vec![text_block.clone()];
                    p
                },
            },
            AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: text.into(),
                partial: partial.clone(),
            },
            AssistantMessageEvent::TextEnd {
                content_index: 0,
                partial: partial.clone(),
            },
        ],
        stop_reason: StopReason::Stop,
        response_id: "resp_1".into(),
        model_name: "test-model".into(),
    }
}

/// Helper: create a tool-use turn.
pub fn tool_use_turn(tool_id: &str, tool_name: &str, arguments: serde_json::Value) -> ScriptedTurn {
    let tool_block = ContentBlock::ToolCall {
        id: tool_id.into(),
        name: tool_name.into(),
        arguments: arguments.clone(),
        thought_signature: None,
    };
    let mut msg = AssistantMessage::empty("test", "test-model");
    msg.content.push(tool_block.clone());
    let partial = msg.clone();

    let json_str = arguments.to_string();

    ScriptedTurn {
        events: vec![
            AssistantMessageEvent::Start {
                content_index: None,
                partial: partial.clone(),
            },
            AssistantMessageEvent::ToolcallStart {
                content_index: 0,
                partial: {
                    let mut p = partial.clone();
                    p.content = vec![ContentBlock::ToolCall {
                        id: tool_id.into(),
                        name: tool_name.into(),
                        arguments: serde_json::json!({}),
                        thought_signature: None,
                    }];
                    p
                },
            },
            AssistantMessageEvent::ToolcallDelta {
                content_index: 0,
                delta: json_str,
                partial: partial.clone(),
            },
            AssistantMessageEvent::ToolcallEnd {
                content_index: 0,
                partial: partial.clone(),
            },
        ],
        stop_reason: StopReason::ToolUse,
        response_id: "resp_tool".into(),
        model_name: "test-model".into(),
    }
}

pub fn faux_model(api: &str) -> Model {
    faux_model_with_window(api, 0)
}

/// Like [`faux_model`] but with an explicit `context_window`. The default
/// [`faux_model`] keeps `context_window: 0` (never auto-compacts under the
/// context-window-gated trigger); tests that exercise auto compaction should
/// pick a window appropriate to their scenario.
pub fn faux_model_with_window(api: &str, context_window: u32) -> Model {
    Model {
        id: "faux-model".into(),
        name: "Faux Model".into(),
        api: api.into(),
        provider: "faux".into(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ModelInput::Text],
        cost: ModelCost {
            known: true,
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window,
        max_tokens: 0,
        headers: None,
        compat: None,
    }
}

pub fn faux_text_turn(text: &str) -> FauxResponse {
    FauxResponse {
        text_deltas: vec![text.to_string()],
        thinking_deltas: vec![],
        tool_calls: vec![],
    }
}
