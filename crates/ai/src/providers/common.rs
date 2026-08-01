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
pub(crate) struct ProviderEventLimits {
    pub(crate) events: usize,
    pub(crate) content_blocks: usize,
    pub(crate) content_bytes: usize,
    pub(crate) tool_calls: usize,
    pub(crate) tool_argument_bytes: usize,
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
pub(crate) enum ProviderLimit {
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

pub(crate) struct ProviderEventBudget {
    limits: ProviderEventLimits,
    events: usize,
    content_blocks: usize,
    content_bytes: usize,
    tool_calls: usize,
    tool_argument_bytes: usize,
}

impl ProviderEventBudget {
    pub(crate) const fn new(limits: ProviderEventLimits) -> Self {
        Self {
            limits,
            events: 0,
            content_blocks: 0,
            content_bytes: 0,
            tool_calls: 0,
            tool_argument_bytes: 0,
        }
    }

    /// Account for one streamed event by its *delta* instead of re-scanning
    /// the accumulated message snapshot on every event, keeping long streams
    /// O(n) rather than O(n²). Every built-in provider emits text and tool
    /// arguments exclusively through delta events (start blocks are empty or
    /// echoed back as deltas), so the running totals equal the contents of
    /// the current message. Signatures ride the end events and are charged
    /// once per block.
    pub(crate) fn observe(&mut self, event: &AssistantMessageEvent) -> Result<(), ProviderLimit> {
        self.events = checked_total(self.events, 1, self.limits.events, ProviderLimit::Events)?;
        match event {
            AssistantMessageEvent::Start { partial, .. } => {
                // One-shot snapshot. Start payloads are empty for every
                // built-in provider, so this stays O(1) while still covering
                // image blocks, which have no dedicated stream events.
                self.observe_snapshot(&partial.content)
            }
            AssistantMessageEvent::TextStart { .. } | AssistantMessageEvent::ThinkingStart { .. } => {
                self.content_blocks = checked_total(
                    self.content_blocks,
                    1,
                    self.limits.content_blocks,
                    ProviderLimit::ContentBlocks,
                )?;
                // Initial block text is re-emitted as a TextDelta by every
                // built-in provider; only the block boundary is charged here.
                Ok(())
            }
            AssistantMessageEvent::ToolcallStart { content_index, partial } => {
                self.content_blocks = checked_total(
                    self.content_blocks,
                    1,
                    self.limits.content_blocks,
                    ProviderLimit::ContentBlocks,
                )?;
                self.tool_calls = checked_total(
                    self.tool_calls,
                    1,
                    self.limits.tool_calls,
                    ProviderLimit::ToolCalls,
                )?;
                if let Some(ContentBlock::ToolCall {
                    id,
                    name,
                    thought_signature,
                    ..
                }) = partial.content.get(*content_index as usize)
                {
                    self.charge_content(id.len())?;
                    self.charge_content(name.len())?;
                    if let Some(signature) = thought_signature {
                        self.charge_content(signature.len())?;
                    }
                }
                Ok(())
            }
            AssistantMessageEvent::TextDelta { delta, .. }
            | AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                self.charge_content(delta.len())
            }
            AssistantMessageEvent::ToolcallDelta { delta, .. } => {
                // Google re-emits the full serialized arguments once per
                // function call; every other provider streams true fragments.
                // Either shape lands at the same accumulated total, with only
                // pathological repeated snapshots over-accounting.
                self.tool_argument_bytes = checked_total(
                    self.tool_argument_bytes,
                    delta.len(),
                    self.limits.tool_argument_bytes,
                    ProviderLimit::ToolArgumentBytes,
                )?;
                Ok(())
            }
            AssistantMessageEvent::TextEnd { content_index, partial } => {
                if let Some(ContentBlock::Text { text_signature, .. }) =
                    partial.content.get(*content_index as usize)
                    && let Some(signature) = text_signature
                {
                    self.charge_content(signature.len())?;
                }
                Ok(())
            }
            AssistantMessageEvent::ThinkingEnd { content_index, partial } => {
                if let Some(ContentBlock::Thinking {
                    thinking_signature, ..
                }) = partial.content.get(*content_index as usize)
                    && let Some(signature) = thinking_signature
                {
                    self.charge_content(signature.len())?;
                }
                Ok(())
            }
            AssistantMessageEvent::ToolcallEnd { content_index, partial } => {
                if let Some(ContentBlock::ToolCall {
                    thought_signature, ..
                }) = partial.content.get(*content_index as usize)
                    && let Some(signature) = thought_signature
                {
                    self.charge_content(signature.len())?;
                }
                Ok(())
            }
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => Ok(()),
        }
    }

    fn charge_content(&mut self, added: usize) -> Result<(), ProviderLimit> {
        self.content_bytes = checked_total(
            self.content_bytes,
            added,
            self.limits.content_bytes,
            ProviderLimit::ContentBytes,
        )?;
        Ok(())
    }

    /// Full-snapshot accounting, used only for `Start` events.
    fn observe_snapshot(&mut self, content: &[ContentBlock]) -> Result<(), ProviderLimit> {
        self.content_blocks = checked_total(
            self.content_blocks,
            content.len(),
            self.limits.content_blocks,
            ProviderLimit::ContentBlocks,
        )?;
        for block in content {
            match block {
                ContentBlock::Text {
                    text,
                    text_signature,
                } => {
                    self.charge_content(text.len())?;
                    if let Some(signature) = text_signature {
                        self.charge_content(signature.len())?;
                    }
                }
                ContentBlock::Thinking {
                    thinking,
                    thinking_signature,
                    ..
                } => {
                    self.charge_content(thinking.len())?;
                    if let Some(signature) = thinking_signature {
                        self.charge_content(signature.len())?;
                    }
                }
                ContentBlock::Image { data, mime_type } => {
                    self.charge_content(data.len())?;
                    self.charge_content(mime_type.len())?;
                }
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                    thought_signature,
                } => {
                    self.tool_calls = checked_total(
                        self.tool_calls,
                        1,
                        self.limits.tool_calls,
                        ProviderLimit::ToolCalls,
                    )?;
                    self.charge_content(id.len())?;
                    self.charge_content(name.len())?;
                    if let Some(signature) = thought_signature {
                        self.charge_content(signature.len())?;
                    }
                    let argument_bytes = serialized_json_bytes(arguments)
                        .map_err(|_| ProviderLimit::ToolArgumentBytes)?;
                    self.tool_argument_bytes = checked_total(
                        self.tool_argument_bytes,
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
        parse_terminal_tool_arguments(
            self.values
                .get(&provider_index)
                .map(String::as_str)
                .unwrap_or(""),
        )
    }
}

/// Strictly parse accumulated terminal tool arguments.
///
/// Providers may legitimately omit argument deltas for parameter-less calls,
/// leaving the accumulation empty; that is a valid `{}` argument set rather
/// than malformed JSON.
pub(crate) fn parse_terminal_tool_arguments(
    accumulated: &str,
) -> Result<serde_json::Value, String> {
    if accumulated.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    parse_terminal_json(accumulated)
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
                    message,
                }) => {
                    for event in events {
                        yield event;
                    }
                    partial.stop_reason = reason.clone();
                    partial.error_message = Some(message);
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
    error: impl std::fmt::Display,
) -> AssistantMessageEvent {
    partial.stop_reason = StopReason::Error;
    partial.error_message = Some(format!("Provider protocol error: {error}"));
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
        // Truncate on char boundaries; `replacement` may be multi-byte.
        sanitized.chars().take(64).collect()
    } else if sanitized.is_empty() {
        "tool_0".to_string()
    } else {
        sanitized
    }
}
