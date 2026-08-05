use super::*;

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
    encode_json_record(record, kind).map_err(journal_error)
}

pub(super) fn decode_durable_record<T: DeserializeOwned>(
    line: &str,
    line_number: usize,
    path: &Path,
    kind: &str,
) -> Result<T, CodingSessionError> {
    decode_json_record(line, line_number, kind)
        .map_err(|error| session_error(format!("{error} in {}", path.display())))
}

pub(super) fn decode_durable_value(
    line: &str,
    line_number: usize,
    path: &Path,
    kind: &str,
) -> Result<Value, CodingSessionError> {
    decode_json_value(line, line_number, kind)
        .map_err(|error| session_error(format!("{error} in {}", path.display())))
}

pub(super) fn json_string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field)?.as_str().map(str::to_owned)
}

pub(super) fn json_u32_field(value: &Value, field: &str) -> Option<u32> {
    value.get(field)?.as_u64()?.try_into().ok()
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
    let mut expected_sequence = 0_u64;
    visit_lines(event_log_path, |line, line_number| {
        (|| -> Result<(), CodingSessionError> {
            expected_sequence = expected_sequence
                .checked_add(1)
                .ok_or_else(|| session_error("session event sequence overflowed"))?;
            let event = decode_event_line(line, line_number, event_log_path)?;
            validate_contiguous_session_sequence(&event, expected_sequence)?;
            validate_event_for_session(&event, session_id)
        })()
        .map_err(journal_codec_error)
    })
    .map_err(journal_error)?;

    expected_sequence
        .checked_add(1)
        .ok_or_else(|| session_error("session event sequence overflowed"))
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
