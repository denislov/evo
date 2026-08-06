//! `CodeIntelligenceService` 生命周期 / shutdown / cancel / panic 测试。

use std::sync::Arc;
use std::time::Duration;

use crate::{
    CacheIdentity, CacheStatus, CodeIntelligenceError, CodeIntelligenceHandle,
    CodeIntelligenceService, CodeIntelligenceServiceOptions, IndexStatus, ParserVersion,
    QueryBackend, QueryKind, QueryRequest, QueryResponse, RevisionId, ServiceShutdownReason,
    ServiceState, SkeletonQueryBackend,
};
use workspace_runtime::api::{WorkspaceId, WorkspaceKind};

fn test_identity() -> CacheIdentity {
    CacheIdentity {
        workspace: WorkspaceId::user_supplied(WorkspaceKind::Source, "demo").unwrap(),
        revision: RevisionId::parse("rev-1").unwrap(),
        parser_version: ParserVersion::Version(1),
    }
}

fn options() -> CodeIntelligenceServiceOptions {
    CodeIntelligenceServiceOptions {
        identity: test_identity(),
        ..Default::default()
    }
}

fn start_service(
    options: CodeIntelligenceServiceOptions,
) -> (CodeIntelligenceHandle, crate::CodeIntelligenceTask) {
    CodeIntelligenceService::new(options).start().unwrap()
}

#[tokio::test]
async fn status_query_round_trip() {
    let (handle, task) = start_service(options());
    let response = handle.submit(QueryRequest::status()).await.unwrap();
    assert_eq!(response.kind, QueryKind::Status);
    assert_eq!(response.status.state, ServiceState::Running);
    assert_eq!(response.status.identity, test_identity());
    assert_eq!(response.status.cache, CacheStatus::Missing);
    assert_eq!(response.status.budget.files, 0);
    handle.shutdown("test done");
    let exit = task.join().await;
    assert_eq!(exit.reason, ServiceShutdownReason::Manual);
    assert_eq!(exit.handled_queries, 1);
    assert!(!exit.panicked);
}

#[tokio::test]
async fn full_lifecycle_idle_to_stopped() {
    let service = CodeIntelligenceService::new(options());
    assert_eq!(service.state(), ServiceState::Idle);
    let (handle, task) = service.start().unwrap();
    assert!(handle.is_running());
    assert_eq!(
        handle
            .submit(QueryRequest::status())
            .await
            .unwrap()
            .status
            .state,
        ServiceState::Running
    );
    handle.shutdown("lifecycle test");
    assert!(!handle.is_running());
    let exit = task.join().await;
    assert_eq!(exit.reason, ServiceShutdownReason::Manual);
    assert!(!exit.panicked);
}

#[tokio::test]
async fn double_start_is_rejected() {
    let service = CodeIntelligenceService::new(options());
    let (_, _task) = service.clone().start().unwrap();
    let err = service.start().unwrap_err();
    assert!(matches!(err, CodeIntelligenceError::AlreadyRunning));
}

#[tokio::test]
async fn submit_during_shutdown_is_rejected() {
    let (handle, _task) = start_service(options());
    handle.shutdown("immediate");
    let err = handle.submit(QueryRequest::status()).await.unwrap_err();
    assert!(matches!(err, CodeIntelligenceError::ShuttingDown { .. }));
}

#[tokio::test]
async fn submit_after_join_is_rejected() {
    let (handle, task) = start_service(options());
    handle.shutdown("stop");
    let exit = task.join().await;
    assert_eq!(exit.reason, ServiceShutdownReason::Manual);
    let err = handle.submit(QueryRequest::status()).await.unwrap_err();
    assert!(matches!(err, CodeIntelligenceError::NotRunning));
}

#[tokio::test]
async fn shutdown_is_idempotent() {
    let (handle, task) = start_service(options());
    handle.shutdown("first");
    handle.shutdown("second");
    handle.shutdown("third");
    let exit = task.join().await;
    assert_eq!(exit.reason, ServiceShutdownReason::Manual);
    assert!(!exit.panicked);
}

#[tokio::test]
async fn all_handles_dropped_exits_senders_dropped() {
    let (handle, task) = start_service(options());
    drop(handle); // 唯一 sender 关闭 -> actor 退出。
    let exit = task.join().await;
    assert_eq!(exit.reason, ServiceShutdownReason::SendersDropped);
    assert!(!exit.panicked);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_queued_requests_but_not_in_flight() {
    // 慢 backend：第一个被处理的请求（in-flight）执行中；另一个请求在
    // 队列中。shutdown 后：in-flight 完成并返回其响应，队列中未处理的
    // 请求收到 ShuttingDown（cancel 语义）。
    use std::sync::atomic::{AtomicUsize, Ordering};
    static BACKEND_STARTS: AtomicUsize = AtomicUsize::new(0);
    struct SlowBackend;
    impl QueryBackend for SlowBackend {
        fn query(&self, _: &QueryRequest) -> Result<QueryResponse, CodeIntelligenceError> {
            BACKEND_STARTS.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(100));
            Err(CodeIntelligenceError::Unimplemented {
                kind: "slow".into(),
                phase: "test",
            })
        }
    }
    let options = CodeIntelligenceServiceOptions {
        identity: test_identity(),
        backend: Some(Arc::new(SlowBackend)),
        ..Default::default()
    };
    let (handle, task) = start_service(options);

    let in_flight_handle = handle.clone();
    let in_flight = tokio::spawn(async move {
        in_flight_handle
            .submit(QueryRequest::new(
                QueryKind::FileSymbols,
                serde_json::json!({"file": "a.rs"}),
            ))
            .await
    });
    let queued_handle = handle.clone();
    let queued = tokio::spawn(async move {
        queued_handle
            .submit(QueryRequest::new(
                QueryKind::Reference,
                serde_json::json!({"name": "foo"}),
            ))
            .await
    });
    // 等 backend 真正开始处理第一个请求（in-flight）再 shutdown。
    while BACKEND_STARTS.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    handle.shutdown("cancel");
    let exit = task.join().await;
    assert_eq!(exit.reason, ServiceShutdownReason::Manual);
    assert_eq!(exit.handled_queries, 1);
    // 结果集合与顺序无关：恰好一个请求完成（Unimplemented），
    // 另一个被取消（ShuttingDown）。
    let mut unimplemented = 0;
    let mut shutting_down = 0;
    for result in [in_flight.await.unwrap(), queued.await.unwrap()] {
        match result {
            Err(CodeIntelligenceError::Unimplemented { .. }) => unimplemented += 1,
            Err(CodeIntelligenceError::ShuttingDown { .. }) => shutting_down += 1,
            other => panic!("unexpected result: {other:?}"),
        }
    }
    assert_eq!(unimplemented, 1);
    assert_eq!(shutting_down, 1);
}

#[tokio::test]
async fn backend_panic_fails_closed() {
    struct PanicBackend;
    impl QueryBackend for PanicBackend {
        fn query(&self, _: &QueryRequest) -> Result<QueryResponse, CodeIntelligenceError> {
            panic!("injected backend panic");
        }
    }
    let options = CodeIntelligenceServiceOptions {
        identity: test_identity(),
        backend: Some(Arc::new(PanicBackend)),
        ..Default::default()
    };
    let (handle, task) = start_service(options);
    let submit_handle = handle.clone();
    let submit = tokio::spawn(async move {
        submit_handle
            .submit(QueryRequest::new(
                QueryKind::Definition,
                serde_json::json!({"name": "x"}),
            ))
            .await
    });
    // panic 后 actor 退出；join 不传播 panic。
    let exit = task.join().await;
    assert_eq!(exit.reason, ServiceShutdownReason::Panic);
    assert!(exit.panicked);
    let err = submit.await.unwrap().unwrap_err();
    assert!(matches!(err, crate::CodeIntelligenceError::QueryPanicked));
}

#[tokio::test]
async fn unimplemented_kinds_report_their_phase() {
    let (handle, task) = start_service(options());
    for (kind, phase) in [
        (QueryKind::FileSymbols, "ARC-810"),
        (QueryKind::Definition, "ARC-810"),
        (QueryKind::Reference, "ARC-810"),
        (QueryKind::Diagnostics, "ARC-820"),
    ] {
        let err = handle
            .submit(QueryRequest::new(kind, serde_json::json!({})))
            .await
            .unwrap_err();
        assert!(
            matches!(
                &err,
                CodeIntelligenceError::Unimplemented { kind: k, phase: p }
                    if k == &kind.as_str().to_string() && p == &phase
            ),
            "kind {kind:?} should report {phase}, got {err:?}"
        );
    }
    handle.shutdown("done");
    let exit = task.join().await;
    assert_eq!(exit.handled_queries, 4);
}

#[tokio::test]
async fn skeleton_backend_answers_unimplemented() {
    let backend = SkeletonQueryBackend;
    let err = backend
        .query(&QueryRequest::new(
            QueryKind::Diagnostics,
            serde_json::json!({}),
        ))
        .unwrap_err();
    assert!(matches!(
        err,
        CodeIntelligenceError::Unimplemented {
            phase: "ARC-820",
            ..
        }
    ));
}

#[tokio::test]
async fn status_reports_cache_states() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join(".evo_index.bin");
    let identity = test_identity();

    // 1. 已就绪的缓存 → Ready。
    let mut cache = crate::IndexCache::new(Some(cache_path.clone()), identity.clone());
    cache
        .save(crate::IndexCacheData {
            schema_version: crate::INDEX_SCHEMA_VERSION,
            built_at_unix_secs: 0,
            files: vec![],
        })
        .unwrap();
    let options = CodeIntelligenceServiceOptions {
        identity: identity.clone(),
        cache_path: Some(cache_path.clone()),
        ..Default::default()
    };
    let (handle, task) = start_service(options);
    let status = handle.submit(QueryRequest::status()).await.unwrap();
    assert_eq!(status.status.cache, CacheStatus::Ready);
    handle.shutdown("done");
    let _ = task.join().await;

    // 2. identity 不匹配 → RebuildRequired。
    let other = CacheIdentity {
        workspace: WorkspaceId::user_supplied(WorkspaceKind::Source, "other").unwrap(),
        ..identity.clone()
    };
    let options = CodeIntelligenceServiceOptions {
        identity: other,
        cache_path: Some(cache_path.clone()),
        ..Default::default()
    };
    let (handle, task) = start_service(options);
    let status = handle.submit(QueryRequest::status()).await.unwrap();
    assert!(matches!(
        status.status.cache,
        CacheStatus::RebuildRequired { .. }
    ));
    handle.shutdown("done");
    let _ = task.join().await;

    // 3. 缓存损坏 → RebuildRequired（不 panic）。
    std::fs::write(&cache_path, b"corrupted-cache-bytes").unwrap();
    let (handle, task) = start_service(crate::CodeIntelligenceServiceOptions {
        identity: identity.clone(),
        cache_path: Some(cache_path.clone()),
        ..Default::default()
    });
    let status = handle.submit(QueryRequest::status()).await.unwrap();
    assert!(matches!(
        status.status.cache,
        CacheStatus::RebuildRequired { .. }
    ));
    handle.shutdown("done");
    let _ = task.join().await;
}

#[tokio::test]
async fn concurrent_status_queries_are_serialized() {
    let (handle, task) = start_service(options());
    let mut futures = Vec::new();
    for _ in 0..10 {
        futures.push(handle.submit(QueryRequest::status()));
    }
    for future in futures {
        let response = future.await.unwrap();
        assert_eq!(response.kind, QueryKind::Status);
    }
    handle.shutdown("done");
    let exit = task.join().await;
    assert_eq!(exit.handled_queries, 10);
}

#[test]
fn query_request_golden_json() {
    let request = QueryRequest::new(
        QueryKind::FileSymbols,
        serde_json::json!({"file": "src/main.rs"}),
    );
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "kind": "file_symbols",
            "context": {"file": "src/main.rs"}
        })
    );
    let back: QueryRequest = serde_json::from_value(json).unwrap();
    assert_eq!(back, request);
}

#[test]
fn query_request_status_default_context() {
    let request = QueryRequest::status();
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json, serde_json::json!({"kind": "status", "context": null}));
    let back: QueryRequest = serde_json::from_value(json).unwrap();
    assert_eq!(back, request);
}

#[test]
fn query_kind_serde_round_trip() {
    for kind in [
        QueryKind::Status,
        QueryKind::FileSymbols,
        QueryKind::Definition,
        QueryKind::Reference,
        QueryKind::Diagnostics,
    ] {
        let json = serde_json::to_value(kind).unwrap();
        let back: QueryKind = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back, kind);
        assert_eq!(json, serde_json::json!(kind.as_str()));
    }
}

#[test]
fn index_status_round_trip_json() {
    let status = IndexStatus {
        state: ServiceState::Running,
        identity: test_identity(),
        cache: CacheStatus::RebuildRequired {
            reason: "corrupted".into(),
        },
        budget: crate::BudgetSnapshot {
            files: 3,
            total_bytes: 42,
            active_parses: 1,
        },
    };
    let json = serde_json::to_value(&status).unwrap();
    let back: IndexStatus = serde_json::from_value(json).unwrap();
    assert_eq!(back, status);
}

#[test]
fn service_state_serde_and_display() {
    assert_eq!(
        serde_json::to_value(ServiceState::Running).unwrap(),
        serde_json::json!("running")
    );
    assert_eq!(ServiceState::Idle.as_str(), "idle");
    assert_eq!(ServiceState::Stopped.as_str(), "stopped");
}

#[tokio::test]
async fn burst_status_queries_all_succeed() {
    let (handle, task) = start_service(options());
    let futures: Vec<_> = (0..10)
        .map(|_| handle.submit(QueryRequest::status()))
        .collect();
    for future in futures {
        let response = future.await.unwrap();
        assert_eq!(response.kind, QueryKind::Status);
    }
    handle.shutdown("done");
    let exit = task.join().await;
    assert_eq!(exit.handled_queries, 10);
}
