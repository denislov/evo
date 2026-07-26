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

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    fn decode(chunks: Vec<Vec<u8>>) -> Result<Vec<ServerSentEvent>, SseError> {
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(decoder.push(&chunk)?);
        }
        events.extend(decoder.finish()?);
        Ok(events)
    }

    #[test]
    fn basic_event_and_fields() {
        let events = decode(vec![
            b"id: 7\nevent: message\nretry: 250\ndata: hello\n\n".to_vec(),
        ])
        .unwrap();
        assert_eq!(
            events,
            vec![ServerSentEvent {
                event: Some("message".into()),
                data: "hello".into(),
                id: Some("7".into()),
                retry: Some(250),
            }]
        );
    }

    #[test]
    fn multi_line_data_preserves_newlines_across_chunks() {
        let events = decode(vec![
            b"event: message\ndata: line1\n".to_vec(),
            b"data: line2\n\n".to_vec(),
        ])
        .unwrap();
        assert_eq!(events[0].data, "line1\nline2");
        assert_eq!(events[0].event.as_deref(), Some("message"));
    }

    #[test]
    fn accepts_lf_crlf_and_cr_line_endings() {
        for input in [
            b"data: one\n\n".as_slice(),
            b"data: one\r\n\r\n".as_slice(),
            b"data: one\r\r".as_slice(),
        ] {
            assert_eq!(decode(vec![input.to_vec()]).unwrap()[0].data, "one");
        }
    }

    #[test]
    fn utf8_scalar_may_split_at_every_byte_boundary() {
        let input = "data: 你好\n\n".as_bytes();
        for split in 0..=input.len() {
            let events = decode(vec![input[..split].to_vec(), input[split..].to_vec()]).unwrap();
            assert_eq!(events[0].data, "你好");
        }
    }

    #[test]
    fn eof_dispatches_a_valid_final_field_without_a_blank_line() {
        let events = decode(vec![b"data: final".to_vec()]).unwrap();
        assert_eq!(events[0].data, "final");
    }

    #[test]
    fn eof_does_not_manufacture_payload_from_arbitrary_bytes() {
        assert!(
            decode(vec![b"not-an-sse-field".to_vec()])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn comments_unknown_fields_and_id_with_nul_are_ignored() {
        let events = decode(vec![
            b": comment\nunknown: value\nid: bad\0id\ndata:\n\n".to_vec(),
        ])
        .unwrap();
        assert_eq!(events[0].data, "");
        assert_eq!(events[0].id, None);
    }

    #[test]
    fn rejects_invalid_utf8() {
        assert_eq!(
            decode(vec![vec![b'd', b'a', b't', b'a', b':', b' ', 0xff, b'\n']]),
            Err(SseError::InvalidUtf8)
        );
    }

    #[test]
    fn rejects_oversized_line_and_event() {
        let oversized_line = vec![b'a'; MAX_LINE_BYTES + 1];
        assert_eq!(
            decode(vec![oversized_line]),
            Err(SseError::LineTooLarge {
                limit: MAX_LINE_BYTES
            })
        );

        let line = format!("data: {}\n", "a".repeat(MAX_LINE_BYTES - 6));
        let mut decoder = SseDecoder::default();
        for _ in 0..=(MAX_EVENT_BYTES / (MAX_LINE_BYTES - 6)) {
            match decoder.push(line.as_bytes()) {
                Ok(_) => continue,
                Err(error) => {
                    assert_eq!(
                        error,
                        SseError::EventTooLarge {
                            limit: MAX_EVENT_BYTES
                        }
                    );
                    return;
                }
            }
        }
        panic!("oversized event should fail");
    }

    #[tokio::test]
    async fn iterate_sse_propagates_read_errors() {
        let body = stream::iter(vec![
            Ok::<_, String>(Bytes::from_static(b"data: first\n\n")),
            Err("broken body".into()),
        ]);
        let results: Vec<_> = iterate_sse(body).collect().await;
        assert_eq!(results[0].as_ref().unwrap().data, "first");
        assert_eq!(results[1], Err(SseError::Read("broken body".to_string())));
    }

    #[tokio::test]
    async fn iterate_sse_enforces_aggregate_response_bytes_before_decoding() {
        let body = stream::iter(vec![
            Ok::<_, String>(Bytes::from_static(b"data: one\n\n")),
            Ok(Bytes::from_static(b"data: two\n\n")),
        ]);
        let results = iterate_sse_with_limits(
            body,
            SseInvocationLimits {
                response_bytes: 15,
                response_events: 10,
            },
        )
        .collect::<Vec<_>>()
        .await;

        assert_eq!(results[0].as_ref().unwrap().data, "one");
        assert_eq!(results[1], Err(SseError::ResponseTooLarge { limit: 15 }));
    }

    #[tokio::test]
    async fn iterate_sse_enforces_aggregate_event_count() {
        let body = stream::iter(vec![Ok::<_, String>(Bytes::from_static(
            b"data: one\n\ndata: two\n\n",
        ))]);
        let results = iterate_sse_with_limits(
            body,
            SseInvocationLimits {
                response_bytes: 1024,
                response_events: 1,
            },
        )
        .collect::<Vec<_>>()
        .await;

        assert_eq!(results[0].as_ref().unwrap().data, "one");
        assert_eq!(results[1], Err(SseError::TooManyEvents { limit: 1 }));
    }
}
