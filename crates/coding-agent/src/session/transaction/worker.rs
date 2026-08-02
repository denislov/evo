use super::*;

pub(super) fn writer_registry_key(handle: &SessionHandle) -> PathBuf {
    handle
        .session_dir()
        .canonicalize()
        .unwrap_or_else(|_| handle.session_dir().to_path_buf())
}

pub(super) fn execute_writer_command(
    store: &SessionLogStore,
    handle: &mut SessionHandle,
    write_lease: &mut SessionWriteLease,
    command: SessionTransactionWriterCommand,
) -> Result<SessionCommitReceipt, CodingSessionError> {
    match command {
        SessionTransactionWriterCommand::InitializeSession { event } => {
            if !matches!(&event.data, SessionEventData::SessionCreated { .. }) {
                return Err(CodingSessionError::Session {
                    message: "session writer initialize command requires SessionCreated".into(),
                });
            }
            if !store.read_events(handle)?.is_empty() {
                return Err(CodingSessionError::Session {
                    message: "session writer cannot initialize a non-empty event log".into(),
                });
            }
            let sequence = store.append_events_with_cursor(handle, &[event], write_lease)?;
            Ok(SessionCommitReceipt {
                committed_session_sequence: Some(sequence),
            })
        }
        SessionTransactionWriterCommand::Checkpoint { events } => {
            let committed_session_sequence = if events.is_empty() {
                None
            } else {
                Some(store.append_events_with_cursor(handle, &events, write_lease)?)
            };
            Ok(SessionCommitReceipt {
                committed_session_sequence,
            })
        }
        SessionTransactionWriterCommand::Finalize {
            events,
            outbox_records,
            updated_at,
            active_leaf_id,
        } => {
            let committed_session_sequence =
                store.append_events_and_outbox(handle, &events, &outbox_records, write_lease)?;
            if active_leaf_id.is_some() {
                store.update_manifest(
                    handle,
                    ManifestPatch::new()
                        .updated_at(updated_at)
                        .active_leaf_id(active_leaf_id),
                )?;
                refresh_writer_handle(store, handle)?;
            }
            Ok(SessionCommitReceipt {
                committed_session_sequence: Some(committed_session_sequence),
            })
        }
        SessionTransactionWriterCommand::CommitSessionMutation {
            events,
            outbox_records,
            manifest_patch,
            operation_id,
        } => {
            let committed_session_sequence = if events.is_empty() {
                None
            } else if outbox_records.is_empty() {
                Some(store.append_events_with_cursor(handle, &events, write_lease)?)
            } else {
                Some(
                    store
                        .append_events_and_outbox(handle, &events, &outbox_records, write_lease)
                        .map_err(|error| mutation_commit_error(operation_id.as_deref(), error))?,
                )
            };
            store
                .update_manifest(handle, manifest_patch)
                .map_err(|error| mutation_commit_error(operation_id.as_deref(), error))?;
            refresh_writer_handle(store, handle)
                .map_err(|error| mutation_commit_error(operation_id.as_deref(), error))?;
            Ok(SessionCommitReceipt {
                committed_session_sequence,
            })
        }
        SessionTransactionWriterCommand::CommitSessionNameIfUnset {
            events,
            manifest_patch,
            operation_id,
        } => {
            let committed_session_sequence = if events.is_empty() {
                None
            } else {
                Some(
                    store
                        .append_events_with_cursor(handle, &events, write_lease)
                        .map_err(|error| mutation_commit_error(Some(&operation_id), error))?,
                )
            };
            if handle.manifest().name.is_some() {
                return Ok(SessionCommitReceipt {
                    committed_session_sequence,
                });
            }
            store
                .update_manifest(handle, manifest_patch)
                .map_err(|error| mutation_commit_error(Some(&operation_id), error))?;
            refresh_writer_handle(store, handle)
                .map_err(|error| mutation_commit_error(Some(&operation_id), error))?;
            Ok(SessionCommitReceipt {
                committed_session_sequence,
            })
        }
    }
}

pub(super) fn refresh_writer_handle(
    store: &SessionLogStore,
    handle: &mut SessionHandle,
) -> Result<(), CodingSessionError> {
    let session_id = handle.manifest().session_id.clone();
    *handle = store.open_session_id(&session_id)?;
    Ok(())
}

pub(super) fn mutation_commit_error(
    operation_id: Option<&str>,
    error: CodingSessionError,
) -> CodingSessionError {
    match operation_id {
        Some(operation_id) => CodingSessionError::PartialCommit {
            operation_id: operation_id.to_owned(),
            message: error.to_string(),
        },
        None => error,
    }
}
