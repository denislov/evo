use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::{JournalError, JournalErrorKind};
use crate::frame::MAX_JOURNAL_RECORD_BYTES;

const REVERSE_READ_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalReadBudget {
    pub max_items: usize,
    pub max_bytes: usize,
}

impl JournalReadBudget {
    pub const fn new(max_items: usize, max_bytes: usize) -> Self {
        Self {
            max_items,
            max_bytes,
        }
    }

    fn validate(self) -> Result<Self, JournalError> {
        if self.max_items == 0 || self.max_bytes == 0 {
            return Err(JournalError::new(
                JournalErrorKind::InvalidInput,
                "journal read budget must allow at least one item and one byte",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalTailCursor {
    pub before_sequence: u64,
    pub byte_offset: u64,
}

impl JournalTailCursor {
    pub fn opaque_token(self) -> String {
        format!("journal-v1:{}:{}", self.before_sequence, self.byte_offset)
    }
}

#[derive(Debug)]
pub struct JournalTailPage<T> {
    pub items: Vec<T>,
    pub omitted_items: usize,
    pub continuation: Option<JournalTailCursor>,
    pub retained_bytes: usize,
    pub scanned_bytes: u64,
}

struct ReverseLine {
    bytes: Vec<u8>,
    start_offset: u64,
}

pub fn read_tail<T>(
    path: &Path,
    budget: JournalReadBudget,
    mut decode: impl FnMut(&str, usize) -> Result<T, JournalError>,
    sequence: impl Fn(&T) -> Result<u64, JournalError>,
) -> Result<JournalTailPage<T>, JournalError> {
    let budget = budget.validate()?;
    let mut file = File::open(path).map_err(|error| io_error("open journal", path, error))?;
    let file_len = file
        .metadata()
        .map_err(|error| io_error("inspect journal", path, error))?
        .len();
    let mut offset = file_len;
    let mut items = Vec::with_capacity(budget.max_items.min(1024));
    let mut retained_bytes = 0_usize;
    let mut oldest_sequence = None;
    let mut oldest_offset = file_len;

    while items.len() < budget.max_items && retained_bytes < budget.max_bytes {
        let Some(line) = read_previous_line(&mut file, &mut offset, path)? else {
            break;
        };
        if line.bytes.is_empty() {
            continue;
        }
        let frame_bytes = line.bytes.len().saturating_add(1);
        if !items.is_empty() && retained_bytes.saturating_add(frame_bytes) > budget.max_bytes {
            break;
        }
        let text = decode_utf8_line(&line.bytes, 0, path)?;
        let item = decode(text, 0)?;
        let current_sequence = sequence(&item)?;
        if let Some(previous_newer) = oldest_sequence
            && current_sequence.checked_add(1) != Some(previous_newer)
        {
            return Err(JournalError::new(
                JournalErrorKind::Corrupt,
                format!(
                    "journal sequence is not contiguous in reverse: expected={}, actual={current_sequence}",
                    previous_newer.saturating_sub(1)
                ),
            ));
        }
        oldest_sequence = Some(current_sequence);
        oldest_offset = line.start_offset;
        retained_bytes = retained_bytes.saturating_add(frame_bytes);
        items.push(item);
    }

    if offset == 0
        && let Some(oldest_sequence) = oldest_sequence
        && oldest_sequence != 1
    {
        return Err(JournalError::new(
            JournalErrorKind::Corrupt,
            format!(
                "journal sequence is not contiguous at the log start: expected=1, actual={oldest_sequence}"
            ),
        ));
    }
    let omitted_u64 = oldest_sequence
        .map(|sequence| sequence.saturating_sub(1))
        .unwrap_or_default();
    let omitted_items = usize::try_from(omitted_u64).unwrap_or(usize::MAX);
    let continuation = oldest_sequence.and_then(|sequence| {
        (sequence > 1).then_some(JournalTailCursor {
            before_sequence: sequence,
            byte_offset: oldest_offset,
        })
    });
    items.reverse();
    Ok(JournalTailPage {
        items,
        omitted_items,
        continuation,
        retained_bytes,
        scanned_bytes: file_len.saturating_sub(offset),
    })
}

pub fn visit_lines(
    path: &Path,
    mut visitor: impl FnMut(&str, usize) -> Result<(), JournalError>,
) -> Result<(), JournalError> {
    let file = File::open(path).map_err(|error| io_error("open journal", path, error))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut line_number = 0_usize;
    while read_bounded_line(&mut reader, &mut line, path)? {
        line_number = line_number.saturating_add(1);
        let text = decode_utf8_line(&line, line_number, path)?;
        if !text.trim().is_empty() {
            visitor(text, line_number)?;
        }
    }
    Ok(())
}

pub fn read_first_line(path: &Path) -> Result<Option<String>, JournalError> {
    let file = File::open(path).map_err(|error| io_error("open journal", path, error))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    if !read_bounded_line(&mut reader, &mut line, path)? {
        return Ok(None);
    }
    decode_utf8_line(&line, 1, path).map(|line| Some(line.to_owned()))
}

pub fn decode_utf8_line<'a>(
    line: &'a [u8],
    line_number: usize,
    path: &Path,
) -> Result<&'a str, JournalError> {
    std::str::from_utf8(line).map_err(|error| {
        JournalError::new(
            JournalErrorKind::Corrupt,
            format!(
                "journal record is not UTF-8 at line {line_number} in {}: {error}",
                path.display()
            ),
        )
    })
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    path: &Path,
) -> Result<bool, JournalError> {
    line.clear();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| io_error("read journal record", path, error))?;
        if available.is_empty() {
            return Ok(!line.is_empty());
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(take) > MAX_JOURNAL_RECORD_BYTES {
            return Err(JournalError::new(
                JournalErrorKind::Corrupt,
                format!(
                    "journal record exceeds {MAX_JOURNAL_RECORD_BYTES} bytes in {}",
                    path.display()
                ),
            ));
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if line.last() == Some(&b'\n') {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(true);
        }
    }
}

fn read_previous_line(
    file: &mut File,
    cursor: &mut u64,
    path: &Path,
) -> Result<Option<ReverseLine>, JournalError> {
    let mut end = *cursor;
    if end == 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(end.saturating_sub(1)))
        .and_then(|_| {
            let mut byte = [0_u8; 1];
            file.read_exact(&mut byte)?;
            if byte[0] == b'\n' {
                end = end.saturating_sub(1);
            }
            Ok(())
        })
        .map_err(|error| io_error("read journal in reverse", path, error))?;
    if end == 0 {
        *cursor = 0;
        return Ok(None);
    }

    let mut search_end = end;
    let mut newer_chunks = Vec::<Vec<u8>>::new();
    loop {
        let chunk_start = search_end.saturating_sub(REVERSE_READ_CHUNK_BYTES as u64);
        let chunk_len = usize::try_from(search_end.saturating_sub(chunk_start)).map_err(|_| {
            JournalError::new(
                JournalErrorKind::Corrupt,
                "reverse journal read chunk does not fit in memory",
            )
        })?;
        let mut chunk = vec![0_u8; chunk_len];
        file.seek(SeekFrom::Start(chunk_start))
            .and_then(|_| file.read_exact(&mut chunk))
            .map_err(|error| io_error("read journal in reverse", path, error))?;

        if let Some(newline) = chunk.iter().rposition(|byte| *byte == b'\n') {
            let start_offset = chunk_start.saturating_add(newline as u64).saturating_add(1);
            let mut bytes = Vec::with_capacity(
                chunk.len().saturating_sub(newline + 1)
                    + newer_chunks.iter().map(Vec::len).sum::<usize>(),
            );
            bytes.extend_from_slice(&chunk[newline + 1..]);
            for newer in newer_chunks.iter().rev() {
                bytes.extend_from_slice(newer);
            }
            strip_cr_and_validate(&mut bytes, path)?;
            *cursor = start_offset;
            return Ok(Some(ReverseLine {
                bytes,
                start_offset,
            }));
        }

        newer_chunks.push(chunk);
        if chunk_start == 0 {
            let mut bytes = Vec::with_capacity(newer_chunks.iter().map(Vec::len).sum());
            for newer in newer_chunks.iter().rev() {
                bytes.extend_from_slice(newer);
            }
            strip_cr_and_validate(&mut bytes, path)?;
            *cursor = 0;
            return Ok(Some(ReverseLine {
                bytes,
                start_offset: 0,
            }));
        }
        search_end = chunk_start;
    }
}

fn strip_cr_and_validate(bytes: &mut Vec<u8>, path: &Path) -> Result<(), JournalError> {
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    if bytes.len() > MAX_JOURNAL_RECORD_BYTES {
        return Err(JournalError::new(
            JournalErrorKind::Corrupt,
            format!(
                "journal record exceeds {MAX_JOURNAL_RECORD_BYTES} bytes in {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn io_error(action: &str, path: &Path, error: std::io::Error) -> JournalError {
    JournalError::new(
        JournalErrorKind::Io,
        format!("failed to {action} {}: {error}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{decode_json_record, encode_json_record};
    use std::io::Write;

    #[test]
    fn tail_read_is_bounded_and_contiguous() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.jsonl");
        let mut file = File::create(&path).unwrap();
        for sequence in 1_u64..=100 {
            file.write_all(
                &encode_json_record(&serde_json::json!({"sequence": sequence}), "event").unwrap(),
            )
            .unwrap();
        }
        file.flush().unwrap();

        let page = read_tail(
            &path,
            JournalReadBudget::new(8, 1024 * 1024),
            |line, number| decode_json_record::<serde_json::Value>(line, number, "event"),
            |value| {
                value["sequence"]
                    .as_u64()
                    .ok_or_else(|| JournalError::new(JournalErrorKind::Codec, "missing sequence"))
            },
        )
        .unwrap();
        assert_eq!(page.items.len(), 8);
        assert_eq!(page.items[0]["sequence"], 93);
        assert_eq!(page.omitted_items, 92);
        assert_eq!(page.continuation.unwrap().before_sequence, 93);
        assert!(page.scanned_bytes < file.metadata().unwrap().len());
    }
}
