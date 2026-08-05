use std::fs;
use std::path::Path;

use tokio_util::sync::CancellationToken;

use super::{GcOptions, WorktreeRecord, WorktreeRegistry};
use crate::contract::{WorkspaceHandle, WorkspaceKind, WorkspaceLifecycle};
use crate::worktree::{WorkingTreeMode, WorktreeCreationMode};

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_secs()
}

fn registry_root() -> (tempfile::TempDir, tempfile::TempDir) {
    (
        tempfile::tempdir().expect("registry tempdir"),
        tempfile::tempdir().expect("source tempdir"),
    )
}

/// Create and register a real (copy-mode) managed worktree.
fn registered_worktree(registry: &WorktreeRegistry, source: &Path, owner: &str) -> WorktreeRecord {
    let handle = WorkspaceHandle::new(WorkspaceKind::Source, source).expect("source handle");
    fs::create_dir_all(source).expect("source dir");
    fs::write(source.join("file.txt"), "v1").expect("source file");
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
    fs::write(root.join("tracked.txt"), "v1").expect("tracked file");
    git(&["add", "tracked.txt"]);
    git(&["commit", "-q", "-m", "initial"]);
}

#[test]
fn register_and_load_round_trip() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = registered_worktree(&registry, source_dir.path(), "op-owner");

    let loaded = registry.load(&record.id).expect("loads").expect("present");
    assert_eq!(loaded, record);
    assert_eq!(loaded.kind, WorkspaceKind::ManagedChild);
    assert_eq!(loaded.creation_mode, WorktreeCreationMode::Copy);
    assert_eq!(loaded.lifecycle, WorkspaceLifecycle::Ready);
    assert_eq!(loaded.owner_operation, "op-owner");
    assert!(loaded.dest.starts_with(registry.worktrees_root()));
}

#[test]
fn ids_are_unique_across_creations() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let first = registered_worktree(&registry, source_dir.path(), "op-a");
    let second = registered_worktree(&registry, source_dir.path(), "op-b");
    assert_ne!(first.id, second.id);
    assert_ne!(first.dest, second.dest);
    assert_eq!(registry.load_all().expect("loads all").len(), 2);
}

#[test]
fn load_all_returns_sorted_records() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    registered_worktree(&registry, source_dir.path(), "op-1");
    registered_worktree(&registry, source_dir.path(), "op-2");

    let ids = registry
        .load_all()
        .expect("loads all")
        .into_iter()
        .map(|record| record.id)
        .collect::<Vec<_>>();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
}

#[test]
fn register_rejects_dest_outside_the_worktrees_root() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let mut record = registered_worktree(&registry, source_dir.path(), "op-1");
    record.dest = source_dir.path().join("not-managed");
    let error = registry.register(&record).expect_err("must reject");
    assert!(error.to_string().contains("does not match id"));
}

#[test]
fn transition_persists_and_rejects_illegal_steps() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = registered_worktree(&registry, source_dir.path(), "op-1");

    registry
        .transition(&record.id, WorkspaceLifecycle::Active, now_seconds() + 10)
        .expect("active");
    let loaded = registry.load(&record.id).expect("loads").expect("present");
    assert_eq!(loaded.lifecycle, WorkspaceLifecycle::Active);
    assert_eq!(loaded.updated_at, now_seconds() + 10);

    let error = registry
        .transition(&record.id, WorkspaceLifecycle::Removed, now_seconds() + 20)
        .expect_err("cannot jump to Removed");
    assert!(matches!(
        error,
        super::RegistryError::InvalidTransition { .. }
    ));
}

#[test]
fn corrupted_record_fails_closed() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = registered_worktree(&registry, source_dir.path(), "op-1");
    fs::write(
        registry_dir
            .path()
            .join("registry")
            .join(format!("{}.json", record.id)),
        b"{not json",
    )
    .expect("corrupt record");

    let error = registry.load(&record.id).expect_err("must fail closed");
    assert!(error.to_string().contains("cannot decode record"));
}

#[test]
fn leftover_temp_file_does_not_break_loading() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = registered_worktree(&registry, source_dir.path(), "op-1");
    fs::write(
        registry_dir
            .path()
            .join("registry")
            .join(format!("{}.json.tmp.9999", record.id)),
        b"partial",
    )
    .expect("leftover temp");

    assert_eq!(registry.load_all().expect("loads").len(), 1);
}

#[test]
fn recover_removes_interrupted_and_stale_but_keeps_orphans() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");

    let interrupted = registered_worktree(&registry, source_dir.path(), "op-1");
    fs::write(interrupted.dest.join("partial.txt"), "partial").expect("half materialized");
    let mut interrupted_record = registry
        .load(&interrupted.id)
        .expect("loads")
        .expect("present");
    interrupted_record.lifecycle = WorkspaceLifecycle::Creating;
    registry.register(&interrupted_record).expect("re-register");

    let stale = registered_worktree(&registry, source_dir.path(), "op-2");
    fs::remove_dir_all(&stale.dest).expect("drop directory");

    let orphan = registry.worktrees_root().join("child-unknown-orphan");
    fs::create_dir_all(&orphan).expect("orphan dir");
    fs::write(orphan.join("unknown.txt"), "x").expect("orphan file");

    let report = registry.recover().expect("recovers");
    assert_eq!(report.interrupted.len(), 1);
    assert_eq!(report.stale_records.len(), 1);
    assert_eq!(report.orphans, vec![orphan.clone()]);

    assert!(
        !interrupted.dest.exists(),
        "interrupted worktree directory must be removed"
    );
    assert!(
        registry.load(&interrupted.id).expect("loads").is_none(),
        "interrupted record must be removed"
    );
    assert!(registry.load(&stale.id).expect("loads").is_none());
    assert!(
        orphan.exists(),
        "orphan directory must never be auto-deleted"
    );
}

#[test]
fn gc_removes_only_dead_owners_past_max_age() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let old_dead = registered_worktree(&registry, source_dir.path(), "op-finished");
    let young_dead = registered_worktree(&registry, source_dir.path(), "op-finished");
    let alive = registered_worktree(&registry, source_dir.path(), "op-alive");
    registry
        .transition(&old_dead.id, WorkspaceLifecycle::Active, now_seconds())
        .expect("active");
    registry
        .transition(
            &young_dead.id,
            WorkspaceLifecycle::Active,
            now_seconds() + 9_000,
        )
        .expect("active");

    let options = GcOptions {
        now: now_seconds() + 10_000,
        max_age_seconds: 5_000,
        disk_budget_bytes: None,
        owner_liveness: Box::new(|owner| owner == "op-alive"),
        dry_run: false,
    };
    let report = registry.gc(&options).expect("gc runs");
    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].id, old_dead.id);
    assert!(!old_dead.dest.exists());
    assert!(registry.load(&old_dead.id).expect("loads").is_none());
    assert!(young_dead.dest.exists());
    assert!(alive.dest.exists());
}

#[test]
fn gc_dry_run_removes_nothing_but_reports_candidates() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = registered_worktree(&registry, source_dir.path(), "op-finished");

    let options = GcOptions {
        now: now_seconds() + 10_000,
        max_age_seconds: 1,
        disk_budget_bytes: None,
        owner_liveness: Box::new(|_| false),
        dry_run: true,
    };
    let report = registry.gc(&options).expect("gc runs");
    assert_eq!(report.candidates, vec![record.id]);
    assert!(report.removed.is_empty());
    assert!(record.dest.exists());
}

#[test]
fn gc_honors_disk_budget() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    registered_worktree(&registry, source_dir.path(), "op-finished");
    registered_worktree(&registry, source_dir.path(), "op-finished");

    let options = GcOptions {
        now: now_seconds() + 10_000,
        max_age_seconds: 1,
        disk_budget_bytes: Some(1),
        owner_liveness: Box::new(|_| false),
        dry_run: false,
    };
    let report = registry.gc(&options).expect("gc runs");
    assert_eq!(report.removed.len(), 1, "stop after reclaiming the budget");
}

#[test]
fn gc_removes_git_linked_worktrees_including_registration() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    git_source(source_dir.path());
    let handle = WorkspaceHandle::new(WorkspaceKind::Source, source_dir.path()).expect("handle");
    let record = registry
        .create_managed(
            &handle,
            "op-finished",
            None,
            WorkingTreeMode::CleanTracked,
            &CancellationToken::new(),
        )
        .expect("git worktree created");
    assert_eq!(record.creation_mode, WorktreeCreationMode::GitLinked);

    let options = GcOptions {
        now: now_seconds() + 10_000,
        max_age_seconds: 1,
        disk_budget_bytes: None,
        owner_liveness: Box::new(|_| false),
        dry_run: false,
    };
    let report = registry.gc(&options).expect("gc runs");
    assert_eq!(report.removed.len(), 1);
    assert!(!record.dest.exists());

    let listing = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(source_dir.path())
        .output()
        .expect("git list");
    let listing = String::from_utf8_lossy(&listing.stdout);
    assert!(
        !listing.contains(&record.dest.display().to_string()),
        "git worktree registration must be pruned: {listing}"
    );
}
