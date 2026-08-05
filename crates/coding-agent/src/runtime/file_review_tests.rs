use tempfile::TempDir;

use super::*;

fn change(path: &str) -> CodingAgentFileChangeSnapshot {
    CodingAgentFileChangeSnapshot {
        path: path.into(),
        mutation_kind: "apply_patch".into(),
        source: "agent_edit".into(),
        operation_id: "operation-1".into(),
        tool_call_id: Some("tool-1".into()),
        session_id: Some("session-1".into()),
        turn_id: Some("turn-1".into()),
        updated_sequence: 7,
        before_revision: Some("before".into()),
        after_revision: "after".into(),
        after_exists: true,
        first_changed_line: Some(1),
        added_lines: Some(1),
        removed_lines: Some(1),
        diff: None,
        hunks: Vec::new(),
    }
}

#[test]
fn authorization_keys_same_tool_batch_by_path() {
    let changes = vec![change("first.txt"), change("second.txt")];
    let identity = CodingAgentFileChangeIdentity {
        operation_id: "operation-1".into(),
        tool_call_id: Some("tool-1".into()),
        path: "second.txt".into(),
    };
    let authorized =
        authorize_change_identity(&changes, &identity, CodingAgentFileRevision::new(7)).unwrap();
    assert_eq!(authorized.path, "second.txt");
}

#[test]
fn action_request_binds_revision_hash_and_hunk_identity() {
    let change = change("notes.txt");
    let file = CodingAgentFileReviewActionRequest::from(&change);
    let request = CodingAgentHunkReviewActionRequest {
        file,
        hunk_id: "hunk-1".into(),
    };
    assert_eq!(request.file.revision.value(), 7);
    assert_eq!(request.file.after_revision, "after");
    assert_eq!(request.hunk_id, "hunk-1");
}

#[test]
fn fingerprint_errors_map_to_target_changed_before_stale_revision() {
    let error = map_tracker_error(change_tracker::ChangeTrackerError::InvalidFact {
        message: "review target fingerprint is stale: notes.txt".into(),
    });
    assert_eq!(error.code(), "file_review_target_changed");
}

#[tokio::test]
async fn action_target_refuses_same_content_replacement() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "after\n").unwrap();
    let mut change = change("notes.txt");
    change.after_revision = content_revision(b"after\n");
    let target = prepare_action_target(dir.path(), &change).await.unwrap();

    std::fs::rename(&path, dir.path().join("old.txt")).unwrap();
    std::fs::write(&path, "after\n").unwrap();

    let error = verify_action_target(&target, &change).await.unwrap_err();
    assert_eq!(error.code(), "file_review_target_changed");
}

#[tokio::test]
async fn reject_commit_uses_capability_bound_target_and_revision() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "after\n").unwrap();
    let mut change = change("notes.txt");
    change.after_revision = content_revision(b"after\n");
    let target = prepare_reject_target(dir.path(), &change.path)
        .await
        .unwrap();
    let mutation = FileMutation::begin(&target).await.unwrap();
    assert_eq!(
        verify_action_target(&target, &change).await.unwrap(),
        Some(b"after\n".to_vec())
    );
    let plan = RejectPlan {
        path: "notes.txt".into(),
        expected_sequence: 7,
        expected_revision: change.after_revision.clone(),
        expected_exists: true,
        target_fingerprint: target.target_fingerprint().into(),
        replacement: RejectReplacement::Write(b"before\n".to_vec()),
    };
    commit_reject_plan(target, mutation, &plan).await.unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("notes.txt")).unwrap(),
        b"before\n"
    );
}

#[tokio::test]
async fn reject_commit_refuses_a_replaced_target() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "after\n").unwrap();
    let mut change = change("notes.txt");
    change.after_revision = content_revision(b"after\n");
    let target = prepare_reject_target(dir.path(), &change.path)
        .await
        .unwrap();
    let mutation = FileMutation::begin(&target).await.unwrap();
    std::fs::rename(&path, dir.path().join("old.txt")).unwrap();
    std::fs::write(&path, "replacement\n").unwrap();
    let plan = RejectPlan {
        path: "notes.txt".into(),
        expected_sequence: 7,
        expected_revision: change.after_revision,
        expected_exists: true,
        target_fingerprint: target.target_fingerprint().into(),
        replacement: RejectReplacement::Delete,
    };
    let error = commit_reject_plan(target, mutation, &plan)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "file_review_target_changed");
    assert_eq!(std::fs::read(&path).unwrap(), b"replacement\n");
}

#[tokio::test]
async fn reject_write_refuses_a_replaced_target() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "after\n").unwrap();
    let mut change = change("notes.txt");
    change.after_revision = content_revision(b"after\n");
    let target = prepare_reject_target(dir.path(), &change.path)
        .await
        .unwrap();
    let mutation = FileMutation::begin(&target).await.unwrap();
    std::fs::rename(&path, dir.path().join("old.txt")).unwrap();
    std::fs::write(&path, "replacement\n").unwrap();
    let plan = RejectPlan {
        path: "notes.txt".into(),
        expected_sequence: 7,
        expected_revision: change.after_revision,
        expected_exists: true,
        target_fingerprint: target.target_fingerprint().into(),
        replacement: RejectReplacement::Write(b"before\n".to_vec()),
    };
    let error = commit_reject_plan(target, mutation, &plan)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "file_review_target_changed");
    assert_eq!(std::fs::read(&path).unwrap(), b"replacement\n");
    assert_eq!(
        std::fs::read(dir.path().join("old.txt")).unwrap(),
        b"after\n"
    );
}

#[tokio::test]
async fn session_list_open_and_reject_hunk_round_trip() {
    let dir = TempDir::new().unwrap();
    let registry = TempDir::new().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "after\n").unwrap();
    let session = CodingAgentSession::non_persistent_internal(
        crate::runtime::facade::CodingAgentSessionOptions::new()
            .with_cwd(dir.path())
            .with_worktree_registry_dir(registry.path()),
    )
    .await
    .unwrap();
    let filesystem = WorkspaceAccessHandle::open_source(dir.path().to_path_buf()).unwrap();
    let target = filesystem
        .prepare_target_for_tool("write", "notes.txt")
        .await
        .unwrap();
    session
        .runtime_host
        .review_service
        .mutation_tracking("session-1", "turn-1", "operation-1")
        .unwrap()
        .record(
            "tool-1",
            change_tracker::ChangeReceipt {
                path: "notes.txt".into(),
                target_fingerprint: target.target_fingerprint().into(),
                before_revision: Some(content_revision(b"before\n")),
                after_revision: content_revision(b"after\n"),
                after_exists: true,
                byte_delta: 0,
                line_delta: 0,
                origin: "edit".into(),
                unified_diff: Some(
                    "--- notes.txt\n+++ notes.txt\n@@ -1,1 +1,1 @@\n-before\n+after".into(),
                ),
            },
        )
        .await
        .unwrap();

    let changes = session.list_changes().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].mutation_kind, "edit");
    let opened = session
        .open_change(CodingAgentFileReviewRequest::from(&changes[0]))
        .await
        .unwrap();
    assert_eq!(opened.content, "after\n");
    assert_eq!(opened.added_lines, Some(1));
    let request = CodingAgentHunkReviewActionRequest {
        file: CodingAgentFileReviewActionRequest::from(&changes[0]),
        hunk_id: changes[0].hunks[0].id.clone(),
    };
    session.reject_hunk(request).await.unwrap();

    assert_eq!(std::fs::read(&path).unwrap(), b"before\n");
    assert!(session.list_changes().unwrap().is_empty());
}

#[tokio::test]
async fn session_accept_and_reject_file_cover_creation_and_deletion() {
    let dir = TempDir::new().unwrap();
    let registry = TempDir::new().unwrap();
    let session = CodingAgentSession::non_persistent_internal(
        crate::runtime::facade::CodingAgentSessionOptions::new()
            .with_cwd(dir.path())
            .with_worktree_registry_dir(registry.path()),
    )
    .await
    .unwrap();
    let tracking = session
        .runtime_host
        .review_service
        .mutation_tracking("session-1", "turn-1", "operation-1")
        .unwrap();

    std::fs::write(dir.path().join("created.txt"), "created\n").unwrap();
    tracking
        .record(
            "tool-create",
            change_tracker::ChangeReceipt {
                path: "created.txt".into(),
                target_fingerprint: "vacant-target".into(),
                before_revision: None,
                after_revision: content_revision(b"created\n"),
                after_exists: true,
                byte_delta: 8,
                line_delta: 1,
                origin: "write".into(),
                unified_diff: Some(
                    "--- created.txt\n+++ created.txt\n@@ -1,0 +1,1 @@\n+created".into(),
                ),
            },
        )
        .await
        .unwrap();
    let created = session.list_changes().unwrap().remove(0);
    assert_eq!(created.hunks.len(), 1);
    session
        .reject_file(CodingAgentFileReviewActionRequest::from(&created))
        .await
        .unwrap();
    assert!(!dir.path().join("created.txt").exists());

    tracking
        .record(
            "tool-delete",
            change_tracker::ChangeReceipt {
                path: "deleted.txt".into(),
                target_fingerprint: "deleted-target".into(),
                before_revision: Some(content_revision(b"restore\n")),
                after_revision: content_revision(b""),
                after_exists: false,
                byte_delta: -8,
                line_delta: -1,
                origin: "apply_patch".into(),
                unified_diff: Some(
                    "--- deleted.txt\n+++ deleted.txt\n@@ -1,1 +1,0 @@\n-restore".into(),
                ),
            },
        )
        .await
        .unwrap();
    let deleted = session.list_changes().unwrap().remove(0);
    let opened = session
        .open_change(CodingAgentFileReviewRequest::from(&deleted))
        .await
        .unwrap();
    assert!(opened.content.is_empty());
    assert!(opened.external_editor_target.is_none());
    session
        .reject_file(CodingAgentFileReviewActionRequest::from(&deleted))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("deleted.txt")).unwrap(),
        b"restore\n"
    );

    std::fs::write(dir.path().join("accepted.txt"), "after\n").unwrap();
    let filesystem = WorkspaceAccessHandle::open_source(dir.path().to_path_buf()).unwrap();
    let target = filesystem
        .prepare_target_for_tool("write", "accepted.txt")
        .await
        .unwrap();
    tracking
        .record(
            "tool-accept",
            change_tracker::ChangeReceipt {
                path: "accepted.txt".into(),
                target_fingerprint: target.target_fingerprint().into(),
                before_revision: Some(content_revision(b"before\n")),
                after_revision: content_revision(b"after\n"),
                after_exists: true,
                byte_delta: 0,
                line_delta: 0,
                origin: "edit".into(),
                unified_diff: Some(
                    "--- accepted.txt\n+++ accepted.txt\n@@ -1,1 +1,1 @@\n-before\n+after".into(),
                ),
            },
        )
        .await
        .unwrap();
    let accepted = session.list_changes().unwrap().remove(0);
    session
        .accept_file(CodingAgentFileReviewActionRequest::from(&accepted))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("accepted.txt")).unwrap(),
        b"after\n"
    );
    assert!(session.list_changes().unwrap().is_empty());
}
