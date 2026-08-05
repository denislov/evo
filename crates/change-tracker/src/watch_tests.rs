use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc::sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use super::{
    CommandSender, FsEventService, Incoming, WatchOptions, forward_raw_result, take_watch_gap,
};
use crate::event::{FsChangeKind, FsEvent, GitMetaEvent};

fn git(root: &Path, args: &[&str]) {
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
}

fn git_source(root: &Path) {
    git(root, &["init", "-q"]);
    std::fs::write(root.join("tracked.txt"), "v1\n").expect("tracked file");
    git(root, &["add", "tracked.txt"]);
    git(root, &["commit", "-q", "-m", "initial"]);
}

fn start(root: &Path) -> (FsEventService, broadcast::Receiver<FsEvent>) {
    let handle = workspace_runtime::api::WorkspaceHandle::new(
        workspace_runtime::api::WorkspaceKind::Source,
        root,
    )
    .expect("handle");
    let service = FsEventService::start(&handle, WatchOptions::default()).expect("service starts");
    let events = service.events();
    (service, events)
}

fn recv_until(
    rx: &mut broadcast::Receiver<FsEvent>,
    timeout: Duration,
    matches: impl Fn(&FsEvent) -> bool,
) -> Vec<FsEvent> {
    let deadline = Instant::now() + timeout;
    let mut seen = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(event) => {
                let matched = matches(&event);
                seen.push(event);
                if matched {
                    return seen;
                }
            }
            Err(broadcast::error::TryRecvError::Empty) => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for event; seen: {seen:?}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(broadcast::error::TryRecvError::Lagged(lost)) => {
                panic!("consumer lagged and lost {lost} events")
            }
            Err(broadcast::error::TryRecvError::Closed) => {
                panic!("event stream closed before match")
            }
        }
    }
}

fn collect_quiet(rx: &mut broadcast::Receiver<FsEvent>, window: Duration) -> Vec<FsEvent> {
    let deadline = Instant::now() + window;
    let mut events = Vec::new();
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(event) => events.push(event),
            Err(broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(broadcast::error::TryRecvError::Lagged(lost)) => {
                panic!("consumer lagged and lost {lost} events")
            }
            Err(broadcast::error::TryRecvError::Closed) => break,
        }
    }
    events
}

fn workspace_events(events: &[FsEvent]) -> Vec<&crate::SemanticEvent> {
    events
        .iter()
        .filter_map(|event| match event {
            FsEvent::Workspace(semantic) => Some(semantic),
            _ => None,
        })
        .collect()
}

#[test]
fn create_modify_remove_emit_semantic_events() {
    let root = tempfile::tempdir().expect("tempdir");
    let (_service, mut events) = start(root.path());

    std::fs::write(root.path().join("file.txt"), "one\n").expect("create");
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Workspace(semantic)
                if semantic.path == Path::new("file.txt")
                    && semantic.kind == FsChangeKind::Created
        )
    });

    std::fs::write(root.path().join("file.txt"), "two\n").expect("modify");
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Workspace(semantic)
                if semantic.path == Path::new("file.txt")
                    && semantic.kind == FsChangeKind::Modified
        )
    });

    std::fs::remove_file(root.path().join("file.txt")).expect("remove");
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Workspace(semantic)
                if semantic.path == Path::new("file.txt")
                    && semantic.kind == FsChangeKind::Removed
        )
    });
}

#[test]
fn rename_pairs_into_a_single_event() {
    let root = tempfile::tempdir().expect("tempdir");
    let (_service, mut events) = start(root.path());
    std::fs::write(root.path().join("old.txt"), "content\n").expect("create");

    std::fs::rename(root.path().join("old.txt"), root.path().join("new.txt")).expect("rename");
    let events = recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Workspace(semantic) if semantic.kind == FsChangeKind::Renamed
        )
    });
    let renamed = workspace_events(&events)
        .into_iter()
        .filter(|semantic| semantic.kind == FsChangeKind::Renamed)
        .collect::<Vec<_>>();
    assert_eq!(renamed.len(), 1, "rename must pair into one event");
    let semantic = renamed.into_iter().next().expect("renamed event");
    assert_eq!(semantic.path, Path::new("new.txt"));
    assert_eq!(semantic.from.as_deref(), Some(Path::new("old.txt")));
}

#[test]
fn debounce_merges_a_burst_on_the_same_path() {
    let root = tempfile::tempdir().expect("tempdir");
    let (_service, mut events) = start(root.path());

    for index in 0..8 {
        std::fs::write(root.path().join("busy.txt"), format!("{index}\n")).expect("write");
        std::thread::sleep(Duration::from_millis(2));
    }
    let seen = recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Workspace(semantic) if semantic.path == Path::new("busy.txt")
        )
    });
    let quiet = collect_quiet(&mut events, Duration::from_millis(150));
    let modified = workspace_events(&seen)
        .into_iter()
        .chain(workspace_events(&quiet))
        .filter(|semantic| semantic.path == Path::new("busy.txt"))
        .count();
    assert!(
        modified <= 2,
        "burst must collapse inside the debounce window, saw {modified} events"
    );
}

#[test]
fn gitignored_paths_are_filtered() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::write(root.path().join(".gitignore"), "target/\n").expect("gitignore");
    let (_service, mut events) = start(root.path());

    std::fs::create_dir_all(root.path().join("target/sub")).expect("ignored dir");
    std::fs::write(root.path().join("target/sub/out.txt"), "x").expect("ignored file");
    std::fs::rename(
        root.path().join("target/sub/out.txt"),
        root.path().join("target/sub/renamed.txt"),
    )
    .expect("ignored rename");

    std::fs::write(root.path().join("src.txt"), "kept\n").expect("tracked file");
    let seen = recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Workspace(semantic) if semantic.path == Path::new("src.txt")
        )
    });

    let quiet = collect_quiet(&mut events, Duration::from_millis(150));
    let ignored = workspace_events(&seen)
        .into_iter()
        .chain(workspace_events(&quiet))
        .filter(|semantic| semantic.path.starts_with("target"))
        .count();
    assert_eq!(
        ignored, 0,
        "gitignored paths must not emit workspace events"
    );
}

#[test]
fn git_add_and_commit_emit_index_and_head_events() {
    let root = tempfile::tempdir().expect("tempdir");
    git_source(root.path());
    let (_service, mut events) = start(root.path());

    std::fs::write(root.path().join("tracked.txt"), "v2\n").expect("edit");
    git(root.path(), &["add", "tracked.txt"]);
    recv_until(
        &mut events,
        Duration::from_secs(5),
        |event| matches!(event, FsEvent::Git(git) if git.kind == GitMetaEvent::IndexChanged),
    );

    git(root.path(), &["commit", "-q", "-m", "second"]);
    recv_until(
        &mut events,
        Duration::from_secs(5),
        |event| matches!(event, FsEvent::Git(git) if git.kind == GitMetaEvent::HeadMoved),
    );
}

#[test]
fn git_lock_lifecycle_emits_operation_events() {
    let root = tempfile::tempdir().expect("tempdir");
    git_source(root.path());
    let (_service, mut events) = start(root.path());

    let lock = root.path().join(".git/index.lock");
    std::fs::write(&lock, "lock").expect("create lock");
    recv_until(
        &mut events,
        Duration::from_secs(5),
        |event| matches!(event, FsEvent::Git(git) if git.kind == GitMetaEvent::OperationStarted),
    );

    std::fs::remove_file(&lock).expect("remove lock");
    recv_until(
        &mut events,
        Duration::from_secs(5),
        |event| matches!(event, FsEvent::Git(git) if git.kind == GitMetaEvent::OperationCompleted),
    );
}

#[test]
fn git_metadata_is_not_emitted_as_workspace_changes() {
    let root = tempfile::tempdir().expect("tempdir");
    git_source(root.path());
    let (_service, mut events) = start(root.path());

    std::fs::write(root.path().join("tracked.txt"), "v2\n").expect("edit");
    git(root.path(), &["add", "tracked.txt"]);
    git(root.path(), &["commit", "-q", "-m", "second"]);
    let seen = recv_until(
        &mut events,
        Duration::from_secs(5),
        |event| matches!(event, FsEvent::Git(git) if git.kind == GitMetaEvent::HeadMoved),
    );
    let quiet = collect_quiet(&mut events, Duration::from_millis(150));
    let workspace = workspace_events(&seen)
        .into_iter()
        .chain(workspace_events(&quiet))
        .filter(|semantic| semantic.path.starts_with(".git"))
        .count();
    assert_eq!(workspace, 0, ".git must never leak into workspace events");
}

#[test]
fn add_root_watches_a_second_workspace_within_budget() {
    let first = tempfile::tempdir().expect("first");
    let second = tempfile::tempdir().expect("second");
    let options = WatchOptions {
        max_roots: 2,
        ..WatchOptions::default()
    };
    let handle = workspace_runtime::api::WorkspaceHandle::new(
        workspace_runtime::api::WorkspaceKind::Source,
        first.path(),
    )
    .expect("handle");
    let service = FsEventService::start(&handle, options).expect("service starts");
    let mut events = service.events();
    let second_handle = workspace_runtime::api::WorkspaceHandle::new(
        workspace_runtime::api::WorkspaceKind::Source,
        second.path(),
    )
    .expect("handle");
    service.add_root(&second_handle).expect("second root added");

    std::fs::write(second.path().join("child.txt"), "x").expect("second root file");
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Workspace(semantic) if semantic.path == Path::new("child.txt")
        )
    });
}

#[test]
fn add_root_budget_fails_closed() {
    let first = tempfile::tempdir().expect("first");
    let second = tempfile::tempdir().expect("second");
    let options = WatchOptions {
        max_roots: 1,
        ..WatchOptions::default()
    };
    let handle = workspace_runtime::api::WorkspaceHandle::new(
        workspace_runtime::api::WorkspaceKind::Source,
        first.path(),
    )
    .expect("handle");
    let service = FsEventService::start(&handle, options).expect("service starts");
    let second_handle = workspace_runtime::api::WorkspaceHandle::new(
        workspace_runtime::api::WorkspaceKind::Source,
        second.path(),
    )
    .expect("handle");

    let error = service
        .add_root(&second_handle)
        .expect_err("budget exceeded");
    assert!(error.to_string().contains("budget"));
}

#[test]
fn sequence_is_monotonic_across_flushes() {
    let root = tempfile::tempdir().expect("tempdir");
    let (_service, mut events) = start(root.path());

    std::fs::write(root.path().join("a.txt"), "a").expect("write a");
    let seen = recv_until(
        &mut events,
        Duration::from_secs(5),
        |event| matches!(event, FsEvent::Workspace(semantic) if semantic.path == Path::new("a.txt")),
    );
    let first = workspace_events(&seen)
        .into_iter()
        .find(|semantic| semantic.path == Path::new("a.txt"))
        .expect("a.txt event");
    std::fs::write(root.path().join("b.txt"), "b").expect("write b");
    let seen = recv_until(
        &mut events,
        Duration::from_secs(5),
        |event| matches!(event, FsEvent::Workspace(semantic) if semantic.path == Path::new("b.txt")),
    );
    let second = workspace_events(&seen)
        .into_iter()
        .find(|semantic| semantic.path == Path::new("b.txt"))
        .expect("b.txt event");
    assert!(second.sequence > first.sequence);
}

#[test]
fn shutdown_is_idempotent_and_stops_the_stream() {
    let root = tempfile::tempdir().expect("tempdir");
    let (service, mut events) = start(root.path());

    service.shutdown();
    service.shutdown();
    std::fs::write(root.path().join("after.txt"), "x").expect("write after shutdown");
    std::thread::sleep(Duration::from_millis(200));
    let quiet = collect_quiet(&mut events, Duration::from_millis(150));
    assert!(
        workspace_events(&quiet).is_empty(),
        "no events may arrive after shutdown"
    );
}

#[test]
fn initial_receiver_preserves_events_before_first_subscription() {
    let root = tempfile::tempdir().expect("tempdir");
    let handle = workspace_runtime::api::WorkspaceHandle::new(
        workspace_runtime::api::WorkspaceKind::Source,
        root.path(),
    )
    .expect("handle");
    let service = FsEventService::start(&handle, WatchOptions::default()).expect("service starts");

    std::fs::write(root.path().join("early.txt"), "early\n").expect("early write");
    thread::sleep(Duration::from_millis(150));
    let mut events = service.events();
    recv_until(
        &mut events,
        Duration::from_secs(5),
        |event| matches!(event, FsEvent::Workspace(semantic) if semantic.path == Path::new("early.txt")),
    );
}

#[test]
fn dynamic_directories_are_watched_recursively() {
    let root = tempfile::tempdir().expect("tempdir");
    let (_service, mut events) = start(root.path());

    std::fs::create_dir_all(root.path().join("new/deep/tree")).expect("nested directories");
    std::fs::write(root.path().join("new/deep/tree/file.txt"), "content\n").expect("nested file");
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Workspace(semantic)
                if semantic.path == Path::new("new/deep/tree/file.txt")
        )
    });
}

#[test]
fn rename_across_ignore_boundary_degrades_to_remove_and_create() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::write(root.path().join(".gitignore"), "ignored/\n").expect("gitignore");
    std::fs::create_dir(root.path().join("ignored")).expect("ignored dir");
    let (_service, mut events) = start(root.path());

    std::fs::write(root.path().join("visible.txt"), "visible\n").expect("visible file");
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Workspace(semantic)
                if semantic.path == Path::new("visible.txt")
                    && semantic.kind == FsChangeKind::Created
        )
    });
    std::fs::rename(
        root.path().join("visible.txt"),
        root.path().join("ignored/moved.txt"),
    )
    .expect("rename into ignored");
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Workspace(semantic)
                if semantic.path == Path::new("visible.txt")
                    && semantic.kind == FsChangeKind::Removed
        )
    });

    std::fs::write(root.path().join("ignored/source.txt"), "ignored\n").expect("ignored source");
    thread::sleep(Duration::from_millis(100));
    std::fs::rename(
        root.path().join("ignored/source.txt"),
        root.path().join("returned.txt"),
    )
    .expect("rename out of ignored");
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Workspace(semantic)
                if semantic.path == Path::new("returned.txt")
                    && semantic.kind == FsChangeKind::Created
        )
    });
}

#[test]
fn git_events_identify_their_root_in_a_mixed_multi_root_service() {
    let plain = tempfile::tempdir().expect("plain root");
    let repository = tempfile::tempdir().expect("repository root");
    git_source(repository.path());
    let options = WatchOptions {
        max_roots: 2,
        ..WatchOptions::default()
    };
    let plain_handle = workspace_runtime::api::WorkspaceHandle::new(
        workspace_runtime::api::WorkspaceKind::Source,
        plain.path(),
    )
    .expect("plain handle");
    let service = FsEventService::start(&plain_handle, options).expect("service starts");
    let mut events = service.events();
    let repository_handle = workspace_runtime::api::WorkspaceHandle::new(
        workspace_runtime::api::WorkspaceKind::Source,
        repository.path(),
    )
    .expect("repository handle");
    service
        .add_root(&repository_handle)
        .expect("repository root added");

    std::fs::write(repository.path().join("tracked.txt"), "v2\n").expect("edit");
    git(repository.path(), &["add", "tracked.txt"]);
    let canonical_repository = std::fs::canonicalize(repository.path()).expect("canonical root");
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Git(git)
                if git.root == canonical_repository
                    && git.kind == GitMetaEvent::IndexChanged
        )
    });
}

#[test]
fn linked_worktree_gitdir_events_use_the_worktree_root() {
    let repository = tempfile::tempdir().expect("repository");
    let worktrees = tempfile::tempdir().expect("worktree parent");
    git_source(repository.path());
    let child = worktrees.path().join("child");
    let child_arg = child.to_str().expect("utf8 child path");
    git(
        repository.path(),
        &["worktree", "add", "-q", "-b", "child-branch", child_arg],
    );
    let handle = workspace_runtime::api::WorkspaceHandle::new(
        workspace_runtime::api::WorkspaceKind::ManagedChild,
        &child,
    )
    .expect("worktree handle");
    let service = FsEventService::start(&handle, WatchOptions::default()).expect("service starts");
    let mut events = service.events();

    std::fs::write(child.join("tracked.txt"), "child\n").expect("edit worktree");
    git(&child, &["add", "tracked.txt"]);
    git(&child, &["commit", "-q", "-m", "child commit"]);
    let canonical_child = std::fs::canonicalize(&child).expect("canonical child");
    let seen = recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Git(git)
                if git.root == canonical_child && git.kind == GitMetaEvent::HeadMoved
        )
    });
    let quiet = collect_quiet(&mut events, Duration::from_millis(150));
    assert!(
        workspace_events(&seen)
            .into_iter()
            .chain(workspace_events(&quiet))
            .all(|event| !event.path.starts_with(".git")),
        "the linked worktree control file must not enter the workspace stream"
    );
}

#[test]
fn overlapping_roots_attribute_events_to_the_most_specific_root() {
    let parent = tempfile::tempdir().expect("parent");
    let child = parent.path().join("child");
    std::fs::create_dir(&child).expect("child root");
    let options = WatchOptions {
        max_roots: 2,
        ..WatchOptions::default()
    };
    let parent_handle = workspace_runtime::api::WorkspaceHandle::new(
        workspace_runtime::api::WorkspaceKind::Source,
        parent.path(),
    )
    .expect("parent handle");
    let service = FsEventService::start(&parent_handle, options).expect("service starts");
    let mut events = service.events();
    let child_handle = workspace_runtime::api::WorkspaceHandle::new(
        workspace_runtime::api::WorkspaceKind::ManagedChild,
        &child,
    )
    .expect("child handle");
    service.add_root(&child_handle).expect("child root added");

    std::fs::write(child.join("owned.txt"), "child\n").expect("child write");
    let canonical_child = std::fs::canonicalize(&child).expect("canonical child");
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Workspace(semantic)
                if semantic.root == canonical_child
                    && semantic.path == Path::new("owned.txt")
        )
    });
}

#[test]
fn nested_git_ref_updates_emit_head_moved() {
    let root = tempfile::tempdir().expect("tempdir");
    git_source(root.path());
    let (_service, mut events) = start(root.path());

    git(root.path(), &["branch", "topic"]);
    recv_until(
        &mut events,
        Duration::from_secs(5),
        |event| matches!(event, FsEvent::Git(git) if git.kind == GitMetaEvent::HeadMoved),
    );
}

#[test]
fn continuous_writes_flush_with_bounded_latency() {
    let root = tempfile::tempdir().expect("tempdir");
    let options = WatchOptions {
        debounce: Duration::from_millis(50),
        event_queue: 4096,
        ..WatchOptions::default()
    };
    let handle = workspace_runtime::api::WorkspaceHandle::new(
        workspace_runtime::api::WorkspaceKind::Source,
        root.path(),
    )
    .expect("handle");
    let service = FsEventService::start(&handle, options).expect("service starts");
    let mut events = service.events();
    let path = root.path().join("busy.txt");
    let writer = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut revision = 0u64;
        while Instant::now() < deadline {
            std::fs::write(&path, format!("{revision}\n")).expect("busy write");
            revision += 1;
            thread::sleep(Duration::from_millis(1));
        }
    });

    let started = Instant::now();
    recv_until(
        &mut events,
        Duration::from_millis(300),
        |event| matches!(event, FsEvent::Workspace(semantic) if semantic.path == Path::new("busy.txt")),
    );
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "continuous activity must not postpone a flush indefinitely"
    );
    writer.join().expect("writer joins");
}

#[test]
fn shutdown_wakes_a_worker_with_a_long_debounce() {
    let root = tempfile::tempdir().expect("tempdir");
    let options = WatchOptions {
        debounce: Duration::from_secs(30),
        ..WatchOptions::default()
    };
    let handle = workspace_runtime::api::WorkspaceHandle::new(
        workspace_runtime::api::WorkspaceKind::Source,
        root.path(),
    )
    .expect("handle");
    let service = FsEventService::start(&handle, options).expect("service starts");

    let started = Instant::now();
    service.shutdown();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "shutdown must wake the worker instead of waiting for debounce"
    );
}

#[test]
fn raw_queue_overflow_becomes_one_exact_watch_gap() {
    let (command_tx, command_rx) = sync_channel(1);
    command_tx
        .try_send(Incoming::Shutdown)
        .expect("fill command queue");
    let command: CommandSender = Arc::new(Mutex::new(command_tx));
    let lost = AtomicU64::new(0);

    forward_raw_result(
        &command,
        &lost,
        Ok(notify::Event::new(notify::EventKind::Other)),
    );
    assert_eq!(lost.load(Ordering::Relaxed), 1);
    assert_eq!(take_watch_gap(&lost), Some(FsEvent::WatchGap { lost: 1 }));
    assert_eq!(take_watch_gap(&lost), None);
    drop(command_rx);
}

#[test]
fn invalid_watch_options_return_structured_errors() {
    let root = tempfile::tempdir().expect("tempdir");
    let handle = workspace_runtime::api::WorkspaceHandle::new(
        workspace_runtime::api::WorkspaceKind::Source,
        root.path(),
    )
    .expect("handle");
    for options in [
        WatchOptions {
            max_roots: 0,
            ..WatchOptions::default()
        },
        WatchOptions {
            event_queue: 0,
            ..WatchOptions::default()
        },
        WatchOptions {
            debounce: Duration::ZERO,
            ..WatchOptions::default()
        },
    ] {
        let error = FsEventService::start(&handle, options)
            .err()
            .expect("invalid options");
        assert!(error.to_string().contains("options"));
    }
}
