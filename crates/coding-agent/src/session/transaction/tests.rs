use super::*;

#[cfg(test)]
mod queue_backpressure_tests {
    use super::*;
    use crate::session::manifest::PersistedWorkspaceScope;
    use crate::session::repository::CreateSessionOptions;

    fn writer_fixture(
        session_id: &str,
    ) -> (
        tempfile::TempDir,
        SessionLogStore,
        SessionHandle,
        SessionTransactionWriter,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionLogStore::new(temp.path().join("sessions"));
        let workspace_scope = PersistedWorkspaceScope::Projectless {
            workspace_id: format!("workspace-{session_id}"),
        };
        let handle = store
            .create_session(
                CreateSessionOptions::new(session_id, "2026-08-02T00:00:00Z")
                    .default_agent_profile_id(ProfileId::from("default"))
                    .workspace_scope(workspace_scope.clone()),
            )
            .unwrap();
        let writer = SessionTransactionWriter::new(store.clone(), handle.clone()).unwrap();
        writer
            .initialize_session_with_receipt_blocking(SessionEventEnvelope::new(
                session_id,
                "event-created",
                "2026-08-02T00:00:00Z",
                SessionEventData::SessionCreated {
                    cwd: None,
                    workspace_scope: Some(workspace_scope),
                },
            ))
            .unwrap();
        (temp, store, handle, writer)
    }

    fn checkpoint(session_id: &str, index: usize) -> SessionEventEnvelope {
        SessionEventEnvelope::new(
            session_id,
            format!("event-checkpoint-{index:03}"),
            "2026-08-02T00:00:01Z",
            SessionEventData::DiagnosticEmitted {
                level: DiagnosticLevel::Info,
                message: format!("checkpoint-{index:03}"),
            },
        )
    }

    #[test]
    fn hundred_checkpoint_burst_survives_a_slow_writer_without_loss_or_hard_failure() {
        let session_id = "session-backpressure-burst";
        let (_temp, store, handle, writer) = writer_fixture(session_id);
        writer.set_command_delay(Duration::from_millis(200));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let results = runtime.block_on(async {
            let writes = (0..100).map(|index| {
                let writer = writer.clone();
                async move {
                    writer
                        .append_checkpoint_events(vec![checkpoint(session_id, index)])
                        .await
                }
            });
            let drain = async {
                loop {
                    // One command is executing while the other 99 fill the
                    // bounded queue. Release the injected delay only after the
                    // complete reliability-plan burst has been absorbed.
                    if writer.remaining_queue_capacity()
                        <= SESSION_TRANSACTION_WRITER_CAPACITY.saturating_sub(99)
                    {
                        writer.set_command_delay(Duration::ZERO);
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            };
            let (results, ()) = tokio::join!(futures::future::join_all(writes), drain);
            results
        });
        assert!(results.iter().all(Result::is_ok), "{results:?}");
        assert_eq!(SESSION_TRANSACTION_WRITER_CAPACITY, 128);

        let events = store.read_events(&handle).unwrap();
        let committed = events
            .iter()
            .filter(|event| {
                matches!(
                    &event.data,
                    SessionEventData::DiagnosticEmitted { message, .. }
                        if message.starts_with("checkpoint-")
                )
            })
            .count();
        assert_eq!(committed, 100);
        writer.shutdown().unwrap();
    }

    #[test]
    fn enqueue_timeout_is_a_structured_queue_saturated_failure() {
        let session_id = "session-backpressure-timeout";
        let (_temp, _store, _handle, writer) = writer_fixture(session_id);
        writer.set_command_delay(Duration::from_millis(200));
        writer.set_enqueue_timeout(Duration::from_millis(25));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let results = runtime.block_on(async {
            let writes = (0..130).map(|index| {
                let writer = writer.clone();
                async move {
                    writer
                        .append_checkpoint_events(vec![checkpoint(session_id, index)])
                        .await
                }
            });
            let drain = async {
                while writer.remaining_queue_capacity() != 0 {
                    tokio::task::yield_now().await;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                writer.set_command_delay(Duration::ZERO);
            };
            let (results, ()) = tokio::join!(futures::future::join_all(writes), drain);
            results
        });
        let failures = results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .collect::<Vec<_>>();
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(matches!(
            failures[0],
            CodingSessionError::SessionWriteFailure {
                reason: SessionWriteFailureReason::QueueSaturated,
                ..
            }
        ));
        writer.shutdown().unwrap();
    }
}
