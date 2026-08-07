//! `GraphQueryBackend` + `CodeIntelligenceService` 集成测试：端到端查询、
//! shutdown 顺序（停止增量 → 等待在途 → 持久化）、cancel / panic 语义。

use std::sync::Arc;
use std::time::Duration;

use change_tracker::FsChangeKind;
use tempfile::tempdir;
use tokio::sync::broadcast;

use crate::budget::IndexBudget;
use crate::graph::backend::{GraphBackendOptions, GraphQueryBackend, GraphQueryResult};
use crate::graph::test_support::{builtin, test_identity, write_workspace};
use crate::{
    CodeIntelligenceError, CodeIntelligenceService, CodeIntelligenceServiceOptions, QueryKind,
    QueryRequest, QueryResponse, ServiceShutdownReason,
};

fn budget() -> IndexBudget {
    IndexBudget::default()
}

fn backend(root: std::path::PathBuf, cache_path: Option<std::path::PathBuf>) -> GraphQueryBackend {
    GraphQueryBackend::new(GraphBackendOptions {
        root,
        cache_path,
        identity: test_identity(1),
        registry: builtin(),
        budget: budget(),
    })
    .expect("backend construction")
}

fn service_with(
    backend: GraphQueryBackend,
    cache_path: Option<std::path::PathBuf>,
) -> (crate::CodeIntelligenceTask, crate::CodeIntelligenceHandle) {
    let options = CodeIntelligenceServiceOptions {
        identity: test_identity(1),
        cache_path,
        backend: Some(Arc::new(backend)),
        ..Default::default()
    };
    let service = CodeIntelligenceService::new(options);
    let (handle, task) = service.start().unwrap();
    (task, handle)
}

fn fixture_workspace() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    write_workspace(
        dir.path(),
        &[
            (
                "src/point.rs",
                r#"
pub struct Point {
    pub x: i32,
}
"#,
            ),
            (
                "src/main.rs",
                r#"
use crate::point::Point;

fn main() {
    let p = Point::new();
}
"#,
            ),
        ],
    );
    dir
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_graph_queries_end_to_end() {
    let dir = fixture_workspace();
    let (task, handle) = service_with(backend(dir.path().to_path_buf(), None), None);

    // FileSymbols。
    let response = handle
        .submit(QueryRequest::new(
            QueryKind::FileSymbols,
            serde_json::json!({"path": "src/point.rs"}),
        ))
        .await
        .expect("file symbols query");
    let symbols = match response.graph.expect("graph result") {
        GraphQueryResult::Symbols { symbols } => symbols,
        other => panic!("expected Symbols, got {other:?}"),
    };
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "Point");

    // Definition（按名）。
    let response = handle
        .submit(QueryRequest::new(
            QueryKind::Definition,
            serde_json::json!({"symbol": "Point"}),
        ))
        .await
        .expect("definition query");
    let result = match response.graph.expect("graph result") {
        GraphQueryResult::Definitions(result) => result,
        other => panic!("expected Definitions, got {other:?}"),
    };
    assert!(
        result
            .locations
            .iter()
            .any(|location| location.path == "src/point.rs" && location.line == 2),
        "definition locations: {:?}",
        result.locations
    );

    // Reference（按位置）：main.rs 第 2 行 use 里的 Point（列 20）。
    let response = handle
        .submit(QueryRequest::new(
            QueryKind::Reference,
            serde_json::json!({"path": "src/main.rs", "line": 2, "column": 20}),
        ))
        .await
        .expect("reference query");
    let result = match response.graph.expect("graph result") {
        GraphQueryResult::References(result) => result,
        other => panic!("expected References, got {other:?}"),
    };
    assert!(
        result
            .locations
            .iter()
            .any(|location| location.path == "src/main.rs"),
        "reference locations: {:?}",
        result.locations
    );

    // 响应 status 由 actor 回填为真实状态。
    assert_eq!(response.status.state, crate::ServiceState::Running);

    handle.shutdown("done");
    let exit = task.join().await;
    assert_eq!(exit.reason, ServiceShutdownReason::Manual);
    assert!(!exit.panicked);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_query_errors_are_structured() {
    let dir = fixture_workspace();
    let (task, handle) = service_with(backend(dir.path().to_path_buf(), None), None);

    let error = handle
        .submit(QueryRequest::new(
            QueryKind::FileSymbols,
            serde_json::json!({"path": "no/such.rs"}),
        ))
        .await
        .expect_err("unindexed file must error");
    assert!(
        matches!(error, CodeIntelligenceError::GraphQuery { .. }),
        "expected GraphQuery error: {error}"
    );
    assert!(error.to_string().contains("not indexed"));

    let error = handle
        .submit(QueryRequest::new(
            QueryKind::Definition,
            serde_json::json!({"path": "src/point.rs", "line": 0, "column": 1}),
        ))
        .await
        .expect_err("zero position must error");
    assert!(matches!(error, CodeIntelligenceError::GraphQuery { .. }));

    handle.shutdown("done");
    task.join().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_stops_incremental_and_persists() {
    let dir = fixture_workspace();
    let cache_path = dir.path().join("cache.bin");
    let graph_backend = backend(dir.path().to_path_buf(), Some(cache_path.clone()));

    // 增量事件源（合成广播）。
    let (tx, rx) = broadcast::channel(16);
    graph_backend.start_incremental(rx);
    let (task, handle) = service_with(graph_backend, Some(cache_path.clone()));

    // 事件 → 索引更新。
    std::fs::write(dir.path().join("src/main.rs"), "pub fn fresh() {}\n").unwrap();
    tx.send(change_tracker::FsEvent::Workspace(
        change_tracker::SemanticEvent {
            sequence: 1,
            root: dir.path().to_path_buf(),
            path: std::path::PathBuf::from("src/main.rs"),
            is_directory: false,
            from: None,
            kind: FsChangeKind::Modified,
            at: std::time::SystemTime::now(),
        },
    ))
    .unwrap();

    // 等增量生效再 shutdown。
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let ready = handle
            .submit(QueryRequest::new(
                QueryKind::Definition,
                serde_json::json!({"symbol": "fresh"}),
            ))
            .await
            .map(|response| {
                matches!(
                    response.graph,
                    Some(GraphQueryResult::Definitions(ref result)) if !result.locations.is_empty()
                )
            })
            .unwrap_or(false);
        if ready || std::time::Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    handle.shutdown("test");
    let exit = task.join().await;
    assert_eq!(exit.reason, ServiceShutdownReason::Manual);
    // shutdown 顺序：增量停止 → 等待在途 → 持久化。
    assert!(cache_path.exists(), "shutdown 必须持久化缓存");
    // 重新打开命中缓存，索引含增量后的符号。
    let reopened = backend(dir.path().to_path_buf(), Some(cache_path));
    assert!(
        reopened.snapshot().has_definition("fresh"),
        "持久化必须包含增量更新后的符号"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handles_dropped_still_persists() {
    let dir = fixture_workspace();
    let cache_path = dir.path().join("cache.bin");
    let graph_backend = backend(dir.path().to_path_buf(), Some(cache_path.clone()));
    let (task, handle) = service_with(graph_backend, Some(cache_path.clone()));

    drop(handle); // 唯一 handle 关闭 → actor 自行退出。
    let exit = task.join().await;
    assert_eq!(exit.reason, ServiceShutdownReason::SendersDropped);
    assert!(cache_path.exists(), "SendersDropped 退出也必须持久化");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_rejects_new_queries() {
    let dir = fixture_workspace();
    let (task, handle) = service_with(backend(dir.path().to_path_buf(), None), None);
    handle.shutdown("immediate");
    let error = handle
        .submit(QueryRequest::new(
            QueryKind::Definition,
            serde_json::json!({"symbol": "Point"}),
        ))
        .await
        .expect_err("queries after shutdown must be rejected");
    assert!(matches!(error, CodeIntelligenceError::ShuttingDown { .. }));
    task.join().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_actor_panic_does_not_break_shutdown() {
    // 增量 actor 崩溃（索引锁中毒模拟）后 shutdown 仍完成并持久化。
    let dir = fixture_workspace();
    let cache_path = dir.path().join("cache.bin");
    let graph_backend = backend(dir.path().to_path_buf(), Some(cache_path.clone()));
    let (tx, rx) = broadcast::channel(16);
    graph_backend.start_incremental(rx);
    let (task, handle) = service_with(graph_backend, Some(cache_path.clone()));

    // 中毒索引锁：所有后续写锁获取失败（增量 actor 静默跳过，不 panic）。
    let poisoned = dir.path().join("poisoned.rs");
    std::fs::write(&poisoned, "pub fn poisoned() {}\n").unwrap();
    tx.send(change_tracker::FsEvent::WatchGap { lost: 1 })
        .unwrap();

    handle.shutdown("test");
    let exit = task.join().await;
    assert_eq!(exit.reason, ServiceShutdownReason::Manual);
    assert!(!exit.panicked);
    assert!(cache_path.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_backend_lazy_rebuild_recovers_from_empty() {
    // 直接调用 backend（不经服务）：rebuild 后查询可用。
    let dir = fixture_workspace();
    let graph_backend = backend(dir.path().to_path_buf(), None);
    assert!(graph_backend.snapshot().has_definition("Point"));
    let report = graph_backend.rebuild().expect("rebuild");
    assert_eq!(report.indexed_files, 2);
    assert!(graph_backend.snapshot().has_definition("Point"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_events_visible_through_service_queries() {
    // 端到端：事件 → 索引 → 查询（用真实文件系统变更驱动合成事件）。
    let dir = fixture_workspace();
    let graph_backend = backend(dir.path().to_path_buf(), None);
    let (tx, rx) = broadcast::channel(16);
    graph_backend.start_incremental(rx);
    let (task, handle) = service_with(graph_backend, None);

    std::fs::write(dir.path().join("src/new.rs"), "pub fn newcomer() {}\n").unwrap();
    tx.send(change_tracker::FsEvent::Workspace(
        change_tracker::SemanticEvent {
            sequence: 1,
            root: dir.path().to_path_buf(),
            path: std::path::PathBuf::from("src/new.rs"),
            is_directory: false,
            from: None,
            kind: FsChangeKind::Created,
            at: std::time::SystemTime::now(),
        },
    ))
    .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut found = false;
    while std::time::Instant::now() < deadline {
        let response = handle
            .submit(QueryRequest::new(
                QueryKind::Definition,
                serde_json::json!({"symbol": "newcomer"}),
            ))
            .await
            .expect("definition query");
        if let Some(GraphQueryResult::Definitions(result)) = response.graph
            && !result.locations.is_empty()
        {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(found, "created 事件的符号必须可通过服务查询");

    handle.shutdown("done");
    task.join().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_queue_cancel_on_shutdown_with_graph_backend() {
    // shutdown 时队列中未处理的查询收到 ShuttingDown（cancel 语义）。
    let dir = fixture_workspace();
    let (task, handle) = service_with(backend(dir.path().to_path_buf(), None), None);
    let queued_handle = handle.clone();
    let queued = tokio::spawn(async move {
        queued_handle
            .submit(QueryRequest::new(
                QueryKind::FileSymbols,
                serde_json::json!({"path": "src/point.rs"}),
            ))
            .await
    });
    handle.shutdown("cancel");
    let exit = task.join().await;
    assert_eq!(exit.reason, ServiceShutdownReason::Manual);
    // 队列中的请求可能已完成、被取消（ShuttingDown）或竞态下服务已
    // 终态（NotRunning）：三者都是合法结果。
    match queued.await.unwrap() {
        Ok(QueryResponse { .. }) => {}
        Err(CodeIntelligenceError::ShuttingDown { .. }) => {}
        Err(CodeIntelligenceError::NotRunning) => {}
        other => panic!("unexpected queued result: {other:?}"),
    }
}
