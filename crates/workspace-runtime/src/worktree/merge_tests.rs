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

fn git(root: &Path, args: &[&str]) -> String {
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
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn git_source(root: &Path) {
    git(root, &["init", "-q"]);
    fs::write(root.join("tracked.txt"), "v1\n").expect("tracked file");
    git(root, &["add", "tracked.txt"]);
    git(root, &["commit", "-q", "-m", "initial"]);
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
fn merge_applies_copy_mode_worktrees_against_the_creation_baseline() {
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
    fs::write(record.dest.join("file.txt"), "child-v2").expect("child edit");
    fs::write(record.dest.join("added.txt"), "added").expect("child add");
    registry
        .transition(&record.id, WorkspaceLifecycle::Active, now_seconds())
        .expect("active");
    registry
        .transition(&record.id, WorkspaceLifecycle::MergePending, now_seconds())
        .expect("merge pending");

    let changeset = build_changeset(&registry, &record.id).expect("copy changeset");
    assert_eq!(changeset.entries.len(), 2);
    apply_merge(&registry, &record.id).expect("copy merge applies");
    assert_eq!(
        fs::read_to_string(source_dir.path().join("file.txt")).expect("merged file"),
        "child-v2"
    );
    assert_eq!(
        fs::read_to_string(source_dir.path().join("added.txt")).expect("merged add"),
        "added"
    );
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

#[test]
fn parent_untracked_file_conflicts_with_a_child_addition() {
    let (registry_dir, source_dir) = registry_root();
    git_source(source_dir.path());
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = pending_worktree(&registry, source_dir.path(), "op-untracked", |child| {
        fs::write(child.join("same.txt"), "child\n").expect("child add");
    });
    fs::write(source_dir.path().join("same.txt"), "parent\n").expect("parent untracked");

    let error = apply_merge(&registry, &record.id).expect_err("untracked conflict");
    assert!(matches!(error, MergeError::Conflict { .. }));
    assert_eq!(
        fs::read_to_string(source_dir.path().join("same.txt")).expect("parent retained"),
        "parent\n"
    );
}

#[test]
fn dirty_source_state_is_part_of_the_baseline_not_a_child_change() {
    let (registry_dir, source_dir) = registry_root();
    git_source(source_dir.path());
    fs::write(source_dir.path().join("tracked.txt"), "parent-dirty\n").expect("dirty source");
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = pending_worktree(&registry, source_dir.path(), "op-dirty-base", |child| {
        fs::write(child.join("child.txt"), "child\n").expect("child add");
    });

    let changeset = build_changeset(&registry, &record.id).expect("changeset");
    assert_eq!(changeset.entries.len(), 1);
    assert_eq!(changeset.entries[0].path, Path::new("child.txt"));
    apply_merge(&registry, &record.id).expect("merge applies over unchanged dirty baseline");
    assert_eq!(
        fs::read_to_string(source_dir.path().join("tracked.txt")).expect("dirty retained"),
        "parent-dirty\n"
    );
}

#[test]
fn oversized_changeset_fails_closed_without_applying_or_transitioning() {
    let (registry_dir, source_dir) = registry_root();
    git_source(source_dir.path());
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = pending_worktree(&registry, source_dir.path(), "op-large", |child| {
        for index in 0..=super::MAX_CHANGESET_ENTRIES {
            fs::write(child.join(format!("generated-{index:04}.txt")), b"x").expect("child add");
        }
    });

    let error = apply_merge(&registry, &record.id).expect_err("oversized merge rejected");
    assert!(matches!(error, MergeError::ChangeSetTooLarge { .. }));
    assert!(!source_dir.path().join("generated-0000.txt").exists());
    assert_eq!(
        registry.load(&record.id).unwrap().unwrap().lifecycle,
        WorkspaceLifecycle::MergePending
    );
}

#[test]
fn pre_cancelled_merge_keeps_parent_and_proposal_unchanged() {
    let (registry_dir, source_dir) = registry_root();
    git_source(source_dir.path());
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = pending_worktree(&registry, source_dir.path(), "op-cancel", |child| {
        fs::write(child.join("new.txt"), "child\n").expect("child add");
    });
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = super::apply_merge_cancellable(&registry, &record.id, &cancellation)
        .expect_err("cancelled merge");
    assert!(matches!(
        error,
        MergeError::Cancelled | MergeError::Git(crate::worktree::WorktreeError::Cancelled)
    ));
    assert!(!source_dir.path().join("new.txt").exists());
    assert_eq!(
        registry.load(&record.id).unwrap().unwrap().lifecycle,
        WorkspaceLifecycle::MergePending
    );
}

#[cfg(unix)]
#[test]
fn replacing_a_parent_symlink_never_writes_through_it() {
    use std::os::unix::fs::symlink;

    let (registry_dir, source_dir) = registry_root();
    git_source(source_dir.path());
    let outside = source_dir.path().parent().unwrap().join("outside.txt");
    fs::write(&outside, "outside-original\n").expect("outside file");
    symlink(&outside, source_dir.path().join("link.txt")).expect("parent symlink");
    git(source_dir.path(), &["add", "link.txt"]);
    git(source_dir.path(), &["commit", "-q", "-m", "add symlink"]);
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = pending_worktree(&registry, source_dir.path(), "op-symlink", |child| {
        fs::remove_file(child.join("link.txt")).expect("remove child symlink");
        fs::write(child.join("link.txt"), "child-regular\n").expect("write regular file");
    });

    apply_merge(&registry, &record.id).expect("merge replaces symlink safely");
    assert_eq!(fs::read_to_string(&outside).unwrap(), "outside-original\n");
    assert!(
        !fs::symlink_metadata(source_dir.path().join("link.txt"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(source_dir.path().join("link.txt")).unwrap(),
        "child-regular\n"
    );
}

#[test]
fn startup_recovery_rolls_back_a_prepared_partial_merge() {
    let (registry_dir, source_dir) = registry_root();
    git_source(source_dir.path());
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = pending_worktree(&registry, source_dir.path(), "op-recover", |child| {
        fs::write(child.join("tracked.txt"), "child\n").expect("child edit");
    });
    super::prepare_transaction(&registry, &record, &CancellationToken::new())
        .expect("transaction prepared");
    fs::write(source_dir.path().join("tracked.txt"), "partial-child\n").expect("partial apply");

    let report = registry.recover().expect("recovery succeeds");
    assert_eq!(report.merges_rolled_back, vec![record.id.clone()]);
    assert_eq!(
        fs::read_to_string(source_dir.path().join("tracked.txt")).unwrap(),
        "v1\n"
    );
    assert_eq!(
        registry.load(&record.id).unwrap().unwrap().lifecycle,
        WorkspaceLifecycle::MergePending
    );
}

#[test]
fn startup_recovery_removes_an_incomplete_transaction_without_a_journal() {
    let (registry_dir, source_dir) = registry_root();
    git_source(source_dir.path());
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = pending_worktree(&registry, source_dir.path(), "op-incomplete", |child| {
        fs::write(child.join("tracked.txt"), "child\n").expect("child edit");
    });
    let transaction = registry.transaction_dir(&record.id);
    fs::create_dir_all(transaction.join("backup")).expect("partial backup directory");
    fs::write(transaction.join("backup/tracked.txt"), "v1\n").expect("partial backup");

    let report = registry.recover().expect("recovery succeeds");
    assert!(report.merges_rolled_back.is_empty());
    assert!(report.merges_completed.is_empty());
    assert!(!transaction.exists());
    assert_eq!(
        fs::read_to_string(source_dir.path().join("tracked.txt")).unwrap(),
        "v1\n"
    );
    assert_eq!(
        registry.load(&record.id).unwrap().unwrap().lifecycle,
        WorkspaceLifecycle::MergePending
    );
}

#[test]
fn startup_recovery_completes_an_applied_transaction() {
    let (registry_dir, source_dir) = registry_root();
    git_source(source_dir.path());
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = pending_worktree(&registry, source_dir.path(), "op-applied", |child| {
        fs::write(child.join("tracked.txt"), "child\n").expect("child edit");
    });
    let changeset = build_changeset(&registry, &record.id).expect("changeset");
    super::prepare_transaction(&registry, &record, &CancellationToken::new())
        .expect("transaction prepared");
    super::apply_entries(&record, &changeset.entries, &CancellationToken::new())
        .expect("entries applied");
    super::mark_transaction_applied(&registry, &record).expect("transaction applied");

    let report = registry.recover().expect("recovery succeeds");
    assert_eq!(report.merges_completed, vec![record.id.clone()]);
    assert!(report.merges_rolled_back.is_empty());
    assert!(!registry.transaction_dir(&record.id).exists());
    assert_eq!(
        fs::read_to_string(source_dir.path().join("tracked.txt")).unwrap(),
        "child\n"
    );
    assert_eq!(
        registry.load(&record.id).unwrap().unwrap().lifecycle,
        WorkspaceLifecycle::Merged
    );
}
