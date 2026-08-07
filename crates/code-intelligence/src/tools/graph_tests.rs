//! `code_graph` 工具的端到端测试：真实服务（临时 workspace）+ 参数校验 +
//! 输出预算截断标记 + cancel 贯通 + 结构化错误。

use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio_util::sync::CancellationToken;
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind};
use tool_runtime::api::{DynamicTool, ToolCallContext};

use crate::budget::IndexBudget;
use crate::error::CodeIntelligenceError;
use crate::graph::backend::GraphBackendOptions;
use crate::graph::test_support::{builtin, test_identity, write_workspace};
use crate::service::{
    CodeIntelligenceHandle, CodeIntelligenceService, CodeIntelligenceServiceOptions,
    CodeIntelligenceTask, QueryBackend, QueryRequest, QueryResponse, ServiceShutdownReason,
};
use crate::tools::budget::QueryOutputBudget;
use crate::tools::graph::{code_graph_tool, code_graph_tool_with_budget};

async fn service_with_workspace(
    files: &[(&str, &str)],
) -> (
    tempfile::TempDir,
    CodeIntelligenceHandle,
    CodeIntelligenceTask,
) {
    let dir = tempfile::tempdir().unwrap();
    write_workspace(dir.path(), files);
    let backend = crate::graph::backend::GraphQueryBackend::new(GraphBackendOptions {
        root: dir.path().to_path_buf(),
        cache_path: None,
        identity: test_identity(1),
        registry: builtin(),
        budget: IndexBudget::default(),
    })
    .expect("backend builds");
    let service = CodeIntelligenceService::new(CodeIntelligenceServiceOptions {
        identity: test_identity(1),
        cache_path: None,
        budget: IndexBudget::default(),
        languages: builtin(),
        backend: Some(Arc::new(backend)),
    });
    let (handle, task) = service.start().expect("service starts");
    (dir, handle, task)
}

async fn run_tool(
    tool: &Arc<dyn DynamicTool>,
    args: serde_json::Value,
) -> Result<serde_json::Value, ToolError> {
    let context = ToolCallContext::new(
        tool.definition().id.clone(),
        "call-1",
        CancellationToken::new(),
    );
    let output = tool.execute(context, args).await?;
    let ToolContent::Json { value } = &output.content[0] else {
        panic!("tool output must be JSON content");
    };
    Ok(value.clone())
}

#[tokio::test]
async fn symbols_query_lists_file_symbols() {
    let (_dir, handle, task) =
        service_with_workspace(&[("a.rs", "pub fn alpha() {}\npub struct Beta {}\n")]).await;
    let tool = code_graph_tool(handle.clone());
    let value = run_tool(&tool, json!({"query": "symbols", "path": "a.rs"}))
        .await
        .expect("symbols query succeeds");
    assert_eq!(value["query"], "symbols");
    assert_eq!(value["count"], 2);
    let results = value["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["name"], "alpha");
    assert_eq!(results[1]["name"], "Beta");
    assert!(value.get("truncated").is_none());
    handle.shutdown("test");
    task.join().await;
}

#[tokio::test]
async fn definitions_query_resolves_by_symbol_name() {
    let (_dir, handle, task) =
        service_with_workspace(&[("a.rs", "pub fn alpha() {}\nfn uses() { alpha(); }\n")]).await;
    let tool = code_graph_tool(handle.clone());
    let value = run_tool(&tool, json!({"query": "definitions", "symbol": "alpha"}))
        .await
        .expect("definitions query succeeds");
    assert_eq!(value["query"], "definitions");
    assert_eq!(value["count"], 1);
    assert_eq!(value["results"][0]["path"], "a.rs");
    handle.shutdown("test");
    task.join().await;
}

#[tokio::test]
async fn search_query_ranks_and_truncates_with_marker() {
    let (_dir, handle, task) = service_with_workspace(&[(
        "a.rs",
        "pub fn target_alpha() {}\npub fn target_beta() {}\npub fn target_gamma() {}\n",
    )])
    .await;
    let tool = code_graph_tool(handle.clone());
    let value = run_tool(
        &tool,
        json!({"query": "search", "symbol": "target_", "limit": 2}),
    )
    .await
    .expect("search query succeeds");
    assert_eq!(value["count"], 3);
    assert_eq!(value["results"].as_array().unwrap().len(), 2);
    assert_eq!(value["truncated"], true);
    handle.shutdown("test");
    task.join().await;
}

#[tokio::test]
async fn byte_budget_truncation_is_marked() {
    let (_dir, handle, task) = service_with_workspace(&[(
        "a.rs",
        "pub fn target_alpha() {}\npub fn target_beta() {}\npub fn target_gamma() {}\n",
    )])
    .await;
    let tool = code_graph_tool_with_budget(
        handle.clone(),
        QueryOutputBudget {
            max_items: 100,
            max_bytes: 200,
        },
    );
    let value = run_tool(&tool, json!({"query": "symbols", "path": "a.rs"}))
        .await
        .expect("symbols query succeeds");
    assert_eq!(value["count"], 3);
    let kept = value["results"].as_array().unwrap().len();
    assert!(kept < 3, "byte budget must drop results");
    assert_eq!(value["truncated"], true);
    handle.shutdown("test");
    task.join().await;
}

#[tokio::test]
async fn unindexed_file_returns_structured_unavailable() {
    let (_dir, handle, task) = service_with_workspace(&[("a.rs", "pub fn alpha() {}\n")]).await;
    let tool = code_graph_tool(handle.clone());
    let error = run_tool(&tool, json!({"query": "symbols", "path": "missing.rs"}))
        .await
        .expect_err("unindexed file must fail");
    assert_eq!(error.kind, ToolErrorKind::Unavailable);
    assert!(error.message.contains("not indexed"), "{}", error.message);
    handle.shutdown("test");
    task.join().await;
}

#[tokio::test]
async fn stopped_service_returns_structured_unavailable() {
    let (_dir, handle, task) = service_with_workspace(&[("a.rs", "pub fn alpha() {}\n")]).await;
    let tool = code_graph_tool(handle.clone());
    handle.shutdown("test");
    let exit = task.join().await;
    assert_eq!(exit.reason, ServiceShutdownReason::Manual);
    let error = run_tool(&tool, json!({"query": "symbols", "path": "a.rs"}))
        .await
        .expect_err("stopped service must fail");
    assert_eq!(error.kind, ToolErrorKind::Unavailable);
    assert!(error.message.contains("not running"));
}

#[tokio::test]
async fn invalid_arguments_fail_fast() {
    let (_dir, handle, task) = service_with_workspace(&[("a.rs", "pub fn alpha() {}\n")]).await;
    let tool = code_graph_tool(handle.clone());
    for args in [
        json!({}),                                                  // 缺 query
        json!({"query": "symbols"}),                                // symbols 缺 path
        json!({"query": "definitions"}),                            // 缺 symbol 与位置
        json!({"query": "bogus", "path": "a.rs"}),                  // 未知模式
        json!({"query": "search"}),                                 // search 缺 symbol
        json!({"query": "definitions", "path": "a.rs", "line": 1}), // 缺 column
    ] {
        let error = run_tool(&tool, args.clone())
            .await
            .expect_err("invalid arguments must fail");
        assert_eq!(
            error.kind,
            ToolErrorKind::InvalidArguments,
            "args {args}: {}",
            error.message
        );
    }
    handle.shutdown("test");
    task.join().await;
}

/// 阻塞 backend：查询进入后阻塞直到 release，用于 cancel 贯通测试。
struct BlockingBackend {
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl QueryBackend for BlockingBackend {
    fn query(&self, _request: &QueryRequest) -> Result<QueryResponse, CodeIntelligenceError> {
        let _ = self
            .release
            .lock()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(30));
        Err(CodeIntelligenceError::NotRunning)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_propagates_to_in_flight_query() {
    let dir = tempfile::tempdir().unwrap();
    write_workspace(dir.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let service = CodeIntelligenceService::new(CodeIntelligenceServiceOptions {
        identity: test_identity(1),
        cache_path: None,
        budget: IndexBudget::default(),
        languages: builtin(),
        backend: Some(Arc::new(BlockingBackend {
            release: Mutex::new(release_rx),
        })),
    });
    let (handle, task) = service.start().expect("service starts");
    let tool = code_graph_tool(handle.clone());
    let cancel = CancellationToken::new();
    let cancel_for_timer = cancel.clone();
    let timer = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel_for_timer.cancel();
    });
    let context = ToolCallContext::new(tool.definition().id.clone(), "call-1", cancel);
    let error = tool
        .execute(context, json!({"query": "symbols", "path": "a.rs"}))
        .await
        .expect_err("cancelled query must fail");
    assert_eq!(error.kind, ToolErrorKind::Cancelled);
    timer.await.unwrap();
    // 释放阻塞 backend，让服务收尾。
    drop(release_tx);
    handle.shutdown("test");
    task.join().await;
}
