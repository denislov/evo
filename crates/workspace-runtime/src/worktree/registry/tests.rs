use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread;

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
fn capacity_bounds_concurrent_live_worktrees() {
    let (registry_dir, source_dir) = registry_root();
    let registry =
        WorktreeRegistry::open_with_capacity(registry_dir.path(), Some(2)).expect("registry opens");
    assert_eq!(registry.capacity(), Some(2));
    let handle = WorkspaceHandle::new(WorkspaceKind::Source, source_dir.path()).expect("handle");
    fs::create_dir_all(source_dir.path()).expect("source dir");

    let create = || {
        registry
            .create_managed(
                &handle,
                "op-capacity",
                None,
                WorkingTreeMode::PreserveWorkingTree,
                &CancellationToken::new(),
            )
            .expect("worktree within capacity")
    };
    let first = create();
    let second = create();
    let error = registry
        .create_managed(
            &handle,
            "op-capacity-over",
            None,
            WorkingTreeMode::PreserveWorkingTree,
            &CancellationToken::new(),
        )
        .expect_err("capacity exhausted");
    assert!(matches!(
        error,
        super::RegistryError::CapacityExhausted {
            active: 2,
            capacity: 2
        }
    ));

    registry
        .transition(&first.id, WorkspaceLifecycle::Active, now_seconds())
        .expect("active");
    registry
        .transition(&first.id, WorkspaceLifecycle::Discarded, now_seconds())
        .expect("discarded");
    registry
        .discard(&first.id)
        .expect("discarded worktree is reclaimed");
    let third = create();
    assert_ne!(third.id, second.id);
}

#[test]
fn discard_removes_materialization_record_and_registration() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    git_source(source_dir.path());
    let record = registered_worktree(&registry, source_dir.path(), "op-finished");
    assert!(record.dest.exists(), "worktree materialized");

    registry.discard(&record.id).expect("discard succeeds");
    assert!(
        registry
            .load(&record.id)
            .expect("load after discard")
            .is_none(),
        "record removed"
    );
    assert!(!record.dest.exists(), "materialization removed");
    registry.discard(&record.id).expect("discard is idempotent");
}

#[test]
fn discard_rejects_unverified_identities() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    git_source(source_dir.path());
    let record = registered_worktree(&registry, source_dir.path(), "op-finished");

    let victim = registry_dir.path().join("victim.json");
    fs::write(&victim, b"must survive").expect("victim file");
    let error = registry
        .discard("../victim")
        .expect_err("path traversal rejected");
    assert!(matches!(error, super::RegistryError::InvalidRecord { .. }));
    assert!(victim.exists(), "outside file must not be deleted");
    assert!(record.dest.exists(), "worktree untouched");

    let mut forged = record.clone();
    forged.dest = registry_dir.path().join("registry").join("forged");
    fs::write(
        registry.record_path(&record.id),
        serde_json::to_vec(&forged).expect("encode forged record"),
    )
    .expect("write forged record");
    let error = registry
        .discard(&record.id)
        .expect_err("forged destination rejected");
    assert!(matches!(error, super::RegistryError::InvalidRecord { .. }));
    assert!(record.dest.exists(), "worktree still untouched");
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
    super::write_record_atomic(
        &registry.record_path(&interrupted_record.id),
        &interrupted_record,
    )
    .expect("re-register");

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
fn startup_maintenance_collects_dead_process_records_but_keeps_live_records() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let dead = registered_worktree(&registry, source_dir.path(), "op-dead");
    let live = registered_worktree(&registry, source_dir.path(), "op-live");

    let mut exited_owner =
        std::process::Command::new(std::env::current_exe().expect("test binary"))
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("owner process starts");
    let exited_pid = exited_owner.id();
    assert!(exited_owner.wait().expect("owner process exits").success());
    let mut dead_record = registry
        .load(&dead.id)
        .expect("load dead")
        .expect("dead record");
    dead_record.owner_pid = exited_pid;
    super::write_record_atomic(&registry.record_path(&dead.id), &dead_record)
        .expect("write dead owner");

    drop(registry);
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry reopens");
    let report = registry.startup_maintenance().expect("maintenance runs");
    assert_eq!(report.gc.removed.len(), 1);
    assert_eq!(report.gc.removed[0].id, dead.id);
    assert!(registry.load(&dead.id).expect("load dead after").is_none());
    assert!(registry.load(&live.id).expect("load live after").is_some());
    assert!(live.dest.exists(), "live process worktree must remain");
}

#[test]
fn recover_cleans_a_cleaning_record_after_interrupted_discard() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = registered_worktree(&registry, source_dir.path(), "op-cleaning");
    let mut cleaning = registry.load(&record.id).expect("load").expect("record");
    cleaning.lifecycle = WorkspaceLifecycle::Cleaning;
    super::write_record_atomic(&registry.record_path(&record.id), &cleaning)
        .expect("write cleaning state");

    let report = registry.recover().expect("recover runs");
    assert_eq!(report.interrupted, vec![cleaning]);
    assert!(!record.dest.exists());
    assert!(registry.load(&record.id).expect("load after").is_none());
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
        owner_liveness: Box::new(|record| record.owner_operation == "op-alive"),
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

#[test]
fn invalid_ids_cannot_escape_the_registry_directory() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let victim = registry_dir.path().join("victim.json");
    fs::write(&victim, b"must survive").expect("victim file");

    let error = registry
        .remove("../victim")
        .expect_err("path traversal rejected");
    assert!(matches!(error, super::RegistryError::InvalidRecord { .. }));
    assert!(victim.exists(), "outside file must not be deleted");
    let _ = source_dir;
}

#[test]
fn record_identity_must_match_its_filename_and_destination() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = registered_worktree(&registry, source_dir.path(), "op-1");
    let path = registry_dir
        .path()
        .join("registry")
        .join(format!("{}.json", record.id));
    let mut forged = record.clone();
    forged.id = "child-forged".into();
    fs::write(
        &path,
        serde_json::to_vec(&forged).expect("encode forged record"),
    )
    .expect("write forged record");

    let error = registry
        .load(&record.id)
        .expect_err("forged identity rejected");
    assert!(matches!(error, super::RegistryError::InvalidRecord { .. }));
}

#[cfg(unix)]
#[test]
fn symlink_parent_cannot_pass_destination_containment() {
    use std::os::unix::fs::symlink;

    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = registered_worktree(&registry, source_dir.path(), "op-1");
    fs::remove_dir_all(&record.dest).expect("remove materialization");
    let external = tempfile::tempdir().expect("external dir");
    let link = registry.worktrees_root().join("link");
    symlink(external.path(), &link).expect("create symlink");
    let mut forged = record.clone();
    forged.dest = link.join(&record.id);

    let error = registry
        .register(&forged)
        .expect_err("symlink parent rejected");
    assert!(matches!(error, super::RegistryError::InvalidRecord { .. }));
    assert!(external.path().exists());
}

#[test]
fn cancelled_creation_removes_creating_record() {
    let (registry_dir, source_dir) = registry_root();
    fs::create_dir_all(source_dir.path()).expect("source dir");
    fs::write(source_dir.path().join("file.txt"), "v1").expect("source file");
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let handle = WorkspaceHandle::new(WorkspaceKind::Source, source_dir.path()).expect("handle");
    let token = CancellationToken::new();
    token.cancel();

    let error = registry
        .create_managed(
            &handle,
            "op-cancelled",
            None,
            WorkingTreeMode::PreserveWorkingTree,
            &token,
        )
        .expect_err("cancelled creation fails");
    assert!(matches!(error, super::RegistryError::Worktree(_)));
    assert!(
        registry
            .load_all()
            .expect("load after cancellation")
            .is_empty(),
        "normal cancellation must not leave a durable Creating record"
    );
}

#[test]
fn concurrent_transitions_are_serialized() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = registered_worktree(&registry, source_dir.path(), "op-1");
    let registry = Arc::new(registry);
    let first = Arc::clone(&registry);
    let second = Arc::clone(&registry);
    let id = record.id.clone();
    let left = thread::spawn(move || first.transition(&id, WorkspaceLifecycle::Active, 10));
    let id = record.id.clone();
    let right = thread::spawn(move || second.transition(&id, WorkspaceLifecycle::Active, 11));
    let results = [
        left.join().expect("left joins"),
        right.join().expect("right joins"),
    ];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        registry
            .load(&record.id)
            .expect("load final")
            .expect("record")
            .lifecycle,
        WorkspaceLifecycle::Active
    );
}

#[test]
fn gc_does_not_collect_merge_pending_worktrees() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = registered_worktree(&registry, source_dir.path(), "op-finished");
    registry
        .transition(&record.id, WorkspaceLifecycle::Active, 10)
        .expect("active");
    registry
        .transition(&record.id, WorkspaceLifecycle::MergePending, 11)
        .expect("merge pending");
    let report = registry
        .gc(&GcOptions {
            now: now_seconds() + 10_000,
            max_age_seconds: 1,
            disk_budget_bytes: None,
            owner_liveness: Box::new(|_| false),
            dry_run: false,
        })
        .expect("gc runs");
    assert!(report.candidates.is_empty());
    assert!(record.dest.exists());
}

#[cfg(unix)]
#[test]
fn gc_size_does_not_follow_child_symlinks() {
    use std::os::unix::fs::symlink;

    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = registered_worktree(&registry, source_dir.path(), "op-finished");
    let external = tempfile::tempdir().expect("external dir");
    fs::write(external.path().join("large.bin"), vec![b'x'; 16 * 1024]).expect("external file");
    symlink(external.path(), record.dest.join("external")).expect("child symlink");

    let report = registry
        .gc(&GcOptions {
            now: now_seconds() + 10_000,
            max_age_seconds: 1,
            disk_budget_bytes: None,
            owner_liveness: Box::new(|_| false),
            dry_run: false,
        })
        .expect("gc runs");
    assert!(report.removed[0].bytes_reclaimed < 16 * 1024);
}

#[test]
fn gc_crash_midway_is_retried_and_converges_on_the_next_pass() {
    let (registry_dir, source_dir) = registry_root();
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry opens");
    let record = registered_worktree(&registry, source_dir.path(), "op-gc-crash");
    // Simulate a crash at GC's exact durable boundary: materialization and
    // auxiliary state are gone, while the Ready record has not been deleted.
    super::remove_materialization(&registry, &record).expect("materialization removed");
    assert!(!record.dest.exists());
    assert!(registry.load(&record.id).expect("load").is_some());
    drop(registry);
    let registry = WorktreeRegistry::open(registry_dir.path()).expect("registry reopens");

    let report = registry
        .gc(&GcOptions {
            now: now_seconds() + 10_000,
            max_age_seconds: 1,
            disk_budget_bytes: None,
            owner_liveness: Box::new(|_| false),
            dry_run: false,
        })
        .expect("gc retries and converges");
    assert_eq!(report.removed.len(), 1);
    assert!(!record.dest.exists());
    assert!(registry.load(&record.id).expect("load").is_none());
}
