use super::*;

const REVERSE_READ_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionEventReadBudget {
    pub(crate) max_items: usize,
    pub(crate) max_bytes: usize,
}

impl SessionEventReadBudget {
    pub(crate) const fn new(max_items: usize, max_bytes: usize) -> Self {
        Self {
            max_items,
            max_bytes,
        }
    }

    fn validate(self) -> Result<Self, CodingSessionError> {
        if self.max_items == 0 || self.max_bytes == 0 {
            return Err(CodingSessionError::Input {
                message: "session event read budget must allow at least one item and one byte"
                    .into(),
            });
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionEventCursor {
    pub(crate) before_session_sequence: u64,
    byte_offset: u64,
}

impl SessionEventCursor {
    pub(crate) fn opaque_token(self) -> String {
        format!(
            "event-v1:{}:{}",
            self.before_session_sequence, self.byte_offset
        )
    }
}

#[derive(Debug)]
pub(crate) struct BoundedSessionEvents {
    pub(crate) events: Vec<SessionEventEnvelope>,
    pub(crate) omitted_items: usize,
    pub(crate) continuation: Option<SessionEventCursor>,
    #[cfg(test)]
    pub(crate) retained_bytes: usize,
    #[cfg(test)]
    pub(crate) scanned_bytes: u64,
}

#[derive(Debug)]
struct ReverseLine {
    bytes: Vec<u8>,
    start_offset: u64,
}

impl SessionLogStore {
    pub(crate) fn read_events_bounded(
        &self,
        handle: &SessionHandle,
        budget: SessionEventReadBudget,
    ) -> Result<BoundedSessionEvents, CodingSessionError> {
        let mut events = Vec::with_capacity(budget.max_items.min(1024));
        let mut metadata = self.visit_events_rev(handle, budget, |event| {
            events.push(event);
            Ok(())
        })?;
        events.reverse();
        metadata.events = events;
        Ok(metadata)
    }

    pub(crate) fn visit_events_rev(
        &self,
        handle: &SessionHandle,
        budget: SessionEventReadBudget,
        mut visitor: impl FnMut(SessionEventEnvelope) -> Result<(), CodingSessionError>,
    ) -> Result<BoundedSessionEvents, CodingSessionError> {
        let budget = budget.validate()?;
        let event_log_path = event_log_path(&handle.session_dir, &handle.manifest)?;
        let mut file = File::open(&event_log_path).map_err(|error| {
            session_error(format!(
                "failed to open session event log {}: {error}",
                event_log_path.display()
            ))
        })?;
        let file_len = file
            .metadata()
            .map_err(|error| {
                session_error(format!(
                    "failed to inspect session event log {}: {error}",
                    event_log_path.display()
                ))
            })?
            .len();
        let mut offset = file_len;
        let mut retained_items = 0_usize;
        let mut retained_bytes = 0_usize;
        let mut oldest_sequence = None;
        let mut oldest_offset = file_len;

        while retained_items < budget.max_items && retained_bytes < budget.max_bytes {
            let Some(line) = read_previous_line(&mut file, &mut offset, &event_log_path)? else {
                break;
            };
            if line.bytes.is_empty() {
                continue;
            }
            let frame_bytes = line.bytes.len().saturating_add(1);
            if retained_items > 0 && retained_bytes.saturating_add(frame_bytes) > budget.max_bytes {
                break;
            }
            let text = decode_utf8_line(&line.bytes, 0, &event_log_path)?;
            let event = decode_event_line(text, 0, &event_log_path)?;
            validate_event_for_session(&event, &handle.manifest.session_id)?;
            let sequence = event.session_sequence.ok_or_else(|| {
                session_error(format!(
                    "session event sequence is missing: event_id={}",
                    event.event_id
                ))
            })?;
            if let Some(previous_newer) = oldest_sequence
                && sequence.checked_add(1) != Some(previous_newer)
            {
                return Err(session_error(format!(
                    "session event sequence is not contiguous in reverse: expected={}, actual={sequence}",
                    previous_newer.saturating_sub(1)
                )));
            }
            oldest_sequence = Some(sequence);
            oldest_offset = line.start_offset;
            retained_items = retained_items.saturating_add(1);
            retained_bytes = retained_bytes.saturating_add(frame_bytes);
            visitor(event)?;
        }

        if offset == 0
            && let Some(oldest_sequence) = oldest_sequence
            && oldest_sequence != 1
        {
            return Err(session_error(format!(
                "session event sequence is not contiguous at the log start: expected=1, actual={oldest_sequence}"
            )));
        }
        let omitted_u64 = oldest_sequence
            .map(|sequence| sequence.saturating_sub(1))
            .unwrap_or_default();
        let omitted_items = usize::try_from(omitted_u64).unwrap_or(usize::MAX);
        let continuation = oldest_sequence.and_then(|sequence| {
            (sequence > 1).then_some(SessionEventCursor {
                before_session_sequence: sequence,
                byte_offset: oldest_offset,
            })
        });
        #[cfg(test)]
        let scanned_bytes = file_len.saturating_sub(offset);
        Ok(BoundedSessionEvents {
            events: Vec::new(),
            omitted_items,
            continuation,
            #[cfg(test)]
            retained_bytes,
            #[cfg(test)]
            scanned_bytes,
        })
    }
}

fn read_previous_line(
    file: &mut File,
    cursor: &mut u64,
    path: &Path,
) -> Result<Option<ReverseLine>, CodingSessionError> {
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
        .map_err(|error| reverse_read_error(path, error))?;
    if end == 0 {
        *cursor = 0;
        return Ok(None);
    }

    let mut search_end = end;
    let mut newer_chunks = Vec::<Vec<u8>>::new();
    loop {
        let chunk_start = search_end.saturating_sub(REVERSE_READ_CHUNK_BYTES as u64);
        let chunk_len = usize::try_from(search_end.saturating_sub(chunk_start))
            .map_err(|_| session_error("reverse session read chunk does not fit in memory"))?;
        let mut chunk = vec![0_u8; chunk_len];
        file.seek(SeekFrom::Start(chunk_start))
            .and_then(|_| file.read_exact(&mut chunk))
            .map_err(|error| reverse_read_error(path, error))?;

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

fn strip_cr_and_validate(bytes: &mut Vec<u8>, path: &Path) -> Result<(), CodingSessionError> {
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    if bytes.len() > MAX_SESSION_RECORD_BYTES {
        return Err(session_error(format!(
            "durable session record exceeds {MAX_SESSION_RECORD_BYTES} bytes in {}",
            path.display()
        )));
    }
    Ok(())
}

fn reverse_read_error(path: &Path, error: std::io::Error) -> CodingSessionError {
    session_error(format!(
        "failed to read durable session records in reverse from {}: {error}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::session::event::DiagnosticLevel;
    use crate::session::service::SessionService;
    use crate::session::view::CodingAgentSessionOptions;

    const LARGE_SESSION_EVENTS: u64 = 100_000;
    const TEST_PAGE_ITEMS: usize = 128;
    const TEST_PAGE_BYTES: usize = 1024 * 1024;

    #[test]
    fn hundred_thousand_event_hydration_read_is_time_and_memory_bounded() {
        let temp = tempfile::tempdir().expect("temp session root");
        let store = SessionLogStore::new(temp.path().join("sessions"));
        let handle = store
            .create_session(CreateSessionOptions::new(
                "bounded-hydration",
                "2026-08-02T00:00:00Z",
            ))
            .expect("create bounded hydration session");
        write_large_event_log(&handle);

        let started = Instant::now();
        let page = store
            .read_events_bounded(
                &handle,
                SessionEventReadBudget::new(TEST_PAGE_ITEMS, TEST_PAGE_BYTES),
            )
            .expect("read bounded hydration page");
        let elapsed = started.elapsed();

        assert_eq!(page.events.len(), TEST_PAGE_ITEMS);
        assert_eq!(
            page.omitted_items,
            usize::try_from(LARGE_SESSION_EVENTS).expect("fixture count fits usize")
                - TEST_PAGE_ITEMS
        );
        let continuation = page.continuation.expect("older events remain");
        assert_eq!(
            continuation.before_session_sequence,
            LARGE_SESSION_EVENTS - TEST_PAGE_ITEMS as u64 + 1
        );
        assert!(page.retained_bytes <= TEST_PAGE_BYTES);
        assert!(
            page.scanned_bytes
                <= u64::try_from(TEST_PAGE_BYTES + MAX_SESSION_RECORD_BYTES)
                    .expect("test byte budget fits u64")
        );
        assert!(
            page.events.capacity() <= TEST_PAGE_ITEMS,
            "bounded read must not reserve for the full log"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "reverse bootstrap read took {elapsed:?}"
        );

        let options = CodingAgentSessionOptions::new()
            .with_session_log_root(temp.path().join("sessions"))
            .with_session_id("bounded-hydration");
        let bootstrap_started = Instant::now();
        let hydration = SessionService::hydrate(&options).expect("hydrate bounded session");
        let bootstrap_elapsed = bootstrap_started.elapsed();

        assert_eq!(hydration.summary.session_id, "bounded-hydration");
        assert_eq!(hydration.summary.storage.session_id(), "bounded-hydration");
        assert_eq!(
            hydration.summary.storage.export_path(),
            handle.session_dir()
        );
        assert!(
            hydration
                .summary
                .storage
                .open_event_log()
                .expect("open event log through opaque storage handle")
                .metadata()
                .expect("event log metadata")
                .len()
                > 0
        );
        assert_eq!(hydration.cwd.as_deref(), Some("/workspace"));
        assert_eq!(hydration.diagnostics.len(), 10_000);
        assert_eq!(hydration.omitted_items, 90_000);
        assert_eq!(
            hydration
                .continuation
                .as_ref()
                .expect("full bootstrap exposes continuation")
                .before_session_sequence(),
            90_001
        );
        assert!(
            bootstrap_elapsed < Duration::from_secs(3),
            "bounded session bootstrap took {bootstrap_elapsed:?}"
        );
    }

    fn write_large_event_log(handle: &SessionHandle) {
        let path = event_log_path(&handle.session_dir, &handle.manifest)
            .expect("resolve fixture event log");
        let file = File::create(path).expect("replace fixture event log");
        let mut writer = BufWriter::new(file);
        for sequence in 1..=LARGE_SESSION_EVENTS {
            let data = if sequence == 1 {
                SessionEventData::SessionCreated {
                    cwd: Some("/workspace".into()),
                    workspace_scope: handle.manifest.workspace_scope.clone(),
                }
            } else {
                SessionEventData::DiagnosticEmitted {
                    level: DiagnosticLevel::Info,
                    message: format!("diagnostic-{sequence}"),
                }
            };
            let event = SessionEventEnvelope::new(
                handle.manifest.session_id.clone(),
                format!("event-{sequence}"),
                "2026-08-02T00:00:01Z",
                data,
            )
            .with_session_sequence(sequence);
            let encoded =
                encode_durable_record(&event, "session event").expect("encode fixture event");
            writer
                .write_all(&encoded)
                .and_then(|_| writer.write_all(b"\n"))
                .expect("write fixture event");
        }
        writer.flush().expect("flush fixture event log");
    }
}
