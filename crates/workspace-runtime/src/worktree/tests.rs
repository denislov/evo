use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use super::{WorkingTreeMode, WorktreeBuilder, WorktreeError};

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

fn worktree_report(
    repo: &TempDir,
    dest_root: &TempDir,
    mode: WorkingTreeMode,
) -> super::WorktreeReport {
    let dest = dest_root.path().join("child");
    WorktreeBuilder::new(repo.path(), &dest)
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
    assert_eq!(report.commit.as_deref(), Some(head.as_str()));
    assert_eq!(
        fs::read_to_string(dest_root.path().join("child/tracked.txt")).expect("tracked file"),
        "tracked-v1"
    );
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
    assert_eq!(report.files_deleted, 1);
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

    let report = WorktreeBuilder::new(&source, &dest)
        .create()
        .expect("copy worktree creates");
    assert_eq!(report.commit, None);
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

    let report = WorktreeBuilder::new(&source, &dest)
        .create()
        .expect("copy worktree creates");
    assert_eq!(report.symlinks_copied, 1);
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

    let error = WorktreeBuilder::new(&source, dest_root.path().join("child"))
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

    let error = WorktreeBuilder::new(repo.path(), &dest)
        .create()
        .expect_err("existing destination must be rejected");
    assert!(matches!(error, WorktreeError::DestinationExists { .. }));
}

#[test]
fn destination_inside_source_is_rejected() {
    let repo = tempfile::tempdir().expect("tempdir");
    git_source(repo.path());
    let nested = repo.path().join("child");

    let error = WorktreeBuilder::new(repo.path(), &nested)
        .create()
        .expect_err("nested destination must be rejected");
    assert!(matches!(
        error,
        WorktreeError::DestinationInsideSource { .. }
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

    let error = WorktreeBuilder::new(repo.path(), &dest)
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
        WorktreeBuilder::new(builder_source, builder_dest)
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

#[test]
fn missing_source_is_rejected() {
    let root = tempfile::tempdir().expect("tempdir");
    let error = WorktreeBuilder::new(root.path().join("missing"), root.path().join("child"))
        .create()
        .expect_err("missing source must be rejected");
    assert!(matches!(error, WorktreeError::SourceUnavailable { .. }));
}
