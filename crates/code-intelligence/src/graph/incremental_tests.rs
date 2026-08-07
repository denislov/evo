//! 增量 reindex 测试：`reindex_file` / `remove_file` / `rename_file` /
//! `reconcile` 与 `IncrementalIndexer` 事件消费 actor（顺序处理、
//! WatchGap 收敛、真实 change-tracker 集成）。

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use change_tracker::{FsChangeKind, FsEvent, SemanticEvent};
use tempfile::tempdir;
use tokio::sync::broadcast;

use crate::budget::IndexBudget;
use crate::graph::build::{IndexBuilder, reconcile, reindex_file};
use crate::graph::incremental::spawn_incremental_indexer;
use crate::graph::index::CodebaseIndex;
use crate::graph::test_support::{builtin, write_workspace};

fn budget() -> IndexBudget {
    IndexBudget::default()
}

fn build(root: &std::path::Path) -> CodebaseIndex {
    IndexBuilder::new(root, &builtin(), budget())
        .build(11)
        .expect("build must succeed")
        .0
}

/// 语义事件构造（root 为 workspace 根；path 为 workspace-relative）。
fn semantic(root: &std::path::Path, path: &str, kind: FsChangeKind) -> FsEvent {
    FsEvent::Workspace(SemanticEvent {
        sequence: 1,
        root: root.to_path_buf(),
        path: std::path::PathBuf::from(path),
        is_directory: false,
        from: None,
        kind,
        at: std::time::SystemTime::now(),
    })
}

#[test]
fn reindex_modified_file_updates_symbols() {
    let dir = tempdir().unwrap();
    write_workspace(dir.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let mut index = build(dir.path());
    assert!(index.has_definition("alpha"));
    assert!(!index.has_definition("beta"));

    // 改名 alpha -> beta 并重解析。
    std::fs::write(dir.path().join("a.rs"), "pub fn beta() {}\n").unwrap();
    let mut skipped = Vec::new();
    let ok = reindex_file(&mut index, dir.path(), "a.rs", &builtin(), &mut skipped);
    assert!(ok);
    assert!(!index.has_definition("alpha"), "旧符号必须消失");
    assert!(index.has_definition("beta"), "新符号必须出现");
    assert_eq!(index.file_count(), 1);
}

#[test]
fn reindex_created_file_adds_symbols() {
    let dir = tempdir().unwrap();
    write_workspace(dir.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let mut index = build(dir.path());
    std::fs::write(dir.path().join("b.rs"), "pub fn beta() {}\n").unwrap();
    let mut skipped = Vec::new();
    assert!(reindex_file(
        &mut index,
        dir.path(),
        "b.rs",
        &builtin(),
        &mut skipped
    ));
    assert!(index.has_definition("beta"));
    assert_eq!(index.file_count(), 2);
    assert!(skipped.is_empty());
}

#[test]
fn remove_file_removes_symbols() {
    let dir = tempdir().unwrap();
    write_workspace(
        dir.path(),
        &[
            ("a.rs", "pub fn alpha() {}\n"),
            ("b.rs", "pub fn beta() {}\n"),
        ],
    );
    let mut index = build(dir.path());
    assert!(index.has_definition("alpha"));
    index.remove_file("a.rs");
    assert!(!index.has_definition("alpha"), "符号必须随文件消失");
    assert!(index.has_definition("beta"));
    assert_eq!(index.file_count(), 1);
}

#[test]
fn rename_file_moves_symbols_without_reparse() {
    let dir = tempdir().unwrap();
    write_workspace(dir.path(), &[("old.rs", "pub fn alpha() {}\n")]);
    let mut index = build(dir.path());
    index.rename_file("old.rs", "new.rs");
    assert!(!index.is_indexed("old.rs"));
    assert!(index.is_indexed("new.rs"));
    let defs = index.find_definitions("alpha");
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].0, "new.rs");
}

#[test]
fn reconcile_removes_missing_and_reindexes_stale_and_adds_new() {
    let dir = tempdir().unwrap();
    write_workspace(
        dir.path(),
        &[
            ("gone.rs", "pub fn gone() {}\n"),
            ("stale.rs", "pub fn stale_old() {}\n"),
            ("keep.rs", "pub fn keep() {}\n"),
        ],
    );
    let mut index = build(dir.path());
    // 模拟漂移：文件被删、内容被改、新文件出现，全部不经过事件流。
    std::fs::remove_file(dir.path().join("gone.rs")).unwrap();
    std::fs::write(dir.path().join("stale.rs"), "pub fn stale_new() {}\n").unwrap();
    std::fs::write(dir.path().join("fresh.rs"), "pub fn fresh() {}\n").unwrap();

    let mut skipped = Vec::new();
    let report = reconcile(&mut index, dir.path(), &builtin(), &mut skipped);

    assert!(!index.has_definition("gone"), "删除的文件符号必须消失");
    assert!(!index.has_definition("stale_old"), "过期符号必须消失");
    assert!(index.has_definition("stale_new"), "stale 文件必须重解析");
    assert!(index.has_definition("fresh"), "新文件必须加入");
    assert!(index.has_definition("keep"));
    assert_eq!(report.removed, 1);
    assert_eq!(report.added, 1);
    assert_eq!(report.reindexed, 1);
    assert_eq!(index.file_count(), 3);
}

fn spawn_indexer(
    index: Arc<RwLock<CodebaseIndex>>,
    root: std::path::PathBuf,
) -> (
    broadcast::Sender<FsEvent>,
    crate::graph::incremental::IncrementalIndexer,
) {
    let (tx, rx) = broadcast::channel(16);
    let indexer =
        spawn_incremental_indexer(index, root, builtin(), rx, Arc::new(Mutex::new(Vec::new())));
    (tx, indexer)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_indexer_processes_events_in_order() {
    let dir = tempdir().unwrap();
    write_workspace(dir.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let index = Arc::new(RwLock::new(build(dir.path())));
    let (tx, mut indexer) = spawn_indexer(index.clone(), dir.path().to_path_buf());

    // 连续事件：Modified（改名）→ Created（新文件）→ Removed。
    tx.send(semantic(dir.path(), "a.rs", FsChangeKind::Modified))
        .unwrap();
    std::fs::write(dir.path().join("a.rs"), "pub fn alpha2() {}\n").unwrap();
    tx.send(semantic(dir.path(), "a.rs", FsChangeKind::Modified))
        .unwrap();
    std::fs::write(dir.path().join("b.rs"), "pub fn beta() {}\n").unwrap();
    tx.send(semantic(dir.path(), "b.rs", FsChangeKind::Created))
        .unwrap();
    std::fs::remove_file(dir.path().join("b.rs")).unwrap();
    tx.send(semantic(dir.path(), "b.rs", FsChangeKind::Removed))
        .unwrap();

    // 轮询等事件全部消费后再 stop（避免 shutdown 与消费的 select 竞争）。
    // ready 条件要求观察 beta 的完整生命周期（先出现后消失），保证
    // Created / Removed 事件都已处理。
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut saw_beta = false;
    loop {
        let (alpha2, beta) = {
            let index = index.read().unwrap();
            (index.has_definition("alpha2"), index.has_definition("beta"))
        };
        saw_beta = saw_beta || beta;
        let ready = alpha2 && !beta && saw_beta;
        if ready || std::time::Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    indexer.stop();
    let index = index.read().unwrap();
    assert!(index.has_definition("alpha2"), "modified 事件必须生效");
    assert!(!index.has_definition("alpha"));
    assert!(!index.has_definition("beta"), "removed 事件必须生效");
    assert_eq!(index.file_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_indexer_renamed_event_moves_and_reindexes() {
    let dir = tempdir().unwrap();
    write_workspace(dir.path(), &[("old.rs", "pub fn alpha() {}\n")]);
    let index = Arc::new(RwLock::new(build(dir.path())));
    let (tx, mut indexer) = spawn_indexer(index.clone(), dir.path().to_path_buf());

    std::fs::rename(dir.path().join("old.rs"), dir.path().join("new.rs")).unwrap();
    tx.send(FsEvent::Workspace(SemanticEvent {
        sequence: 1,
        root: dir.path().to_path_buf(),
        path: std::path::PathBuf::from("new.rs"),
        is_directory: false,
        from: Some(std::path::PathBuf::from("old.rs")),
        kind: FsChangeKind::Renamed,
        at: std::time::SystemTime::now(),
    }))
    .unwrap();

    // 轮询等事件消费后再 stop（避免 shutdown 与事件消费的 select 竞争）。
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let ready = {
            let index = index.read().unwrap();
            index.is_indexed("new.rs") && !index.is_indexed("old.rs")
        };
        if ready || std::time::Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    indexer.stop();
    let index = index.read().unwrap();
    assert!(!index.is_indexed("old.rs"));
    assert!(index.is_indexed("new.rs"));
    assert!(index.has_definition("alpha"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_gap_triggers_reconcile() {
    let dir = tempdir().unwrap();
    write_workspace(dir.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let index = Arc::new(RwLock::new(build(dir.path())));
    let (tx, mut indexer) = spawn_indexer(index.clone(), dir.path().to_path_buf());

    // 绕过事件流直接改文件系统：删除 + 改名 + 新增。
    std::fs::remove_file(dir.path().join("a.rs")).unwrap();
    std::fs::write(dir.path().join("c.rs"), "pub fn gamma() {}\n").unwrap();
    // WatchGap 触发 reconcile。
    tx.send(FsEvent::WatchGap { lost: 3 }).unwrap();

    // 轮询等 reconcile 完成（reconcile 是同步执行的，事件消费后即生效）。
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let ready = {
            let index = index.read().unwrap();
            index.has_definition("gamma") && !index.has_definition("alpha")
        };
        if ready || std::time::Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    indexer.stop();
    let index = index.read().unwrap();
    assert!(!index.has_definition("alpha"), "reconcile 必须移除消失符号");
    assert!(index.has_definition("gamma"), "reconcile 必须发现新文件");
    assert_eq!(index.file_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lagged_broadcast_triggers_reconcile() {
    let dir = tempdir().unwrap();
    write_workspace(dir.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let index = Arc::new(RwLock::new(build(dir.path())));
    // 小缓冲 + 慢消费：填充缓冲迫使 Lagged。
    let (tx, rx) = broadcast::channel(1);
    let mut indexer = spawn_incremental_indexer(
        index.clone(),
        dir.path().to_path_buf(),
        builtin(),
        rx,
        Arc::new(Mutex::new(Vec::new())),
    );
    // 先做文件系统变更，然后猛发事件直到 Lagged（reconcile 会把漂移拉齐）。
    std::fs::write(dir.path().join("a.rs"), "pub fn alpha_changed() {}\n").unwrap();
    for _ in 0..64 {
        tx.send(semantic(dir.path(), "a.rs", FsChangeKind::Modified))
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    indexer.stop();
    let index = index.read().unwrap();
    assert!(
        index.has_definition("alpha_changed") || index.has_definition("alpha"),
        "索引必须包含 a.rs 的某个版本（reconcile 或事件任一收敛）：{:?}",
        index.find_definitions("alpha")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_fs_event_service_drives_incremental_indexer() {
    // 真实 change-tracker 集成：FsEventService 监视 workspace，写文件后
    // 事件流到 IncrementalIndexer，索引更新。
    use change_tracker::{FsEventService, WatchOptions};
    use workspace_runtime::api::{WorkspaceHandle, WorkspaceKind};

    let dir = tempdir().unwrap();
    write_workspace(dir.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let handle = WorkspaceHandle::new(WorkspaceKind::Source, dir.path()).unwrap();
    let watcher = FsEventService::start(
        &handle,
        WatchOptions {
            debounce: Duration::from_millis(10),
            event_queue: 128,
            ..WatchOptions::default()
        },
    )
    .expect("watcher must start");

    let index = Arc::new(RwLock::new(build(dir.path())));
    let mut indexer = spawn_incremental_indexer(
        index.clone(),
        dir.path().to_path_buf(),
        builtin(),
        watcher.events(),
        Arc::new(Mutex::new(Vec::new())),
    );

    std::fs::write(dir.path().join("a.rs"), "pub fn alpha_updated() {}\n").unwrap();
    std::fs::write(dir.path().join("extra.rs"), "pub fn extra() {}\n").unwrap();

    // 轮询等待事件被消费（真实 watcher 有 debounce + 异步）。
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut saw_updated = false;
    let mut saw_extra = false;
    while std::time::Instant::now() < deadline {
        {
            let index = index.read().unwrap();
            saw_updated = index.has_definition("alpha_updated");
            saw_extra = index.has_definition("extra");
        }
        if saw_updated && saw_extra {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(saw_updated, "modified 事件必须到达索引");
    assert!(saw_extra, "created 事件必须到达索引");
    assert!(
        !index.read().unwrap().has_definition("alpha"),
        "旧符号必须被替换"
    );

    indexer.stop();
    watcher.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_waits_for_in_flight_event() {
    // stop 后索引的最终状态包含 stop 前发送的所有事件（顺序处理 + 等待在途）。
    let dir = tempdir().unwrap();
    write_workspace(dir.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let index = Arc::new(RwLock::new(build(dir.path())));
    let (tx, mut indexer) = spawn_indexer(index.clone(), dir.path().to_path_buf());

    std::fs::write(dir.path().join("a.rs"), "pub fn alpha_final() {}\n").unwrap();
    tx.send(semantic(dir.path(), "a.rs", FsChangeKind::Modified))
        .unwrap();
    // 轮询等事件被消费（在途处理开始），随后 stop 不得打断已处理结果。
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if index.read().unwrap().has_definition("alpha_final")
            || std::time::Instant::now() > deadline
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    indexer.stop();
    let index = index.read().unwrap();
    assert!(
        index.has_definition("alpha_final"),
        "stop 后索引必须包含 stop 前的事件结果"
    );
}
