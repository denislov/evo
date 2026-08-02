use super::*;

impl SessionLogStore {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            append_lock: Arc::new(Mutex::new(())),
            #[cfg(test)]
            io_faults: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_io_faults(root: impl Into<PathBuf>, io_faults: SessionIoFaultPlan) -> Self {
        Self {
            root: root.into(),
            append_lock: Arc::new(Mutex::new(())),
            io_faults: Some(io_faults),
        }
    }

    #[cfg(test)]
    fn take_io_fault(&self) -> Option<SessionIoFault> {
        self.io_faults.as_ref().and_then(SessionIoFaultPlan::take)
    }

    #[cfg(not(test))]
    fn take_io_fault(&self) -> Option<SessionIoFault> {
        None
    }

    pub(crate) fn acquire_write_lease(
        &self,
        handle: &SessionHandle,
    ) -> Result<SessionWriteLease, CodingSessionError> {
        let (lock_file, tail_recoveries) = self.acquire_repaired_lock(handle)?;
        let event_log_path = event_log_path(&handle.session_dir, &handle.manifest)?;
        let next_sequence =
            self.next_session_sequence(&event_log_path, &handle.manifest.session_id)?;
        Ok(SessionWriteLease {
            _lock_file: lock_file,
            next_sequence,
            tail_recoveries,
        })
    }

    /// Repairs a possible torn final frame without scanning the complete log.
    /// The returned lock is deliberately dropped before bounded hydration.
    pub(crate) fn repair_tails_for_bounded_read(
        &self,
        handle: &SessionHandle,
    ) -> Result<(), CodingSessionError> {
        drop(self.acquire_repaired_lock(handle)?);
        Ok(())
    }

    fn acquire_repaired_lock(
        &self,
        handle: &SessionHandle,
    ) -> Result<(File, Vec<String>), CodingSessionError> {
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
        Ok((lock_file, tail_recoveries))
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
        options.workspace_scope.to_product().map_err(|error| {
            session_error(format!("invalid persisted workspace scope: {error}"))
        })?;
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
        let manifest = SessionManifest::new(
            session_id.clone(),
            options.created_at,
            options.workspace_scope,
        )
        .with_name(options.name)
        .with_default_agent_profile_id(options.default_agent_profile_id);
        let initialization = (|| -> Result<(), CodingSessionError> {
            fs::create_dir(session_dir.join("blobs")).map_err(|error| {
                session_error(format!(
                    "failed to create blobs directory for {session_id}: {error}"
                ))
            })?;

            fs::create_dir(session_dir.join("index")).map_err(|error| {
                session_error(format!(
                    "failed to create index directory for {session_id}: {error}"
                ))
            })?;

            write_manifest(&session_dir, &manifest)?;

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

    pub(crate) fn append_events_with_cursor(
        &self,
        handle: &SessionHandle,
        events: &[SessionEventEnvelope],
        lease: &mut SessionWriteLease,
    ) -> Result<u64, CodingSessionError> {
        let _append_guard = self
            .append_lock
            .lock_resource("session append serialization")?;
        self.append_events_locked(handle, events, lease)
    }

    pub(crate) fn append_events_and_outbox(
        &self,
        handle: &SessionHandle,
        events: &[SessionEventEnvelope],
        records: &[DurableOutboxRecordCandidate],
        lease: &mut SessionWriteLease,
    ) -> Result<u64, CodingSessionError> {
        let _append_guard = self
            .append_lock
            .lock_resource("session append serialization")?;
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
        next_session_sequence(event_log_path, session_id)
    }

    fn append_prepared_events_locked(
        &self,
        handle: &SessionHandle,
        events: &[SessionEventEnvelope],
    ) -> Result<(), CodingSessionError> {
        let event_log_path = event_log_path(&handle.session_dir, &handle.manifest)?;
        let records = events
            .iter()
            .map(|event| encode_durable_record(event, "session event"))
            .collect::<Result<Vec<_>, _>>()?;
        append_durable_records(
            &event_log_path,
            &records,
            "session event",
            self.take_io_fault(),
        )
    }

    fn append_outbox_locked(
        &self,
        handle: &SessionHandle,
        records: &[DurableOutboxRecord],
    ) -> Result<(), CodingSessionError> {
        if records.is_empty() {
            return Ok(());
        }

        let outbox_path = outbox_log_path(&handle.session_dir, &handle.manifest)?;
        let records = records
            .iter()
            .map(|record| encode_durable_record(record, "session outbox record"))
            .collect::<Result<Vec<_>, _>>()?;
        append_durable_records(
            &outbox_path,
            &records,
            "session outbox record",
            self.take_io_fault(),
        )
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

    pub(crate) fn session_creation_workspace(
        &self,
        summary: &SessionSummary,
    ) -> Result<SessionCreationWorkspace, CodingSessionError> {
        let handle = self.open_session(&summary.session_dir)?;
        self.session_creation_workspace_for_handle(&handle)
    }

    pub(crate) fn session_creation_workspace_for_handle(
        &self,
        handle: &SessionHandle,
    ) -> Result<SessionCreationWorkspace, CodingSessionError> {
        let event_log_path = event_log_path(&handle.session_dir, &handle.manifest)?;
        let file = File::open(&event_log_path).map_err(|error| {
            session_error(format!(
                "failed to open session event log {}: {error}",
                event_log_path.display()
            ))
        })?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        if !read_bounded_line(&mut reader, &mut line, &event_log_path)? {
            return Err(session_error(format!(
                "session event log {} is missing its SessionCreated frame",
                event_log_path.display()
            )));
        }
        let line = decode_utf8_line(&line, 1, &event_log_path)?;
        if line.trim().is_empty() {
            return Err(session_error(format!(
                "session event log {} has an empty first frame",
                event_log_path.display()
            )));
        }
        let event = decode_event_line(line, 1, &event_log_path)?;
        validate_contiguous_session_sequence(&event, 1)?;
        validate_event_for_session(&event, &handle.manifest.session_id)?;
        match event.data {
            SessionEventData::SessionCreated {
                cwd,
                workspace_scope,
            } => Ok(SessionCreationWorkspace {
                cwd,
                workspace_scope,
            }),
            _ => Err(session_error(format!(
                "session event log {} must begin with SessionCreated",
                event_log_path.display()
            ))),
        }
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
                        || crate::kernel::operation::OperationKind::from_str(kind).is_none()
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
        let mut manifest = read_manifest(&handle.session_dir)?;
        patch.apply(&mut manifest);
        validate_manifest(&manifest)?;
        write_manifest(&handle.session_dir, &manifest)
    }

    pub(crate) fn migrate_manifest_workspace(
        &self,
        handle: &SessionHandle,
        workspace_scope: PersistedWorkspaceScope,
    ) -> Result<SessionHandle, CodingSessionError> {
        let _lease = self.acquire_write_lease(handle)?;
        let mut manifest = read_manifest(&handle.session_dir)?;
        if manifest.workspace_scope.is_none() {
            self.update_manifest(
                handle,
                ManifestPatch::new().workspace_migration(workspace_scope),
            )?;
            manifest = read_manifest(&handle.session_dir)?;
        }
        validate_manifest(&manifest)?;
        Ok(SessionHandle {
            session_dir: handle.session_dir.clone(),
            manifest,
        })
    }

    pub(crate) fn remove_session(&self, handle: &SessionHandle) -> Result<(), CodingSessionError> {
        let session_dir = self.resolve_existing_session_dir(handle.session_dir())?;
        self.remove_created_session_dir(&session_dir)
    }

    fn remove_created_session_dir(&self, session_dir: &Path) -> Result<(), CodingSessionError> {
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
    pub(crate) workspace_scope: PersistedWorkspaceScope,
}
