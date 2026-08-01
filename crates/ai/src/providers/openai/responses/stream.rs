use std::collections::HashMap;

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
    process_with_api_name(body, model, cancel, "openai-responses")
}

pub fn process_with_api_name<E>(
    body: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
    model: Model,
    cancel: Option<CancellationToken>,
    api_name: &str,
) -> EventStream
where
    E: std::fmt::Display + Send + 'static,
{
    process_sse(body, model, cancel, ResponsesHandler::default(), api_name)
}

#[derive(Debug)]
enum OutputKind {
    Text,
    Tool { arguments: String },
}

#[derive(Debug)]
struct OutputState {
    content_index: u32,
    kind: OutputKind,
    ended: bool,
}

#[derive(Default)]
struct ResponsesHandler {
    started: bool,
    response_id: Option<String>,
    usage: Option<wire::ResponseUsage>,
    outputs: HashMap<String, OutputState>,
    output_order: Vec<String>,
    last_text_output: Option<String>,
    last_tool_output: Option<String>,
    synthetic_output_id: u64,
}

impl ResponsesHandler {
    fn next_synthetic_id(&mut self, prefix: &str) -> String {
        self.synthetic_output_id = self.synthetic_output_id.saturating_add(1);
        format!("{prefix}-{}", self.synthetic_output_id)
    }

    fn start_text(
        &mut self,
        item_id: Option<String>,
        partial: &mut AssistantMessage,
        events: &mut Vec<AssistantMessageEvent>,
    ) -> String {
        let key = item_id.unwrap_or_else(|| self.next_synthetic_id("text"));
        if self.outputs.contains_key(&key) {
            self.last_text_output = Some(key.clone());
            return key;
        }
        let content_index = partial.content.len() as u32;
        partial.content.push(ContentBlock::Text {
            text: String::new(),
            text_signature: None,
        });
        self.outputs.insert(
            key.clone(),
            OutputState {
                content_index,
                kind: OutputKind::Text,
                ended: false,
            },
        );
        self.output_order.push(key.clone());
        self.last_text_output = Some(key.clone());
        events.push(AssistantMessageEvent::TextStart {
            content_index,
            partial: partial.clone(),
        });
        key
    }

    fn start_tool(
        &mut self,
        item: wire::OutputItem,
        partial: &mut AssistantMessage,
        events: &mut Vec<AssistantMessageEvent>,
    ) {
        let key = item.id.clone();
        if self.outputs.contains_key(&key) {
            self.last_tool_output = Some(key);
            return;
        }
        let content_index = partial.content.len() as u32;
        partial.content.push(ContentBlock::ToolCall {
            id: item.call_id.unwrap_or_else(|| item.id.clone()),
            name: item.name.unwrap_or_default(),
            arguments: serde_json::json!({}),
            thought_signature: None,
        });
        self.outputs.insert(
            key.clone(),
            OutputState {
                content_index,
                kind: OutputKind::Tool {
                    arguments: item.arguments.unwrap_or_default(),
                },
                ended: false,
            },
        );
        self.output_order.push(key.clone());
        self.last_tool_output = Some(key);
        events.push(AssistantMessageEvent::ToolcallStart {
            content_index,
            partial: partial.clone(),
        });
    }

    fn finish_output(
        &mut self,
        key: &str,
        partial: &mut AssistantMessage,
    ) -> Result<Option<AssistantMessageEvent>, String> {
        let Some(output) = self.outputs.get_mut(key) else {
            return Ok(None);
        };
        if output.ended {
            return Ok(None);
        }
        output.ended = true;

        let event = match &output.kind {
            OutputKind::Text => AssistantMessageEvent::TextEnd {
                content_index: output.content_index,
                partial: partial.clone(),
            },
            OutputKind::Tool { arguments } => {
                let parsed = parse_terminal_tool_arguments(arguments)?;
                if let Some(ContentBlock::ToolCall {
                    arguments: value, ..
                }) = partial.content.get_mut(output.content_index as usize)
                {
                    *value = parsed;
                }
                AssistantMessageEvent::ToolcallEnd {
                    content_index: output.content_index,
                    partial: partial.clone(),
                }
            }
        };
        Ok(Some(event))
    }

    fn failure_message(kind: &str, response: &wire::ResponseInfo) -> String {
        if let Some(error) = &response.error {
            let code = error.code.as_deref().unwrap_or("unknown_code");
            let error_type = error.error_type.as_deref().unwrap_or("unknown_type");
            return format!("{kind}: {error_type}/{code}: {}", error.message);
        }
        if let Some(details) = &response.incomplete_details
            && let Some(reason) = &details.reason
        {
            return format!("{kind}: {reason}");
        }
        format!("{kind}: provider returned status {:?}", response.status)
    }
}

impl SseEventHandler for ResponsesHandler {
    fn handle_event(
        &mut self,
        data: &str,
        partial: &mut AssistantMessage,
        _model: &Model,
    ) -> Result<SseEventResult, String> {
        let event = wire::ResponseStreamEvent::parse(data)
            .map_err(|error| format!("Responses event parse error: {error}"))?;
        let mut events = Vec::new();

        match event {
            wire::ResponseStreamEvent::ResponseCreated { response } => {
                if self.started {
                    return Err("duplicate response.created event".into());
                }
                self.response_id = Some(response.id);
                partial.response_id = self.response_id.clone();
                partial.timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                events.push(AssistantMessageEvent::Start {
                    content_index: None,
                    partial: partial.clone(),
                });
                self.started = true;
            }
            wire::ResponseStreamEvent::OutputItemAdded { item } => {
                if item.item_type == "function_call" {
                    self.start_tool(item, partial, &mut events);
                }
            }
            wire::ResponseStreamEvent::ContentPartAdded { item_id, part } => {
                if part.part_type == "output_text" || part.part_type == "text" {
                    let key = self.start_text(item_id, partial, &mut events);
                    if let Some(text) = part.text
                        && !text.is_empty()
                    {
                        let output = self.outputs.get(&key).expect("text output was inserted");
                        if let Some(ContentBlock::Text { text: value, .. }) =
                            partial.content.get_mut(output.content_index as usize)
                        {
                            value.push_str(&text);
                        }
                        events.push(AssistantMessageEvent::TextDelta {
                            content_index: output.content_index,
                            delta: text,
                            partial: partial.clone(),
                        });
                    }
                }
            }
            wire::ResponseStreamEvent::OutputTextDelta { item_id, delta } => {
                let key = item_id
                    .or_else(|| self.last_text_output.clone())
                    .ok_or_else(|| {
                        "output_text.delta arrived before a text output item".to_string()
                    })?;
                let output = self
                    .outputs
                    .get(&key)
                    .ok_or_else(|| format!("output_text.delta references unknown item {key}"))?;
                if output.ended || !matches!(output.kind, OutputKind::Text) {
                    return Err(format!(
                        "output_text.delta references closed/non-text item {key}"
                    ));
                }
                if let Some(ContentBlock::Text { text, .. }) =
                    partial.content.get_mut(output.content_index as usize)
                {
                    text.push_str(&delta);
                }
                events.push(AssistantMessageEvent::TextDelta {
                    content_index: output.content_index,
                    delta,
                    partial: partial.clone(),
                });
            }
            wire::ResponseStreamEvent::FunctionCallArgumentsDelta { item_id, delta } => {
                let key = item_id
                    .or_else(|| self.last_tool_output.clone())
                    .ok_or_else(|| {
                        "function_call_arguments.delta arrived before a tool output item"
                            .to_string()
                    })?;
                let output = self.outputs.get_mut(&key).ok_or_else(|| {
                    format!("function_call_arguments.delta references unknown item {key}")
                })?;
                let OutputKind::Tool { arguments } = &mut output.kind else {
                    return Err(format!(
                        "function_call_arguments.delta references non-tool item {key}"
                    ));
                };
                if output.ended {
                    return Err(format!(
                        "function_call_arguments.delta references closed item {key}"
                    ));
                }
                arguments.push_str(&delta);
                let parsed = parse_streaming_json(arguments);
                if let Some(ContentBlock::ToolCall {
                    arguments: value, ..
                }) = partial.content.get_mut(output.content_index as usize)
                {
                    *value = parsed;
                }
                events.push(AssistantMessageEvent::ToolcallDelta {
                    content_index: output.content_index,
                    delta,
                    partial: partial.clone(),
                });
            }
            wire::ResponseStreamEvent::OutputItemDone { item } => {
                let key = if self.outputs.contains_key(&item.id) {
                    item.id
                } else if item.item_type == "message" {
                    self.last_text_output.clone().unwrap_or(item.id)
                } else {
                    self.last_tool_output.clone().unwrap_or(item.id)
                };
                if let Some(event) = self.finish_output(&key, partial)? {
                    events.push(event);
                }
            }
            wire::ResponseStreamEvent::ResponseCompleted { response } => {
                if !self.started {
                    return Err("response.completed arrived before response.created".into());
                }
                partial.response_id = Some(response.id);
                self.usage = response.usage;
                return Ok(SseEventResult::ProviderDone(events));
            }
            wire::ResponseStreamEvent::ResponseFailed { response } => {
                return Ok(SseEventResult::ProviderError {
                    events,
                    reason: StopReason::Error,
                    message: Self::failure_message("response failed", &response),
                });
            }
            wire::ResponseStreamEvent::ResponseIncomplete { response } => {
                return Ok(SseEventResult::ProviderError {
                    events,
                    reason: StopReason::Error,
                    message: Self::failure_message("response incomplete", &response),
                });
            }
            wire::ResponseStreamEvent::ResponseCancelled { response } => {
                return Ok(SseEventResult::ProviderError {
                    events,
                    reason: StopReason::Aborted,
                    message: Self::failure_message("response cancelled", &response),
                });
            }
            wire::ResponseStreamEvent::Error { error } => {
                let code = error.code.as_deref().unwrap_or("unknown_code");
                let error_type = error.error_type.as_deref().unwrap_or("unknown_type");
                return Ok(SseEventResult::ProviderError {
                    events,
                    reason: StopReason::Error,
                    message: format!("provider error {error_type}/{code}: {}", error.message),
                });
            }
            wire::ResponseStreamEvent::Bookkeeping => {}
            wire::ResponseStreamEvent::Unknown { event_type, raw } => {
                let content_bearing = event_type.contains(".delta")
                    || event_type.contains("content")
                    || event_type.contains("output_item")
                    || raw.get("delta").is_some()
                    || raw.get("item").is_some();
                let terminal_like = ["complete", "failed", "error", "incomplete", "cancel"]
                    .iter()
                    .any(|marker| event_type.contains(marker));
                if content_bearing || terminal_like {
                    return Err(format!(
                        "unsupported significant Responses event `{event_type}`"
                    ));
                }
            }
        }

        Ok(SseEventResult::Continue(events))
    }

    fn finish(
        &mut self,
        partial: &mut AssistantMessage,
        model: &Model,
    ) -> Result<Vec<AssistantMessageEvent>, String> {
        let mut events = Vec::new();
        for key in self.output_order.clone() {
            if let Some(event) = self.finish_output(&key, partial)? {
                events.push(event);
            }
        }

        if let Some(usage) = &self.usage {
            partial.usage = map_usage(usage, model);
        }
        partial.stop_reason = if partial
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolCall { .. }))
        {
            StopReason::ToolUse
        } else {
            StopReason::Stop
        };
        Ok(events)
    }
}

fn map_usage(usage: &wire::ResponseUsage, model: &Model) -> Usage {
    let cache_tokens = usage
        .input_tokens_details
        .as_ref()
        .map(|d| d.cached_tokens)
        .unwrap_or(0);
    // `input_tokens` is the total prompt size and includes the cached subset,
    // mirroring the completions-path semantics where `input` excludes cache
    // hits to avoid double-billing the cached portion.
    let non_cached_input = usage.input_tokens.saturating_sub(cache_tokens);

    let mut result = Usage {
        input: non_cached_input,
        output: usage.output_tokens,
        cache_read: cache_tokens,
        cache_write: 0,
        total_tokens: if usage.total_tokens == 0 {
            crate::protocol::usage::saturating_token_total(usage.input_tokens, usage.output_tokens)
        } else {
            usage.total_tokens
        },
        cost: Cost::default(),
    };
    calculate_cost(model, &mut result);
    result
}
