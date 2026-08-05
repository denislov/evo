use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::{JournalError, JournalErrorKind};
use crate::frame::MAX_JOURNAL_RECORD_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalPaths {
    pub records: PathBuf,
    pub outbox: PathBuf,
    pub writer_lock: PathBuf,
}

impl JournalPaths {
    pub fn new(
        records: impl Into<PathBuf>,
        outbox: impl Into<PathBuf>,
        writer_lock: impl Into<PathBuf>,
    ) -> Self {
        Self {
            records: records.into(),
            outbox: outbox.into(),
            writer_lock: writer_lock.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendFault {
    WriteAfterBytes(usize),
    Sync,
}

#[derive(Debug)]
pub struct JournalWriteLease {
    _lock_file: File,
    next_sequence: u64,
    tail_recoveries: Vec<String>,
}

impl JournalWriteLease {
    pub const fn committed_sequence(&self) -> u64 {
        self.next_sequence.saturating_sub(1)
    }

    pub const fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    pub fn tail_recoveries(&self) -> &[String] {
        &self.tail_recoveries
    }

    pub fn advance_to(&mut self, committed_sequence: u64) -> Result<(), JournalError> {
        if committed_sequence < self.committed_sequence() {
            return Err(JournalError::new(
                JournalErrorKind::InvalidInput,
                "journal committed sequence cannot regress",
            ));
        }
        self.next_sequence = committed_sequence.checked_add(1).ok_or_else(|| {
            JournalError::new(
                JournalErrorKind::WriteRejected,
                "journal sequence overflowed",
            )
        })?;
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct JournalStore {
    append_lock: Arc<Mutex<()>>,
}

impl JournalStore {
    pub fn create_log(path: &Path) -> Result<(), JournalError> {
        File::create_new(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| io_error("create journal", path, error))
    }

    pub fn acquire_write_lease(
        &self,
        paths: &JournalPaths,
        validate_record_tail: impl FnOnce(&str) -> Result<(), JournalError>,
        validate_outbox_tail: impl FnOnce(&str) -> Result<(), JournalError>,
        next_sequence: impl FnOnce(&Path) -> Result<u64, JournalError>,
    ) -> Result<JournalWriteLease, JournalError> {
        let lock_file = acquire_lock(&paths.writer_lock)?;
        let mut tail_recoveries = Vec::new();
        if let Some(recovery) =
            repair_unterminated_tail(&paths.records, "journal record", validate_record_tail)?
        {
            tail_recoveries.push(recovery);
        }
        if let Some(recovery) =
            repair_unterminated_tail(&paths.outbox, "journal outbox record", validate_outbox_tail)?
        {
            tail_recoveries.push(recovery);
        }
        Ok(JournalWriteLease {
            _lock_file: lock_file,
            next_sequence: next_sequence(&paths.records)?,
            tail_recoveries,
        })
    }

    pub fn repair_tails_for_read(
        &self,
        paths: &JournalPaths,
        validate_record_tail: impl FnOnce(&str) -> Result<(), JournalError>,
        validate_outbox_tail: impl FnOnce(&str) -> Result<(), JournalError>,
    ) -> Result<Vec<String>, JournalError> {
        let _lock_file = acquire_lock(&paths.writer_lock)?;
        let mut recoveries = Vec::new();
        if let Some(recovery) =
            repair_unterminated_tail(&paths.records, "journal record", validate_record_tail)?
        {
            recoveries.push(recovery);
        }
        if let Some(recovery) =
            repair_unterminated_tail(&paths.outbox, "journal outbox record", validate_outbox_tail)?
        {
            recoveries.push(recovery);
        }
        Ok(recoveries)
    }

    pub fn append_records(
        &self,
        path: &Path,
        frames: &[Vec<u8>],
        kind: &str,
        lease: &mut JournalWriteLease,
        committed_sequence: u64,
        fault: Option<AppendFault>,
    ) -> Result<(), JournalError> {
        let _guard = self.append_guard()?;
        append_frames(path, frames, kind, fault)?;
        lease.advance_to(committed_sequence)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn append_checkpoint(
        &self,
        paths: &JournalPaths,
        record_frames: &[Vec<u8>],
        outbox_frames: &[Vec<u8>],
        lease: &mut JournalWriteLease,
        committed_sequence: u64,
        record_fault: Option<AppendFault>,
        outbox_fault: Option<AppendFault>,
    ) -> Result<(), JournalError> {
        if record_frames.is_empty() {
            return Err(JournalError::new(
                JournalErrorKind::InvalidInput,
                "journal checkpoint requires at least one record",
            ));
        }
        let _guard = self.append_guard()?;
        if !outbox_frames.is_empty() {
            append_frames(
                &paths.outbox,
                outbox_frames,
                "journal outbox record",
                outbox_fault,
            )?;
        }
        append_frames(
            &paths.records,
            record_frames,
            "journal record",
            record_fault,
        )?;
        lease.advance_to(committed_sequence)
    }

    fn append_guard(&self) -> Result<std::sync::MutexGuard<'_, ()>, JournalError> {
        self.append_lock.lock().map_err(|_| {
            JournalError::new(
                JournalErrorKind::Io,
                "journal append serialization lock is poisoned",
            )
        })
    }
}

fn acquire_lock(path: &Path) -> Result<File, JournalError> {
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| io_error("open journal writer lock", path, error))?;
    if let Err(error) = lock_file.try_lock() {
        return Err(match error {
            std::fs::TryLockError::WouldBlock => JournalError::new(
                JournalErrorKind::LockBusy,
                format!(
                    "journal already has a writer in another process (lock {})",
                    path.display()
                ),
            ),
            std::fs::TryLockError::Error(error) => {
                io_error("acquire journal writer lock", path, error)
            }
        });
    }
    Ok(lock_file)
}

fn append_frames(
    path: &Path,
    frames: &[Vec<u8>],
    kind: &str,
    fault: Option<AppendFault>,
) -> Result<(), JournalError> {
    if frames.is_empty() {
        return Ok(());
    }
    let file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|error| io_error(&format!("open {kind}"), path, error))?;
    let mut writer = BufWriter::new(file);

    if let Some(AppendFault::WriteAfterBytes(limit)) = fault {
        let mut remaining = limit;
        for frame in frames {
            let write_len = remaining.min(frame.len());
            writer
                .write_all(&frame[..write_len])
                .map_err(|error| io_error(&format!("append {kind}"), path, error))?;
            remaining = remaining.saturating_sub(write_len);
            if write_len < frame.len() {
                break;
            }
        }
        writer
            .flush()
            .map_err(|error| io_error(&format!("flush partial {kind}"), path, error))?;
        return Err(JournalError::new(
            JournalErrorKind::Io,
            format!(
                "failed to append {kind} to {}: injected ENOSPC",
                path.display()
            ),
        ));
    }

    for frame in frames {
        writer
            .write_all(frame)
            .map_err(|error| io_error(&format!("append {kind}"), path, error))?;
    }
    writer
        .flush()
        .map_err(|error| io_error(&format!("flush {kind}"), path, error))?;
    if matches!(fault, Some(AppendFault::Sync)) {
        return Err(JournalError::new(
            JournalErrorKind::Io,
            format!(
                "failed to sync {kind} {}: injected fsync failure",
                path.display()
            ),
        ));
    }
    writer
        .get_ref()
        .sync_data()
        .map_err(|error| io_error(&format!("sync {kind}"), path, error))
}

fn repair_unterminated_tail(
    path: &Path,
    kind: &str,
    validate: impl FnOnce(&str) -> Result<(), JournalError>,
) -> Result<Option<String>, JournalError> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| io_error(&format!("open {kind} for tail inspection"), path, error))?;
    let length = file
        .metadata()
        .map_err(|error| io_error(&format!("inspect {kind}"), path, error))?
        .len();
    if length == 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::End(-1))
        .map_err(|error| io_error(&format!("inspect {kind} tail"), path, error))?;
    let mut last_byte = [0_u8; 1];
    file.read_exact(&mut last_byte)
        .map_err(|error| io_error(&format!("inspect {kind} tail"), path, error))?;
    if last_byte[0] == b'\n' {
        return Ok(None);
    }

    let inspection_bytes = u64::try_from(MAX_JOURNAL_RECORD_BYTES)
        .expect("journal record limit fits u64")
        .saturating_add(1);
    let inspection_start = length.saturating_sub(inspection_bytes);
    file.seek(SeekFrom::Start(inspection_start))
        .map_err(|error| io_error(&format!("seek {kind} tail"), path, error))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| io_error(&format!("read {kind} tail"), path, error))?;
    let (tail_start, tail) = match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(index) => (
            inspection_start
                .checked_add(u64::try_from(index + 1).expect("buffer index fits u64"))
                .ok_or_else(|| {
                    JournalError::new(JournalErrorKind::Corrupt, "journal tail offset overflowed")
                })?,
            &bytes[index + 1..],
        ),
        None if inspection_start == 0 => (0, bytes.as_slice()),
        None => {
            return Err(JournalError::new(
                JournalErrorKind::Corrupt,
                format!(
                    "unterminated {kind} tail exceeds {MAX_JOURNAL_RECORD_BYTES} bytes in {}; automatic recovery cannot find a safe frame boundary",
                    path.display()
                ),
            ));
        }
    };
    if std::str::from_utf8(tail)
        .ok()
        .is_some_and(|line| validate(line).is_ok())
    {
        file.seek(SeekFrom::End(0))
            .map_err(|error| io_error(&format!("seek {kind} tail"), path, error))?;
        file.write_all(b"\n")
            .map_err(|error| io_error(&format!("terminate valid {kind} tail"), path, error))?;
        file.sync_data()
            .map_err(|error| io_error(&format!("sync repaired {kind} tail"), path, error))?;
        return Ok(Some(format!(
            "recovered unterminated valid {kind} frame in {} by appending its missing newline",
            path.display()
        )));
    }

    let discarded = length.saturating_sub(tail_start);
    file.set_len(tail_start)
        .map_err(|error| io_error(&format!("truncate torn {kind} tail"), path, error))?;
    file.sync_data()
        .map_err(|error| io_error(&format!("sync truncated {kind} tail"), path, error))?;
    Ok(Some(format!(
        "recovered torn {kind} tail in {} by discarding {discarded} bytes after the last complete frame",
        path.display()
    )))
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

    fn paths(root: &Path) -> JournalPaths {
        JournalPaths::new(
            root.join("events.jsonl"),
            root.join("outbox.jsonl"),
            root.join(".writer.lock"),
        )
    }

    fn validator(line: &str) -> Result<(), JournalError> {
        decode_json_record::<serde_json::Value>(line, 0, "test").map(|_| ())
    }

    #[test]
    fn lease_repairs_torn_tail_and_rejects_a_second_writer() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        JournalStore::create_log(&paths.records).unwrap();
        JournalStore::create_log(&paths.outbox).unwrap();
        let good = encode_json_record(&serde_json::json!({"sequence": 1}), "test").unwrap();
        std::fs::write(&paths.records, [&good[..], b"{torn"].concat()).unwrap();

        let store = JournalStore::default();
        let lease = store
            .acquire_write_lease(&paths, validator, validator, |_| Ok(2))
            .unwrap();
        assert_eq!(lease.tail_recoveries().len(), 1);
        assert_eq!(std::fs::read(&paths.records).unwrap(), good);
        assert_eq!(
            store
                .acquire_write_lease(&paths, validator, validator, |_| Ok(2))
                .unwrap_err()
                .kind(),
            JournalErrorKind::LockBusy
        );
        drop(lease);
    }

    #[test]
    fn checkpoint_makes_outbox_durable_before_record_failure() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        JournalStore::create_log(&paths.records).unwrap();
        JournalStore::create_log(&paths.outbox).unwrap();
        let store = JournalStore::default();
        let mut lease = store
            .acquire_write_lease(&paths, validator, validator, |_| Ok(1))
            .unwrap();
        let event = encode_json_record(&serde_json::json!({"sequence": 1}), "event").unwrap();
        let outbox = encode_json_record(&serde_json::json!({"cursor": 1}), "outbox").unwrap();
        assert!(
            store
                .append_checkpoint(
                    &paths,
                    &[event],
                    std::slice::from_ref(&outbox),
                    &mut lease,
                    1,
                    Some(AppendFault::WriteAfterBytes(1)),
                    None,
                )
                .is_err()
        );
        assert_eq!(std::fs::read(&paths.outbox).unwrap(), outbox);
        assert_eq!(lease.committed_sequence(), 0);
    }
}
