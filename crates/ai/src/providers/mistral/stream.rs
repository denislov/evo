use super::wire;
use crate::model::Model;
use crate::model::calculate_cost;
use crate::protocol::stream::EventStream;
use crate::protocol::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Cost, StopReason, Usage,
};
use crate::providers::common::{
    ProviderTerminalLatch, SseEventHandler, SseEventResult, SseTransportTerminal,
    ToolArgumentAssembler, process_sse, start_once,
};
use bytes::Bytes;
use futures::Stream;
use std::collections::HashMap;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenBlock {
    Text(u32),
    Thinking(u32),
}

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
        MistralHandler::default(),
        "mistral-conversations",
    )
}

#[derive(Default)]
struct MistralHandler {
    first_event: bool,
    current_block: Option<OpenBlock>,
    tool_index_map: HashMap<u32, u32>,
    tool_args: ToolArgumentAssembler,
    finish_reason: Option<String>,
    terminal: ProviderTerminalLatch,
}

impl SseEventHandler for MistralHandler {
    fn handle_event(
        &mut self,
        data: &str,
        partial: &mut AssistantMessage,
        model: &Model,
    ) -> Result<SseEventResult, String> {
        let chunk: wire::ChatCompletionChunk =
            serde_json::from_str(data).map_err(|e| format!("SSE parse error: {}", e))?;

        let mut events = Vec::new();

        if let Some(event) = start_once(
            &mut self.first_event,
            partial,
            chunk.id.clone(),
            chunk.model.clone(),
        ) {
            events.push(event);
        }

        for choice in chunk.choices {
            if choice.index != 0 {
                return Err(format!(
                    "unsupported Mistral completion choice index {}",
                    choice.index
                ));
            }
            if let Some(reason) = choice.finish_reason.as_deref()
                && !reason.is_empty()
                && reason != "null"
            {
                self.finish_reason = choice.finish_reason.clone();
                self.terminal.observe();
            }

            if let Some(content) = choice.delta.content {
                match content {
                    wire::ContentDelta::Text(text) => {
                        if !text.is_empty() {
                            events.extend(emit_text_delta(partial, &mut self.current_block, text));
                        }
                    }
                    wire::ContentDelta::Parts(parts) => {
                        for part in parts {
                            match part {
                                wire::ContentDeltaPart::Text { text } => {
                                    if !text.is_empty() {
                                        events.extend(emit_text_delta(
                                            partial,
                                            &mut self.current_block,
                                            text,
                                        ));
                                    }
                                }
                                wire::ContentDeltaPart::Thinking { thinking } => {
                                    let text = thinking
                                        .into_iter()
                                        .map(|part| match part {
                                            wire::ThinkingDeltaPart::Text { text } => text,
                                        })
                                        .collect::<Vec<_>>()
                                        .join("");
                                    if !text.is_empty() {
                                        events.extend(emit_thinking_delta(
                                            partial,
                                            &mut self.current_block,
                                            text,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let Some(tool_calls) = choice.delta.tool_calls {
                if let Some(event) = finish_current_block(partial, &mut self.current_block) {
                    events.push(event);
                }
                for tool_call in tool_calls {
                    if tool_call
                        .tool_type
                        .as_deref()
                        .is_some_and(|kind| kind != "function")
                    {
                        return Err(format!(
                            "unsupported Mistral tool call type {}",
                            tool_call.tool_type.as_deref().unwrap_or_default()
                        ));
                    }
                    let provider_index = tool_call.index.unwrap_or(0);
                    let content_pos = match self.tool_index_map.get(&provider_index) {
                        Some(&pos) => pos,
                        None => {
                            let pos = partial.content.len() as u32;
                            self.tool_index_map.insert(provider_index, pos);
                            partial.content.push(ContentBlock::ToolCall {
                                id: tool_call.id.clone().unwrap_or_default(),
                                name: String::new(),
                                arguments: serde_json::json!({}),
                                kind: Default::default(),
                                thought_signature: None,
                            });
                            events.push(AssistantMessageEvent::ToolcallStart {
                                content_index: pos,
                                partial: partial.clone(),
                            });
                            pos
                        }
                    };

                    if let Some(id) = tool_call.id
                        && let Some(ContentBlock::ToolCall { id: block_id, .. }) =
                            partial.content.get_mut(content_pos as usize)
                    {
                        *block_id = id;
                    }

                    if let Some(function) = tool_call.function {
                        if let Some(name) = function.name
                            && let Some(ContentBlock::ToolCall {
                                name: block_name, ..
                            }) = partial.content.get_mut(content_pos as usize)
                        {
                            *block_name = name;
                        }
                        if let Some(args) = function.arguments {
                            let parsed = self.tool_args.append(provider_index, &args);
                            if let Some(ContentBlock::ToolCall { arguments, .. }) =
                                partial.content.get_mut(content_pos as usize)
                            {
                                *arguments = parsed;
                            }
                            events.push(AssistantMessageEvent::ToolcallDelta {
                                content_index: content_pos,
                                delta: args,
                                partial: partial.clone(),
                            });
                        }
                    }
                }
            }
        }

        if let Some(usage) = &chunk.usage {
            partial.usage = map_usage(usage, model);
        }

        Ok(SseEventResult::Continue(events))
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

        for (provider_index, content_pos) in &self.tool_index_map {
            let parsed = self
                .tool_args
                .finish(*provider_index)
                .map_err(|_| "malformed final tool arguments".to_string())?;
            if let Some(ContentBlock::ToolCall { arguments, .. }) =
                partial.content.get_mut(*content_pos as usize)
            {
                *arguments = parsed;
            }
            events.push(AssistantMessageEvent::ToolcallEnd {
                content_index: *content_pos,
                partial: partial.clone(),
            });
        }

        partial.stop_reason = map_finish_reason(self.finish_reason.as_deref());

        Ok(events)
    }

    fn accept_transport_terminal(&self, terminal: SseTransportTerminal) -> Result<(), String> {
        self.terminal.accept(terminal)
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

fn map_finish_reason(reason: Option<&str>) -> StopReason {
    match reason {
        Some("length") | Some("model_length") => StopReason::Length,
        Some("tool_calls") => StopReason::ToolUse,
        Some("error") => StopReason::Error,
        Some("stop") | None => StopReason::Stop,
        _ => StopReason::Stop,
    }
}

fn map_usage(usage: &wire::ChatUsage, model: &Model) -> Usage {
    let mut result = Usage {
        input: usage.prompt_tokens,
        output: usage.completion_tokens,
        reasoning_tokens: 0,
        cache_read: 0,
        cache_write: 0,
        total_tokens: if usage.total_tokens == 0 {
            crate::protocol::usage::saturating_token_total(
                usage.prompt_tokens,
                usage.completion_tokens,
            )
        } else {
            usage.total_tokens
        },
        cost: Cost::default(),
    };
    calculate_cost(model, &mut result);
    result
}
