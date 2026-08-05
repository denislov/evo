use std::path::Path;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use super::{FsEventService, WatchOptions};
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
    std::thread::sleep(Duration::from_millis(200));

    std::fs::write(root.path().join("src.txt"), "kept\n").expect("tracked file");
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(
            event,
            FsEvent::Workspace(semantic) if semantic.path == Path::new("src.txt")
        )
    });

    let quiet = collect_quiet(&mut events, Duration::from_millis(150));
    let ignored = workspace_events(&quiet)
        .into_iter()
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
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(event, FsEvent::Git(GitMetaEvent::IndexChanged))
    });

    git(root.path(), &["commit", "-q", "-m", "second"]);
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(event, FsEvent::Git(GitMetaEvent::HeadMoved))
    });
}

#[test]
fn git_lock_lifecycle_emits_operation_events() {
    let root = tempfile::tempdir().expect("tempdir");
    git_source(root.path());
    let (_service, mut events) = start(root.path());

    let lock = root.path().join(".git/index.lock");
    std::fs::write(&lock, "lock").expect("create lock");
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(event, FsEvent::Git(GitMetaEvent::OperationStarted))
    });

    std::fs::remove_file(&lock).expect("remove lock");
    recv_until(&mut events, Duration::from_secs(5), |event| {
        matches!(event, FsEvent::Git(GitMetaEvent::OperationCompleted))
    });
}

#[test]
fn git_metadata_is_not_emitted_as_workspace_changes() {
    let root = tempfile::tempdir().expect("tempdir");
    git_source(root.path());
    let (_service, mut events) = start(root.path());

    std::fs::write(root.path().join(".git/config"), "# touched\n").expect("touch config");
    std::thread::sleep(Duration::from_millis(200));
    let quiet = collect_quiet(&mut events, Duration::from_millis(150));
    let workspace = workspace_events(&quiet)
        .into_iter()
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
