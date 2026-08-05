use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

use super::*;

fn options() -> HunkTrackerOptions {
    HunkTrackerOptions {
        causal_window: Duration::from_millis(5),
        ..HunkTrackerOptions::default()
    }
}

fn context() -> TrackingContext {
    TrackingContext {
        session_id: "session-1".into(),
        turn_id: "turn-2".into(),
        operation_id: "operation-3".into(),
    }
}

fn receipt(path: &str, before: Option<&[u8]>, after: &[u8], diff: Option<&str>) -> ChangeReceipt {
    ChangeReceipt {
        path: path.into(),
        target_fingerprint: format!("target:{path}"),
        before_revision: before.map(revision),
        after_revision: revision(after),
        byte_delta: i64::try_from(after.len()).unwrap()
            - i64::try_from(before.map_or(0, <[u8]>::len)).unwrap(),
        line_delta: 0,
        origin: "edit".into(),
        unified_diff: diff.map(str::to_owned),
    }
}

fn event(root: &Path, path: &str, kind: FsChangeKind) -> FsEvent {
    FsEvent::Workspace(SemanticEvent {
        sequence: 1,
        root: std::fs::canonicalize(root).unwrap(),
        path: PathBuf::from(path),
        from: None,
        kind,
        at: SystemTime::now(),
    })
}

fn rename_event(root: &Path, from: &str, to: &str) -> FsEvent {
    FsEvent::Workspace(SemanticEvent {
        sequence: 2,
        root: std::fs::canonicalize(root).unwrap(),
        path: PathBuf::from(to),
        from: Some(PathBuf::from(from)),
        kind: FsChangeKind::Renamed,
        at: SystemTime::now(),
    })
}

fn patch(old_start: usize, new_start: usize, removed: &str, added: &str) -> String {
    format!(
        "--- notes.txt\n+++ notes.txt\n@@ -{old_start},1 +{new_start},1 @@\n-{removed}\n+{added}"
    )
}

async fn expire_window() {
    tokio::time::sleep(Duration::from_millis(8)).await;
}

#[tokio::test]
async fn receipt_then_event_is_attributed_to_agent_by_revision() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "after\n").unwrap();
    let tracker = HunkTracker::start(dir.path(), options()).unwrap();
    let handle = tracker.handle();
    let change = receipt(
        "notes.txt",
        Some(b"before\n"),
        b"after\n",
        Some(&patch(1, 1, "before", "after")),
    );

    handle
        .record_receipt(change, ChangeSource::AgentEdit, context())
        .await
        .unwrap();
    handle
        .observe(event(dir.path(), "notes.txt", FsChangeKind::Modified))
        .await
        .unwrap();

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.files.len(), 1);
    assert_eq!(snapshot.files[0].source, ChangeSource::AgentEdit);
    assert_eq!(snapshot.files[0].context, Some(context()));
    assert_eq!(snapshot.facts.len(), 1);
    assert_eq!(snapshot.reconcile, ReconcileState::Ready);
    tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn event_then_receipt_matches_only_the_exact_after_revision() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "after\n").unwrap();
    let tracker = HunkTracker::start(dir.path(), options()).unwrap();
    let handle = tracker.handle();
    handle
        .observe(event(dir.path(), "notes.txt", FsChangeKind::Modified))
        .await
        .unwrap();
    handle
        .record_receipt(
            receipt(
                "notes.txt",
                Some(b"before\n"),
                b"after\n",
                Some(&patch(1, 1, "before", "after")),
            ),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();
    expire_window().await;

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.files[0].source, ChangeSource::AgentEdit);
    tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn unmatched_revision_becomes_external_on_an_agent_file() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "agent\n").unwrap();
    let tracker = HunkTracker::start(dir.path(), options()).unwrap();
    let handle = tracker.handle();
    handle
        .record_receipt(
            receipt(
                "notes.txt",
                Some(b"before\n"),
                b"agent\n",
                Some(&patch(1, 1, "before", "agent")),
            ),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();
    std::fs::write(dir.path().join("notes.txt"), "external\n").unwrap();
    handle
        .observe(event(dir.path(), "notes.txt", FsChangeKind::Modified))
        .await
        .unwrap();
    expire_window().await;

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.files[0].source,
        ChangeSource::ExternalEditOnAgentFile
    );
    assert_eq!(
        snapshot.files[0].before_revision,
        Some(revision(b"agent\n"))
    );
    assert_eq!(snapshot.files[0].after_revision, revision(b"external\n"));
    assert_eq!(snapshot.facts.len(), 2);
    assert_eq!(snapshot.facts[0].source, ChangeSource::AgentEdit);
    assert_eq!(snapshot.facts[0].context, Some(context()));
    assert_eq!(
        snapshot.facts[1].source,
        ChangeSource::ExternalEditOnAgentFile
    );
    assert!(snapshot.facts[1].hunks[0].unified_diff.is_some());
    tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn untouched_file_is_classified_as_external() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("outside.txt"), "outside\n").unwrap();
    let tracker = HunkTracker::start(dir.path(), options()).unwrap();
    let handle = tracker.handle();
    handle
        .observe(event(dir.path(), "outside.txt", FsChangeKind::Created))
        .await
        .unwrap();
    expire_window().await;

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.files[0].source, ChangeSource::ExternalEdit);
    assert_eq!(snapshot.files[0].context, None);
    tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn merge_and_hook_receipts_preserve_explicit_sources() {
    for source in [ChangeSource::MergeApply, ChangeSource::HookEdit] {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "after\n").unwrap();
        let tracker = HunkTracker::start(dir.path(), options()).unwrap();
        let handle = tracker.handle();
        handle
            .record_receipt(
                receipt(
                    "notes.txt",
                    Some(b"before\n"),
                    b"after\n",
                    Some(&patch(1, 1, "before", "after")),
                ),
                source,
                context(),
            )
            .await
            .unwrap();
        assert_eq!(handle.snapshot().await.unwrap().files[0].source, source);
        tracker.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn content_identity_survives_position_drift() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "after\n").unwrap();
    let tracker = HunkTracker::start(dir.path(), options()).unwrap();
    let handle = tracker.handle();
    handle
        .record_receipt(
            receipt(
                "notes.txt",
                Some(b"before\n"),
                b"after\n",
                Some(&patch(2, 2, "before", "after")),
            ),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();
    let first = handle.snapshot().await.unwrap().files[0].hunks[0]
        .id
        .clone();
    let mut shifted = receipt(
        "notes.txt",
        Some(b"prefix\nbefore\n"),
        b"prefix\nafter\n",
        Some(&patch(20, 20, "before", "after")),
    );
    shifted.after_revision = revision(b"after\n");
    handle
        .record_receipt(shifted, ChangeSource::AgentEdit, context())
        .await
        .unwrap();
    let second = handle.snapshot().await.unwrap().files[0].hunks[0]
        .id
        .clone();

    assert_eq!(first, second);
    tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn rename_keeps_file_state_and_hunk_identity() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("old.txt"), "after\n").unwrap();
    let tracker = HunkTracker::start(dir.path(), options()).unwrap();
    let handle = tracker.handle();
    handle
        .record_receipt(
            receipt(
                "old.txt",
                Some(b"before\n"),
                b"after\n",
                Some(&patch(1, 1, "before", "after")),
            ),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();
    let id = handle.snapshot().await.unwrap().files[0].hunks[0]
        .id
        .clone();
    std::fs::rename(dir.path().join("old.txt"), dir.path().join("new.txt")).unwrap();
    handle
        .observe(rename_event(dir.path(), "old.txt", "new.txt"))
        .await
        .unwrap();
    std::fs::write(dir.path().join("new.txt"), "newer\n").unwrap();
    handle
        .record_receipt(
            receipt(
                "new.txt",
                Some(b"after\n"),
                b"newer\n",
                Some(&patch(1, 1, "after", "newer")),
            ),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.files[0].path, PathBuf::from("new.txt"));
    assert_eq!(snapshot.files[0].hunks[0].id, id);
    assert_eq!(snapshot.facts.len(), 2);
    tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn watch_gap_requires_reconciliation_and_accumulates_loss() {
    let dir = TempDir::new().unwrap();
    let tracker = HunkTracker::start(dir.path(), options()).unwrap();
    let handle = tracker.handle();
    handle.observe(FsEvent::WatchGap { lost: 2 }).await.unwrap();
    handle.observe(FsEvent::WatchGap { lost: 3 }).await.unwrap();
    assert_eq!(
        handle.snapshot().await.unwrap().reconcile,
        ReconcileState::Required { lost: 5 }
    );
    tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn budgets_and_invalid_sources_fail_closed_without_mutating_snapshot() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "after\n").unwrap();
    let mut limited = options();
    limited.max_diff_bytes = 16;
    let tracker = HunkTracker::start(dir.path(), limited).unwrap();
    let handle = tracker.handle();
    let oversized = handle
        .record_receipt(
            receipt(
                "notes.txt",
                Some(b"before\n"),
                b"after\n",
                Some(&patch(1, 1, "before", "after")),
            ),
            ChangeSource::AgentEdit,
            context(),
        )
        .await;
    assert!(matches!(
        oversized,
        Err(ChangeTrackerError::BudgetExceeded { .. })
    ));
    let invalid = handle
        .record_receipt(
            receipt("notes.txt", None, b"after\n", None),
            ChangeSource::ExternalEdit,
            context(),
        )
        .await;
    assert!(matches!(
        invalid,
        Err(ChangeTrackerError::InvalidFact { .. })
    ));
    assert!(handle.snapshot().await.unwrap().files.is_empty());
    tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn change_fact_and_hunk_budgets_are_transactional() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "after\n").unwrap();
    let mut limited = options();
    limited.max_change_facts = 1;
    limited.max_hunks_per_file = 1;
    let tracker = HunkTracker::start(dir.path(), limited).unwrap();
    let handle = tracker.handle();
    handle
        .record_receipt(
            receipt(
                "notes.txt",
                Some(b"before\n"),
                b"after\n",
                Some(&patch(1, 1, "before", "after")),
            ),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();
    let before = handle.snapshot().await.unwrap();
    let fact_error = handle
        .record_receipt(
            receipt("notes.txt", Some(b"after\n"), b"after\n", None),
            ChangeSource::AgentEdit,
            context(),
        )
        .await;
    assert!(matches!(
        fact_error,
        Err(ChangeTrackerError::BudgetExceeded { .. })
    ));
    assert_eq!(handle.snapshot().await.unwrap(), before);
    tracker.shutdown().await.unwrap();

    let mut limited = options();
    limited.max_hunks_per_file = 1;
    let tracker = HunkTracker::start(dir.path(), limited).unwrap();
    let handle = tracker.handle();
    let two_hunks = "--- notes.txt\n+++ notes.txt\n@@ -1,1 +1,1 @@\n-before\n+after\n@@ -8,1 +8,1 @@\n-old\n+new";
    let error = handle
        .record_receipt(
            receipt("notes.txt", Some(b"before\n"), b"after\n", Some(two_hunks)),
            ChangeSource::AgentEdit,
            context(),
        )
        .await;
    assert!(matches!(
        error,
        Err(ChangeTrackerError::BudgetExceeded { .. })
    ));
    assert!(handle.snapshot().await.unwrap().facts.is_empty());
    tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn repeated_snapshots_are_deterministic_and_shutdown_closes_handle() {
    let dir = TempDir::new().unwrap();
    let tracker = HunkTracker::start(dir.path(), options()).unwrap();
    let handle = tracker.handle();
    let first = handle.snapshot().await.unwrap();
    let second = handle.snapshot().await.unwrap();
    assert_eq!(first, second);
    tracker.shutdown().await.unwrap();
    assert!(matches!(
        handle.snapshot().await,
        Err(ChangeTrackerError::Shutdown)
    ));
}

#[tokio::test]
async fn oversized_content_is_hashed_without_retaining_an_unbounded_diff() {
    let dir = TempDir::new().unwrap();
    let content = b"0123456789abcdef";
    std::fs::write(dir.path().join("large.txt"), content).unwrap();
    let mut limited = options();
    limited.max_content_bytes = 4;
    let tracker = HunkTracker::start(dir.path(), limited).unwrap();
    let handle = tracker.handle();
    handle
        .observe(event(dir.path(), "large.txt", FsChangeKind::Created))
        .await
        .unwrap();
    expire_window().await;
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.files[0].after_revision, revision(content));
    assert_eq!(snapshot.files[0].hunks[0].unified_diff, None);
    tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn saturated_command_queue_fails_closed() {
    let (commands, _receiver) = mpsc::channel(1);
    let (reply, _reply_receiver) = oneshot::channel();
    assert!(commands.try_send(Command::Snapshot { reply }).is_ok());
    let handle = HunkTrackerHandle { commands };
    assert!(matches!(
        handle.snapshot().await,
        Err(ChangeTrackerError::BudgetExceeded { .. })
    ));
}

#[tokio::test]
async fn combined_service_forwards_real_fs_events_into_revision_correlation() {
    let dir = TempDir::new().unwrap();
    let workspace =
        WorkspaceHandle::new(workspace_runtime::api::WorkspaceKind::Source, dir.path()).unwrap();
    let watch_options = WatchOptions {
        debounce: Duration::from_millis(10),
        ..WatchOptions::default()
    };
    let hunk_options = HunkTrackerOptions {
        causal_window: Duration::from_secs(1),
        ..options()
    };
    let service = HunkTrackingService::start(&workspace, watch_options, hunk_options).unwrap();
    let handle = service.handle();

    std::fs::write(dir.path().join("notes.txt"), "after\n").unwrap();
    handle
        .record_receipt(
            receipt(
                "notes.txt",
                None,
                b"after\n",
                Some(&patch(0, 1, "", "after")),
            ),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    let snapshot = loop {
        let snapshot = handle.snapshot().await.unwrap();
        if snapshot.pending_receipts == 0 {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "watch event did not match receipt"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(snapshot.pending_events, 0);
    assert_eq!(snapshot.facts.len(), 1);
    assert_eq!(snapshot.facts[0].source, ChangeSource::AgentEdit);
    service.shutdown().await.unwrap();
}

#[test]
fn start_without_a_tokio_runtime_returns_a_structured_error() {
    let dir = TempDir::new().unwrap();
    assert!(matches!(
        HunkTracker::start(dir.path(), options()),
        Err(ChangeTrackerError::WatchFailed { .. })
    ));
}
