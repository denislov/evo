use super::*;

pub(super) fn append_durable_records(
    path: &Path,
    records: &[Vec<u8>],
    kind: &str,
    fault: Option<SessionIoFault>,
) -> Result<(), CodingSessionError> {
    let file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|error| {
            session_error(format!("failed to open {kind} {}: {error}", path.display()))
        })?;
    let mut writer = BufWriter::new(file);

    if let Some(SessionIoFault::WriteAfterBytes(limit)) = fault {
        let mut remaining = limit;
        for record in records {
            let write_len = remaining.min(record.len());
            writer.write_all(&record[..write_len]).map_err(|error| {
                session_error(format!(
                    "failed to append {kind} to {}: {error}",
                    path.display()
                ))
            })?;
            remaining = remaining.saturating_sub(write_len);
            if write_len < record.len() {
                break;
            }
        }
        writer.flush().map_err(|error| {
            session_error(format!(
                "failed to flush partial {kind} to {}: {error}",
                path.display()
            ))
        })?;
        return Err(session_error(format!(
            "failed to append {kind} to {}: {}",
            path.display(),
            injected_no_space_error()
        )));
    }

    for record in records {
        writer.write_all(record).map_err(|error| {
            session_error(format!(
                "failed to append {kind} to {}: {error}",
                path.display()
            ))
        })?;
    }
    writer.flush().map_err(|error| {
        session_error(format!(
            "failed to flush {kind} {}: {error}",
            path.display()
        ))
    })?;
    if matches!(fault, Some(SessionIoFault::Sync)) {
        return Err(session_error(format!(
            "failed to sync {kind} {}: injected fsync failure",
            path.display()
        )));
    }
    writer.get_ref().sync_data().map_err(|error| {
        session_error(format!("failed to sync {kind} {}: {error}", path.display()))
    })
}

pub(super) fn injected_no_space_error() -> std::io::Error {
    #[cfg(unix)]
    {
        std::io::Error::from_raw_os_error(libc::ENOSPC)
    }
    #[cfg(not(unix))]
    {
        std::io::Error::other("injected ENOSPC")
    }
}

pub(super) fn normalize_session_id_impl(value: &str) -> Result<String, CodingSessionError> {
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

pub(super) fn write_manifest(
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

pub(super) fn read_manifest(session_dir: &Path) -> Result<SessionManifest, CodingSessionError> {
    let manifest_path = session_dir.join(SESSION_MANIFEST_FILE);
    let content = fs::read_to_string(&manifest_path).map_err(|error| {
        session_error(format!(
            "failed to read session manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    decode_manifest(&content, &manifest_path)
}

pub(super) fn decode_manifest(
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
        (Some(SESSION_SCHEMA), Some(LEGACY_SESSION_VERSION | SESSION_VERSION)) => {
            serde_json::from_value(value).map_err(|error| {
                session_error(format!(
                    "failed to decode session manifest {}: {error}",
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

pub(super) fn decode_event_line(
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

pub(super) fn encode_durable_record(
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

pub(super) fn decode_durable_record<T: DeserializeOwned>(
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

pub(super) fn decode_durable_value(
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

pub(super) fn json_string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field)?.as_str().map(str::to_owned)
}

pub(super) fn json_u32_field(value: &Value, field: &str) -> Option<u32> {
    value.get(field)?.as_u64()?.try_into().ok()
}

pub(super) fn create_empty_event_log(session_dir: &Path) -> Result<(), CodingSessionError> {
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

pub(super) fn create_empty_outbox_log(session_dir: &Path) -> Result<(), CodingSessionError> {
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
pub(super) fn sync_directory(path: &Path) -> Result<(), CodingSessionError> {
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
pub(super) fn sync_directory(_path: &Path) -> Result<(), CodingSessionError> {
    // Windows rename durability is provided by the replacement operation; the
    // standard library does not expose a portable directory fsync handle.
    Ok(())
}

pub(super) fn validate_manifest(manifest: &SessionManifest) -> Result<(), CodingSessionError> {
    if manifest.schema != SESSION_SCHEMA {
        return Err(session_error(format!(
            "unsupported session manifest schema: {}",
            manifest.schema
        )));
    }
    match manifest.version {
        LEGACY_SESSION_VERSION
            if manifest.workspace_scope.is_none() && !manifest.workspace_migrated_from_legacy => {}
        SESSION_VERSION => {
            let scope = manifest
                .workspace_scope
                .as_ref()
                .ok_or_else(|| session_error("v2 session manifest is missing workspace scope"))?;
            scope.to_product().map_err(|error| {
                session_error(format!("invalid persisted workspace scope: {error}"))
            })?;
        }
        _ => {
            return Err(session_error(format!(
                "unsupported session manifest version: {}",
                manifest.version
            )));
        }
    }
    validate_relative_manifest_path(&manifest.event_log)?;
    validate_relative_manifest_path(&manifest.outbox_log)?;
    Ok(())
}

pub(super) fn validate_relative_manifest_path(path: &str) -> Result<(), CodingSessionError> {
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

pub(super) fn event_log_path(
    session_dir: &Path,
    manifest: &SessionManifest,
) -> Result<PathBuf, CodingSessionError> {
    validate_relative_manifest_path(&manifest.event_log)?;
    Ok(session_dir.join(&manifest.event_log))
}

pub(super) fn outbox_log_path(
    session_dir: &Path,
    manifest: &SessionManifest,
) -> Result<PathBuf, CodingSessionError> {
    validate_relative_manifest_path(&manifest.outbox_log)?;
    Ok(session_dir.join(&manifest.outbox_log))
}

pub(super) fn next_session_sequence(
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

pub(super) fn repair_unterminated_tail(
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

pub(super) fn read_bounded_line(
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

pub(super) fn decode_utf8_line<'a>(
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

pub(super) fn validate_contiguous_session_sequence(
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

pub(super) fn validate_event_for_session(
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

pub(super) fn session_error(message: impl Into<String>) -> CodingSessionError {
    CodingSessionError::Session {
        message: message.into(),
    }
}
