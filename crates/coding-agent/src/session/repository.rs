use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, value::RawValue};
use sha2::{Digest, Sha256};

use super::manifest::{
    EVENT_SCHEMA, EVENT_VERSION, SESSION_EVENT_LOG_FILE, SESSION_MANIFEST_FILE,
    SESSION_OUTBOX_LOG_FILE, SESSION_SCHEMA, SESSION_VERSION, SessionManifest,
    default_agent_profile_id,
};
use super::replay::{ReplayFold, ReplayIndex, SessionReplay};
use crate::events::outbox::{
    DurableOutboxRecord, DurableOutboxRecordCandidate, OUTBOX_SCHEMA, OUTBOX_VERSION,
};
use crate::runtime::facade::{CodingSessionError, ProfileId};
use crate::session::event::SessionEventEnvelope;

const SESSION_WRITER_LOCK_FILE: &str = ".writer.lock";
const MAX_SESSION_RECORD_BYTES: usize = 1024 * 1024;
const MAX_SESSION_PAYLOAD_BYTES: usize = MAX_SESSION_RECORD_BYTES - 4096;
const DURABLE_FRAME_SCHEMA: &str = "evo.session.frame";
const DURABLE_FRAME_VERSION: u32 = 2;
#[cfg(test)]
const DURABLE_FRAME_FIELD: &str = "_evo_frame";
static MANIFEST_TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableFrameMetadata {
    schema: String,
    version: u32,
    payload_bytes: u32,
    sha256: String,
}

#[derive(Debug, serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableFrame {
    #[serde(rename = "_evo_frame")]
    metadata: DurableFrameMetadata,
    payload: Box<RawValue>,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionLogStore {
    root: PathBuf,
    append_lock: Arc<Mutex<()>>,
    #[cfg(test)]
    sequence_scans: Arc<AtomicUsize>,
    #[cfg(test)]
    failures: Arc<Mutex<StoreFailureState>>,
}

#[derive(Debug)]
pub(crate) struct SessionWriteLease {
    _lock_file: File,
    next_sequence: u64,
    tail_recoveries: Vec<String>,
}

impl SessionWriteLease {
    pub(crate) fn committed_sequence(&self) -> u64 {
        self.next_sequence.saturating_sub(1)
    }

    pub(crate) fn tail_recoveries(&self) -> &[String] {
        &self.tail_recoveries
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreFailurePoint {
    CreateBlobs,
    CreateIndex,
    WriteManifest,
    CreateEventLog,
    AppendEvents,
    AppendOutbox,
    UpdateManifest,
    RemoveSession,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct StoreFailureState {
    create_blobs: Option<usize>,
    create_index: Option<usize>,
    write_manifest: Option<usize>,
    create_event_log: Option<usize>,
    append_events: Option<usize>,
    append_outbox: Option<usize>,
    update_manifest: Option<usize>,
    remove_session: Option<usize>,
}

#[derive(Debug)]
pub(crate) enum SessionCreateError {
    Create(CodingSessionError),
    CleanupFailed {
        session_id: String,
        session_dir: PathBuf,
        create_error: CodingSessionError,
        cleanup_error: CodingSessionError,
    },
}

impl SessionCreateError {
    #[cfg(test)]
    pub(crate) fn code(&self) -> &'static str {
        "session"
    }
}

impl fmt::Display for SessionCreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create(error) => error.fmt(formatter),
            Self::CleanupFailed {
                session_id,
                session_dir,
                create_error,
                cleanup_error,
            } => write!(
                formatter,
                "session initialization failed for {session_id} at {}: {create_error}; cleanup failed: {cleanup_error}",
                session_dir.display()
            ),
        }
    }
}

impl std::error::Error for SessionCreateError {}

impl From<CodingSessionError> for SessionCreateError {
    fn from(error: CodingSessionError) -> Self {
        Self::Create(error)
    }
}

impl From<SessionCreateError> for CodingSessionError {
    fn from(error: SessionCreateError) -> Self {
        match error {
            SessionCreateError::Create(error) => error,
            cleanup_failed @ SessionCreateError::CleanupFailed { .. } => {
                CodingSessionError::Session {
                    message: cleanup_failed.to_string(),
                }
            }
        }
    }
}

impl SessionLogStore {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            append_lock: Arc::new(Mutex::new(())),
            #[cfg(test)]
            sequence_scans: Arc::new(AtomicUsize::new(0)),
            #[cfg(test)]
            failures: Arc::new(Mutex::new(StoreFailureState::default())),
        }
    }

    pub(crate) fn acquire_write_lease(
        &self,
        handle: &SessionHandle,
    ) -> Result<SessionWriteLease, CodingSessionError> {
        let lock_path = handle.session_dir.join(SESSION_WRITER_LOCK_FILE);
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| {
                session_error(format!(
                    "failed to open session writer lock {}: {error}",
                    lock_path.display()
                ))
            })?;
        if let Err(error) = lock_file.try_lock() {
            return Err(match error {
                std::fs::TryLockError::WouldBlock => session_error(format!(
                    "session already has a writer in another process (lock {})",
                    lock_path.display()
                )),
                std::fs::TryLockError::Error(error) => session_error(format!(
                    "failed to acquire session writer lock {}: {error}",
                    lock_path.display()
                )),
            });
        }
        let event_log_path = event_log_path(&handle.session_dir, &handle.manifest)?;
        let outbox_path = outbox_log_path(&handle.session_dir, &handle.manifest)?;
        let mut tail_recoveries = Vec::new();
        if let Some(recovery) =
            repair_unterminated_tail(&event_log_path, "session event", |line| {
                decode_event_line(line, 0, &event_log_path).map(|_| ())
            })?
        {
            tail_recoveries.push(recovery);
        }
        if let Some(recovery) =
            repair_unterminated_tail(&outbox_path, "session outbox record", |line| {
                decode_durable_record::<DurableOutboxRecord>(
                    line,
                    0,
                    &outbox_path,
                    "session outbox record",
                )
                .map(|_| ())
            })?
        {
            tail_recoveries.push(recovery);
        }
        let next_sequence =
            self.next_session_sequence(&event_log_path, &handle.manifest.session_id)?;
        Ok(SessionWriteLease {
            _lock_file: lock_file,
            next_sequence,
            tail_recoveries,
        })
    }

    #[cfg(test)]
    pub(crate) fn sequence_scan_count(&self) -> usize {
        self.sequence_scans.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn fail_after(&self, point: StoreFailurePoint, successful_calls: usize) {
        let mut failures = self.failures.lock().unwrap();
        let target = match point {
            StoreFailurePoint::CreateBlobs => &mut failures.create_blobs,
            StoreFailurePoint::CreateIndex => &mut failures.create_index,
            StoreFailurePoint::WriteManifest => &mut failures.write_manifest,
            StoreFailurePoint::CreateEventLog => &mut failures.create_event_log,
            StoreFailurePoint::AppendEvents => &mut failures.append_events,
            StoreFailurePoint::AppendOutbox => &mut failures.append_outbox,
            StoreFailurePoint::UpdateManifest => &mut failures.update_manifest,
            StoreFailurePoint::RemoveSession => &mut failures.remove_session,
        };
        *target = Some(successful_calls);
    }

    #[cfg(test)]
    fn fail_if_injected(&self, point: StoreFailurePoint) -> Result<(), CodingSessionError> {
        let mut failures = self.failures.lock().unwrap();
        let target = match point {
            StoreFailurePoint::CreateBlobs => &mut failures.create_blobs,
            StoreFailurePoint::CreateIndex => &mut failures.create_index,
            StoreFailurePoint::WriteManifest => &mut failures.write_manifest,
            StoreFailurePoint::CreateEventLog => &mut failures.create_event_log,
            StoreFailurePoint::AppendEvents => &mut failures.append_events,
            StoreFailurePoint::AppendOutbox => &mut failures.append_outbox,
            StoreFailurePoint::UpdateManifest => &mut failures.update_manifest,
            StoreFailurePoint::RemoveSession => &mut failures.remove_session,
        };
        let Some(remaining) = target.as_mut() else {
            return Ok(());
        };
        if *remaining > 0 {
            *remaining -= 1;
            return Ok(());
        }
        *target = None;
        if point == StoreFailurePoint::AppendEvents {
            Err(CodingSessionError::SessionWriteRejected {
                message: format!("injected session store failure at {point:?}"),
            })
        } else {
            Err(session_error(format!(
                "injected session store failure at {point:?}"
            )))
        }
    }

    #[allow(
        clippy::result_large_err,
        reason = "session creation errors retain typed cleanup and partial-initialization evidence"
    )]
    pub(crate) fn create_session(
        &self,
        options: CreateSessionOptions,
    ) -> Result<SessionHandle, SessionCreateError> {
        let session_id = normalize_session_id(&options.session_id)?;
        fs::create_dir_all(&self.root).map_err(|error| {
            session_error(format!(
                "failed to create session log root {}: {error}",
                self.root.display()
            ))
        })?;

        let session_dir = self.root.join(&session_id);
        if session_dir.exists() {
            return Err(session_error(format!(
                "session directory already exists: {}",
                session_dir.display()
            ))
            .into());
        }

        fs::create_dir(&session_dir).map_err(|error| {
            session_error(format!(
                "failed to create session directory {}: {error}",
                session_dir.display()
            ))
        })?;
        let manifest = SessionManifest::new(session_id.clone(), options.created_at)
            .with_name(options.name)
            .with_default_agent_profile_id(options.default_agent_profile_id);
        let initialization = (|| -> Result<(), CodingSessionError> {
            #[cfg(test)]
            self.fail_if_injected(StoreFailurePoint::CreateBlobs)?;
            fs::create_dir(session_dir.join("blobs")).map_err(|error| {
                session_error(format!(
                    "failed to create blobs directory for {session_id}: {error}"
                ))
            })?;
            #[cfg(test)]
            self.fail_if_injected(StoreFailurePoint::CreateIndex)?;
            fs::create_dir(session_dir.join("index")).map_err(|error| {
                session_error(format!(
                    "failed to create index directory for {session_id}: {error}"
                ))
            })?;
            #[cfg(test)]
            self.fail_if_injected(StoreFailurePoint::WriteManifest)?;
            write_manifest(&session_dir, &manifest)?;
            #[cfg(test)]
            self.fail_if_injected(StoreFailurePoint::CreateEventLog)?;
            create_empty_event_log(&session_dir)?;
            create_empty_outbox_log(&session_dir)?;
            sync_directory(&session_dir)
        })();
        if let Err(create_error) = initialization {
            return Err(match self.remove_created_session_dir(&session_dir) {
                Ok(()) => SessionCreateError::Create(create_error),
                Err(cleanup_error) => SessionCreateError::CleanupFailed {
                    session_id,
                    session_dir,
                    create_error,
                    cleanup_error,
                },
            });
        }

        Ok(SessionHandle {
            session_dir,
            manifest,
        })
    }

    pub(crate) fn open_session(&self, path: &Path) -> Result<SessionHandle, CodingSessionError> {
        let session_dir = self.resolve_existing_session_dir(path)?;
        let manifest = read_manifest(&session_dir)?;

        validate_manifest(&manifest)?;
        let event_log_path = event_log_path(&session_dir, &manifest)?;
        if !event_log_path.is_file() {
            return Err(session_error(format!(
                "session event log is missing: {}",
                event_log_path.display()
            )));
        }
        let outbox_path = outbox_log_path(&session_dir, &manifest)?;
        if !outbox_path.is_file() {
            create_empty_outbox_log(&session_dir)?;
            sync_directory(&session_dir)?;
        }

        Ok(SessionHandle {
            session_dir,
            manifest,
        })
    }

    pub(crate) fn open_session_id(
        &self,
        session_id: &str,
    ) -> Result<SessionHandle, CodingSessionError> {
        let session_id = normalize_session_id(session_id)?;
        self.open_session(Path::new(&session_id))
    }

    pub(crate) fn try_open_session_id(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionHandle>, CodingSessionError> {
        let session_id = normalize_session_id(session_id)?;
        let candidate = self.root.join(&session_id);
        if !candidate.exists() {
            return Ok(None);
        }
        self.open_session(Path::new(&session_id)).map(Some)
    }

    pub(crate) fn list_sessions(&self) -> Result<Vec<SessionSummary>, CodingSessionError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(session_error(format!(
                    "failed to read session log root {}: {error}",
                    self.root.display()
                )));
            }
        };

        let mut sessions = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                session_error(format!(
                    "failed to read session log root entry {}: {error}",
                    self.root.display()
                ))
            })?;
            let file_type = entry.file_type().map_err(|error| {
                session_error(format!(
                    "failed to inspect session log root entry {}: {error}",
                    entry.path().display()
                ))
            })?;
            if !file_type.is_dir() {
                continue;
            }

            let session_dir = entry.path();
            if !session_dir.join(SESSION_MANIFEST_FILE).is_file() {
                continue;
            }

            let handle = self.open_session(&session_dir)?;
            sessions.push(SessionSummary::from_handle(&handle));
        }

        sessions.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        Ok(sessions)
    }

    #[cfg(test)]
    pub(crate) fn append_events(
        &self,
        handle: &SessionHandle,
        events: &[SessionEventEnvelope],
    ) -> Result<(), CodingSessionError> {
        let _append_guard = self.append_lock.lock().unwrap();
        let event_log_path = event_log_path(&handle.session_dir, &handle.manifest)?;
        let mut lease = SessionWriteLease {
            _lock_file: OpenOptions::new()
                .read(true)
                .write(true)
                .open(&event_log_path)
                .map_err(|error| {
                    session_error(format!(
                        "failed to open test session event log {}: {error}",
                        event_log_path.display()
                    ))
                })?,
            next_sequence: self
                .next_session_sequence(&event_log_path, &handle.manifest.session_id)?,
            tail_recoveries: Vec::new(),
        };
        self.append_events_locked(handle, events, &mut lease)
            .map(|_| ())
    }

    pub(crate) fn append_events_with_cursor(
        &self,
        handle: &SessionHandle,
        events: &[SessionEventEnvelope],
        lease: &mut SessionWriteLease,
    ) -> Result<u64, CodingSessionError> {
        let _append_guard = self.append_lock.lock().unwrap();
        self.append_events_locked(handle, events, lease)
    }

    pub(crate) fn append_events_and_outbox(
        &self,
        handle: &SessionHandle,
        events: &[SessionEventEnvelope],
        records: &[DurableOutboxRecordCandidate],
        lease: &mut SessionWriteLease,
    ) -> Result<u64, CodingSessionError> {
        let _append_guard = self.append_lock.lock().unwrap();
        let prepared_events = self.prepare_events_locked(handle, events, lease.next_sequence)?;
        let committed_through_session_sequence = prepared_events
            .last()
            .and_then(|event| event.session_sequence)
            .ok_or_else(|| {
                session_error("outbox commit requires at least one sequenced session event")
            })?;
        let source_event_ids = prepared_events
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let records = records
            .iter()
            .cloned()
            .map(|candidate| {
                if candidate.session_id != handle.manifest.session_id {
                    return Err(session_error(format!(
                        "outbox session {} does not match session {}",
                        candidate.session_id, handle.manifest.session_id
                    )));
                }
                if candidate
                    .source_event_ids
                    .iter()
                    .any(|event_id| !source_event_ids.contains(event_id.as_str()))
                {
                    return Err(session_error(format!(
                        "outbox record {} references an event outside its commit batch",
                        candidate.record_id
                    )));
                }
                candidate
                    .commit(committed_through_session_sequence)
                    .map_err(session_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        // Ordering is intentional: a durable outbox-only tail is explicit
        // recovery evidence, while events are never made visible before their
        // publication obligation is durable.
        self.append_outbox_locked(handle, &records)?;
        self.append_prepared_events_locked(handle, &prepared_events)?;
        lease.next_sequence = committed_through_session_sequence
            .checked_add(1)
            .ok_or_else(|| session_error("session event sequence overflowed"))?;
        Ok(committed_through_session_sequence)
    }

    fn append_events_locked(
        &self,
        handle: &SessionHandle,
        events: &[SessionEventEnvelope],
        lease: &mut SessionWriteLease,
    ) -> Result<u64, CodingSessionError> {
        let prepared_events = self.prepare_events_locked(handle, events, lease.next_sequence)?;
        self.append_prepared_events_locked(handle, &prepared_events)?;
        let committed_sequence = prepared_events
            .last()
            .and_then(|event| event.session_sequence)
            .unwrap_or_else(|| lease.committed_sequence());
        lease.next_sequence = committed_sequence
            .checked_add(1)
            .ok_or_else(|| session_error("session event sequence overflowed"))?;
        Ok(committed_sequence)
    }

    fn prepare_events_locked(
        &self,
        handle: &SessionHandle,
        events: &[SessionEventEnvelope],
        next_sequence: u64,
    ) -> Result<Vec<SessionEventEnvelope>, CodingSessionError> {
        (next_sequence..)
            .zip(events)
            .map(|(next_sequence, event)| {
                let event = event.clone().with_session_sequence(next_sequence);
                validate_event_for_session(&event, &handle.manifest.session_id)?;
                Ok(event)
            })
            .collect()
    }

    fn next_session_sequence(
        &self,
        event_log_path: &Path,
        session_id: &str,
    ) -> Result<u64, CodingSessionError> {
        #[cfg(test)]
        self.sequence_scans.fetch_add(1, Ordering::AcqRel);
        next_session_sequence(event_log_path, session_id)
    }

    fn append_prepared_events_locked(
        &self,
        handle: &SessionHandle,
        events: &[SessionEventEnvelope],
    ) -> Result<(), CodingSessionError> {
        #[cfg(test)]
        self.fail_if_injected(StoreFailurePoint::AppendEvents)?;
        let event_log_path = event_log_path(&handle.session_dir, &handle.manifest)?;
        let records = events
            .iter()
            .map(|event| encode_durable_record(event, "session event"))
            .collect::<Result<Vec<_>, _>>()?;
        let file = OpenOptions::new()
            .append(true)
            .open(&event_log_path)
            .map_err(|error| {
                session_error(format!(
                    "failed to open session event log {}: {error}",
                    event_log_path.display()
                ))
            })?;
        let mut writer = BufWriter::new(file);

        for record in records {
            writer.write_all(&record).map_err(|error| {
                session_error(format!(
                    "failed to append session event to {}: {error}",
                    event_log_path.display()
                ))
            })?;
        }

        writer.flush().map_err(|error| {
            session_error(format!(
                "failed to flush session event log {}: {error}",
                event_log_path.display()
            ))
        })?;
        writer.get_ref().sync_data().map_err(|error| {
            session_error(format!(
                "failed to sync session event log {}: {error}",
                event_log_path.display()
            ))
        })
    }

    fn append_outbox_locked(
        &self,
        handle: &SessionHandle,
        records: &[DurableOutboxRecord],
    ) -> Result<(), CodingSessionError> {
        if records.is_empty() {
            return Ok(());
        }
        #[cfg(test)]
        self.fail_if_injected(StoreFailurePoint::AppendOutbox)?;
        let outbox_path = outbox_log_path(&handle.session_dir, &handle.manifest)?;
        let records = records
            .iter()
            .map(|record| encode_durable_record(record, "session outbox record"))
            .collect::<Result<Vec<_>, _>>()?;
        let file = OpenOptions::new()
            .append(true)
            .open(&outbox_path)
            .map_err(|error| {
                session_error(format!(
                    "failed to open session outbox {}: {error}",
                    outbox_path.display()
                ))
            })?;
        let mut writer = BufWriter::new(file);
        for record in records {
            writer.write_all(&record).map_err(|error| {
                session_error(format!(
                    "failed to append session outbox record to {}: {error}",
                    outbox_path.display()
                ))
            })?;
        }
        writer.flush().map_err(|error| {
            session_error(format!(
                "failed to flush session outbox {}: {error}",
                outbox_path.display()
            ))
        })?;
        writer.get_ref().sync_data().map_err(|error| {
            session_error(format!(
                "failed to sync session outbox {}: {error}",
                outbox_path.display()
            ))
        })
    }

    pub(crate) fn read_events(
        &self,
        handle: &SessionHandle,
    ) -> Result<Vec<SessionEventEnvelope>, CodingSessionError> {
        let mut events = Vec::new();
        self.visit_events(handle, |event| {
            events.push(event);
            Ok(())
        })?;
        Ok(events)
    }

    fn visit_events(
        &self,
        handle: &SessionHandle,
        mut visitor: impl FnMut(SessionEventEnvelope) -> Result<(), CodingSessionError>,
    ) -> Result<(), CodingSessionError> {
        let event_log_path = event_log_path(&handle.session_dir, &handle.manifest)?;
        let file = File::open(&event_log_path).map_err(|error| {
            session_error(format!(
                "failed to open session event log {}: {error}",
                event_log_path.display()
            ))
        })?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut expected_sequence = 0_u64;
        let mut line_number = 0_usize;
        while read_bounded_line(&mut reader, &mut line, &event_log_path)? {
            line_number += 1;
            let line = decode_utf8_line(&line, line_number, &event_log_path)?;
            if line.trim().is_empty() {
                continue;
            }
            expected_sequence = expected_sequence
                .checked_add(1)
                .ok_or_else(|| session_error("session event sequence overflowed"))?;
            let event = decode_event_line(line, line_number, &event_log_path)?;
            validate_contiguous_session_sequence(&event, expected_sequence)?;
            validate_event_for_session(&event, &handle.manifest.session_id)?;
            visitor(event)?;
        }
        Ok(())
    }

    pub(crate) fn replay_session(
        &self,
        handle: &SessionHandle,
    ) -> Result<SessionReplay, CodingSessionError> {
        let mut index = ReplayIndex::default();
        self.visit_events(handle, |event| {
            index.observe(&event);
            Ok(())
        })?;
        let mut fold = ReplayFold::new(index);
        self.visit_events(handle, |event| {
            fold.observe(&event);
            Ok(())
        })?;
        Ok(fold.finish())
    }

    pub(crate) fn read_outbox(
        &self,
        handle: &SessionHandle,
    ) -> Result<Vec<DurableOutboxRecord>, CodingSessionError> {
        let outbox_path = outbox_log_path(&handle.session_dir, &handle.manifest)?;
        let file = File::open(&outbox_path).map_err(|error| {
            session_error(format!(
                "failed to open session outbox {}: {error}",
                outbox_path.display()
            ))
        })?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut records = Vec::new();
        let mut record_ids = std::collections::HashSet::new();
        let mut previous_sequence = 0_u64;
        let mut line_number = 0_usize;
        while read_bounded_line(&mut reader, &mut line, &outbox_path)? {
            line_number += 1;
            let line = decode_utf8_line(&line, line_number, &outbox_path)?;
            if line.trim().is_empty() {
                continue;
            }
            let record: DurableOutboxRecord =
                decode_durable_record(line, line_number, &outbox_path, "session outbox record")?;
            if record.schema != OUTBOX_SCHEMA || record.version != OUTBOX_VERSION {
                return Err(session_error(format!(
                    "unsupported session outbox schema/version at {} line {}",
                    outbox_path.display(),
                    line_number
                )));
            }
            if record.session_id != handle.manifest.session_id
                || record.committed_through_session_sequence == 0
                || record.source_event_ids.is_empty()
                || record.operation_kind.as_deref().is_some_and(|kind| {
                    kind.trim().is_empty()
                        || crate::runtime::control::OperationKind::from_str(kind).is_none()
                })
                || record
                    .source_event_ids
                    .iter()
                    .any(|event_id| event_id.trim().is_empty())
            {
                return Err(session_error(format!(
                    "invalid session outbox identity at {} line {}",
                    outbox_path.display(),
                    line_number
                )));
            }
            if record.committed_through_session_sequence < previous_sequence {
                return Err(session_error(format!(
                    "session outbox cursor regressed at {} line {}",
                    outbox_path.display(),
                    line_number
                )));
            }
            if !record_ids.insert(record.record_id.clone()) {
                return Err(session_error(format!(
                    "duplicate session outbox record {}",
                    record.record_id
                )));
            }
            previous_sequence = record.committed_through_session_sequence;
            records.push(record);
        }
        Ok(records)
    }

    pub(crate) fn update_manifest(
        &self,
        handle: &SessionHandle,
        patch: ManifestPatch,
    ) -> Result<(), CodingSessionError> {
        #[cfg(test)]
        self.fail_if_injected(StoreFailurePoint::UpdateManifest)?;
        let mut manifest = read_manifest(&handle.session_dir)?;
        patch.apply(&mut manifest);
        validate_manifest(&manifest)?;
        write_manifest(&handle.session_dir, &manifest)
    }

    pub(crate) fn remove_session(&self, handle: &SessionHandle) -> Result<(), CodingSessionError> {
        let session_dir = self.resolve_existing_session_dir(handle.session_dir())?;
        self.remove_created_session_dir(&session_dir)
    }

    fn remove_created_session_dir(&self, session_dir: &Path) -> Result<(), CodingSessionError> {
        #[cfg(test)]
        self.fail_if_injected(StoreFailurePoint::RemoveSession)?;
        fs::remove_dir_all(session_dir).map_err(|error| {
            session_error(format!(
                "failed to remove session directory {}: {error}",
                session_dir.display()
            ))
        })
    }

    fn resolve_existing_session_dir(&self, path: &Path) -> Result<PathBuf, CodingSessionError> {
        let root = self.root.canonicalize().map_err(|error| {
            session_error(format!(
                "failed to resolve session log root {}: {error}",
                self.root.display()
            ))
        })?;
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        let session_dir = candidate.canonicalize().map_err(|error| {
            session_error(format!(
                "failed to resolve session directory {}: {error}",
                candidate.display()
            ))
        })?;
        if !session_dir.starts_with(&root) {
            return Err(session_error(format!(
                "session directory is outside store root: {}",
                session_dir.display()
            )));
        }
        if !session_dir.is_dir() {
            return Err(session_error(format!(
                "session path is not a directory: {}",
                session_dir.display()
            )));
        }
        Ok(session_dir)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreateSessionOptions {
    pub(crate) session_id: String,
    pub(crate) name: Option<String>,
    pub(crate) created_at: String,
    pub(crate) default_agent_profile_id: ProfileId,
}

impl CreateSessionOptions {
    pub(crate) fn new(session_id: impl Into<String>, created_at: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            name: None,
            created_at: created_at.into(),
            default_agent_profile_id: default_agent_profile_id(),
        }
    }

    pub(crate) fn default_agent_profile_id(mut self, profile_id: ProfileId) -> Self {
        self.default_agent_profile_id = profile_id;
        self
    }

    pub(crate) fn name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionHandle {
    session_dir: PathBuf,
    manifest: SessionManifest,
}

impl SessionHandle {
    pub(crate) fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    pub(crate) fn manifest(&self) -> &SessionManifest {
        &self.manifest
    }

    #[cfg(test)]
    pub(crate) fn event_log_path(&self) -> Result<PathBuf, CodingSessionError> {
        event_log_path(&self.session_dir, &self.manifest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSummary {
    pub(crate) session_id: String,
    pub(crate) name: Option<String>,
    pub(crate) session_dir: PathBuf,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) active_leaf_id: Option<String>,
}

impl SessionSummary {
    fn from_handle(handle: &SessionHandle) -> Self {
        Self {
            session_id: handle.manifest.session_id.clone(),
            name: handle.manifest.name.clone(),
            session_dir: handle.session_dir.clone(),
            created_at: handle.manifest.created_at.clone(),
            updated_at: handle.manifest.updated_at.clone(),
            active_leaf_id: handle.manifest.active_leaf_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ManifestPatch {
    updated_at: Option<String>,
    name: Option<Option<String>>,
    active_branch_id: Option<Option<String>>,
    active_leaf_id: Option<Option<String>>,
    default_agent_profile_id: Option<ProfileId>,
}

impl ManifestPatch {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn updated_at(mut self, updated_at: impl Into<String>) -> Self {
        self.updated_at = Some(updated_at.into());
        self
    }

    pub(crate) fn name(mut self, name: Option<String>) -> Self {
        self.name = Some(name);
        self
    }

    #[cfg(test)]
    pub(crate) fn active_branch_id(mut self, active_branch_id: Option<String>) -> Self {
        self.active_branch_id = Some(active_branch_id);
        self
    }

    pub(crate) fn active_leaf_id(mut self, active_leaf_id: Option<String>) -> Self {
        self.active_leaf_id = Some(active_leaf_id);
        self
    }

    pub(crate) fn default_agent_profile_id(mut self, profile_id: ProfileId) -> Self {
        self.default_agent_profile_id = Some(profile_id);
        self
    }

    fn apply(self, manifest: &mut SessionManifest) {
        if let Some(updated_at) = self.updated_at {
            manifest.updated_at = updated_at;
        }
        if let Some(name) = self.name {
            manifest.name = name;
        }
        if let Some(active_branch_id) = self.active_branch_id {
            manifest.active_branch_id = active_branch_id;
        }
        if let Some(active_leaf_id) = self.active_leaf_id {
            manifest.active_leaf_id = active_leaf_id;
        }
        if let Some(default_agent_profile_id) = self.default_agent_profile_id {
            manifest.default_agent_profile_id = default_agent_profile_id;
        }
    }
}

fn normalize_session_id(value: &str) -> Result<String, CodingSessionError> {
    let session_id = value.trim();
    if session_id.is_empty() {
        return Err(session_error("session id must not be empty"));
    }
    if !session_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(session_error(format!(
            "session id contains unsupported path characters: {session_id}"
        )));
    }
    Ok(session_id.to_owned())
}

fn write_manifest(
    session_dir: &Path,
    manifest: &SessionManifest,
) -> Result<(), CodingSessionError> {
    let manifest_path = session_dir.join(SESSION_MANIFEST_FILE);
    let mut bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| session_error(format!("failed to serialize session manifest: {error}")))?;
    bytes.push(b'\n');
    let temp_id = MANIFEST_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_path = session_dir.join(format!(
        ".{SESSION_MANIFEST_FILE}.tmp.{}.{}",
        std::process::id(),
        temp_id
    ));
    let result = (|| {
        let mut temp = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .map_err(|error| {
                session_error(format!(
                    "failed to create temporary session manifest {}: {error}",
                    temp_path.display()
                ))
            })?;
        temp.write_all(&bytes).map_err(|error| {
            session_error(format!(
                "failed to write temporary session manifest {}: {error}",
                temp_path.display()
            ))
        })?;
        temp.sync_all().map_err(|error| {
            session_error(format!(
                "failed to sync temporary session manifest {}: {error}",
                temp_path.display()
            ))
        })?;
        fs::rename(&temp_path, &manifest_path).map_err(|error| {
            session_error(format!(
                "failed to atomically replace session manifest {}: {error}",
                manifest_path.display()
            ))
        })?;
        sync_directory(session_dir)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn read_manifest(session_dir: &Path) -> Result<SessionManifest, CodingSessionError> {
    let manifest_path = session_dir.join(SESSION_MANIFEST_FILE);
    let content = fs::read_to_string(&manifest_path).map_err(|error| {
        session_error(format!(
            "failed to read session manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    decode_manifest(&content, &manifest_path)
}

fn decode_manifest(
    content: &str,
    manifest_path: &Path,
) -> Result<SessionManifest, CodingSessionError> {
    let value: Value = serde_json::from_str(content).map_err(|error| {
        session_error(format!(
            "failed to parse session manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let schema = json_string_field(&value, "schema");
    let version = json_u32_field(&value, "version");
    match (schema.as_deref(), version) {
        (Some(SESSION_SCHEMA), Some(SESSION_VERSION)) => {
            serde_json::from_value(value).map_err(|error| {
                session_error(format!(
                    "failed to decode v{SESSION_VERSION} session manifest {}: {error}",
                    manifest_path.display()
                ))
            })
        }
        _ => Err(session_error(format!(
            "unsupported session manifest decoder: schema={}, version={}; recovery: open with a compatible evo release or migrate the manifest",
            schema.as_deref().unwrap_or("<missing>"),
            version.map_or_else(|| "<missing>".to_owned(), |value| value.to_string()),
        ))),
    }
}

fn decode_event_line(
    line: &str,
    line_number: usize,
    event_log_path: &Path,
) -> Result<SessionEventEnvelope, CodingSessionError> {
    let value = decode_durable_value(line, line_number, event_log_path, "session event")?;
    let schema = json_string_field(&value, "schema");
    let version = json_u32_field(&value, "version");
    let event_id = json_string_field(&value, "event_id");
    match (schema.as_deref(), version) {
        (Some(EVENT_SCHEMA), Some(EVENT_VERSION)) => serde_json::from_value(value).map_err(|error| {
            session_error(format!(
                "failed to decode v{EVENT_VERSION} session event at line {line_number} in {}: {error}",
                event_log_path.display()
            ))
        }),
        _ => Err(session_error(format!(
            "unsupported session event decoder: schema={}, version={}, event_id={}; recovery: open with a compatible evo release or migrate the session event log",
            schema.as_deref().unwrap_or("<missing>"),
            version.map_or_else(|| "<missing>".to_owned(), |value| value.to_string()),
            event_id.as_deref().unwrap_or("<missing>"),
        ))),
    }
}

fn encode_durable_record(
    record: &impl Serialize,
    kind: &str,
) -> Result<Vec<u8>, CodingSessionError> {
    let payload = serde_json::to_vec(record)
        .map_err(|error| session_error(format!("failed to serialize {kind}: {error}")))?;
    if payload.len() > MAX_SESSION_PAYLOAD_BYTES {
        return Err(CodingSessionError::SessionWriteRejected {
            message: format!("{kind} payload exceeds {MAX_SESSION_PAYLOAD_BYTES} bytes"),
        });
    }
    let payload = String::from_utf8(payload)
        .map_err(|error| session_error(format!("failed to encode {kind} as UTF-8: {error}")))?;
    let raw_payload = RawValue::from_string(payload)
        .map_err(|error| session_error(format!("failed to frame {kind} payload: {error}")))?;
    let payload_bytes = raw_payload.get().as_bytes();
    let metadata = DurableFrameMetadata {
        schema: DURABLE_FRAME_SCHEMA.into(),
        version: DURABLE_FRAME_VERSION,
        payload_bytes: payload_bytes
            .len()
            .try_into()
            .map_err(|_| session_error(format!("{kind} payload length overflowed")))?,
        sha256: format!("{:x}", Sha256::digest(payload_bytes)),
    };
    let mut framed = serde_json::to_vec(&DurableFrame {
        metadata,
        payload: raw_payload,
    })
    .map_err(|error| session_error(format!("failed to frame {kind}: {error}")))?;
    if framed.len().saturating_add(1) > MAX_SESSION_RECORD_BYTES {
        return Err(CodingSessionError::SessionWriteRejected {
            message: format!("framed {kind} exceeds {MAX_SESSION_RECORD_BYTES} bytes"),
        });
    }
    framed.push(b'\n');
    Ok(framed)
}

fn decode_durable_record<T: DeserializeOwned>(
    line: &str,
    line_number: usize,
    path: &Path,
    kind: &str,
) -> Result<T, CodingSessionError> {
    let value = decode_durable_value(line, line_number, path, kind)?;
    serde_json::from_value(value).map_err(|error| {
        session_error(format!(
            "failed to decode {kind} at line {line_number} in {}: {error}",
            path.display()
        ))
    })
}

#[cfg(test)]
pub(crate) fn decode_durable_record_for_tests<T: DeserializeOwned>(
    line: &str,
) -> Result<T, CodingSessionError> {
    decode_durable_record(
        line,
        1,
        Path::new("<test durable record>"),
        "test durable record",
    )
}

fn decode_durable_value(
    line: &str,
    line_number: usize,
    path: &Path,
    kind: &str,
) -> Result<Value, CodingSessionError> {
    let frame: DurableFrame = serde_json::from_str(line).map_err(|error| {
        session_error(format!(
            "failed to parse required v{DURABLE_FRAME_VERSION} {kind} frame at line {line_number} in {}: {error}; recovery: start a fresh 0.6.1 session store",
            path.display()
        ))
    })?;
    let DurableFrame { metadata, payload } = frame;
    if metadata.schema != DURABLE_FRAME_SCHEMA || metadata.version != DURABLE_FRAME_VERSION {
        return Err(session_error(format!(
            "unsupported {kind} frame at line {line_number} in {}: schema={}, version={}; recovery: start a fresh 0.6.1 session store",
            path.display(),
            metadata.schema,
            metadata.version
        )));
    }
    let payload_bytes = payload.get().as_bytes();
    if payload_bytes.len() > MAX_SESSION_PAYLOAD_BYTES {
        return Err(session_error(format!(
            "{kind} payload exceeds {MAX_SESSION_PAYLOAD_BYTES} bytes at line {line_number} in {}",
            path.display()
        )));
    }
    if usize::try_from(metadata.payload_bytes).ok() != Some(payload_bytes.len()) {
        return Err(session_error(format!(
            "{kind} frame length mismatch at line {line_number} in {}",
            path.display()
        )));
    }
    let actual_sha256 = format!("{:x}", Sha256::digest(payload_bytes));
    if metadata.sha256 != actual_sha256 {
        return Err(session_error(format!(
            "{kind} frame checksum mismatch at line {line_number} in {}",
            path.display()
        )));
    }
    serde_json::from_str(payload.get()).map_err(|error| {
        session_error(format!(
            "failed to decode verified {kind} payload at line {line_number} in {}: {error}",
            path.display()
        ))
    })
}

fn json_string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field)?.as_str().map(str::to_owned)
}

fn json_u32_field(value: &Value, field: &str) -> Option<u32> {
    value.get(field)?.as_u64()?.try_into().ok()
}

fn create_empty_event_log(session_dir: &Path) -> Result<(), CodingSessionError> {
    let event_log_path = session_dir.join(SESSION_EVENT_LOG_FILE);
    File::create_new(&event_log_path)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            session_error(format!(
                "failed to create session event log {}: {error}",
                event_log_path.display()
            ))
        })
}

fn create_empty_outbox_log(session_dir: &Path) -> Result<(), CodingSessionError> {
    let outbox_path = session_dir.join(SESSION_OUTBOX_LOG_FILE);
    File::create_new(&outbox_path)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            session_error(format!(
                "failed to create session outbox {}: {error}",
                outbox_path.display()
            ))
        })
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), CodingSessionError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            session_error(format!(
                "failed to sync session directory {}: {error}",
                path.display()
            ))
        })
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), CodingSessionError> {
    // Windows rename durability is provided by the replacement operation; the
    // standard library does not expose a portable directory fsync handle.
    Ok(())
}

fn validate_manifest(manifest: &SessionManifest) -> Result<(), CodingSessionError> {
    if manifest.schema != SESSION_SCHEMA {
        return Err(session_error(format!(
            "unsupported session manifest schema: {}",
            manifest.schema
        )));
    }
    if manifest.version != SESSION_VERSION {
        return Err(session_error(format!(
            "unsupported session manifest version: {}",
            manifest.version
        )));
    }
    validate_relative_manifest_path(&manifest.event_log)?;
    validate_relative_manifest_path(&manifest.outbox_log)?;
    Ok(())
}

fn validate_relative_manifest_path(path: &str) -> Result<(), CodingSessionError> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() {
        return Err(session_error("manifest event log path must not be empty"));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(session_error(format!(
                    "manifest event log path must be relative and contained: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn event_log_path(
    session_dir: &Path,
    manifest: &SessionManifest,
) -> Result<PathBuf, CodingSessionError> {
    validate_relative_manifest_path(&manifest.event_log)?;
    Ok(session_dir.join(&manifest.event_log))
}

fn outbox_log_path(
    session_dir: &Path,
    manifest: &SessionManifest,
) -> Result<PathBuf, CodingSessionError> {
    validate_relative_manifest_path(&manifest.outbox_log)?;
    Ok(session_dir.join(&manifest.outbox_log))
}

fn next_session_sequence(
    event_log_path: &Path,
    session_id: &str,
) -> Result<u64, CodingSessionError> {
    let file = File::open(event_log_path).map_err(|error| {
        session_error(format!(
            "failed to open session event log {}: {error}",
            event_log_path.display()
        ))
    })?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut expected_sequence = 0_u64;
    let mut line_number = 0_usize;
    while read_bounded_line(&mut reader, &mut line, event_log_path)? {
        line_number += 1;
        let line = decode_utf8_line(&line, line_number, event_log_path)?;
        if line.trim().is_empty() {
            continue;
        }
        expected_sequence = expected_sequence
            .checked_add(1)
            .ok_or_else(|| session_error("session event sequence overflowed"))?;
        let event = decode_event_line(line, line_number, event_log_path)?;
        validate_contiguous_session_sequence(&event, expected_sequence)?;
        validate_event_for_session(&event, session_id)?;
    }

    expected_sequence
        .checked_add(1)
        .ok_or_else(|| session_error("session event sequence overflowed"))
}

fn repair_unterminated_tail(
    path: &Path,
    kind: &str,
    validate: impl FnOnce(&str) -> Result<(), CodingSessionError>,
) -> Result<Option<String>, CodingSessionError> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| {
            session_error(format!(
                "failed to open {kind} log for tail inspection {}: {error}",
                path.display()
            ))
        })?;
    let length = file
        .metadata()
        .map_err(|error| {
            session_error(format!(
                "failed to inspect {kind} log {}: {error}",
                path.display()
            ))
        })?
        .len();
    if length == 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::End(-1)).map_err(|error| {
        session_error(format!(
            "failed to inspect {kind} tail {}: {error}",
            path.display()
        ))
    })?;
    let mut last_byte = [0_u8; 1];
    file.read_exact(&mut last_byte).map_err(|error| {
        session_error(format!(
            "failed to inspect {kind} tail {}: {error}",
            path.display()
        ))
    })?;
    if last_byte[0] == b'\n' {
        return Ok(None);
    }

    let inspection_bytes = u64::try_from(MAX_SESSION_RECORD_BYTES)
        .expect("session record limit fits u64")
        .saturating_add(1);
    let inspection_start = length.saturating_sub(inspection_bytes);
    file.seek(SeekFrom::Start(inspection_start))
        .map_err(|error| {
            session_error(format!(
                "failed to seek {kind} tail {}: {error}",
                path.display()
            ))
        })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|error| {
        session_error(format!(
            "failed to read {kind} tail {}: {error}",
            path.display()
        ))
    })?;
    let (tail_start, tail) = match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(index) => (
            inspection_start
                .checked_add(u64::try_from(index + 1).expect("buffer index fits u64"))
                .ok_or_else(|| session_error(format!("{kind} tail offset overflowed")))?,
            &bytes[index + 1..],
        ),
        None if inspection_start == 0 => (0, bytes.as_slice()),
        None => {
            return Err(session_error(format!(
                "unterminated {kind} tail exceeds {MAX_SESSION_RECORD_BYTES} bytes in {}; automatic recovery cannot find a safe frame boundary",
                path.display()
            )));
        }
    };
    let tail_text = std::str::from_utf8(tail);
    if tail_text.ok().is_some_and(|line| validate(line).is_ok()) {
        file.seek(SeekFrom::End(0)).map_err(|error| {
            session_error(format!(
                "failed to seek {kind} tail {}: {error}",
                path.display()
            ))
        })?;
        file.write_all(b"\n").map_err(|error| {
            session_error(format!(
                "failed to terminate valid {kind} tail {}: {error}",
                path.display()
            ))
        })?;
        file.sync_data().map_err(|error| {
            session_error(format!(
                "failed to sync repaired {kind} tail {}: {error}",
                path.display()
            ))
        })?;
        return Ok(Some(format!(
            "recovered unterminated valid {kind} frame in {} by appending its missing newline",
            path.display()
        )));
    }

    let discarded = length.saturating_sub(tail_start);
    file.set_len(tail_start).map_err(|error| {
        session_error(format!(
            "failed to truncate torn {kind} tail {}: {error}",
            path.display()
        ))
    })?;
    file.sync_data().map_err(|error| {
        session_error(format!(
            "failed to sync truncated {kind} tail {}: {error}",
            path.display()
        ))
    })?;
    Ok(Some(format!(
        "recovered torn {kind} tail in {} by discarding {discarded} bytes after the last complete frame",
        path.display()
    )))
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    path: &Path,
) -> Result<bool, CodingSessionError> {
    line.clear();
    loop {
        let available = reader.fill_buf().map_err(|error| {
            session_error(format!(
                "failed to read durable session record {}: {error}",
                path.display()
            ))
        })?;
        if available.is_empty() {
            return Ok(!line.is_empty());
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(take) > MAX_SESSION_RECORD_BYTES {
            return Err(session_error(format!(
                "durable session record exceeds {MAX_SESSION_RECORD_BYTES} bytes in {}",
                path.display()
            )));
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

fn decode_utf8_line<'a>(
    line: &'a [u8],
    line_number: usize,
    path: &Path,
) -> Result<&'a str, CodingSessionError> {
    std::str::from_utf8(line).map_err(|error| {
        session_error(format!(
            "durable session record at line {line_number} in {} is not UTF-8: {error}",
            path.display()
        ))
    })
}

fn validate_contiguous_session_sequence(
    event: &SessionEventEnvelope,
    expected_sequence: u64,
) -> Result<(), CodingSessionError> {
    let actual_sequence = event.session_sequence.ok_or_else(|| {
        session_error(format!(
            "session event sequence is missing: event_id={}, expected={expected_sequence}",
            event.event_id
        ))
    })?;
    if actual_sequence != expected_sequence {
        return Err(session_error(format!(
            "session event sequence is not contiguous: event_id={}, expected={}, actual={}",
            event.event_id, expected_sequence, actual_sequence
        )));
    }
    Ok(())
}

fn validate_event_for_session(
    event: &SessionEventEnvelope,
    session_id: &str,
) -> Result<(), CodingSessionError> {
    if event.schema != EVENT_SCHEMA {
        return Err(session_error(format!(
            "unsupported session event schema: {}",
            event.schema
        )));
    }
    if event.version != EVENT_VERSION {
        return Err(session_error(format!(
            "unsupported session event version: {}",
            event.version
        )));
    }
    if event.session_id != session_id {
        return Err(session_error(format!(
            "session event {} belongs to {}, expected {}",
            event.event_id, event.session_id, session_id
        )));
    }
    Ok(())
}

fn session_error(message: impl Into<String>) -> CodingSessionError {
    CodingSessionError::Session {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::{
        DiagnosticLevel, OperationKind, PersistedContentBlock, PersistedRole, PersistedToolResult,
        SessionEventData,
    };
    use crate::session::replay::{MessageStatus, ToolCallStatus, TranscriptItem};
    use std::sync::Barrier;

    fn create_options(session_id: &str) -> CreateSessionOptions {
        CreateSessionOptions::new(session_id, "2026-06-29T00:00:00Z")
    }

    fn event(session_id: &str, event_id: &str, data: SessionEventData) -> SessionEventEnvelope {
        SessionEventEnvelope::new(session_id, event_id, "2026-06-29T00:00:01Z", data)
    }

    #[test]
    fn create_session_writes_manifest_event_log_and_directories() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());

        let handle = store.create_session(create_options("sess_store")).unwrap();

        assert!(handle.session_dir().is_dir());
        assert!(handle.session_dir().join("blobs").is_dir());
        assert!(handle.session_dir().join("index").is_dir());
        assert!(handle.session_dir().join(SESSION_MANIFEST_FILE).is_file());
        assert!(handle.event_log_path().unwrap().is_file());
        assert_eq!(handle.manifest().session_id, "sess_store");
        assert_eq!(handle.manifest().created_at, "2026-06-29T00:00:00Z");
        assert_eq!(handle.manifest().event_log, SESSION_EVENT_LOG_FILE);

        let event_log = fs::read_to_string(handle.event_log_path().unwrap()).unwrap();
        assert!(event_log.is_empty());
    }

    #[test]
    fn create_session_cleans_up_every_failed_initialization_stage() {
        for (stage, session_id) in [
            (StoreFailurePoint::CreateBlobs, "sess_fail_blobs"),
            (StoreFailurePoint::CreateIndex, "sess_fail_index"),
            (StoreFailurePoint::WriteManifest, "sess_fail_manifest"),
            (StoreFailurePoint::CreateEventLog, "sess_fail_event_log"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let store = SessionLogStore::new(temp.path());
            store.fail_after(stage, 0);

            let error = store
                .create_session(create_options(session_id))
                .unwrap_err();

            assert_eq!(error.code(), "session");
            assert!(
                !temp.path().join(session_id).exists(),
                "failed stage {stage:?} should not leave a visible target"
            );
        }
    }

    #[test]
    fn open_session_reads_manifest_and_rejects_paths_outside_root() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store.create_session(create_options("sess_open")).unwrap();

        let opened = store.open_session(handle.session_dir()).unwrap();
        assert_eq!(opened.manifest(), handle.manifest());

        let error = store.open_session(outside.path()).unwrap_err();
        assert_eq!(error.code(), "session");
        assert!(
            error
                .to_string()
                .contains("session directory is outside store root")
        );
    }

    #[test]
    fn try_open_session_id_returns_none_for_missing_and_opens_existing() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());

        assert_eq!(store.try_open_session_id("sess_missing").unwrap(), None);

        let created = store
            .create_session(create_options("sess_try_open"))
            .unwrap();
        let opened = store
            .try_open_session_id(" sess_try_open ")
            .unwrap()
            .unwrap();

        assert_eq!(opened.manifest(), created.manifest());
        assert_eq!(opened.session_dir(), created.session_dir());
    }

    #[test]
    fn list_sessions_returns_native_sessions_sorted_by_updated_at() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let old = store
            .create_session(CreateSessionOptions::new(
                "sess_old",
                "2026-06-29T00:00:00Z",
            ))
            .unwrap();
        let new = store
            .create_session(CreateSessionOptions::new(
                "sess_new",
                "2026-06-29T00:00:01Z",
            ))
            .unwrap();
        fs::create_dir(temp.path().join("legacy-jsonl-directory")).unwrap();
        fs::write(temp.path().join("not-a-session"), "{}\n").unwrap();

        store
            .update_manifest(
                &old,
                ManifestPatch::new()
                    .updated_at("2026-06-29T00:00:03Z")
                    .active_leaf_id(Some("leaf_old".into())),
            )
            .unwrap();

        let summaries = store.list_sessions().unwrap();

        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].session_id, "sess_old");
        assert_eq!(summaries[0].session_dir, old.session_dir().to_path_buf());
        assert_eq!(summaries[0].created_at, "2026-06-29T00:00:00Z");
        assert_eq!(summaries[0].updated_at, "2026-06-29T00:00:03Z");
        assert_eq!(summaries[0].active_leaf_id.as_deref(), Some("leaf_old"));
        assert_eq!(summaries[1].session_id, "sess_new");
        assert_eq!(summaries[1].session_dir, new.session_dir().to_path_buf());
    }

    #[test]
    fn list_sessions_returns_empty_for_missing_root() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path().join("missing"));

        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn append_and_read_events_round_trip_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store.create_session(create_options("sess_events")).unwrap();
        let events = vec![
            event(
                "sess_events",
                "evt_1",
                SessionEventData::SessionCreated {
                    cwd: Some("/tmp/project".into()),
                },
            ),
            event("sess_events", "evt_2", SessionEventData::TurnStarted {})
                .with_operation_id("op_1")
                .with_turn_id("turn_1"),
        ];

        store.append_events(&handle, &events).unwrap();

        let raw = fs::read_to_string(handle.event_log_path().unwrap()).unwrap();
        assert_eq!(raw.lines().count(), 2);
        assert!(raw.contains(DURABLE_FRAME_FIELD));
        assert!(raw.contains("\"kind\":\"session.created\""));
        assert!(raw.contains("\"kind\":\"turn.started\""));

        let decoded = store.read_events(&handle).unwrap();
        assert_eq!(
            decoded,
            vec![
                events[0].clone().with_session_sequence(1),
                events[1].clone().with_session_sequence(2),
            ]
        );
    }

    #[test]
    fn frame_v2_round_trips_exact_float_and_unicode_payload_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store
            .create_session(create_options("sess_float_unicode"))
            .unwrap();
        let event = event(
            "sess_float_unicode",
            "evt_float_unicode",
            SessionEventData::MessageCompleted {
                message_id: "msg_float_unicode".into(),
                content: vec![PersistedContentBlock::Thinking {
                    thinking: "你好！有什么可以帮助你的吗？🦀".into(),
                    thinking_signature: None,
                    redacted: None,
                }],
                finish_reason: Some("stop".into()),
                usage: ai::api::conversation::Usage {
                    input: 3337,
                    output: 7,
                    cache_read: 0,
                    cache_write: 0,
                    total_tokens: 3344,
                    cost: ai::api::conversation::Cost {
                        known: true,
                        input: 0.00046718000000000004,
                        output: 0.0000019600000000000003,
                        cache_read: 0.0,
                        cache_write: 0.0,
                    },
                },
            },
        )
        .with_session_sequence(1);
        let expected_payload = serde_json::to_vec(&event).unwrap();

        store
            .append_events(&handle, std::slice::from_ref(&event))
            .unwrap();

        let raw = fs::read_to_string(handle.event_log_path().unwrap()).unwrap();
        let frame: DurableFrame = serde_json::from_str(raw.trim_end()).unwrap();
        assert_eq!(frame.metadata.version, DURABLE_FRAME_VERSION);
        assert_eq!(frame.payload.get().as_bytes(), expected_payload);
        assert_eq!(
            usize::try_from(frame.metadata.payload_bytes).unwrap(),
            expected_payload.len()
        );
        assert_eq!(
            frame.metadata.sha256,
            format!("{:x}", Sha256::digest(&expected_payload))
        );
        let decoded = store.read_events(&handle).unwrap();
        let SessionEventData::MessageCompleted { content, usage, .. } = &decoded[0].data else {
            panic!("expected decoded message.completed event");
        };
        assert_eq!(
            content,
            &[PersistedContentBlock::Thinking {
                thinking: "你好！有什么可以帮助你的吗？🦀".into(),
                thinking_signature: None,
                redacted: None,
            }]
        );
        assert!((usage.cost.input - 0.00046718000000000004).abs() < f64::EPSILON);
        assert!((usage.cost.output - 0.0000019600000000000003).abs() < f64::EPSILON);
    }

    #[test]
    fn checksummed_frame_rejects_a_tampered_complete_record() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store
            .create_session(create_options("sess_tampered_frame"))
            .unwrap();
        store
            .append_events(
                &handle,
                &[event(
                    "sess_tampered_frame",
                    "evt_original",
                    SessionEventData::TurnStarted {},
                )],
            )
            .unwrap();
        let path = handle.event_log_path().unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let mut value: Value = serde_json::from_str(raw.trim_end()).unwrap();
        value["payload"]["event_id"] = Value::String("evt_tampered".into());
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&value).unwrap()),
        )
        .unwrap();

        let error = store.read_events(&handle).unwrap_err();
        assert!(error.to_string().contains("frame checksum mismatch"));
    }

    #[test]
    fn writer_lease_repairs_only_an_unterminated_tail_and_reports_it() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store
            .create_session(create_options("sess_torn_tail"))
            .unwrap();
        store
            .append_events(
                &handle,
                &[event(
                    "sess_torn_tail",
                    "evt_complete",
                    SessionEventData::TurnStarted {},
                )],
            )
            .unwrap();
        let path = handle.event_log_path().unwrap();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"schema":"torn""#).unwrap();
        file.sync_all().unwrap();

        let lease = store.acquire_write_lease(&handle).unwrap();

        assert_eq!(lease.committed_sequence(), 1);
        assert_eq!(lease.tail_recoveries().len(), 1);
        assert!(lease.tail_recoveries()[0].contains("discarding"));
        assert_eq!(store.read_events(&handle).unwrap().len(), 1);
        assert!(fs::read(&path).unwrap().ends_with(b"\n"));
    }

    #[test]
    fn writer_lease_preserves_a_valid_frame_missing_only_its_newline() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store
            .create_session(create_options("sess_missing_newline"))
            .unwrap();
        store
            .append_events(
                &handle,
                &[event(
                    "sess_missing_newline",
                    "evt_complete",
                    SessionEventData::TurnStarted {},
                )],
            )
            .unwrap();
        let path = handle.event_log_path().unwrap();
        let mut bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.pop(), Some(b'\n'));
        fs::write(&path, bytes).unwrap();

        let lease = store.acquire_write_lease(&handle).unwrap();

        assert_eq!(lease.committed_sequence(), 1);
        assert_eq!(lease.tail_recoveries().len(), 1);
        assert!(lease.tail_recoveries()[0].contains("missing newline"));
        assert_eq!(store.read_events(&handle).unwrap().len(), 1);
    }

    #[test]
    fn oversized_record_is_rejected_before_any_bytes_are_appended() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store
            .create_session(create_options("sess_oversized_record"))
            .unwrap();
        let oversized = event(
            "sess_oversized_record",
            "evt_oversized",
            SessionEventData::DiagnosticEmitted {
                level: DiagnosticLevel::Error,
                message: "x".repeat(MAX_SESSION_PAYLOAD_BYTES),
            },
        );

        let error = store.append_events(&handle, &[oversized]).unwrap_err();

        assert!(matches!(
            error,
            CodingSessionError::SessionWriteRejected { .. }
        ));
        assert!(
            fs::read(handle.event_log_path().unwrap())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn append_events_assigns_contiguous_session_sequences() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store
            .create_session(create_options("sess_sequence"))
            .unwrap();
        let events = vec![
            event(
                "sess_sequence",
                "evt_1",
                SessionEventData::SessionCreated { cwd: None },
            ),
            event("sess_sequence", "evt_2", SessionEventData::TurnStarted {}),
        ];

        store.append_events(&handle, &events).unwrap();

        let decoded = store.read_events(&handle).unwrap();
        assert_eq!(
            decoded
                .iter()
                .map(|event| event.session_sequence)
                .collect::<Vec<_>>(),
            vec![Some(1), Some(2)]
        );

        let raw = fs::read_to_string(handle.event_log_path().unwrap()).unwrap();
        let raw_sequences = raw
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["payload"]
                    ["session_sequence"]
                    .as_u64()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(raw_sequences, vec![1, 2]);
    }

    #[test]
    fn cloned_store_serializes_concurrent_event_appends() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store
            .create_session(create_options("sess_concurrent_append"))
            .unwrap();
        let barrier = Arc::new(Barrier::new(8));
        let threads = (0..8)
            .map(|index| {
                let store = store.clone();
                let handle = handle.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .append_events(
                            &handle,
                            &[event(
                                "sess_concurrent_append",
                                &format!("evt_{index}"),
                                SessionEventData::TurnStarted {},
                            )],
                        )
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }

        let events = store.read_events(&handle).unwrap();
        assert_eq!(events.len(), 8);
        assert_eq!(
            events
                .iter()
                .map(|event| event.session_sequence.unwrap())
                .collect::<Vec<_>>(),
            (1..=8).collect::<Vec<_>>()
        );
    }

    #[test]
    fn read_events_rejects_unframed_legacy_logs() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store
            .create_session(create_options("sess_legacy_sequence"))
            .unwrap();
        let legacy_events = [
            event(
                "sess_legacy_sequence",
                "evt_legacy_1",
                SessionEventData::SessionCreated { cwd: None },
            ),
            event(
                "sess_legacy_sequence",
                "evt_legacy_2",
                SessionEventData::TurnStarted {},
            ),
        ];
        let raw = legacy_events
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(handle.event_log_path().unwrap(), format!("{raw}\n")).unwrap();

        let error = store.read_events(&handle).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed to parse required v2 session event frame"),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains("start a fresh 0.6.1 session store"),
            "{error}"
        );
    }

    #[test]
    fn read_events_rejects_non_contiguous_durable_sequences() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store
            .create_session(create_options("sess_non_contiguous_sequence"))
            .unwrap();
        let events = [
            event(
                "sess_non_contiguous_sequence",
                "evt_1",
                SessionEventData::SessionCreated { cwd: None },
            )
            .with_session_sequence(1),
            event(
                "sess_non_contiguous_sequence",
                "evt_3",
                SessionEventData::TurnStarted {},
            )
            .with_session_sequence(3),
        ];
        let raw = events
            .iter()
            .map(|event| {
                String::from_utf8(encode_durable_record(event, "session event").unwrap()).unwrap()
            })
            .collect::<Vec<_>>()
            .join("");
        fs::write(handle.event_log_path().unwrap(), raw).unwrap();

        let error = store.read_events(&handle).unwrap_err();
        assert!(error.to_string().contains("event_id=evt_3"));
        assert!(error.to_string().contains("expected=2"));
        assert!(error.to_string().contains("actual=3"));
    }

    #[test]
    fn decoder_matrix_rejects_unknown_manifest_and_event_versions_with_recovery_context() {
        let manifest_error = decode_manifest(
            r#"{"schema":"evo.session","version":99}"#,
            Path::new("/tmp/session.json"),
        )
        .unwrap_err();
        assert!(manifest_error.to_string().contains("schema=evo.session"));
        assert!(manifest_error.to_string().contains("version=99"));
        assert!(manifest_error.to_string().contains("recovery:"));

        let future_event = serde_json::json!({
            "schema": "evo.session.event",
            "version": 99,
            "session_id": "sess_future",
            "session_sequence": 1,
            "event_id": "evt-future",
            "created_at": "2026-07-24T00:00:00Z",
            "kind": "turn.started",
            "data": {}
        });
        let framed = encode_durable_record(&future_event, "session event").unwrap();
        let event_error = decode_event_line(
            std::str::from_utf8(&framed).unwrap(),
            7,
            Path::new("/tmp/events.jsonl"),
        )
        .unwrap_err();
        assert!(event_error.to_string().contains("schema=evo.session.event"));
        assert!(event_error.to_string().contains("version=99"));
        assert!(event_error.to_string().contains("event_id=evt-future"));
        assert!(event_error.to_string().contains("recovery:"));
    }

    #[test]
    fn decoder_rejects_frame_v1_without_compatibility_repair() {
        let payload = RawValue::from_string(
            serde_json::to_string(
                &event("sess_v1", "evt_v1", SessionEventData::TurnStarted {})
                    .with_session_sequence(1),
            )
            .unwrap(),
        )
        .unwrap();
        let payload_bytes = payload.get().as_bytes();
        let metadata = DurableFrameMetadata {
            schema: DURABLE_FRAME_SCHEMA.into(),
            version: 1,
            payload_bytes: payload_bytes.len().try_into().unwrap(),
            sha256: format!("{:x}", Sha256::digest(payload_bytes)),
        };
        let line = serde_json::to_string(&DurableFrame { metadata, payload }).unwrap();

        let error = decode_event_line(&line, 1, Path::new("/tmp/events-v1.jsonl")).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unsupported session event frame")
        );
        assert!(error.to_string().contains("version=1"));
        assert!(
            error
                .to_string()
                .contains("start a fresh 0.6.1 session store")
        );
    }

    #[test]
    fn replay_session_folds_canonical_event_log_into_transcript() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store
            .create_session(create_options("sess_replay_store"))
            .unwrap();
        let events = vec![
            event(
                "sess_replay_store",
                "evt_1",
                SessionEventData::OperationStarted {
                    operation: OperationKind::Prompt,
                    runtime_generation: Default::default(),
                },
            )
            .with_operation_id("op_1")
            .with_turn_id("turn_1"),
            event(
                "sess_replay_store",
                "evt_2",
                SessionEventData::TurnInputRecorded {
                    content: vec![PersistedContentBlock::Text {
                        text: "hello".into(),
                    }],
                },
            )
            .with_operation_id("op_1")
            .with_turn_id("turn_1"),
            event(
                "sess_replay_store",
                "evt_3",
                SessionEventData::MessageStarted {
                    message_id: "msg_1".into(),
                    role: PersistedRole::Assistant,
                },
            )
            .with_operation_id("op_1")
            .with_turn_id("turn_1"),
            event(
                "sess_replay_store",
                "evt_4",
                SessionEventData::MessageCompleted {
                    message_id: "msg_1".into(),
                    content: vec![PersistedContentBlock::Text { text: "hi".into() }],
                    finish_reason: Some("stop".into()),
                    usage: Default::default(),
                },
            )
            .with_operation_id("op_1")
            .with_turn_id("turn_1"),
            event(
                "sess_replay_store",
                "evt_6",
                SessionEventData::ToolCallStarted {
                    tool_call_id: "tool_1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "src/lib.rs"}),
                },
            )
            .with_operation_id("op_1")
            .with_turn_id("turn_1"),
            event(
                "sess_replay_store",
                "evt_7",
                SessionEventData::ToolCallCompleted {
                    tool_call_id: "tool_1".into(),
                    result: PersistedToolResult::Text { text: "ok".into() },
                },
            )
            .with_operation_id("op_1")
            .with_turn_id("turn_1"),
            event(
                "sess_replay_store",
                "evt_8",
                SessionEventData::OperationCommitted {
                    new_leaf_id: Some("leaf_1".into()),
                },
            )
            .with_operation_id("op_1")
            .with_turn_id("turn_1"),
        ];

        store.append_events(&handle, &events).unwrap();

        let replay = store.replay_session(&handle).unwrap();

        assert_eq!(replay.session_id, "sess_replay_store");
        assert_eq!(replay.active_leaf_id.as_deref(), Some("leaf_1"));
        assert_eq!(
            replay.transcript,
            vec![
                TranscriptItem::UserInput {
                    turn_id: "turn_1".into(),
                    text: "hello".into(),
                },
                TranscriptItem::AssistantMessage {
                    message_id: "msg_1".into(),
                    content: vec![PersistedContentBlock::Text { text: "hi".into() }],
                    status: MessageStatus::Completed,
                    reasoning_duration_millis: None,
                },
                TranscriptItem::ToolCall {
                    tool_call_id: "tool_1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "src/lib.rs"}),
                    status: ToolCallStatus::Completed,
                    summary: "ok".into(),
                    started_at: "2026-06-29T00:00:01Z".into(),
                    duration_millis: Some(0),
                },
            ]
        );
    }

    #[test]
    fn append_rejects_events_for_another_session() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store
            .create_session(create_options("sess_expected"))
            .unwrap();
        let wrong_event = event(
            "sess_other",
            "evt_1",
            SessionEventData::SessionCreated { cwd: None },
        );

        let error = store.append_events(&handle, &[wrong_event]).unwrap_err();

        assert_eq!(error.code(), "session");
        assert!(
            error
                .to_string()
                .contains("belongs to sess_other, expected sess_expected")
        );
    }

    #[test]
    fn update_manifest_persists_patch() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store
            .create_session(create_options("sess_manifest"))
            .unwrap();

        store
            .update_manifest(
                &handle,
                ManifestPatch::new()
                    .updated_at("2026-06-29T00:00:02Z")
                    .active_branch_id(Some("branch_1".into()))
                    .active_leaf_id(Some("leaf_1".into())),
            )
            .unwrap();

        let opened = store.open_session(handle.session_dir()).unwrap();
        assert_eq!(opened.manifest().updated_at, "2026-06-29T00:00:02Z");
        assert_eq!(
            opened.manifest().active_branch_id.as_deref(),
            Some("branch_1")
        );
        assert_eq!(opened.manifest().active_leaf_id.as_deref(), Some("leaf_1"));
    }

    #[test]
    fn create_session_rejects_path_like_session_id() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());

        let error = store
            .create_session(create_options("../escape"))
            .unwrap_err();

        assert_eq!(error.code(), "session");
        assert!(
            error
                .to_string()
                .contains("session id contains unsupported path characters")
        );
    }

    #[test]
    fn open_session_rejects_manifest_event_log_escape() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path());
        let handle = store
            .create_session(create_options("sess_bad_manifest"))
            .unwrap();
        let mut manifest = handle.manifest().clone();
        manifest.event_log = "../events.jsonl".into();
        write_manifest(handle.session_dir(), &manifest).unwrap();

        let error = store.open_session(handle.session_dir()).unwrap_err();

        assert_eq!(error.code(), "session");
        assert!(
            error
                .to_string()
                .contains("manifest event log path must be relative and contained")
        );
    }
}
