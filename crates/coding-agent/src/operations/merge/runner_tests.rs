use std::fs;
use std::path::Path;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use workspace_runtime::api::{
    WorkingTreeMode, WorkspaceHandle, WorkspaceKind, WorkspaceLifecycle, WorktreeRegistry,
};

use super::runner::{discard_worktree, merge_worktree};
use crate::application::snapshot::SnapshotCoordinator;
use crate::kernel::error::CodingSessionError;
use crate::services::event::EventService;

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_secs()
}

fn git_source(root: &Path) {
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    fs::write(root.join("tracked.txt"), "v1\n").expect("tracked file");
    git(&["add", "tracked.txt"]);
    git(&["commit", "-q", "-m", "initial"]);
}

fn event_service() -> EventService {
    EventService::with_snapshot_coordinator(Arc::new(SnapshotCoordinator::default()))
}

fn pending_worktree(registry: &WorktreeRegistry, source: &Path) -> String {
    let handle = WorkspaceHandle::new(WorkspaceKind::Source, source).expect("source handle");
    let record = registry
        .create_managed(
            &handle,
            "op-child",
            None,
            WorkingTreeMode::PreserveWorkingTree,
            &CancellationToken::new(),
        )
        .expect("worktree");
    fs::write(record.dest.join("tracked.txt"), "child v2\n").expect("child modifies");
    fs::write(record.dest.join("new.txt"), "added\n").expect("child adds");
    registry
        .transition(&record.id, WorkspaceLifecycle::Active, now_seconds())
        .expect("active");
    registry
        .transition(&record.id, WorkspaceLifecycle::MergePending, now_seconds())
        .expect("merge pending");
    record.id
}

#[tokio::test]
async fn merge_applies_into_the_session_workspace_and_cleans_up() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    fs::create_dir_all(&source).expect("source dir");
    git_source(&source);
    let registry = WorktreeRegistry::open(temp.path().join("registry")).expect("registry opens");
    let id = pending_worktree(&registry, &source);
    let events = event_service();

    let outcome = merge_worktree(
        &events,
        &Arc::new(registry.clone()),
        &source,
        "op-merge",
        &id,
    )
    .await
    .expect("merge applies");
    assert_eq!(outcome.applied, 2);
    assert_eq!(
        fs::read_to_string(source.join("tracked.txt")).expect("merged file"),
        "child v2\n"
    );
    assert_eq!(
        fs::read_to_string(source.join("new.txt")).expect("merged add"),
        "added\n"
    );
    assert!(
        registry.load(&id).expect("load").is_none(),
        "merged worktree is cleaned up after apply"
    );
}

#[tokio::test]
async fn merge_refuses_worktrees_owned_by_another_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    fs::create_dir_all(&source).expect("source dir");
    git_source(&source);
    let registry = WorktreeRegistry::open(temp.path().join("registry")).expect("registry opens");
    let id = pending_worktree(&registry, &source);
    let events = event_service();
    let stranger = temp.path().join("stranger");
    fs::create_dir_all(&stranger).expect("stranger dir");

    let error = merge_worktree(
        &events,
        &Arc::new(registry.clone()),
        &stranger,
        "op-merge",
        &id,
    )
    .await
    .expect_err("foreign worktree rejected");
    assert!(matches!(
        error,
        CodingSessionError::UnsupportedCapability { .. }
    ));
    assert!(!source.join("new.txt").exists(), "parent untouched");
}

#[tokio::test]
async fn merge_conflicts_surface_and_keep_the_proposal_retryable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    fs::create_dir_all(&source).expect("source dir");
    git_source(&source);
    let registry = WorktreeRegistry::open(temp.path().join("registry")).expect("registry opens");
    let id = pending_worktree(&registry, &source);
    fs::write(source.join("tracked.txt"), "parent version\n").expect("parent edits same file");
    let events = event_service();

    let error = merge_worktree(
        &events,
        &Arc::new(registry.clone()),
        &source,
        "op-merge",
        &id,
    )
    .await
    .expect_err("conflict detected");
    assert!(matches!(error, CodingSessionError::Conflict { .. }));
    assert_eq!(
        fs::read_to_string(source.join("tracked.txt")).expect("parent untouched"),
        "parent version\n"
    );
    assert_eq!(
        registry.load(&id).expect("load").expect("record").lifecycle,
        WorkspaceLifecycle::MergePending,
        "conflicted proposal remains retryable"
    );
}

#[tokio::test]
async fn merge_stale_parent_surfaces_retryable_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    fs::create_dir_all(&source).expect("source dir");
    git_source(&source);
    let registry = WorktreeRegistry::open(temp.path().join("registry")).expect("registry opens");
    let id = pending_worktree(&registry, &source);
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&source)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git runs");
        assert!(output.status.success());
    };
    fs::write(source.join("parent-edit.txt"), "parent\n").expect("parent edit");
    git(&["add", "parent-edit.txt"]);
    git(&["commit", "-q", "-m", "parent advances"]);
    let events = event_service();

    let error = merge_worktree(
        &events,
        &Arc::new(registry.clone()),
        &source,
        "op-merge",
        &id,
    )
    .await
    .expect_err("stale parent detected");
    assert!(matches!(error, CodingSessionError::Stale { .. }));
    assert!(!source.join("new.txt").exists(), "parent untouched");
}

#[tokio::test]
async fn discard_removes_a_proposal_without_touching_the_parent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    fs::create_dir_all(&source).expect("source dir");
    git_source(&source);
    let registry = WorktreeRegistry::open(temp.path().join("registry")).expect("registry opens");
    let id = pending_worktree(&registry, &source);
    let events = event_service();
    let record = registry.load(&id).expect("load").expect("record");

    discard_worktree(
        &events,
        &Arc::new(registry.clone()),
        &source,
        "op-discard",
        &id,
    )
    .expect("discard succeeds");
    assert!(!record.dest.exists(), "worktree materialization removed");
    assert!(registry.load(&id).expect("load").is_none());
    assert!(
        !source.join("new.txt").exists(),
        "discarded proposal never touches the parent"
    );
}

#[tokio::test]
async fn merge_operation_dispatch_reaches_the_runner_through_a_session() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source");
    fs::create_dir_all(&source).expect("source dir");
    git_source(&source);
    let registry_root = temp.path().join("registry");

    let mut session = crate::runtime::facade::CodingAgentSession::create_internal(
        crate::runtime::facade::CodingAgentSessionOptions::new()
            .with_cwd(&source)
            .with_session_id("sess_merge_dispatch")
            .with_session_log_root(temp.path().join("sessions"))
            .with_worktree_registry_dir(&registry_root),
    )
    .await
    .expect("session opens");

    let error = session
        .run_internal(
            crate::application::operation::contract::CodingAgentOperation::MergeChildWorktree {
                worktree_id: "child-0000000000000000".into(),
            },
        )
        .await
        .expect_err("unknown worktree id fails closed");
    assert!(
        error.to_string().contains("not registered"),
        "dispatch must surface the runner error, got: {error}"
    );

    let error = session
        .run_internal(
            crate::application::operation::contract::CodingAgentOperation::DiscardChildWorktree {
                worktree_id: "child-0000000000000000".into(),
            },
        )
        .await
        .expect_err("unknown worktree id fails closed");
    assert!(
        error.to_string().contains("not registered"),
        "dispatch must surface the runner error, got: {error}"
    );
}
