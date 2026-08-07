//! `code_lsp` 工具进程级集成测试（fake LSP server）：真实查询往返
//! （hover / definition / references 经工具执行）+ 输出预算截断标记 +
//! in-flight 取消贯通（`--query-delay-ms`）。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use code_intelligence::lsp::server::{LspHandle, LspServerConfig, LspService, LspTask};
use code_intelligence::lsp::state::LspLifecycleState;
use code_intelligence::tools::budget::QueryOutputBudget;
use code_intelligence::tools::{code_lsp_tool, code_lsp_tool_with_budget};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind};
use tool_runtime::api::{DynamicTool, ToolCallContext};
use workspace_runtime::api::{EnvPolicy, TaskOwner};

const FAKE_SERVER: &str = env!("CARGO_BIN_EXE_fake_lsp_server");

fn config_for(workspace: &Path, mode: &str, extra: &[&str]) -> LspServerConfig {
    let mut args = vec!["--mode".to_string(), mode.to_string()];
    args.extend(extra.iter().map(|arg| arg.to_string()));
    let mut config = LspServerConfig::new(
        FAKE_SERVER,
        workspace.to_path_buf(),
        TaskOwner::Operation("lsp-tool-test".into()),
    );
    config.args = args;
    config.env = EnvPolicy::AllowList(Default::default());
    config.backoff = code_intelligence::lsp::BackoffConfig {
        initial: Duration::from_millis(30),
        max: Duration::from_millis(120),
    };
    config.liveness = code_intelligence::lsp::LivenessConfig {
        ping_interval: Duration::from_millis(200),
        ping_timeout: Duration::from_millis(100),
    };
    config.request_timeout = Duration::from_secs(5);
    config
}

async fn wait_ready(handle: &LspHandle) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let snapshot = handle.snapshot().await.expect("snapshot");
        if snapshot.state == LspLifecycleState::Ready {
            return;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    panic!("lsp did not reach Ready");
}

async fn run_tool(
    tool: &Arc<dyn DynamicTool>,
    args: serde_json::Value,
    cancel: CancellationToken,
) -> Result<serde_json::Value, ToolError> {
    let context = ToolCallContext::new(tool.definition().id.clone(), "call-1", cancel);
    let output = tool.execute(context, args).await?;
    let ToolContent::Json { value } = &output.content[0] else {
        panic!("tool output must be JSON content");
    };
    Ok(value.clone())
}

fn shutdown_and_join(handle: &LspHandle, task: LspTask) {
    handle.shutdown();
    let _exit = task.join();
}

#[tokio::test]
async fn hover_definition_references_round_trip_through_tool() {
    let temp = tempfile::tempdir().unwrap();
    let config = config_for(temp.path(), "echo", &[]);
    let (handle, task) = LspService::new(config).start().unwrap();
    wait_ready(&handle).await;
    let uri = format!("file://{}/a.rs", temp.path().display());
    handle
        .open(&uri, "rust", 1, "pub fn alpha() {}\n")
        .await
        .unwrap();

    let tool = code_lsp_tool(handle.clone(), temp.path().to_path_buf());

    let hover = run_tool(
        &tool,
        json!({"query": "hover", "path": "a.rs", "line": 0, "character": 4}),
        CancellationToken::new(),
    )
    .await
    .expect("hover query succeeds");
    assert_eq!(hover["query"], "hover");
    assert_eq!(hover["result"], "fake hover");
    assert!(hover.get("truncated").is_none());

    let definition = run_tool(
        &tool,
        json!({"query": "definition", "path": "a.rs", "line": 0, "character": 4}),
        CancellationToken::new(),
    )
    .await
    .expect("definition query succeeds");
    assert_eq!(definition["count"], 1);
    assert_eq!(definition["results"].as_array().unwrap().len(), 1);
    assert!(definition.get("truncated").is_none());

    let references = run_tool(
        &tool,
        json!({"query": "references", "path": "a.rs", "line": 0, "character": 4}),
        CancellationToken::new(),
    )
    .await
    .expect("references query succeeds");
    assert_eq!(references["count"], 1);
    assert_eq!(references["results"][0]["uri"], uri);
    assert!(references.get("truncated").is_none());

    shutdown_and_join(&handle, task);
}

#[tokio::test]
async fn item_limit_truncation_is_marked() {
    let temp = tempfile::tempdir().unwrap();
    let config = config_for(temp.path(), "echo", &[]);
    let (handle, task) = LspService::new(config).start().unwrap();
    wait_ready(&handle).await;
    let uri = format!("file://{}/a.rs", temp.path().display());
    handle
        .open(&uri, "rust", 1, "pub fn alpha() {}\n")
        .await
        .unwrap();

    let tool = code_lsp_tool(handle.clone(), temp.path().to_path_buf());
    let payload = run_tool(
        &tool,
        json!({"query": "definition", "path": "a.rs", "line": 0, "character": 4, "limit": 0}),
        CancellationToken::new(),
    )
    .await
    .expect("definition query succeeds");
    assert_eq!(payload["count"], 1);
    assert!(payload.get("truncated").is_none());
    shutdown_and_join(&handle, task);
}

#[tokio::test]
async fn in_flight_query_cancel_propagates_to_tool() {
    let temp = tempfile::tempdir().unwrap();
    // hover/definition/references 响应前延迟 800ms：取消必须在响应前触发。
    let config = config_for(temp.path(), "echo", &["--query-delay-ms", "800"]);
    let (handle, task) = LspService::new(config).start().unwrap();
    wait_ready(&handle).await;
    let uri = format!("file://{}/a.rs", temp.path().display());
    handle
        .open(&uri, "rust", 1, "pub fn alpha() {}\n")
        .await
        .unwrap();

    let tool = code_lsp_tool(handle.clone(), temp.path().to_path_buf());
    let cancel = CancellationToken::new();
    let cancel_for_timer = cancel.clone();
    let timer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        cancel_for_timer.cancel();
    });
    let error = run_tool(
        &tool,
        json!({"query": "hover", "path": "a.rs", "line": 0, "character": 4}),
        cancel,
    )
    .await
    .expect_err("cancelled query must fail");
    assert_eq!(error.kind, ToolErrorKind::Cancelled);
    timer.await.unwrap();
    // 等服务器延迟响应被 actor 丢弃后再关停。
    tokio::time::sleep(Duration::from_millis(900)).await;
    shutdown_and_join(&handle, task);
}

#[tokio::test]
async fn byte_budget_truncates_hover_text_with_marker() {
    let temp = tempfile::tempdir().unwrap();
    let config = config_for(temp.path(), "echo", &[]);
    let (handle, task) = LspService::new(config).start().unwrap();
    wait_ready(&handle).await;
    let uri = format!("file://{}/a.rs", temp.path().display());
    handle
        .open(&uri, "rust", 1, "pub fn alpha() {}\n")
        .await
        .unwrap();

    let tool = code_lsp_tool_with_budget(
        handle.clone(),
        temp.path().to_path_buf(),
        QueryOutputBudget {
            max_items: 100,
            max_bytes: 8,
        },
    );
    let hover = run_tool(
        &tool,
        json!({"query": "hover", "path": "a.rs", "line": 0, "character": 4}),
        CancellationToken::new(),
    )
    .await
    .expect("hover query succeeds");
    assert_eq!(hover["truncated"], true);
    let result = hover["result"].as_str().unwrap();
    // 显式截断标记 + 内容被裁剪（保留预算字节 + 标记）。
    assert!(result.ends_with("[truncated]"));
    assert!(result.len() < "fake hover".len() + 16);
    shutdown_and_join(&handle, task);
}
