use async_stream::stream;
use bytes::Bytes;
use futures::{Stream, StreamExt};

const MAX_LINE_BYTES: usize = 64 * 1024;
const MAX_EVENT_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_RESPONSE_EVENTS: usize = 16 * 1024;

#[derive(Debug, Clone, Copy)]
struct SseInvocationLimits {
    response_bytes: usize,
    response_events: usize,
}

impl Default for SseInvocationLimits {
    fn default() -> Self {
        Self {
            response_bytes: MAX_RESPONSE_BYTES,
            response_events: MAX_RESPONSE_EVENTS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSentEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
    pub retry: Option<u64>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SseError {
    #[error("SSE read error: {0}")]
    Read(String),
    #[error("SSE line is not valid UTF-8")]
    InvalidUtf8,
    #[error("SSE line exceeds {limit} bytes")]
    LineTooLarge { limit: usize },
    #[error("SSE event exceeds {limit} bytes")]
    EventTooLarge { limit: usize },
    #[error("SSE response exceeds {limit} bytes")]
    ResponseTooLarge { limit: usize },
    #[error("SSE response exceeds {limit} events")]
    TooManyEvents { limit: usize },
}

#[derive(Debug, Default)]
struct SseDecoder {
    buffer: Vec<u8>,
    event_type: Option<String>,
    data_lines: Vec<String>,
    last_event_id: Option<String>,
    retry: Option<u64>,
    event_bytes: usize,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<ServerSentEvent>, SseError> {
        self.buffer.extend_from_slice(chunk);
        self.drain_lines(false)
    }

    fn finish(&mut self) -> Result<Vec<ServerSentEvent>, SseError> {
        let mut events = self.drain_lines(true)?;
        if let Some(event) = self.dispatch_event() {
            events.push(event);
        }
        Ok(events)
    }

    fn drain_lines(&mut self, eof: bool) -> Result<Vec<ServerSentEvent>, SseError> {
        let mut events = Vec::new();

        while let Some(position) = self
            .buffer
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
        {
            if self.buffer[position] == b'\r' && position + 1 == self.buffer.len() && !eof {
                break;
            }

            let delimiter_len = if self.buffer[position] == b'\r'
                && self.buffer.get(position + 1) == Some(&b'\n')
            {
                2
            } else {
                1
            };
            let line = self.buffer[..position].to_vec();
            self.buffer.drain(..position + delimiter_len);
            if let Some(event) = self.process_line(&line)? {
                events.push(event);
            }
        }

        if eof && !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            if let Some(event) = self.process_line(&line)? {
                events.push(event);
            }
        } else if self.buffer.len() > MAX_LINE_BYTES {
            return Err(SseError::LineTooLarge {
                limit: MAX_LINE_BYTES,
            });
        }

        Ok(events)
    }

    fn process_line(&mut self, line: &[u8]) -> Result<Option<ServerSentEvent>, SseError> {
        if line.len() > MAX_LINE_BYTES {
            return Err(SseError::LineTooLarge {
                limit: MAX_LINE_BYTES,
            });
        }
        let line = std::str::from_utf8(line).map_err(|_| SseError::InvalidUtf8)?;

        if line.is_empty() {
            return Ok(self.dispatch_event());
        }
        if line.starts_with(':') {
            return Ok(None);
        }

        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };

        match field {
            "event" => self.event_type = Some(value.to_string()),
            "data" => {
                self.event_bytes = self
                    .event_bytes
                    .checked_add(value.len() + usize::from(!self.data_lines.is_empty()))
                    .ok_or(SseError::EventTooLarge {
                        limit: MAX_EVENT_BYTES,
                    })?;
                if self.event_bytes > MAX_EVENT_BYTES {
                    return Err(SseError::EventTooLarge {
                        limit: MAX_EVENT_BYTES,
                    });
                }
                self.data_lines.push(value.to_string());
            }
            "id" if !value.contains('\0') => self.last_event_id = Some(value.to_string()),
            "retry" => self.retry = value.parse::<u64>().ok(),
            _ => {}
        }

        Ok(None)
    }

    fn dispatch_event(&mut self) -> Option<ServerSentEvent> {
        if self.data_lines.is_empty() {
            self.event_type = None;
            self.retry = None;
            self.event_bytes = 0;
            return None;
        }

        let event = ServerSentEvent {
            event: self.event_type.take(),
            data: self.data_lines.join("\n"),
            id: self.last_event_id.clone(),
            retry: self.retry.take(),
        };
        self.data_lines.clear();
        self.event_bytes = 0;
        Some(event)
    }
}

pub fn iterate_sse<E>(
    body: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
) -> impl Stream<Item = Result<ServerSentEvent, SseError>> + Send
where
    E: std::fmt::Display + Send + 'static,
{
    iterate_sse_with_limits(body, SseInvocationLimits::default())
}

fn iterate_sse_with_limits<E>(
    body: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
    limits: SseInvocationLimits,
) -> impl Stream<Item = Result<ServerSentEvent, SseError>> + Send
where
    E: std::fmt::Display + Send + 'static,
{
    let mut decoder = SseDecoder::default();
    stream! {
        let mut response_bytes = 0_usize;
        let mut response_events = 0_usize;
        futures::pin_mut!(body);
        while let Some(chunk_result) = body.next().await {
            let chunk = match chunk_result {
                Ok(chunk) => chunk,
                Err(error) => {
                    yield Err(SseError::Read(error.to_string()));
                    return;
                }
            };
            response_bytes = match response_bytes.checked_add(chunk.len()) {
                Some(bytes) if bytes <= limits.response_bytes => bytes,
                _ => {
                    yield Err(SseError::ResponseTooLarge {
                        limit: limits.response_bytes,
                    });
                    return;
                }
            };
            match decoder.push(&chunk) {
                Ok(events) => {
                    for event in events {
                        response_events = match response_events.checked_add(1) {
                            Some(events) if events <= limits.response_events => events,
                            _ => {
                                yield Err(SseError::TooManyEvents {
                                    limit: limits.response_events,
                                });
                                return;
                            }
                        };
                        yield Ok(event);
                    }
                }
                Err(error) => {
                    yield Err(error);
                    return;
                }
            }
        }

        match decoder.finish() {
            Ok(events) => {
                for event in events {
                    response_events = match response_events.checked_add(1) {
                        Some(events) if events <= limits.response_events => events,
                        _ => {
                            yield Err(SseError::TooManyEvents {
                                limit: limits.response_events,
                            });
                            return;
                        }
                    };
                    yield Ok(event);
                }
            }
            Err(error) => yield Err(error),
        }
    }
}
