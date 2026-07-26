use crate::model::Model;
use crate::protocol::stream::EventStream;
use crate::protocol::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, StopReason, StreamOptions,
    Usage,
};
use crate::registry::ApiProvider;
use async_stream::stream;
use std::sync::Mutex;

pub struct FauxProvider {
    pub responses: Mutex<FauxState>,
    /// Optional usage reported by every streamed call, overriding the
    /// built-in default (`input: 10, output: 20, total_tokens: 30`). Lets
    /// tests simulate a provider reporting a large accumulated context so
    /// context-window-gated compaction logic can be exercised.
    pub default_usage: Option<Usage>,
}

pub struct FauxState {
    /// Queue of per-call responses. Each call to stream() pops the first entry.
    pub call_queue: Vec<FauxCall>,
    /// Default responses used when call_queue is empty (backward compat).
    pub default_responses: Vec<FauxResponse>,
}

#[derive(Clone)]
pub struct FauxCall {
    pub responses: Vec<FauxResponse>,
    pub stop_reason: StopReason,
}

#[derive(Clone)]
pub struct FauxResponse {
    pub text_deltas: Vec<String>,
    pub thinking_deltas: Vec<String>,
    pub tool_calls: Vec<FauxToolCall>,
}

#[derive(Clone)]
pub struct FauxToolCall {
    pub id: String,
    pub name: String,
    pub deltas: Vec<String>,
    pub final_arguments: serde_json::Value,
}

impl FauxProvider {
    pub fn new(responses: Vec<FauxResponse>) -> Self {
        Self {
            responses: Mutex::new(FauxState {
                call_queue: vec![],
                default_responses: responses,
            }),
            default_usage: None,
        }
    }

    /// Create a provider with a queue of per-call responses.
    /// Each stream() call pops the next FauxCall and replays it.
    pub fn with_call_queue(calls: Vec<FauxCall>) -> Self {
        Self {
            responses: Mutex::new(FauxState {
                call_queue: calls,
                default_responses: vec![],
            }),
            default_usage: None,
        }
    }

    pub fn simple_text(text: &str) -> Self {
        Self::new(vec![FauxResponse {
            text_deltas: vec![text.to_string()],
            thinking_deltas: vec![],
            tool_calls: vec![],
        }])
    }

    /// Create a single faux call with the given responses and stop reason.
    pub fn single_call(responses: Vec<FauxResponse>, stop_reason: StopReason) -> FauxCall {
        FauxCall {
            responses,
            stop_reason,
        }
    }

    /// Create a text-only faux call with the given stop reason.
    pub fn text_call(text: &str, stop_reason: StopReason) -> FauxCall {
        FauxCall {
            responses: vec![FauxResponse {
                text_deltas: vec![text.to_string()],
                thinking_deltas: vec![],
                tool_calls: vec![],
            }],
            stop_reason,
        }
    }

    /// Override the usage reported by every streamed call. Returns `self` for
    /// chaining. Useful for simulating a provider that reports a large
    /// accumulated context so context-window-gated compaction can be tested.
    pub fn with_default_usage(mut self, usage: Usage) -> Self {
        self.default_usage = Some(usage);
        self
    }
}

impl ApiProvider for FauxProvider {
    fn stream(&self, model: &Model, _ctx: Context, _opts: Option<StreamOptions>) -> EventStream {
        let (responses, stop_reason) = {
            let mut state = self.responses.lock().unwrap();
            if let Some(call) = state.call_queue.first().cloned() {
                state.call_queue.remove(0);
                (call.responses, call.stop_reason)
            } else {
                (state.default_responses.clone(), StopReason::Stop)
            }
        };
        let model_id = model.id.clone();
        let usage = self.default_usage.clone().unwrap_or(Usage {
            input: 10,
            output: 20,
            total_tokens: 30,
            ..Default::default()
        });
        Box::pin(stream! {
            let mut partial = AssistantMessage::empty("faux", &model_id);
            partial.provider = Some("faux".into());
            partial.timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            yield AssistantMessageEvent::Start { content_index: None, partial: partial.clone() };

            for resp in &responses {
                if !resp.text_deltas.is_empty() {
                    partial.content.push(ContentBlock::Text {
                        text: String::new(),
                        text_signature: None,
                    });
                    yield AssistantMessageEvent::TextStart { content_index: 0, partial: partial.clone() };
                    for delta in &resp.text_deltas {
                        if let Some(ContentBlock::Text { text, .. }) = partial.content.last_mut() {
                            text.push_str(delta);
                        }
                        yield AssistantMessageEvent::TextDelta {
                            content_index: 0,
                            delta: delta.clone(),
                            partial: partial.clone(),
                        };
                    }
                    yield AssistantMessageEvent::TextEnd { content_index: 0, partial: partial.clone() };
                }

                if !resp.thinking_deltas.is_empty() {
                    partial.content.push(ContentBlock::Thinking {
                        thinking: String::new(),
                        thinking_signature: None,
                        redacted: None,
                    });
                    yield AssistantMessageEvent::ThinkingStart { content_index: 0, partial: partial.clone() };
                    for delta in &resp.thinking_deltas {
                        if let Some(ContentBlock::Thinking { thinking, .. }) = partial.content.last_mut() {
                            thinking.push_str(delta);
                        }
                        yield AssistantMessageEvent::ThinkingDelta {
                            content_index: 0,
                            delta: delta.clone(),
                            partial: partial.clone(),
                        };
                    }
                    yield AssistantMessageEvent::ThinkingEnd { content_index: 0, partial: partial.clone() };
                }

                for tc in &resp.tool_calls {
                    partial.content.push(ContentBlock::ToolCall {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        arguments: tc.final_arguments.clone(),
                        thought_signature: None,
                    });
                    yield AssistantMessageEvent::ToolcallStart { content_index: 0, partial: partial.clone() };
                    let mut accumulated = String::new();
                    for delta in &tc.deltas {
                        accumulated.push_str(delta);
                        if let Some(ContentBlock::ToolCall { arguments, .. }) = partial.content.last_mut() {
                            *arguments = serde_json::json!(&accumulated);
                        }
                        yield AssistantMessageEvent::ToolcallDelta {
                            content_index: 0,
                            delta: delta.to_string(),
                            partial: partial.clone(),
                        };
                    }
                    yield AssistantMessageEvent::ToolcallEnd { content_index: 0, partial: partial.clone() };
                }
            }

            partial.usage = usage;
            partial.stop_reason = stop_reason.clone();

            yield AssistantMessageEvent::Done {
                reason: stop_reason,
                message: partial,
            };
        })
    }
}
