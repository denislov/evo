use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use similar::TextDiff;
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
        tool_call_id: Some("call-4".into()),
    }
}

fn receipt(path: &str, before: Option<&[u8]>, after: &[u8], diff: Option<&str>) -> ChangeReceipt {
    receipt_state(path, before, Some(after), diff)
}

fn receipt_state(
    path: &str,
    before: Option<&[u8]>,
    after: Option<&[u8]>,
    diff: Option<&str>,
) -> ChangeReceipt {
    let after_bytes = after.unwrap_or_default();
    ChangeReceipt {
        path: path.into(),
        target_fingerprint: format!("target:{path}"),
        before_revision: before.map(revision),
        after_revision: revision(after_bytes),
        after_exists: after.is_some(),
        byte_delta: i64::try_from(after_bytes.len()).unwrap()
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
        is_directory: false,
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
        is_directory: false,
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

fn full_patch(path: &str, before: &[u8], after: &[u8]) -> String {
    let before = std::str::from_utf8(before).unwrap();
    let after = std::str::from_utf8(after).unwrap();
    TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(usize::MAX / 4)
        .header(path, path)
        .to_string()
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
        Some(revision(b"before\n"))
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
    let before = b"top\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\nbefore\nbottom\n";
    let after = b"top\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\nafter\nbottom\n";
    std::fs::write(dir.path().join("notes.txt"), after).unwrap();
    let tracker = HunkTracker::start(dir.path(), options()).unwrap();
    let handle = tracker.handle();
    handle
        .record_receipt(
            receipt(
                "notes.txt",
                Some(before),
                after,
                Some(&full_patch("notes.txt", before, after)),
            ),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();
    let first = handle.snapshot().await.unwrap().files[0]
        .hunks
        .iter()
        .find(|hunk| hunk.source == ChangeSource::AgentEdit)
        .unwrap()
        .id
        .clone();
    let shifted = [b"prefix\n".as_slice(), after].concat();
    std::fs::write(dir.path().join("notes.txt"), &shifted).unwrap();
    handle
        .observe(event(dir.path(), "notes.txt", FsChangeKind::Modified))
        .await
        .unwrap();
    expire_window().await;
    let snapshot = handle.snapshot().await.unwrap();
    let second = snapshot.files[0]
        .hunks
        .iter()
        .find(|hunk| hunk.source == ChangeSource::AgentEdit)
        .unwrap()
        .id
        .clone();

    assert_eq!(first, second);
    assert!(
        snapshot.files[0]
            .hunks
            .iter()
            .any(|hunk| hunk.source == ChangeSource::ExternalEditOnAgentFile)
    );
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
async fn consecutive_receipts_recompute_from_original_baseline() {
    let dir = TempDir::new().unwrap();
    let baseline = b"first\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\nsecond\n";
    let first_edit = b"FIRST\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\nsecond\n";
    let second_edit = b"FIRST\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\nSECOND\n";
    std::fs::write(dir.path().join("notes.txt"), first_edit).unwrap();
    let tracker = HunkTracker::start(dir.path(), options()).unwrap();
    let handle = tracker.handle();
    handle
        .record_receipt(
            receipt(
                "notes.txt",
                Some(baseline),
                first_edit,
                Some(&full_patch("notes.txt", baseline, first_edit)),
            ),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();
    let first_id = handle.snapshot().await.unwrap().files[0].hunks[0]
        .id
        .clone();

    std::fs::write(dir.path().join("notes.txt"), second_edit).unwrap();
    handle
        .record_receipt(
            receipt(
                "notes.txt",
                Some(first_edit),
                second_edit,
                Some(&full_patch("notes.txt", first_edit, second_edit)),
            ),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.files[0].before_revision, Some(revision(baseline)));
    assert_eq!(snapshot.files[0].hunks.len(), 2);
    assert!(
        snapshot.files[0]
            .hunks
            .iter()
            .any(|hunk| hunk.id == first_id)
    );
    tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn accept_hunk_updates_baseline_and_reject_plan_reverts_only_remaining_hunk() {
    let dir = TempDir::new().unwrap();
    let baseline = b"first\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\nsecond\n";
    let current = b"FIRST\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\nSECOND\n";
    std::fs::write(dir.path().join("notes.txt"), current).unwrap();
    let tracker = HunkTracker::start(dir.path(), options()).unwrap();
    let handle = tracker.handle();
    handle
        .record_receipt(
            receipt(
                "notes.txt",
                Some(baseline),
                current,
                Some(&full_patch("notes.txt", baseline, current)),
            ),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();
    let initial = handle.snapshot().await.unwrap().files.remove(0);
    let accepted = initial.hunks[0].clone();
    handle
        .accept_hunk(
            &initial.path,
            initial.recorded_sequence,
            accepted.id,
            &initial.after_revision,
            "target:notes.txt",
        )
        .await
        .unwrap();
    let remaining = handle.snapshot().await.unwrap().files.remove(0);
    assert_eq!(remaining.hunks.len(), 1);
    let plan = handle
        .prepare_reject_hunk(
            &remaining.path,
            remaining.recorded_sequence,
            remaining.hunks[0].id.clone(),
            &remaining.after_revision,
            "target:notes.txt",
        )
        .await
        .unwrap();
    let RejectReplacement::Write(replacement) = plan.replacement else {
        panic!("remaining text hunk must produce a write plan");
    };
    assert_ne!(replacement, baseline);
    assert_ne!(replacement, current);
    std::fs::write(dir.path().join("notes.txt"), &replacement).unwrap();
    handle
        .record_receipt(
            receipt(
                "notes.txt",
                Some(current),
                &replacement,
                Some(&full_patch("notes.txt", current, &replacement)),
            ),
            ChangeSource::HookEdit,
            context(),
        )
        .await
        .unwrap();
    assert!(handle.snapshot().await.unwrap().files.is_empty());
    tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn reject_file_distinguishes_created_deleted_and_empty_files() {
    let created_dir = TempDir::new().unwrap();
    std::fs::write(created_dir.path().join("empty.txt"), []).unwrap();
    let tracker = HunkTracker::start(created_dir.path(), options()).unwrap();
    let handle = tracker.handle();
    handle
        .record_receipt(
            receipt("empty.txt", None, b"", Some("")),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();
    let created = handle.snapshot().await.unwrap().files.remove(0);
    assert!(created.after_exists);
    assert!(matches!(
        handle
            .prepare_reject_file(
                &created.path,
                created.recorded_sequence,
                &created.after_revision,
                "target:empty.txt",
            )
            .await
            .unwrap()
            .replacement,
        RejectReplacement::Delete
    ));
    tracker.shutdown().await.unwrap();

    let deleted_dir = TempDir::new().unwrap();
    let tracker = HunkTracker::start(deleted_dir.path(), options()).unwrap();
    let handle = tracker.handle();
    handle
        .record_receipt(
            receipt_state("empty.txt", Some(b""), None, Some("")),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();
    let deleted = handle.snapshot().await.unwrap().files.remove(0);
    assert!(!deleted.after_exists);
    assert!(matches!(
        handle
            .prepare_reject_file(
                &deleted.path,
                deleted.recorded_sequence,
                &deleted.after_revision,
                "target:empty.txt",
            )
            .await
            .unwrap()
            .replacement,
        RejectReplacement::Write(bytes) if bytes.is_empty()
    ));
    tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn stale_hunk_revision_and_fingerprint_fail_closed() {
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
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();
    let snapshot = handle.snapshot().await.unwrap().files.remove(0);
    assert!(
        handle
            .prepare_reject_hunk(
                &snapshot.path,
                snapshot.recorded_sequence,
                HunkId::parse("missing-hunk").unwrap(),
                &snapshot.after_revision,
                "target:notes.txt",
            )
            .await
            .is_err()
    );
    assert!(
        handle
            .prepare_reject_file(
                &snapshot.path,
                snapshot.recorded_sequence,
                "0".repeat(64),
                "target:notes.txt",
            )
            .await
            .is_err()
    );
    assert!(
        handle
            .prepare_reject_file(
                &snapshot.path,
                snapshot.recorded_sequence,
                &snapshot.after_revision,
                "replacement-target",
            )
            .await
            .is_err()
    );
    tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn reject_refuses_binary_or_unavailable_baselines() {
    let dir = TempDir::new().unwrap();
    let before = b"\0before";
    let after = b"\0after";
    std::fs::write(dir.path().join("binary.dat"), after).unwrap();
    let tracker = HunkTracker::start(dir.path(), options()).unwrap();
    let handle = tracker.handle();
    handle
        .record_receipt(
            receipt("binary.dat", Some(before), after, None),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();
    let snapshot = handle.snapshot().await.unwrap().files.remove(0);
    assert!(snapshot.hunks[0].unified_diff.is_none());
    assert!(matches!(
        handle
            .prepare_reject_file(
                &snapshot.path,
                snapshot.recorded_sequence,
                &snapshot.after_revision,
                "target:binary.dat",
            )
            .await,
        Err(ChangeTrackerError::InvalidFact { .. })
    ));
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
    let before = b"before\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\nold\n";
    let after = b"after\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\nnew\n";
    std::fs::write(dir.path().join("notes.txt"), after).unwrap();
    let two_hunks = full_patch("notes.txt", before, after);
    let error = handle
        .record_receipt(
            receipt("notes.txt", Some(before), after, Some(&two_hunks)),
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
    let (snapshots, _) = watch::channel(HunkTrackerSnapshot::empty());
    let (reply, _reply_receiver) = oneshot::channel();
    assert!(commands.try_send(Command::Snapshot { reply }).is_ok());
    let handle = HunkTrackerHandle {
        commands,
        snapshots,
    };
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

#[tokio::test]
async fn directory_events_do_not_stop_file_tracking() {
    let dir = TempDir::new().unwrap();
    let workspace =
        WorkspaceHandle::new(workspace_runtime::api::WorkspaceKind::Source, dir.path()).unwrap();
    let service = HunkTrackingService::start(
        &workspace,
        WatchOptions {
            debounce: Duration::from_millis(10),
            ..WatchOptions::default()
        },
        HunkTrackerOptions {
            causal_window: Duration::from_millis(40),
            ..options()
        },
    )
    .unwrap();
    let handle = service.handle();

    std::fs::create_dir(dir.path().join("nested")).unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    std::fs::write(dir.path().join("nested/notes.txt"), "external\n").unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let snapshot = handle.snapshot().await.unwrap();
        if snapshot
            .files
            .iter()
            .any(|file| file.path == Path::new("nested/notes.txt"))
        {
            assert!(
                snapshot
                    .files
                    .iter()
                    .all(|file| file.path != Path::new("nested"))
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "directory event stopped the file event forwarder"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn checkpoint_round_trip_restores_state_and_hunk_identity() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "before\n").unwrap();
    let tracker = HunkTracker::start(dir.path(), options()).unwrap();
    let handle = tracker.handle();

    std::fs::write(&path, "after\n").unwrap();
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
    let checkpoint = handle.checkpoint().await.unwrap();
    let hunk_id = checkpoint.snapshot().files[0].hunks[0].id.clone();
    let encoded = serde_json::to_vec(&checkpoint).unwrap();
    let decoded: HunkTrackerCheckpoint = serde_json::from_slice(&encoded).unwrap();
    tracker.shutdown().await.unwrap();

    let restored = HunkTracker::start(dir.path(), options()).unwrap();
    let restored_handle = restored.handle();
    restored_handle.restore_checkpoint(decoded).await.unwrap();
    assert_eq!(
        restored_handle.snapshot().await.unwrap(),
        checkpoint.snapshot()
    );

    std::fs::write(&path, "later\n").unwrap();
    restored_handle
        .record_receipt(
            receipt(
                "notes.txt",
                Some(b"after\n"),
                b"later\n",
                Some(&patch(1, 1, "after", "later")),
            ),
            ChangeSource::AgentEdit,
            context(),
        )
        .await
        .unwrap();
    assert_eq!(
        restored_handle.snapshot().await.unwrap().files[0].hunks[0].id,
        hunk_id
    );
    restored.shutdown().await.unwrap();
}

#[tokio::test]
async fn checkpoint_restore_rejects_stale_workspace_and_corrupt_content() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "before\n").unwrap();
    let tracker = HunkTracker::start(dir.path(), options()).unwrap();
    let handle = tracker.handle();
    std::fs::write(&path, "after\n").unwrap();
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
    let checkpoint = handle.checkpoint().await.unwrap();

    std::fs::write(&path, "external\n").unwrap();
    assert!(matches!(
        handle.restore_checkpoint(checkpoint.clone()).await,
        Err(ChangeTrackerError::InvalidFact { .. })
    ));

    std::fs::write(&path, "after\n").unwrap();
    let mut corrupt = checkpoint;
    corrupt.files[0].current.as_mut().unwrap().content = Some(b"corrupt\n".to_vec());
    assert!(matches!(
        handle.restore_checkpoint(corrupt).await,
        Err(ChangeTrackerError::InvalidFact { .. })
    ));
    tracker.shutdown().await.unwrap();
}

#[test]
fn start_without_a_tokio_runtime_returns_a_structured_error() {
    let dir = TempDir::new().unwrap();
    assert!(matches!(
        HunkTracker::start(dir.path(), options()),
        Err(ChangeTrackerError::WatchFailed { .. })
    ));
}
