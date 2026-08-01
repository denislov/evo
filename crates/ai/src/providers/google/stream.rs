use super::wire;
use crate::model::Model;
use crate::model::calculate_cost;
use crate::protocol::stream::EventStream;
use crate::protocol::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Cost, StopReason, Usage,
};
use crate::providers::common::{SseEventHandler, SseEventResult, process_sse};
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
        GoogleHandler::default(),
        "google-generative-ai",
    )
}

#[derive(Default)]
struct GoogleHandler {
    first_event: bool,
    current_block: Option<OpenBlock>,
    tool_serial: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenBlock {
    Text(u32),
    Thinking(u32),
}

impl SseEventHandler for GoogleHandler {
    fn handle_event(
        &mut self,
        data: &str,
        partial: &mut AssistantMessage,
        model: &Model,
    ) -> Result<SseEventResult, String> {
        let response: wire::GenerateContentResponse =
            serde_json::from_str(data).map_err(|e| format!("SSE parse error: {}", e))?;

        let mut events = Vec::new();
        let mut terminal_observed = false;

        if !self.first_event {
            partial.timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            events.push(AssistantMessageEvent::Start {
                content_index: None,
                partial: partial.clone(),
            });
            self.first_event = true;
        }

        for candidate in &response.candidates {
            if let Some(fr) = &candidate.finish_reason
                && !fr.is_empty()
            {
                partial.stop_reason = map_finish_reason(fr);
                terminal_observed = true;
            }

            if let Some(content) = &candidate.content {
                for part in &content.parts {
                    if let Some(fc) = &part.function_call {
                        if let Some(event) = finish_current_block(partial, &mut self.current_block)
                        {
                            events.push(event);
                        }
                        self.tool_serial = self.tool_serial.saturating_add(1);
                        let content_index = partial.content.len() as u32;
                        partial.content.push(ContentBlock::ToolCall {
                            id: format!("{}-{}", fc.name, self.tool_serial),
                            name: fc.name.clone(),
                            arguments: fc.args.clone(),
                            kind: Default::default(),
                            thought_signature: None,
                        });
                        events.push(AssistantMessageEvent::ToolcallStart {
                            content_index,
                            partial: partial.clone(),
                        });
                        events.push(AssistantMessageEvent::ToolcallDelta {
                            content_index,
                            delta: fc.args.to_string(),
                            partial: partial.clone(),
                        });
                        events.push(AssistantMessageEvent::ToolcallEnd {
                            content_index,
                            partial: partial.clone(),
                        });
                    }

                    if let Some(text) = &part.text {
                        let is_thought = part.thought.unwrap_or(false);
                        if is_thought {
                            events.extend(emit_thinking_delta(
                                partial,
                                &mut self.current_block,
                                text.clone(),
                            ));
                        } else {
                            events.extend(emit_text_delta(
                                partial,
                                &mut self.current_block,
                                text.clone(),
                            ));
                        }
                    }
                }
            }
        }

        if let Some(usage) = &response.usage_metadata {
            partial.usage = map_usage(usage, model);
        }

        if terminal_observed {
            Ok(SseEventResult::ProviderDone(events))
        } else {
            Ok(SseEventResult::Continue(events))
        }
    }

    fn finish(
        &mut self,
        partial: &mut AssistantMessage,
        _model: &Model,
    ) -> Result<Vec<AssistantMessageEvent>, String> {
        let mut events = Vec::new();
        if let Some(event) = finish_current_block(partial, &mut self.current_block) {
            events.push(event);
        }
        let has_tool_calls = partial
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolCall { .. }));
        if has_tool_calls {
            partial.stop_reason = StopReason::ToolUse;
        }
        Ok(events)
    }
}

fn emit_text_delta(
    partial: &mut AssistantMessage,
    current_block: &mut Option<OpenBlock>,
    text: String,
) -> Vec<AssistantMessageEvent> {
    let mut events = Vec::new();
    if !matches!(current_block, Some(OpenBlock::Text(_))) {
        if let Some(event) = finish_current_block(partial, current_block) {
            events.push(event);
        }
        let content_index = partial.content.len() as u32;
        partial.content.push(ContentBlock::Text {
            text: String::new(),
            text_signature: None,
        });
        *current_block = Some(OpenBlock::Text(content_index));
        events.push(AssistantMessageEvent::TextStart {
            content_index,
            partial: partial.clone(),
        });
    }

    let content_index = match current_block {
        Some(OpenBlock::Text(index)) => *index,
        _ => unreachable!(),
    };
    if let Some(ContentBlock::Text {
        text: block_text, ..
    }) = partial.content.get_mut(content_index as usize)
    {
        block_text.push_str(&text);
    }
    events.push(AssistantMessageEvent::TextDelta {
        content_index,
        delta: text,
        partial: partial.clone(),
    });
    events
}

fn emit_thinking_delta(
    partial: &mut AssistantMessage,
    current_block: &mut Option<OpenBlock>,
    text: String,
) -> Vec<AssistantMessageEvent> {
    let mut events = Vec::new();
    if !matches!(current_block, Some(OpenBlock::Thinking(_))) {
        if let Some(event) = finish_current_block(partial, current_block) {
            events.push(event);
        }
        let content_index = partial.content.len() as u32;
        partial.content.push(ContentBlock::Thinking {
            thinking: String::new(),
            thinking_signature: None,
            provider_metadata: None,
            redacted: None,
        });
        *current_block = Some(OpenBlock::Thinking(content_index));
        events.push(AssistantMessageEvent::ThinkingStart {
            content_index,
            partial: partial.clone(),
        });
    }

    let content_index = match current_block {
        Some(OpenBlock::Thinking(index)) => *index,
        _ => unreachable!(),
    };
    if let Some(ContentBlock::Thinking { thinking, .. }) =
        partial.content.get_mut(content_index as usize)
    {
        thinking.push_str(&text);
    }
    events.push(AssistantMessageEvent::ThinkingDelta {
        content_index,
        delta: text,
        partial: partial.clone(),
    });
    events
}

fn finish_current_block(
    partial: &AssistantMessage,
    current_block: &mut Option<OpenBlock>,
) -> Option<AssistantMessageEvent> {
    match current_block.take() {
        Some(OpenBlock::Text(content_index)) => Some(AssistantMessageEvent::TextEnd {
            content_index,
            partial: partial.clone(),
        }),
        Some(OpenBlock::Thinking(content_index)) => Some(AssistantMessageEvent::ThinkingEnd {
            content_index,
            partial: partial.clone(),
        }),
        None => None,
    }
}

fn map_finish_reason(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::Stop,
        "MAX_TOKENS" => StopReason::Length,
        "SAFETY" | "RECITATION" | "OTHER" => StopReason::Error,
        _ => StopReason::Stop,
    }
}

fn map_usage(usage: &wire::UsageMetadata, model: &Model) -> Usage {
    let cache_tokens = usage.cached_content_token_count;
    // `promptTokenCount` is the total prompt size including cache hits; keep
    // `input` exclusive of them so the cached portion is not double-billed.
    let non_cached_input = usage.prompt_token_count.saturating_sub(cache_tokens);

    let mut result = Usage {
        input: non_cached_input,
        output: usage.candidates_token_count,
        reasoning_tokens: 0,
        cache_read: cache_tokens,
        cache_write: 0,
        total_tokens: usage.total_token_count,
        cost: Cost::default(),
    };
    calculate_cost(model, &mut result);
    result
}
