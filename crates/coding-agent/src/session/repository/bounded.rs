use super::*;
use event_journal::api::read::{JournalReadBudget, read_tail};

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

impl SessionLogStore {
    pub(crate) fn read_events_bounded(
        &self,
        handle: &SessionHandle,
        budget: SessionEventReadBudget,
    ) -> Result<BoundedSessionEvents, CodingSessionError> {
        self.read_event_tail(handle, budget)
    }

    fn read_event_tail(
        &self,
        handle: &SessionHandle,
        budget: SessionEventReadBudget,
    ) -> Result<BoundedSessionEvents, CodingSessionError> {
        let budget = budget.validate()?;
        let event_log_path = event_log_path(&handle.session_dir, &handle.manifest)?;
        let session_id = handle.manifest.session_id.clone();
        let page = read_tail(
            &event_log_path,
            JournalReadBudget::new(budget.max_items, budget.max_bytes),
            |line, line_number| {
                decode_event_line(line, line_number, &event_log_path).map_err(journal_codec_error)
            },
            |event| {
                validate_event_for_session(event, &session_id).map_err(journal_codec_error)?;
                event.session_sequence.ok_or_else(|| {
                    JournalError::codec(format!(
                        "session event sequence is missing: event_id={}",
                        event.event_id
                    ))
                })
            },
        )
        .map_err(journal_error)?;
        Ok(BoundedSessionEvents {
            events: page.items,
            omitted_items: page.omitted_items,
            continuation: page.continuation.map(|cursor| SessionEventCursor {
                before_session_sequence: cursor.before_sequence,
                byte_offset: cursor.byte_offset,
            }),
            #[cfg(test)]
            retained_bytes: page.retained_bytes,
            #[cfg(test)]
            scanned_bytes: page.scanned_bytes,
        })
    }
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
                <= u64::try_from(TEST_PAGE_BYTES + MAX_JOURNAL_RECORD_BYTES)
                    .expect("test byte budget fits u64")
        );
        assert!(page.events.capacity() <= TEST_PAGE_ITEMS);
        assert!(elapsed < Duration::from_secs(2));

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
        assert!(bootstrap_elapsed < Duration::from_secs(3));
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
            writer.write_all(&encoded).expect("write fixture event");
        }
        writer.flush().expect("flush fixture event log");
    }
}
