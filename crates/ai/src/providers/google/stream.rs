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
                .unwrap()
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
                        let content_index = partial.content.len() as u32;
                        partial.content.push(ContentBlock::ToolCall {
                            id: fc.name.clone(),
                            name: fc.name.clone(),
                            arguments: fc.args.clone(),
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
                        let content_index = partial.content.len() as u32;
                        if is_thought {
                            partial.content.push(ContentBlock::Thinking {
                                thinking: text.clone(),
                                thinking_signature: None,
                                redacted: None,
                            });
                            events.push(AssistantMessageEvent::ThinkingStart {
                                content_index,
                                partial: partial.clone(),
                            });
                            events.push(AssistantMessageEvent::ThinkingDelta {
                                content_index,
                                delta: text.clone(),
                                partial: partial.clone(),
                            });
                            events.push(AssistantMessageEvent::ThinkingEnd {
                                content_index,
                                partial: partial.clone(),
                            });
                        } else {
                            partial.content.push(ContentBlock::Text {
                                text: text.clone(),
                                text_signature: None,
                            });
                            events.push(AssistantMessageEvent::TextStart {
                                content_index,
                                partial: partial.clone(),
                            });
                            events.push(AssistantMessageEvent::TextDelta {
                                content_index,
                                delta: text.clone(),
                                partial: partial.clone(),
                            });
                            events.push(AssistantMessageEvent::TextEnd {
                                content_index,
                                partial: partial.clone(),
                            });
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
        let has_tool_calls = partial
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolCall { .. }));
        if has_tool_calls {
            partial.stop_reason = StopReason::ToolUse;
        }
        Ok(Vec::new())
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
    let mut result = Usage {
        input: usage.prompt_token_count,
        output: usage.candidates_token_count,
        cache_read: 0,
        cache_write: 0,
        total_tokens: usage.total_token_count,
        cost: Cost::default(),
    };
    calculate_cost(model, &mut result);
    result
}
