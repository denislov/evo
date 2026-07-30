use async_stream::stream;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use std::collections::HashMap;
use std::io::Write;
use tokio_util::sync::CancellationToken;

use crate::protocol::json::{parse_streaming_json, parse_terminal_json};
use crate::protocol::{AssistantMessage, AssistantMessageEvent, ContentBlock, StopReason};

use crate::model::Model;
use crate::protocol::stream::EventStream;
use crate::transport::sse::iterate_sse;

const MAX_PROVIDER_EVENTS: usize = 64 * 1024;
const MAX_CONTENT_BLOCKS: usize = 2 * 1024;
const MAX_CONTENT_BYTES: usize = 32 * 1024 * 1024;
const MAX_TOOL_CALLS: usize = 256;
const MAX_TOOL_ARGUMENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
struct ProviderEventLimits {
    events: usize,
    content_blocks: usize,
    content_bytes: usize,
    tool_calls: usize,
    tool_argument_bytes: usize,
}

impl Default for ProviderEventLimits {
    fn default() -> Self {
        Self {
            events: MAX_PROVIDER_EVENTS,
            content_blocks: MAX_CONTENT_BLOCKS,
            content_bytes: MAX_CONTENT_BYTES,
            tool_calls: MAX_TOOL_CALLS,
            tool_argument_bytes: MAX_TOOL_ARGUMENT_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderLimit {
    Events,
    ContentBlocks,
    ContentBytes,
    ToolCalls,
    ToolArgumentBytes,
}

impl ProviderLimit {
    const fn name(self) -> &'static str {
        match self {
            Self::Events => "events",
            Self::ContentBlocks => "content blocks",
            Self::ContentBytes => "content bytes",
            Self::ToolCalls => "tool calls",
            Self::ToolArgumentBytes => "tool argument bytes",
        }
    }
}

struct ProviderEventBudget {
    limits: ProviderEventLimits,
    events: usize,
}

impl ProviderEventBudget {
    const fn new(limits: ProviderEventLimits) -> Self {
        Self { limits, events: 0 }
    }

    fn observe(&mut self, event: &AssistantMessageEvent) -> Result<(), ProviderLimit> {
        self.events = checked_total(self.events, 1, self.limits.events, ProviderLimit::Events)?;
        let message = event_message(event);
        if message.content.len() > self.limits.content_blocks {
            return Err(ProviderLimit::ContentBlocks);
        }

        let mut content_bytes = 0_usize;
        let mut tool_calls = 0_usize;
        let mut tool_argument_bytes = 0_usize;
        for block in &message.content {
            match block {
                ContentBlock::Text {
                    text,
                    text_signature,
                } => {
                    content_bytes = checked_total(
                        content_bytes,
                        text.len(),
                        self.limits.content_bytes,
                        ProviderLimit::ContentBytes,
                    )?;
                    if let Some(signature) = text_signature {
                        content_bytes = checked_total(
                            content_bytes,
                            signature.len(),
                            self.limits.content_bytes,
                            ProviderLimit::ContentBytes,
                        )?;
                    }
                }
                ContentBlock::Thinking {
                    thinking,
                    thinking_signature,
                    ..
                } => {
                    content_bytes = checked_total(
                        content_bytes,
                        thinking.len(),
                        self.limits.content_bytes,
                        ProviderLimit::ContentBytes,
                    )?;
                    if let Some(signature) = thinking_signature {
                        content_bytes = checked_total(
                            content_bytes,
                            signature.len(),
                            self.limits.content_bytes,
                            ProviderLimit::ContentBytes,
                        )?;
                    }
                }
                ContentBlock::Image { data, mime_type } => {
                    content_bytes = checked_total(
                        content_bytes,
                        data.len(),
                        self.limits.content_bytes,
                        ProviderLimit::ContentBytes,
                    )?;
                    content_bytes = checked_total(
                        content_bytes,
                        mime_type.len(),
                        self.limits.content_bytes,
                        ProviderLimit::ContentBytes,
                    )?;
                }
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                    thought_signature,
                } => {
                    tool_calls = checked_total(
                        tool_calls,
                        1,
                        self.limits.tool_calls,
                        ProviderLimit::ToolCalls,
                    )?;
                    for value in [id.as_str(), name.as_str()] {
                        content_bytes = checked_total(
                            content_bytes,
                            value.len(),
                            self.limits.content_bytes,
                            ProviderLimit::ContentBytes,
                        )?;
                    }
                    if let Some(signature) = thought_signature {
                        content_bytes = checked_total(
                            content_bytes,
                            signature.len(),
                            self.limits.content_bytes,
                            ProviderLimit::ContentBytes,
                        )?;
                    }
                    let argument_bytes = serialized_json_bytes(arguments)
                        .map_err(|_| ProviderLimit::ToolArgumentBytes)?;
                    tool_argument_bytes = checked_total(
                        tool_argument_bytes,
                        argument_bytes,
                        self.limits.tool_argument_bytes,
                        ProviderLimit::ToolArgumentBytes,
                    )?;
                }
            }
        }
        Ok(())
    }
}

fn checked_total(
    current: usize,
    added: usize,
    limit: usize,
    kind: ProviderLimit,
) -> Result<usize, ProviderLimit> {
    current
        .checked_add(added)
        .filter(|total| *total <= limit)
        .ok_or(kind)
}

#[derive(Default)]
struct ByteCounter(usize);

impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(buffer.len())
            .ok_or_else(|| std::io::Error::other("serialized JSON byte count overflow"))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialized_json_bytes(value: &serde_json::Value) -> serde_json::Result<usize> {
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.0)
}

fn event_message(event: &AssistantMessageEvent) -> &AssistantMessage {
    match event {
        AssistantMessageEvent::Start { partial, .. }
        | AssistantMessageEvent::TextStart { partial, .. }
        | AssistantMessageEvent::TextDelta { partial, .. }
        | AssistantMessageEvent::TextEnd { partial, .. }
        | AssistantMessageEvent::ThinkingStart { partial, .. }
        | AssistantMessageEvent::ThinkingDelta { partial, .. }
        | AssistantMessageEvent::ThinkingEnd { partial, .. }
        | AssistantMessageEvent::ToolcallStart { partial, .. }
        | AssistantMessageEvent::ToolcallDelta { partial, .. }
        | AssistantMessageEvent::ToolcallEnd { partial, .. } => partial,
        AssistantMessageEvent::Done { message, .. }
        | AssistantMessageEvent::Error { message, .. } => message,
    }
}

pub enum SseEventResult {
    Continue(Vec<AssistantMessageEvent>),
    ProviderDone(Vec<AssistantMessageEvent>),
    ProviderError {
        events: Vec<AssistantMessageEvent>,
        reason: StopReason,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseTransportTerminal {
    DoneMarker,
    Eof,
}

pub(super) fn start_once(
    started: &mut bool,
    partial: &mut AssistantMessage,
    response_id: String,
    response_model: String,
) -> Option<AssistantMessageEvent> {
    if *started {
        return None;
    }
    partial.response_id = Some(response_id);
    partial.response_model = Some(response_model);
    partial.timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    *started = true;
    Some(AssistantMessageEvent::Start {
        content_index: None,
        partial: partial.clone(),
    })
}

#[derive(Default)]
pub(super) struct ToolArgumentAssembler {
    values: HashMap<u32, String>,
}

impl ToolArgumentAssembler {
    pub(super) fn append(&mut self, provider_index: u32, delta: &str) -> serde_json::Value {
        let value = self.values.entry(provider_index).or_default();
        value.push_str(delta);
        parse_streaming_json(value)
    }

    pub(super) fn finish(&self, provider_index: u32) -> Result<serde_json::Value, String> {
        parse_terminal_json(
            self.values
                .get(&provider_index)
                .map(String::as_str)
                .unwrap_or(""),
        )
    }
}

#[derive(Default)]
pub(super) struct ProviderTerminalLatch {
    observed: bool,
}

impl ProviderTerminalLatch {
    pub(super) fn observe(&mut self) {
        self.observed = true;
    }

    pub(super) fn accept(&self, terminal: SseTransportTerminal) -> Result<(), String> {
        match terminal {
            SseTransportTerminal::DoneMarker if self.observed => Ok(()),
            SseTransportTerminal::DoneMarker => {
                Err("received [DONE] before a usable finish reason".into())
            }
            SseTransportTerminal::Eof => {
                Err("stream ended before the required [DONE] marker".into())
            }
        }
    }
}

pub trait SseEventHandler: Send + 'static {
    fn handle_event(
        &mut self,
        data: &str,
        partial: &mut AssistantMessage,
        model: &Model,
    ) -> Result<SseEventResult, String>;

    fn finish(
        &mut self,
        partial: &mut AssistantMessage,
        model: &Model,
    ) -> Result<Vec<AssistantMessageEvent>, String>;

    fn accept_transport_terminal(&self, terminal: SseTransportTerminal) -> Result<(), String> {
        Err(match terminal {
            SseTransportTerminal::DoneMarker => {
                "provider protocol does not accept a [DONE] terminal marker".to_string()
            }
            SseTransportTerminal::Eof => {
                "provider stream ended before a terminal event".to_string()
            }
        })
    }
}

pub fn process_sse<E, H: SseEventHandler>(
    body: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
    model: Model,
    cancel: Option<CancellationToken>,
    mut handler: H,
    api_name: &str,
) -> EventStream
where
    E: std::fmt::Display + Send + 'static,
{
    let api_name = api_name.to_string();
    let limit_api_name = api_name.clone();
    let limit_model = model.clone();
    let inner: EventStream = Box::pin(stream! {
        let mut partial = AssistantMessage::empty(&api_name, &model.id);
        partial.provider = Some(model.provider.clone());

        let sse = iterate_sse(body);
        futures::pin_mut!(sse);

        loop {
            let next_event = match cancel.as_ref() {
                Some(token) => tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        partial.stop_reason = StopReason::Aborted;
                        partial.error_message = Some("Provider stream cancelled".to_string());
                        yield AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            message: partial.clone(),
                        };
                        return;
                    }
                    event = sse.next() => event,
                },
                None => sse.next().await,
            };

            let sse_event = match next_event {
                Some(Ok(e)) => e,
                Some(Err(_error)) => {
                    partial.stop_reason = StopReason::Error;
                    partial.error_message = Some("Provider response stream failed".to_string());
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        message: partial.clone(),
                    };
                    return;
                }
                None => break,
            };

            if sse_event.data == "[DONE]" {
                if let Err(error) = handler
                    .accept_transport_terminal(SseTransportTerminal::DoneMarker)
                {
                    yield terminal_error(&mut partial, &api_name, &model, error);
                    return;
                }
                match handler.finish(&mut partial, &model) {
                    Ok(events) => {
                        for event in events {
                            yield event;
                        }
                        yield terminal_event(partial, &api_name, &model);
                    }
                    Err(error) => yield terminal_error(&mut partial, &api_name, &model, error),
                }
                return;
            }

            match handler.handle_event(&sse_event.data, &mut partial, &model) {
                Ok(SseEventResult::Continue(events)) => {
                    for event in events {
                        yield event;
                    }
                }
                Ok(SseEventResult::ProviderDone(events)) => {
                    for event in events {
                        yield event;
                    }
                    match handler.finish(&mut partial, &model) {
                        Ok(events) => {
                            for event in events {
                                yield event;
                            }
                            yield terminal_event(partial, &api_name, &model);
                        }
                        Err(error) => {
                            yield terminal_error(&mut partial, &api_name, &model, error)
                        }
                    }
                    return;
                }
                Ok(SseEventResult::ProviderError {
                    events,
                    reason,
                    message: _message,
                }) => {
                    for event in events {
                        yield event;
                    }
                    partial.stop_reason = reason.clone();
                    partial.error_message =
                        Some("Provider reported a terminal failure".to_string());
                    yield AssistantMessageEvent::Error {
                        reason,
                        message: partial.clone(),
                    };
                    return;
                }
                Err(error) => {
                    yield terminal_error(&mut partial, &api_name, &model, error);
                    return;
                }
            }
        }

        if let Err(error) = handler.accept_transport_terminal(SseTransportTerminal::Eof) {
            yield terminal_error(&mut partial, &api_name, &model, error);
            return;
        }
        match handler.finish(&mut partial, &model) {
            Ok(events) => {
                for event in events {
                    yield event;
                }
                yield terminal_event(partial, &api_name, &model);
            }
            Err(error) => yield terminal_error(&mut partial, &api_name, &model, error),
        }
    });
    enforce_provider_event_limits(
        inner,
        limit_api_name,
        limit_model,
        ProviderEventLimits::default(),
    )
}

fn enforce_provider_event_limits(
    mut inner: EventStream,
    api_name: String,
    model: Model,
    limits: ProviderEventLimits,
) -> EventStream {
    Box::pin(stream! {
        let mut budget = ProviderEventBudget::new(limits);
        while let Some(event) = inner.next().await {
            match budget.observe(&event) {
                Ok(()) => yield event,
                Err(limit) => {
                    let mut message = AssistantMessage::empty(&api_name, &model.id);
                    message.provider = Some(model.provider.clone());
                    message.stop_reason = StopReason::Error;
                    message.error_message =
                        Some(format!("Provider response exceeded the {} limit", limit.name()));
                    yield AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        message,
                    };
                    return;
                }
            }
        }
    })
}

fn terminal_event(
    mut message: AssistantMessage,
    _api_name: &str,
    _model: &Model,
) -> AssistantMessageEvent {
    match &message.stop_reason {
        StopReason::Stop | StopReason::Length | StopReason::ToolUse => {
            AssistantMessageEvent::Done {
                reason: message.stop_reason.clone(),
                message,
            }
        }
        StopReason::Error | StopReason::Aborted => {
            if message.error_message.is_none() {
                message.error_message = Some("Provider stream ended unsuccessfully".to_string());
            }
            AssistantMessageEvent::Error {
                reason: message.stop_reason.clone(),
                message,
            }
        }
    }
}

fn terminal_error(
    partial: &mut AssistantMessage,
    _api_name: &str,
    _model: &Model,
    _error: impl std::fmt::Display,
) -> AssistantMessageEvent {
    partial.stop_reason = StopReason::Error;
    partial.error_message = Some("Provider protocol error".to_string());
    AssistantMessageEvent::Error {
        reason: StopReason::Error,
        message: partial.clone(),
    }
}

/// Normalize a tool-call id to match the `^[a-zA-Z0-9_-]{1,64}$` pattern.
/// If the id is already valid, return as-is. Otherwise sanitize and truncate.
/// When `replacement` is Some(c), invalid chars are replaced with `c`;
/// when None, invalid chars are removed.
pub fn normalize_tool_call_id(id: &str, replacement: Option<char>) -> String {
    let is_valid = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if is_valid {
        return id.to_string();
    }

    let sanitized: String = match replacement {
        Some(replacement) => id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    replacement
                }
            })
            .collect(),
        None => id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect(),
    };

    if sanitized.len() > 64 {
        sanitized[..64].to_string()
    } else if sanitized.is_empty() {
        "tool_0".to_string()
    } else {
        sanitized
    }
}
