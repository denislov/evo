use std::fs;
use std::path::Path;

use tokio_util::sync::CancellationToken;

use super::{ChangeKind, MergeError, apply_merge, build_changeset};
use crate::contract::{WorkspaceHandle, WorkspaceKind, WorkspaceLifecycle};
use crate::worktree::registry::{WorktreeRecord, WorktreeRegistry};
use crate::worktree::{WorkingTreeMode, WorktreeCreationMode};

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

fn registry_root() -> (tempfile::TempDir, tempfile::TempDir) {
    (
        tempfile::tempdir().expect("registry tempdir"),
        tempfile::tempdir().expect("source tempdir"),
    )
}

fn create_git_worktree(registry: &WorktreeRegistry, source: &Path, owner: &str) -> WorktreeRecord {
    let handle = WorkspaceHandle::new(WorkspaceKind::Source, source).expect("source handle");
    registry
        .create_managed(
            &handle,
            owner,
            None,
            WorkingTreeMode::PreserveWorkingTree,
            &CancellationToken::new(),
        )
        .expect("managed worktree created")
}

/// Put a record into `MergePending` and make a concrete child change.
fn pending_worktree(
    registry: &WorktreeRegistry,
    source: &Path,
    owner: &str,
    change: impl FnOnce(&Path),
) -> WorktreeRecord {
    let record = create_git_worktree(registry, source, owner);
    change(&record.dest);
    registry
        .transition(&record.id, WorkspaceLifecycle::Active, now_seconds())
        .expect("active");
    registry
        .transition(&record.id, WorkspaceLifecycle::MergePending, now_seconds())
        .expect("merge pending")
}

#[test]
fn changeset_lists_added_modified_and_deleted_entries() {
    let (registry_dir, source_dir) = registry_root();
    git_source(source_dir.path());
    fs::write(source_dir.path().join("doomed.txt"), "bye\n").expect("doomed file");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(source_dir.path())
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git runs");
        assert!(output.status.success());
    };
    git(&["add", "doomed.txt"]);
    git(&["commit", "-q", "-m", "second"]);
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = pending_worktree(&registry, source_dir.path(), "op-1", |child| {
        fs::write(child.join("tracked.txt"), "v2\n").expect("modify");
        fs::write(child.join("new.txt"), "added\n").expect("add");
        fs::remove_file(child.join("doomed.txt")).expect("delete");
    });
    let _ = record;

    let changeset = build_changeset(&registry, &record.id).expect("changeset builds");
    assert_eq!(
        changeset.base_revision.as_deref(),
        record.base_revision.as_deref()
    );
    let by_path = |name: &str| {
        changeset
            .entries
            .iter()
            .find(|entry| entry.path == Path::new(name))
            .expect("entry")
    };
    assert_eq!(by_path("tracked.txt").kind, ChangeKind::Modified);
    assert_eq!(by_path("new.txt").kind, ChangeKind::Added);
    assert_eq!(by_path("doomed.txt").kind, ChangeKind::Deleted);
    assert!(by_path("tracked.txt").additions > 0);
    assert!(!changeset.truncated);
}

#[test]
fn merge_applies_changes_into_a_clean_parent() {
    let (registry_dir, source_dir) = registry_root();
    git_source(source_dir.path());
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = pending_worktree(&registry, source_dir.path(), "op-clean", |child| {
        fs::write(child.join("tracked.txt"), "v2\n").expect("modify");
        fs::write(child.join("new.txt"), "added\n").expect("add");
        fs::write(child.join("sub"), "subfile\n").expect("add in dir");
    });
    let _ = record;

    let report = apply_merge(&registry, &record.id).expect("merge applies");
    assert_eq!(report.applied, 3);
    assert_eq!(
        fs::read_to_string(source_dir.path().join("tracked.txt")).expect("modified file"),
        "v2\n"
    );
    assert_eq!(
        fs::read_to_string(source_dir.path().join("new.txt")).expect("new file"),
        "added\n"
    );
    assert_eq!(
        fs::read_to_string(source_dir.path().join("sub")).expect("sub file"),
        "subfile\n"
    );
    assert_eq!(
        registry
            .load(&record.id)
            .expect("load")
            .expect("record")
            .lifecycle,
        WorkspaceLifecycle::Merged
    );

    registry.discard(&record.id).expect("discard after merge");
    assert!(!record.dest.exists());
    assert!(registry.load(&record.id).expect("load").is_none());
}

#[test]
fn merge_detects_conflicts_with_parent_side_changes() {
    let (registry_dir, source_dir) = registry_root();
    git_source(source_dir.path());
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = pending_worktree(&registry, source_dir.path(), "op-conflict", |child| {
        fs::write(child.join("tracked.txt"), "child version\n").expect("child modifies");
    });
    let _ = record;
    fs::write(source_dir.path().join("tracked.txt"), "parent version\n").expect("parent modifies");

    let error = apply_merge(&registry, &record.id).expect_err("conflict detected");
    let MergeError::Conflict { paths } = &error else {
        panic!("expected Conflict, got {error:?}");
    };
    assert_eq!(paths, &[std::path::PathBuf::from("tracked.txt")]);
    assert_eq!(
        fs::read_to_string(source_dir.path().join("tracked.txt")).expect("parent untouched"),
        "parent version\n"
    );
    assert_eq!(
        registry
            .load(&record.id)
            .expect("load")
            .expect("record")
            .lifecycle,
        WorkspaceLifecycle::MergePending,
        "conflicted merge must leave the record mergeable for retry"
    );
}

#[test]
fn merge_rejects_stale_parents() {
    let (registry_dir, source_dir) = registry_root();
    git_source(source_dir.path());
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = pending_worktree(&registry, source_dir.path(), "op-stale", |child| {
        fs::write(child.join("new.txt"), "added\n").expect("add");
    });
    let _ = record;
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(source_dir.path())
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git runs");
        assert!(output.status.success());
    };
    fs::write(source_dir.path().join("parent-edit.txt"), "parent\n").expect("parent edit");
    git(&["add", "parent-edit.txt"]);
    git(&["commit", "-q", "-m", "parent advances"]);

    let error = apply_merge(&registry, &record.id).expect_err("stale parent detected");
    let MergeError::StaleParent { expected, actual } = &error else {
        panic!("expected StaleParent, got {error:?}");
    };
    assert_eq!(expected, &record.base_revision);
    assert!(actual.is_some());
    assert!(!source_dir.path().join("new.txt").exists());
}

#[test]
fn merge_refuses_copy_mode_worktrees() {
    let (registry_dir, source_dir) = registry_root();
    fs::create_dir_all(source_dir.path()).expect("source dir");
    fs::write(source_dir.path().join("file.txt"), "v1").expect("source file");
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let handle = WorkspaceHandle::new(WorkspaceKind::Source, source_dir.path()).expect("handle");
    let record = registry
        .create_managed(
            &handle,
            "op-copy",
            None,
            WorkingTreeMode::PreserveWorkingTree,
            &CancellationToken::new(),
        )
        .expect("copy worktree");
    assert_eq!(record.creation_mode, WorktreeCreationMode::Copy);
    registry
        .transition(&record.id, WorkspaceLifecycle::Active, now_seconds())
        .expect("active");
    registry
        .transition(&record.id, WorkspaceLifecycle::MergePending, now_seconds())
        .expect("merge pending");

    let error = build_changeset(&registry, &record.id).expect_err("copy merge refused");
    assert!(matches!(error, MergeError::CopyWorktreeUnsupported));
    let error = apply_merge(&registry, &record.id).expect_err("copy merge refused");
    assert!(matches!(error, MergeError::CopyWorktreeUnsupported));
}

#[test]
fn merge_refuses_non_merge_pending_records() {
    let (registry_dir, source_dir) = registry_root();
    git_source(source_dir.path());
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = create_git_worktree(&registry, source_dir.path(), "op-ready");

    let error = apply_merge(&registry, &record.id).expect_err("Ready is not mergeable");
    assert!(matches!(error, MergeError::NotMergeable { .. }));
}
