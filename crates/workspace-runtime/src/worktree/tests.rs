use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use super::{
    ManagedWorktree, WorkingTreeMode, WorktreeBuilder, WorktreeCreationMode, WorktreeError,
    parse_status_entries,
};
use crate::contract::{WorkspaceHandle, WorkspaceKind, WorkspaceLifecycle};

fn source_handle(root: &Path) -> WorkspaceHandle {
    WorkspaceHandle::new(WorkspaceKind::Source, root).expect("source handle")
}

fn builder(source: &Path, dest: impl Into<PathBuf>) -> WorktreeBuilder {
    WorktreeBuilder::new(source_handle(source), dest, "op-test")
        .parent_session(Some("session-test".into()))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
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
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Create a git repository with one committed file and return its HEAD.
fn git_source(root: &Path) -> String {
    git(root, &["init", "-q"]);
    fs::write(root.join("tracked.txt"), "tracked-v1").expect("write tracked file");
    git(root, &["add", "tracked.txt"]);
    git(root, &["commit", "-q", "-m", "initial"]);
    git(root, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn worktree_report(repo: &TempDir, dest_root: &TempDir, mode: WorkingTreeMode) -> ManagedWorktree {
    let dest = dest_root.path().join("child");
    builder(repo.path(), &dest)
        .working_tree_mode(mode)
        .create()
        .expect("worktree creates")
}

#[test]
fn git_tracked_checkout_matches_head() {
    let repo = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let head = git_source(repo.path());
    let report = worktree_report(&repo, &dest_root, WorkingTreeMode::PreserveWorkingTree);
    assert_eq!(report.report().commit(), Some(head.as_str()));
    assert_eq!(
        fs::read_to_string(dest_root.path().join("child/tracked.txt")).expect("tracked file"),
        "tracked-v1"
    );
}

#[test]
fn managed_identity_binds_report_owner_base_and_ready_lifecycle() {
    let repo = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let head = git_source(repo.path());

    let managed = worktree_report(&repo, &dest_root, WorkingTreeMode::CleanTracked);

    assert!(managed.root().is_absolute());
    assert_eq!(managed.root(), dest_root.path().join("child"));
    assert_eq!(managed.lease().handle().kind(), WorkspaceKind::ManagedChild);
    assert_eq!(managed.lease().owner_operation(), "op-test");
    assert_eq!(managed.lease().parent_session(), Some("session-test"));
    assert_eq!(managed.lease().base_revision(), Some(head.as_str()));
    assert_eq!(managed.lease().lifecycle(), WorkspaceLifecycle::Ready);
    assert_eq!(managed.creation_mode(), WorktreeCreationMode::GitLinked);
}

#[test]
fn dirty_untracked_and_deleted_files_are_synced() {
    let repo = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    git_source(repo.path());
    fs::write(repo.path().join("tracked.txt"), "tracked-v2").expect("dirty edit");
    fs::write(repo.path().join("untracked.txt"), "untracked-v1").expect("untracked file");
    fs::write(repo.path().join("ignored.log"), "ignored-v1").expect("ignored file");
    fs::write(repo.path().join(".gitignore"), "*.log\n").expect("gitignore");
    fs::write(repo.path().join("deleted.txt"), "to-be-deleted").expect("delete fixture");
    git(repo.path(), &["add", "deleted.txt"]);
    git(repo.path(), &["commit", "-q", "-m", "add delete fixture"]);
    fs::remove_file(repo.path().join("deleted.txt")).expect("delete tracked file");

    let report = worktree_report(&repo, &dest_root, WorkingTreeMode::PreserveWorkingTree);
    let child = dest_root.path().join("child");
    assert_eq!(
        fs::read_to_string(child.join("tracked.txt")).expect("dirty synced"),
        "tracked-v2"
    );
    assert_eq!(
        fs::read_to_string(child.join("untracked.txt")).expect("untracked synced"),
        "untracked-v1"
    );
    assert!(
        !child.join("ignored.log").exists(),
        "ignored files must not be synced"
    );
    assert!(
        !child.join("deleted.txt").exists(),
        "deleted files must be removed from the worktree"
    );
    assert_eq!(report.report().files_deleted(), 1);
}

#[test]
fn staged_rename_removes_the_old_path_and_copies_the_new_path() {
    let repo = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    git_source(repo.path());
    git(repo.path(), &["mv", "tracked.txt", "renamed.txt"]);

    let managed = worktree_report(&repo, &dest_root, WorkingTreeMode::PreserveWorkingTree);
    let child = managed.root();
    assert!(!child.join("tracked.txt").exists());
    assert_eq!(
        fs::read_to_string(child.join("renamed.txt")).expect("renamed file"),
        "tracked-v1"
    );
    assert_eq!(managed.report().files_deleted(), 1);
}

#[test]
fn porcelain_copy_keeps_the_original_and_uses_the_header_as_destination() {
    let entries = parse_status_entries(b"C  copied.txt\0original.txt\0").expect("valid status");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, PathBuf::from("copied.txt"));
    assert_eq!(
        entries[0].previous_path.as_deref(),
        Some(Path::new("original.txt"))
    );
    assert!(!entries[0].renamed);
    assert!(!entries[0].deleted);
}

#[test]
fn malformed_rename_status_fails_closed() {
    let error = parse_status_entries(b"R  renamed.txt\0").expect_err("source path is required");
    assert!(matches!(error, WorktreeError::GitFailed { .. }));
}

#[test]
fn clean_tracked_mode_skips_dirty_and_untracked_files() {
    let repo = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    git_source(repo.path());
    fs::write(repo.path().join("tracked.txt"), "tracked-v2").expect("dirty edit");
    fs::write(repo.path().join("untracked.txt"), "untracked-v1").expect("untracked file");

    let _report = worktree_report(&repo, &dest_root, WorkingTreeMode::CleanTracked);
    let child = dest_root.path().join("child");
    assert_eq!(
        fs::read_to_string(child.join("tracked.txt")).expect("tracked checkout"),
        "tracked-v1"
    );
    assert!(!child.join("untracked.txt").exists());
}

#[test]
fn untracked_directory_files_are_synced_individually() {
    let repo = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    git_source(repo.path());
    fs::create_dir_all(repo.path().join("scaffold/nested")).expect("untracked dirs");
    fs::write(repo.path().join("scaffold/nested/file.txt"), "scaffold-v1")
        .expect("untracked nested file");

    worktree_report(&repo, &dest_root, WorkingTreeMode::PreserveWorkingTree);
    assert_eq!(
        fs::read_to_string(dest_root.path().join("child/scaffold/nested/file.txt"))
            .expect("nested untracked synced"),
        "scaffold-v1"
    );
}

#[cfg(unix)]
#[test]
fn git_dirty_symlink_is_preserved() {
    use std::os::unix::fs::symlink;

    let repo = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    git_source(repo.path());
    fs::write(repo.path().join("target.txt"), "target-v1").expect("symlink target");
    symlink("target.txt", repo.path().join("link.txt")).expect("symlink");

    worktree_report(&repo, &dest_root, WorkingTreeMode::PreserveWorkingTree);
    let link = dest_root.path().join("child/link.txt");
    let metadata = fs::symlink_metadata(&link).expect("worktree symlink exists");
    assert!(metadata.file_type().is_symlink());
    assert_eq!(
        fs::read_link(&link).expect("read link"),
        PathBuf::from("target.txt")
    );
}

#[test]
fn copy_fallback_mirrors_non_git_source() {
    let root = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let source = root.path().join("source");
    let dest = dest_root.path().join("child");
    fs::create_dir_all(source.join("nested")).expect("source dirs");
    fs::write(source.join("file.txt"), "content-v1").expect("source file");
    fs::write(source.join("nested/deep.txt"), "deep-v1").expect("nested file");

    let report = builder(&source, &dest)
        .create()
        .expect("copy worktree creates");
    assert_eq!(report.report().commit(), None);
    assert_eq!(
        fs::read_to_string(dest.join("file.txt")).expect("copied file"),
        "content-v1"
    );
    assert_eq!(
        fs::read_to_string(dest.join("nested/deep.txt")).expect("nested copied file"),
        "deep-v1"
    );
}

#[cfg(unix)]
#[test]
fn copy_fallback_preserves_symlinks() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let source = root.path().join("source");
    let dest = dest_root.path().join("child");
    fs::create_dir_all(&source).expect("source dir");
    fs::write(source.join("target.txt"), "target-v1").expect("target file");
    symlink("target.txt", source.join("link.txt")).expect("symlink");

    let report = builder(&source, &dest)
        .create()
        .expect("copy worktree creates");
    assert_eq!(report.report().symlinks_copied(), 1);
    let metadata = fs::symlink_metadata(dest.join("link.txt")).expect("worktree symlink");
    assert!(metadata.file_type().is_symlink());
}

#[test]
fn clean_tracked_mode_rejects_non_git_source() {
    let root = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let source = root.path().join("source");
    fs::create_dir_all(&source).expect("source dir");
    fs::write(source.join("file.txt"), "v1").expect("source file");

    let error = builder(&source, dest_root.path().join("child"))
        .working_tree_mode(WorkingTreeMode::CleanTracked)
        .create()
        .expect_err("CleanTracked requires a git source");
    assert!(matches!(error, WorktreeError::CopyUnavailable { .. }));
}

#[test]
fn destination_exists_is_rejected() {
    let repo = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    git_source(repo.path());
    let dest = dest_root.path().join("child");
    fs::create_dir(&dest).expect("existing destination");

    let error = builder(repo.path(), &dest)
        .create()
        .expect_err("existing destination must be rejected");
    assert!(matches!(error, WorktreeError::DestinationExists { .. }));
}

#[test]
fn destination_inside_source_is_rejected() {
    let repo = tempfile::tempdir().expect("tempdir");
    git_source(repo.path());
    let nested = repo.path().join("child");

    let error = builder(repo.path(), &nested)
        .create()
        .expect_err("nested destination must be rejected");
    assert!(matches!(
        error,
        WorktreeError::DestinationInsideSource { .. }
    ));
}

#[test]
fn destination_inside_source_through_parent_components_is_rejected() {
    let root = tempfile::tempdir().expect("tempdir");
    let source = root.path().join("source");
    fs::create_dir_all(&source).expect("source dir");
    let disguised = root.path().join("sibling/../source/child");

    let error = builder(&source, disguised)
        .create()
        .expect_err("normalized nested destination must be rejected");
    assert!(matches!(
        error,
        WorktreeError::DestinationInsideSource { .. }
    ));
}

#[cfg(unix)]
#[test]
fn destination_inside_source_through_symlink_parent_is_rejected() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("tempdir");
    let source = root.path().join("source");
    fs::create_dir_all(&source).expect("source dir");
    let alias = root.path().join("source-alias");
    symlink(&source, &alias).expect("source alias");

    let error = builder(&source, alias.join("child"))
        .create()
        .expect_err("symlinked nested destination must be rejected");
    assert!(matches!(
        error,
        WorktreeError::DestinationInsideSource { .. }
    ));
}

#[cfg(unix)]
#[test]
fn dangling_destination_symlink_is_treated_as_existing() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let source = root.path().join("source");
    fs::create_dir_all(&source).expect("source dir");
    let dest = dest_root.path().join("child");
    symlink(dest_root.path().join("missing-target"), &dest).expect("dangling symlink");

    let error = builder(&source, &dest)
        .create()
        .expect_err("dangling destination is occupied");
    assert!(matches!(error, WorktreeError::DestinationExists { .. }));
}

#[test]
fn relative_destination_is_rejected() {
    let root = tempfile::tempdir().expect("tempdir");
    let source = root.path().join("source");
    fs::create_dir_all(&source).expect("source dir");

    let error = builder(&source, PathBuf::from("relative-child"))
        .create()
        .expect_err("managed roots must be absolute");
    assert!(matches!(
        error,
        WorktreeError::DestinationMustBeAbsolute { .. }
    ));
}

#[test]
fn pre_cancelled_create_removes_destination() {
    let repo = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    git_source(repo.path());
    let token = CancellationToken::new();
    token.cancel();
    let dest = dest_root.path().join("child");

    let error = builder(repo.path(), &dest)
        .cancellation_token(token)
        .create()
        .expect_err("cancelled create must fail");
    assert!(matches!(error, WorktreeError::Cancelled));
    assert!(
        !dest.exists(),
        "cancelled create must not leave a half-materialized worktree"
    );
}

#[test]
fn mid_copy_cancellation_removes_destination() {
    let root = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let source = root.path().join("source");
    fs::create_dir_all(&source).expect("source dir");
    for index in 0..4096 {
        fs::write(
            source.join(format!("file-{index}.txt")),
            format!("content-{index}"),
        )
        .expect("source file");
    }
    let dest = dest_root.path().join("child");
    let token = CancellationToken::new();
    let builder_token = token.clone();
    let builder_source = source.clone();
    let builder_dest = dest.clone();
    let handle = std::thread::spawn(move || {
        builder(&builder_source, builder_dest)
            .cancellation_token(builder_token)
            .create()
    });
    while !dest.join("file-0.txt").exists() {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    token.cancel();
    let outcome = handle
        .join()
        .expect("create thread finishes")
        .expect_err("cancelled mid-copy create must fail");
    assert!(matches!(outcome, WorktreeError::Cancelled));
    assert!(
        !dest.exists(),
        "cancelled mid-copy create must remove the destination"
    );
}

#[cfg(unix)]
#[test]
fn cancellation_during_git_worktree_add_kills_the_process_tree_and_cleans_up() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let marker_root = tempfile::tempdir().expect("tempdir");
    git_source(repo.path());
    let marker = marker_root.path().join("hook-started");
    let hook = repo.path().join(".git/hooks/post-checkout");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nprintf started > '{}'\nsleep 30\n",
            marker.display()
        ),
    )
    .expect("write blocking hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("hook executable");

    let dest = dest_root.path().join("child");
    let token = CancellationToken::new();
    let thread_token = token.clone();
    let source = repo.path().to_path_buf();
    let thread_dest = dest.clone();
    let handle = std::thread::spawn(move || {
        builder(&source, thread_dest)
            .working_tree_mode(WorkingTreeMode::CleanTracked)
            .cancellation_token(thread_token)
            .create()
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !marker.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(marker.exists(), "post-checkout hook must start");
    let cancelled_at = std::time::Instant::now();
    token.cancel();
    let error = handle
        .join()
        .expect("create thread finishes")
        .expect_err("cancelled git create must fail");
    assert!(matches!(error, WorktreeError::Cancelled));
    assert!(cancelled_at.elapsed() < std::time::Duration::from_secs(3));
    assert!(
        !dest.exists(),
        "cancelled git create must remove destination"
    );
    assert!(
        !git(repo.path(), &["worktree", "list", "--porcelain"])
            .contains(dest.to_string_lossy().as_ref()),
        "cancelled git create must unregister destination"
    );
}

#[test]
fn missing_source_is_rejected() {
    let root = tempfile::tempdir().expect("tempdir");
    let missing = root.path().join("missing");
    let error = builder(&missing, root.path().join("child"))
        .create()
        .expect_err("missing source must be rejected");
    assert!(matches!(error, WorktreeError::SourceUnavailable { .. }));
}
