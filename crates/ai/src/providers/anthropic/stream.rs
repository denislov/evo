use super::convert::map_stop_reason;
use super::wire;
use crate::model::Model;
use crate::model::calculate_cost;
use crate::protocol::json::parse_streaming_json;
use crate::protocol::stream::EventStream;
use crate::protocol::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Cost, StopReason, Usage,
};
use crate::providers::common::{
    SseEventHandler, SseEventResult, parse_terminal_tool_arguments, process_sse,
};
use bytes::Bytes;
use futures::Stream;
use tokio_util::sync::CancellationToken;

pub fn process<E>(
    body: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
    model: Model,
    cancel: Option<CancellationToken>,
) -> EventStream
where
    E: std::fmt::Display + Send + 'static,
{
    process_sse(
        body,
        model,
        cancel,
        AnthropicHandler::default(),
        "anthropic-messages",
    )
}

#[derive(Default)]
struct AnthropicHandler {
    first_event: bool,
    block_type: Option<String>,
    block_index: u32,
    accumulated_text: String,
    accumulated_thinking: String,
    accumulated_tool_args: String,
    pending_text_signature: Option<String>,
    pending_thinking_signature: Option<String>,
    pending_thought_signature: Option<String>,
    message_usage: wire::MessageUsage,
    stop_reason: Option<StopReason>,
}

impl SseEventHandler for AnthropicHandler {
    fn handle_event(
        &mut self,
        data: &str,
        partial: &mut AssistantMessage,
        model: &Model,
    ) -> Result<SseEventResult, String> {
        let wire_event: wire::StreamEvent =
            serde_json::from_str(data).map_err(|e| format!("SSE parse error: {}", e))?;

        let mut events = Vec::new();

        match wire_event {
            wire::StreamEvent::MessageStart { message } => {
                partial.response_id = Some(message.id);
                partial.response_model = Some(message.model);
                self.message_usage = message.usage;
                partial.timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if !self.first_event {
                    events.push(AssistantMessageEvent::Start {
                        content_index: None,
                        partial: partial.clone(),
                    });
                    self.first_event = true;
                }
            }

            wire::StreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                self.block_index = index;
                match content_block {
                    wire::ContentBlockStart::Text { text } => {
                        self.block_type = Some("text".into());
                        self.accumulated_text = text.clone();
                        partial.content.push(ContentBlock::Text {
                            text: text.clone(),
                            text_signature: None,
                        });
                        events.push(AssistantMessageEvent::TextStart {
                            content_index: index,
                            partial: partial.clone(),
                        });
                        if !self.accumulated_text.is_empty() {
                            events.push(AssistantMessageEvent::TextDelta {
                                content_index: index,
                                delta: text,
                                partial: partial.clone(),
                            });
                        }
                    }
                    wire::ContentBlockStart::Thinking { thinking } => {
                        self.block_type = Some("thinking".into());
                        self.accumulated_thinking = thinking.clone();
                        partial.content.push(ContentBlock::Thinking {
                            thinking: thinking.clone(),
                            thinking_signature: None,
                            provider_metadata: None,
                            redacted: None,
                        });
                        events.push(AssistantMessageEvent::ThinkingStart {
                            content_index: index,
                            partial: partial.clone(),
                        });
                        if !self.accumulated_thinking.is_empty() {
                            events.push(AssistantMessageEvent::ThinkingDelta {
                                content_index: index,
                                delta: thinking,
                                partial: partial.clone(),
                            });
                        }
                    }
                    wire::ContentBlockStart::RedactedThinking { .. } => {
                        self.block_type = Some("thinking".into());
                        partial.content.push(ContentBlock::Thinking {
                            thinking: String::new(),
                            thinking_signature: None,
                            provider_metadata: None,
                            redacted: Some(true),
                        });
                        events.push(AssistantMessageEvent::ThinkingStart {
                            content_index: index,
                            partial: partial.clone(),
                        });
                    }
                    wire::ContentBlockStart::ToolUse { id, name } => {
                        self.block_type = Some("tool_use".into());
                        self.accumulated_tool_args.clear();
                        partial.content.push(ContentBlock::ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: serde_json::json!({}),
                            kind: Default::default(),
                            thought_signature: None,
                        });
                        events.push(AssistantMessageEvent::ToolcallStart {
                            content_index: index,
                            partial: partial.clone(),
                        });
                    }
                }
            }

            wire::StreamEvent::ContentBlockDelta { index, delta } => {
                if index != self.block_index {
                    return Err(format!(
                        "unsupported interleaved Anthropic content block index {index}; active block is {}",
                        self.block_index
                    ));
                }
                match delta {
                    wire::ContentBlockDelta::TextDelta { text } => {
                        if let Some(ContentBlock::Text { text: t, .. }) =
                            partial.content.get_mut(self.block_index as usize)
                        {
                            t.push_str(&text);
                        }
                        self.accumulated_text.push_str(&text);
                        events.push(AssistantMessageEvent::TextDelta {
                            content_index: self.block_index,
                            delta: text,
                            partial: partial.clone(),
                        });
                    }
                    wire::ContentBlockDelta::ThinkingDelta { thinking } => {
                        if let Some(ContentBlock::Thinking { thinking: t, .. }) =
                            partial.content.get_mut(self.block_index as usize)
                        {
                            t.push_str(&thinking);
                        }
                        self.accumulated_thinking.push_str(&thinking);
                        events.push(AssistantMessageEvent::ThinkingDelta {
                            content_index: self.block_index,
                            delta: thinking,
                            partial: partial.clone(),
                        });
                    }
                    wire::ContentBlockDelta::SignatureDelta { signature } => {
                        match self.block_type.as_deref() {
                            Some("thinking") => self.pending_thinking_signature = Some(signature),
                            Some("tool_use") => self.pending_thought_signature = Some(signature),
                            _ => self.pending_text_signature = Some(signature),
                        }
                    }
                    wire::ContentBlockDelta::InputJsonDelta { partial_json } => {
                        self.accumulated_tool_args.push_str(&partial_json);
                        let parsed = parse_streaming_json(&self.accumulated_tool_args);
                        if let Some(ContentBlock::ToolCall { arguments, .. }) =
                            partial.content.get_mut(self.block_index as usize)
                        {
                            *arguments = parsed;
                        }
                        events.push(AssistantMessageEvent::ToolcallDelta {
                            content_index: self.block_index,
                            delta: partial_json,
                            partial: partial.clone(),
                        });
                    }
                }
            }

            wire::StreamEvent::ContentBlockStop { index } => {
                if index != self.block_index {
                    return Err(format!(
                        "Anthropic content block stop index {index} does not match active block {}",
                        self.block_index
                    ));
                }
                match self.block_type.as_deref() {
                    Some("text") => {
                        if let Some(ContentBlock::Text { text_signature, .. }) =
                            partial.content.get_mut(self.block_index as usize)
                        {
                            *text_signature = self.pending_text_signature.take();
                        }
                        events.push(AssistantMessageEvent::TextEnd {
                            content_index: self.block_index,
                            partial: partial.clone(),
                        });
                    }
                    Some("thinking") => {
                        if let Some(ContentBlock::Thinking {
                            thinking_signature, ..
                        }) = partial.content.get_mut(self.block_index as usize)
                        {
                            *thinking_signature = self.pending_thinking_signature.take();
                        }
                        events.push(AssistantMessageEvent::ThinkingEnd {
                            content_index: self.block_index,
                            partial: partial.clone(),
                        });
                    }
                    Some("tool_use") => {
                        if let Some(ContentBlock::ToolCall {
                            arguments,
                            thought_signature,
                            ..
                        }) = partial.content.get_mut(self.block_index as usize)
                        {
                            *arguments =
                                parse_terminal_tool_arguments(&self.accumulated_tool_args)?;
                            *thought_signature = self.pending_thought_signature.take();
                        }
                        events.push(AssistantMessageEvent::ToolcallEnd {
                            content_index: self.block_index,
                            partial: partial.clone(),
                        });
                    }
                    _ => {}
                }
                self.block_type = None;
            }

            wire::StreamEvent::MessageDelta { delta, usage } => {
                self.stop_reason = delta.stop_reason.as_deref().map(map_stop_reason);
                self.message_usage.output_tokens = usage.output_tokens;
                if let Some(cache_read) = usage.cache_read_input_tokens {
                    self.message_usage.cache_read_input_tokens = Some(cache_read);
                }
                if let Some(cache_write) = usage.cache_creation_input_tokens {
                    self.message_usage.cache_creation_input_tokens = Some(cache_write);
                }
            }

            wire::StreamEvent::MessageStop => {
                let mut usage = Usage {
                    input: self.message_usage.input_tokens,
                    output: self.message_usage.output_tokens,
                    reasoning_tokens: 0,
                    cache_read: self.message_usage.cache_read_input_tokens.unwrap_or(0),
                    cache_write: self.message_usage.cache_creation_input_tokens.unwrap_or(0),
                    total_tokens: crate::protocol::usage::saturating_token_total(
                        self.message_usage.input_tokens,
                        self.message_usage.output_tokens,
                    ),
                    cost: Cost::default(),
                };
                calculate_cost(model, &mut usage);

                partial.usage = usage;
                partial.stop_reason = self.stop_reason.clone().unwrap_or(StopReason::Stop);
                partial.provider = Some("anthropic".into());

                return Ok(SseEventResult::ProviderDone(events));
            }

            wire::StreamEvent::Ping => {}
        }

        Ok(SseEventResult::Continue(events))
    }

    fn finish(
        &mut self,
        _partial: &mut AssistantMessage,
        _model: &Model,
    ) -> Result<Vec<AssistantMessageEvent>, String> {
        Ok(Vec::new())
    }
}
