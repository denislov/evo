use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt};

pub const DEFAULT_MAX_JSONL_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonlFrame {
    Line(String),
    TooLarge { max_bytes: usize },
}

pub fn serialize_json_line<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    Ok(line)
}

#[cfg(test)]
pub async fn read_jsonl_lines<R>(mut reader: R) -> std::io::Result<Vec<String>>
where
    R: AsyncRead + Unpin,
{
    let mut lines = Vec::new();
    let mut reader = JsonlLineReader::new(&mut reader);
    while let Some(frame) = reader.read_next_frame().await? {
        match frame {
            JsonlFrame::Line(line) => lines.push(line),
            JsonlFrame::TooLarge { max_bytes } => return Err(frame_too_large(max_bytes)),
        }
    }
    Ok(lines)
}

pub struct JsonlLineReader<R> {
    reader: R,
    pending: Vec<u8>,
    reached_eof: bool,
    discarding_oversized: bool,
    max_frame_bytes: usize,
}

impl<R> JsonlLineReader<R>
where
    R: AsyncRead + Unpin,
{
    #[cfg(test)]
    pub fn new(reader: R) -> Self {
        Self::with_max_frame_bytes(reader, DEFAULT_MAX_JSONL_FRAME_BYTES)
    }

    pub fn with_max_frame_bytes(reader: R, max_frame_bytes: usize) -> Self {
        Self {
            reader,
            pending: Vec::new(),
            reached_eof: false,
            discarding_oversized: false,
            max_frame_bytes: max_frame_bytes.max(1),
        }
    }

    pub async fn read_next_frame(&mut self) -> std::io::Result<Option<JsonlFrame>> {
        loop {
            if self.discarding_oversized {
                if let Some(line_end) = self.pending.iter().position(|byte| *byte == b'\n') {
                    self.pending.drain(..=line_end);
                    self.discarding_oversized = false;
                    return Ok(Some(JsonlFrame::TooLarge {
                        max_bytes: self.max_frame_bytes,
                    }));
                }
                self.pending.clear();
                if self.reached_eof {
                    self.discarding_oversized = false;
                    return Ok(Some(JsonlFrame::TooLarge {
                        max_bytes: self.max_frame_bytes,
                    }));
                }
            }

            if let Some(line_end) = self.pending.iter().position(|byte| *byte == b'\n') {
                if line_end > self.max_frame_bytes {
                    self.pending.drain(..=line_end);
                    return Ok(Some(JsonlFrame::TooLarge {
                        max_bytes: self.max_frame_bytes,
                    }));
                }
                let mut bytes: Vec<u8> = self.pending.drain(..=line_end).collect();
                bytes.pop();
                return Ok(Some(JsonlFrame::Line(line_from_bytes(bytes))));
            }

            if self.reached_eof {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                if self.pending.len() > self.max_frame_bytes {
                    self.pending.clear();
                    return Ok(Some(JsonlFrame::TooLarge {
                        max_bytes: self.max_frame_bytes,
                    }));
                }
                return Ok(Some(JsonlFrame::Line(line_from_bytes(std::mem::take(
                    &mut self.pending,
                )))));
            }

            if self.pending.len() > self.max_frame_bytes {
                self.pending.clear();
                self.discarding_oversized = true;
                continue;
            }

            let mut chunk = [0; 8192];
            let retained_budget = self
                .max_frame_bytes
                .saturating_add(1)
                .saturating_sub(self.pending.len())
                .max(1);
            let read_limit = retained_budget.min(chunk.len());
            let read = self.reader.read(&mut chunk[..read_limit]).await?;
            if read == 0 {
                self.reached_eof = true;
            } else {
                self.pending.extend_from_slice(&chunk[..read]);
            }
        }
    }
}

#[cfg(test)]
fn frame_too_large(max_bytes: usize) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("JSONL frame exceeds {max_bytes}-byte limit"),
    )
}

fn line_from_bytes(mut bytes: Vec<u8>) -> String {
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8_lossy(&bytes).to_string()
}
