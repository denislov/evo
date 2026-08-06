//! coding-agent × extension-host 端到端 agent 循环测试（ARC-730）。
//!
//! 覆盖：真实 agent turn 中 user hooks 生效 —— Observe 事件到达 host
//! 并执行 hook（session_start / user_prompt_submit / stop / post_tool_use）、
//! Tool gate deny 真实阻塞工具调用（工具未执行）、Stop gate block 让循环
//! 继续且 `additionalContext` 注入下一轮 provider 请求；extension 修改
//! 工作区文件经 review tracker 归因 `HookEdit` 并在 review 列表可见、
//! 可 accept/reject。
//!
//! 挂载点：`runtime::facade::lifecycle`。FauxProvider 驱动的完整会话，
//! 与 `application::operation::dispatch_tests` 同一范式；全部使用
//! `current_thread` flavor（避免 session writer 锁并行竞态）。

use super::*;

use ai::api::provider::ApiProvider;
use ai::api::provider::faux::{FauxProvider, FauxResponse, FauxToolCall};
use ai_protocol::api::conversation::{Message, StopReason};
use ai_protocol::api::stream::{EventStream, StreamOptions};
use extension_host::api::{DiagnosticRecord, DiagnosticSink};
use extension_host::api::{
    ExtensionEventKind, ExtensionEventPayload, ExtensionHostOptions, InMemoryTrustStore,
};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind,
};
use tool_contract::api::output::{ToolContent, ToolOutput};
use tool_runtime::api::{DynamicTool, FunctionTool, ToolFuture};

use crate::app::bootstrap::{PromptInvocation, SessionRunOptions};
use crate::app::prompt_runtime::PromptRuntimeOptions;
use crate::authorization::ToolAuthorizationMode;
use crate::operations::prompt::context::PromptTurnOptions;
use crate::runtime::facade::{
    CodingAgentFileReviewActionRequest, CodingAgentHunkReviewActionRequest, CodingAgentOperation,
};
use crate::test_support::ProviderGuard;

fn model(api: &str) -> ai_protocol::api::model::Model {
    ai_protocol::api::model::Model {
        id: "test-model".into(),
        name: "Test Model".into(),
        api: api.into(),
        provider: "test".into(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![ai_protocol::api::model::ModelInput::Text],
        cost: ai_protocol::api::model::ModelCost::default(),
        context_window: 0,
        max_tokens: 0,
        headers: None,
        compat: None,
    }
}

fn prompt_options(api: &str, prompt: &str, tools: Vec<Arc<dyn DynamicTool>>) -> PromptTurnOptions {
    PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: Some("system".into()),
        max_turns: Some(3),
        tools,
        register_builtins: false,
        ai_client: None,
        session: Some(SessionRunOptions::disabled(".".into())),
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: agent_core::api::agent::AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Text(prompt.into()),
    })
}

fn recording_tool(
    name: &str,
    executed: Arc<AtomicUsize>,
    result_text: &'static str,
) -> Arc<dyn DynamicTool> {
    let definition = ToolDefinition {
        id: ToolId::new(name).unwrap(),
        kind: ToolKind::Function,
        description: "Recording tool.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        capabilities: ToolCapabilities {
            read_only: false,
            execution: ToolExecutionMode::Parallel,
            cancel: false,
            timeout: false,
            streaming: false,
            provider_executed: false,
        },
        behavior: ToolBehaviorVersion::V1,
        authorization_risk: AuthorizationRisk::SideEffect,
        requirements: Vec::new(),
    };
    FunctionTool::new(definition, move |_context, _arguments| {
        let executed = executed.clone();
        Box::pin(async move {
            executed.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput {
                content: vec![ToolContent::Text {
                    text: result_text.into(),
                }],
                details: None,
                terminate: false,
            })
        }) as ToolFuture
    })
}

/// 记录每次 provider 请求的 Context（断言 additional_context 注入）。
struct RecordingProvider {
    inner: FauxProvider,
    contexts: Arc<std::sync::Mutex<Vec<ai_protocol::api::conversation::Context>>>,
}

impl RecordingProvider {
    fn new(
        inner: FauxProvider,
    ) -> (
        Self,
        Arc<std::sync::Mutex<Vec<ai_protocol::api::conversation::Context>>>,
    ) {
        let contexts = Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                inner,
                contexts: contexts.clone(),
            },
            contexts,
        )
    }
}

impl ApiProvider for RecordingProvider {
    fn stream(
        &self,
        model: &ai_protocol::api::model::Model,
        ctx: ai_protocol::api::conversation::Context,
        opts: Option<StreamOptions>,
    ) -> EventStream {
        self.contexts.lock().unwrap().push(ctx.clone());
        self.inner.stream(model, ctx, opts)
    }
}

/// 收集 host 诊断的 sink（hook_run 计数）。
#[derive(Debug, Clone, Default)]
struct CollectingSink {
    hook_runs: Arc<AtomicUsize>,
    records: Arc<std::sync::Mutex<Vec<String>>>,
}

impl DiagnosticSink for CollectingSink {
    fn emit(&self, record: DiagnosticRecord) {
        self.records
            .lock()
            .unwrap()
            .push(format!("code={} message={}", record.code, record.message));
        if record.code == "hook_run" {
            self.hook_runs.fetch_add(1, Ordering::SeqCst);
        }
    }
}

fn trusted_extension(
    root: &std::path::Path,
    id: &str,
    hooks: serde_json::Value,
) -> std::path::PathBuf {
    let dir = root.join("extensions").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("extension.json"),
        serde_json::json!({
            "name": id,
            "version": "0.1.0",
            "capabilities": [{"name": "hooks", "description": "user hooks", "risk": "process_execution"}],
            "hooks": hooks,
        })
        .to_string(),
    )
    .unwrap();
    dir
}

fn host_options(
    extensions_root: &std::path::Path,
    trusted_dirs: &[std::path::PathBuf],
    sink: Arc<CollectingSink>,
) -> ExtensionHostOptions {
    let trust = InMemoryTrustStore::new();
    for dir in trusted_dirs {
        trust.trust(dir.clone());
    }
    ExtensionHostOptions {
        global_dirs: vec![extensions_root.to_path_buf()],
        trust_store: Arc::new(trust),
        diagnostics: Some(sink),
        ..Default::default()
    }
}

async fn wait_for(mut condition: impl FnMut() -> bool, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for: {what}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// 完整 agent turn：Observe 事件到达 host 并执行 hook；Tool gate deny
/// 真实阻塞工具调用（工具未执行、turn 正常结束）。
#[tokio::test(flavor = "current_thread")]
async fn agent_loop_fires_observe_hooks_and_tool_gate_blocks_bash() {
    let api = "hooks-e2e-tool-gate";
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let sink = Arc::new(CollectingSink::default());
    let extensions_root = temp.path().join("extensions");
    let observer = trusted_extension(
        temp.path(),
        "observer",
        serde_json::json!([
            {"name": "observe-start", "event": "session_start", "command": "exit 0"},
            {"name": "observe-prompt", "event": "user_prompt_submit", "command": "exit 0"}
        ]),
    );
    let guard = trusted_extension(
        temp.path(),
        "bash-guard",
        serde_json::json!([
            {"name": "tool-guard", "event": "pre_tool_use", "matchTool": "guarded_tool", "command": "echo '{\"decision\":\"deny\",\"reason\":\"blocked by guard\"}'"}
        ]),
    );

    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::single_call(
                vec![FauxResponse {
                    text_deltas: Vec::new(),
                    thinking_deltas: Vec::new(),
                    tool_calls: vec![FauxToolCall {
                        id: "tool-call-guarded".into(),
                        name: "guarded_tool".into(),
                        deltas: vec!["{}".into()],
                        final_arguments: serde_json::json!({}),
                    }],
                }],
                StopReason::ToolUse,
            ),
            FauxProvider::text_call("done", StopReason::Stop),
        ])),
    );
    let bash_executions = Arc::new(AtomicUsize::new(0));
    let tool = recording_tool("guarded_tool", bash_executions.clone(), "ran");
    let mut session = CodingAgentSession::create_internal(
        CodingAgentSessionOptions::new()
            .with_cwd(workspace.clone())
            .with_ai_client(provider_guard.ai_client())
            .with_tool_authorization_mode(ToolAuthorizationMode::Yolo)
            .with_session_id("e2e-tool-gate")
            .with_session_log_root(temp.path())
            .with_extension_host_options(host_options(
                &extensions_root,
                &[observer, guard],
                sink.clone(),
            )),
    )
    .await
    .expect("session opens with a live extension host");

    let outcome = session
        .run_internal(CodingAgentOperation::Prompt(prompt_options(
            api,
            "run bash",
            vec![tool],
        )))
        .await
        .expect("prompt completes");
    assert!(
        matches!(outcome, CodingAgentOperationOutcome::Prompt(_)),
        "prompt turn completes with a denied tool"
    );
    assert_eq!(
        bash_executions.load(Ordering::SeqCst),
        0,
        "Tool gate deny must block the tool in the real agent loop"
    );
    // Observe hooks 已跑（session_start + user_prompt_submit；post_tool_use
    // 在工具被阻塞时不会触发）。
    wait_for(
        || sink.hook_runs.load(Ordering::SeqCst) >= 2,
        "observe hooks run",
    )
    .await;
    let records = sink.records.lock().unwrap().clone();
    assert!(
        records
            .iter()
            .any(|r| r.contains("tool-guard") && r.contains("allow: false")),
        "deny gate run is recorded: {records:?}"
    );
    session
        .shutdown_internal()
        .await
        .expect("session shuts down");
}

/// Stop gate block：工具 turn 结束后不停止（循环继续），且
/// `additionalContext` 注入下一轮 provider 请求的消息流。
#[tokio::test(flavor = "current_thread")]
async fn stop_gate_block_continues_loop_and_injects_additional_context() {
    let api = "hooks-e2e-stop-gate";
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let sink = Arc::new(CollectingSink::default());
    let extensions_root = temp.path().join("extensions");
    let keep_going = trusted_extension(
        temp.path(),
        "keep-going",
        serde_json::json!([
            {"name": "keep-going", "event": "stop", "command": "echo '{\"decision\":\"block\",\"reason\":\"keep working\",\"hookSpecificOutput\":{\"additionalContext\":\"check the fixtures\"}}'"}
        ]),
    );

    let (recording, contexts) = RecordingProvider::new(FauxProvider::with_call_queue(vec![
        FauxProvider::single_call(
            vec![FauxResponse {
                text_deltas: Vec::new(),
                thinking_deltas: Vec::new(),
                tool_calls: vec![FauxToolCall {
                    id: "tool-call-first".into(),
                    name: "work_tool".into(),
                    deltas: vec!["{}".into()],
                    final_arguments: serde_json::json!({}),
                }],
            }],
            StopReason::ToolUse,
        ),
        FauxProvider::text_call("final answer", StopReason::Stop),
    ]));
    let provider_guard = ProviderGuard::register(api, Arc::new(recording));
    let bash_executions = Arc::new(AtomicUsize::new(0));
    let tool = recording_tool("work_tool", bash_executions.clone(), "ran");
    let mut session = CodingAgentSession::create_internal(
        CodingAgentSessionOptions::new()
            .with_cwd(workspace.clone())
            .with_ai_client(provider_guard.ai_client())
            .with_tool_authorization_mode(ToolAuthorizationMode::Yolo)
            .with_session_id("e2e-stop-gate")
            .with_session_log_root(temp.path())
            .with_extension_host_options(host_options(
                &extensions_root,
                &[keep_going],
                sink.clone(),
            )),
    )
    .await
    .expect("session opens with a live extension host");

    let outcome = session
        .run_internal(CodingAgentOperation::Prompt(prompt_options(
            api,
            "use bash then finish",
            vec![tool],
        )))
        .await
        .expect("prompt completes");
    assert!(matches!(outcome, CodingAgentOperationOutcome::Prompt(_)));
    assert_eq!(bash_executions.load(Ordering::SeqCst), 1, "bash tool ran");
    // 两个 provider 调用都被消费：Stop gate block 让工具 turn 结束后的
    // 循环继续（否则第一个工具 turn 后就会 Done）。
    wait_for(
        || {
            let contexts = contexts.lock().unwrap();
            contexts.len() >= 2
        },
        "second provider request happens",
    )
    .await;
    // additional_context 注入：第二次请求的消息流包含 hook 回填的上下文。
    let recorded = contexts.lock().unwrap().clone();
    let injected = recorded
        .iter()
        .skip(1)
        .flat_map(|ctx| ctx.messages.iter())
        .any(|message| {
            matches!(
                message,
                Message::User { content } if content.iter().any(|block| {
                    matches!(block, ai_protocol::api::conversation::ContentBlock::Text { text, .. } if text.contains("check the fixtures"))
                })
            )
        });
    assert!(
        injected,
        "additional context must reach the second provider request"
    );
    session
        .shutdown_internal()
        .await
        .expect("session shuts down");
}

/// extension（hook 命令）修改工作区文件 → review tracker 归因
/// `HookEdit` → review 列表可见、open_change 可读、accept/reject 生效。
#[tokio::test(flavor = "current_thread")]
async fn hook_edit_is_attributed_and_reviewable_end_to_end() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let before = b"before\n";
    std::fs::write(workspace.join("notes.txt"), before).unwrap();
    let sink = Arc::new(CollectingSink::default());
    let extensions_root = temp.path().join("extensions");
    let formatter = trusted_extension(
        temp.path(),
        "formatter",
        serde_json::json!([
            {"name": "formatter", "event": "post_tool_use", "command": "echo 'after' > \"$EVO_WORKSPACE_ROOT/notes.txt\""}
        ]),
    );
    let mut session = CodingAgentSession::non_persistent_internal(
        CodingAgentSessionOptions::new()
            .with_cwd(workspace.clone())
            .with_extension_host_options(host_options(
                &extensions_root,
                &[formatter],
                sink.clone(),
            )),
    )
    .await
    .expect("session opens");

    // 1) 触发 Observe hook：extension 进程修改工作区文件。
    let (session_id, workspace_root) = session.runtime_host.session_identity();
    session.runtime_host.extension_host.submit_event(
        ExtensionEventKind::PostToolUse,
        &session_id,
        &workspace_root,
        ExtensionEventPayload::PostToolUse {
            tool_name: ToolId::new("read_file").unwrap(),
            tool_input: json!({}),
            tool_result: json!({"ok": true}),
            tool_input_truncated: false,
            tool_result_truncated: false,
            duration_ms: None,
            path: None,
        },
    );
    wait_for(
        || sink.hook_runs.load(Ordering::SeqCst) >= 1,
        "formatter hook runs and edits the file",
    )
    .await;
    let after = std::fs::read(workspace.join("notes.txt")).unwrap();
    assert_eq!(after, b"after\n", "the extension process modified the file");

    // 2) 产品把 extension 修改归因 `HookEdit`（review 管线的 record_receipt
    //    路径，与 reject 路径同源）。target_fingerprint 取 review 目标
    //    的真实身份指纹（accept 校验时重新打开同一文件必须一致）。
    let handle = session
        .runtime_host
        .review_service
        .tracker_handle()
        .expect("review tracker starts");
    let fingerprint = {
        let filesystem = workspace_runtime::api::WorkspaceAccessHandle::open_source(&workspace)
            .expect("workspace authority opens");
        filesystem
            .prepare_workspace_review_target("notes.txt")
            .await
            .expect("review target opens")
            .target_fingerprint()
            .to_owned()
    };
    let after_revision = crate::tools::filesystem::mutation_receipt::content_revision(&after);
    let before_revision = crate::tools::filesystem::mutation_receipt::content_revision(before);
    let diff = crate::tools::filesystem::diff::generate_unified_patch(
        "notes.txt",
        std::str::from_utf8(before).unwrap(),
        std::str::from_utf8(&after).unwrap(),
    );
    handle
        .record_receipt(
            change_tracker::ChangeReceipt {
                path: "notes.txt".into(),
                target_fingerprint: fingerprint.clone(),
                before_revision: Some(before_revision.clone()),
                after_revision,
                after_exists: true,
                byte_delta: 0,
                line_delta: 0,
                origin: "hook_edit".into(),
                unified_diff: Some(diff),
            },
            change_tracker::ChangeSource::HookEdit,
            change_tracker::TrackingContext {
                session_id: session_id.clone(),
                turn_id: "review-turn".into(),
                operation_id: "review-op".into(),
                tool_call_id: None,
            },
        )
        .await
        .expect("hook edit receipt is recorded");
    session
        .runtime_host
        .review_service
        .refresh_latest(&handle)
        .expect("review projection refreshes");

    // 3) review 列表可见 + 归因 hook_edit + open_change 可读。
    let changes = session.list_changes().expect("review list is readable");
    let change = changes
        .iter()
        .find(|change| change.path == "notes.txt")
        .expect("the hook edit appears in the review list");
    assert_eq!(change.source, "hook_edit");
    assert!(change.after_exists);
    let opened = session
        .open_change(crate::runtime::facade::CodingAgentFileReviewRequest::from(
            change,
        ))
        .await
        .expect("review opens the hook-edited file");
    assert_eq!(opened.display_path, "notes.txt");
    assert_eq!(opened.content, "after\n");

    // 4) accept hunk 生效；accepted 变更离开 review 列表。
    let hunk_id = change.hunks[0].id.clone();
    session
        .accept_hunk(CodingAgentHunkReviewActionRequest {
            file: CodingAgentFileReviewActionRequest::from(change),
            hunk_id,
        })
        .await
        .expect("hook edit hunk is accepted");
    let changes = session.list_changes().expect("review list is readable");
    assert!(
        !changes.iter().any(|change| change.path == "notes.txt"),
        "accepted changes leave the review list"
    );

    // 5) 第二次 extension 修改 + HookEdit 归因 → reject hunk 生效：文件
    //    回退到上一 accepted baseline。
    std::fs::write(workspace.join("notes.txt"), b"after2\n").unwrap();
    let handle = session
        .runtime_host
        .review_service
        .tracker_handle()
        .expect("review tracker stays");
    let after2_revision = crate::tools::filesystem::mutation_receipt::content_revision(b"after2\n");
    let after_revision = crate::tools::filesystem::mutation_receipt::content_revision(b"after\n");
    let diff =
        crate::tools::filesystem::diff::generate_unified_patch("notes.txt", "after\n", "after2\n");
    handle
        .record_receipt(
            change_tracker::ChangeReceipt {
                path: "notes.txt".into(),
                target_fingerprint: fingerprint,
                before_revision: Some(after_revision),
                after_revision: after2_revision,
                after_exists: true,
                byte_delta: 0,
                line_delta: 0,
                origin: "hook_edit".into(),
                unified_diff: Some(diff),
            },
            change_tracker::ChangeSource::HookEdit,
            change_tracker::TrackingContext {
                session_id: session_id.clone(),
                turn_id: "review-turn".into(),
                operation_id: "review-op".into(),
                tool_call_id: None,
            },
        )
        .await
        .expect("second hook edit receipt is recorded");
    session
        .runtime_host
        .review_service
        .refresh_latest(&handle)
        .expect("review projection refreshes");
    let changes = session.list_changes().expect("review list is readable");
    let change = changes
        .iter()
        .find(|change| change.path == "notes.txt")
        .expect("the second hook edit appears in the review list");
    session
        .reject_hunk(CodingAgentHunkReviewActionRequest {
            file: CodingAgentFileReviewActionRequest::from(change),
            hunk_id: change.hunks[0].id.clone(),
        })
        .await
        .expect("hook edit hunk is rejected");
    assert_eq!(
        std::fs::read(workspace.join("notes.txt")).unwrap(),
        b"after\n",
        "reject restores the pre-edit (accepted baseline) content"
    );

    session
        .shutdown_internal()
        .await
        .expect("session shuts down");
}

/// 手动 compaction 操作发出 `pre_compact` / `post_compact` 事件
/// （Observe gate：hook 真实执行）。ARC-730 产品接线。
#[tokio::test(flavor = "current_thread")]
async fn manual_compaction_fires_pre_and_post_compact_hooks() {
    let api = "hooks-e2e-compact";
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let sink = Arc::new(CollectingSink::default());
    let extensions_root = temp.path().join("extensions");
    let compactor = trusted_extension(
        temp.path(),
        "compactor",
        serde_json::json!([
            {"name": "compactor-pre", "event": "pre_compact", "command": "exit 0"},
            {"name": "compactor-post", "event": "post_compact", "command": "exit 0"}
        ]),
    );

    // 1) 先跑一轮 prompt（持久化 transcript：user + assistant）。
    let provider_guard = ProviderGuard::register(
        api,
        Arc::new(FauxProvider::with_call_queue(vec![
            FauxProvider::text_call("first answer", StopReason::Stop),
            // 2) compaction 的摘要请求。
            FauxProvider::text_call("compact summary", StopReason::Stop),
        ])),
    );
    let mut session = CodingAgentSession::create_internal(
        CodingAgentSessionOptions::new()
            .with_cwd(workspace.clone())
            .with_ai_client(provider_guard.ai_client())
            .with_tool_authorization_mode(ToolAuthorizationMode::Yolo)
            .with_session_id("e2e-compact")
            .with_session_log_root(temp.path())
            .with_extension_host_options(host_options(
                &extensions_root,
                &[compactor],
                sink.clone(),
            )),
    )
    .await
    .expect("session opens with a live extension host");

    let outcome = session
        .run_internal(CodingAgentOperation::Prompt(prompt_options(
            api,
            "first turn",
            Vec::new(),
        )))
        .await
        .expect("prompt completes");
    assert!(matches!(outcome, CodingAgentOperationOutcome::Prompt(_)));

    // 2) 手动 compact：提交 Compact 操作。
    let compact_options = PromptTurnOptions::from_prompt_runtime_options(PromptRuntimeOptions {
        model: model(api),
        api_key: None,
        auth_diagnostics: Vec::new(),
        system_prompt: Some("system".into()),
        max_turns: Some(2),
        tools: Vec::new(),
        register_builtins: false,
        ai_client: None,
        session: Some(SessionRunOptions::disabled(".".into())),
        session_target: None,
        session_name: None,
        thinking_level: None,
        tool_execution: None,
        resources: agent_core::api::agent::AgentResources::default(),
        settings: None,
        invocation: PromptInvocation::Compact {
            custom_instructions: None,
        },
    });
    let outcome = session
        .run_internal(CodingAgentOperation::Compact(compact_options))
        .await
        .expect("manual compaction completes");
    assert!(matches!(outcome, CodingAgentOperationOutcome::Compact(_)));

    // 3) pre_compact + post_compact 事件到达 host 并执行 Observe hook。
    wait_for(
        || sink.hook_runs.load(Ordering::SeqCst) >= 2,
        "pre_compact and post_compact hooks run",
    )
    .await;
    let records = sink.records.lock().unwrap().clone();
    assert!(
        records
            .iter()
            .any(|r| r.contains("compactor-pre") && r.contains("pre_compact")),
        "pre_compact hook runs: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| r.contains("compactor-post") && r.contains("post_compact")),
        "post_compact hook runs: {records:?}"
    );

    session
        .shutdown_internal()
        .await
        .expect("session shuts down");
}
