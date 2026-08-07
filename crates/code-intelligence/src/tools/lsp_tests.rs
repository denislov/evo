//! `code_lsp` 工具测试：参数校验 / uri 构建 / 结果映射在 `lsp.rs` 内嵌
//! 单测覆盖；本文件覆盖服务级路径——spawn 失败（Failed 终态）→
//! 结构化 `Unavailable`、预先取消 → `Cancelled`、shutdown 后 → 结构化
//! `Unavailable`。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind};
use tool_runtime::api::{DynamicTool, ToolCallContext};
use workspace_runtime::api::TaskOwner;

use crate::lsp::server::{LspHandle, LspServerConfig, LspService};
use crate::lsp::state::LspLifecycleState;
use crate::tools::lsp::code_lsp_tool;

fn config(root: &Path, command: &str) -> LspServerConfig {
    LspServerConfig::new(
        command,
        root.to_path_buf(),
        TaskOwner::Operation("lsp-tool-test".into()),
    )
}

async fn wait_failed(handle: &LspHandle) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let snapshot = handle.snapshot().await.expect("snapshot");
        if matches!(snapshot.state, LspLifecycleState::Failed { .. }) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    panic!("state did not reach Failed");
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

#[tokio::test]
async fn failed_server_returns_structured_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let service = LspService::new(config(temp.path(), "/nonexistent/lsp-binary"));
    let (handle, task) = service.start().expect("service starts");
    wait_failed(&handle).await;
    let tool = code_lsp_tool(handle.clone(), temp.path().to_path_buf());
    let error = run_tool(
        &tool,
        json!({"query": "hover", "path": "a.rs", "line": 0, "character": 0}),
        CancellationToken::new(),
    )
    .await
    .expect_err("failed server must fail the query");
    assert_eq!(error.kind, ToolErrorKind::Unavailable);
    assert!(error.message.contains("not ready"), "{}", error.message);
    handle.shutdown();
    task.join().await;
}

#[tokio::test]
async fn pre_cancelled_token_fails_fast_with_cancelled() {
    let temp = tempfile::tempdir().unwrap();
    let service = LspService::new(config(temp.path(), "/nonexistent/lsp-binary"));
    let (handle, task) = service.start().expect("service starts");
    let tool = code_lsp_tool(handle.clone(), temp.path().to_path_buf());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = run_tool(
        &tool,
        json!({"query": "hover", "path": "a.rs", "line": 0, "character": 0}),
        cancel,
    )
    .await
    .expect_err("cancelled tool call must fail");
    assert_eq!(error.kind, ToolErrorKind::Cancelled);
    handle.shutdown();
    task.join().await;
}

#[tokio::test]
async fn stopped_service_returns_structured_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let service = LspService::new(config(temp.path(), "/nonexistent/lsp-binary"));
    let (handle, task) = service.start().expect("service starts");
    wait_failed(&handle).await;
    let tool = code_lsp_tool(handle.clone(), temp.path().to_path_buf());
    handle.shutdown();
    task.join().await;
    let error = run_tool(
        &tool,
        json!({"query": "hover", "path": "a.rs", "line": 0, "character": 0}),
        CancellationToken::new(),
    )
    .await
    .expect_err("stopped lsp must fail the query");
    assert_eq!(error.kind, ToolErrorKind::Unavailable);
    assert!(error.message.contains("not running"), "{}", error.message);
}

#[tokio::test]
async fn invalid_arguments_fail_fast_without_service() {
    // 参数校验发生在查询之前：不启动服务也能断言。
    let temp = tempfile::tempdir().unwrap();
    let service = LspService::new(config(temp.path(), "/nonexistent/lsp-binary"));
    let (handle, task) = service.start().expect("service starts");
    let tool = code_lsp_tool(handle.clone(), temp.path().to_path_buf());
    for args in [
        json!({}), // 缺全部必填
        json!({"query": "bogus", "path": "a.rs", "line": 0, "character": 0}),
        json!({"query": "hover", "path": "../escape.rs", "line": 0, "character": 0}),
        json!({"query": "hover", "path": "", "line": 0, "character": 0}),
    ] {
        let error = run_tool(&tool, args.clone(), CancellationToken::new())
            .await
            .expect_err("invalid arguments must fail");
        assert_eq!(error.kind, ToolErrorKind::InvalidArguments, "args {args}");
    }
    handle.shutdown();
    task.join().await;
}
