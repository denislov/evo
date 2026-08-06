//! coding-agent × extension-host 真实适配器集成测试（ARC-710）。
//!
//! 覆盖：session 生命周期事件到达 host 并触发 Observe hook、Tool gate
//! deny 决策暴露给产品、Stop gate 决策暴露、首次启用展示、shutdown 顺序。
//! 挂载点：`runtime::facade::lifecycle`。

use super::*;

use crate::mutex::MutexExt;
use extension_host::api::{DiagnosticRecord, DiagnosticSink};
use extension_host::api::{
    ExtensionEventKind, ExtensionEventPayload, ExtensionHostOptions, InMemoryTrustStore,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 收集诊断的 sink（host 诊断 -> 测试断言）。
#[derive(Debug, Clone, Default)]
struct CollectingSink {
    records: Arc<std::sync::Mutex<Vec<String>>>,
    hook_runs: Arc<AtomicUsize>,
}

impl DiagnosticSink for CollectingSink {
    fn emit(&self, record: DiagnosticRecord) {
        self.records
            .lock_or_recover("collecting sink records")
            .push(format!("code={} message={}", record.code, record.message));
        if record.code == "hook_run" {
            self.hook_runs.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// 一个可信任的扩展目录（含一个 observe hook）。
fn trusted_extension(
    root: &std::path::Path,
    id: &str,
    event: &str,
    command: &str,
) -> std::path::PathBuf {
    let dir = root.join("extensions").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("extension.json"),
        serde_json::json!({
            "name": id,
            "version": "0.1.0",
            "capabilities": [{"name": "hooks", "description": "user hooks", "risk": "process_execution"}],
            "hooks": [
                {"name": format!("{id}-hook"), "event": event, "command": command}
            ]
        })
        .to_string(),
    )
    .unwrap();
    dir
}

fn host_options(
    extensions_root: &std::path::Path,
    trusted_dir: &std::path::Path,
    sink: Arc<CollectingSink>,
) -> ExtensionHostOptions {
    let trust = InMemoryTrustStore::new();
    trust.trust(trusted_dir.to_path_buf());
    ExtensionHostOptions {
        global_dirs: vec![extensions_root.to_path_buf()],
        trust_store: Arc::new(trust),
        diagnostics: Some(sink),
        ..Default::default()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn session_lifecycle_events_reach_the_host_and_fire_observe_hooks() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let extension = trusted_extension(
        temp.path(),
        "lifecycle-observer",
        "session_start",
        "echo observed",
    );
    // 补一个 session_end hook（同一扩展，验证生命周期两端）。
    let extension_json = extension.join("extension.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&extension_json).unwrap()).unwrap();
    manifest["hooks"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "name": "lifecycle-observer-end",
            "event": "session_end",
            "command": "echo observed"
        }));
    std::fs::write(&extension_json, manifest.to_string()).unwrap();
    let sink = Arc::new(CollectingSink::default());
    let options = CodingAgentSessionOptions::new()
        .with_cwd(workspace.clone())
        .with_extension_host_options(host_options(
            &temp.path().join("extensions"),
            &extension,
            sink.clone(),
        ));

    let mut session = CodingAgentSession::non_persistent_internal(options)
        .await
        .expect("non-persistent session opens");
    wait_for(
        || sink.hook_runs.load(Ordering::SeqCst) >= 1,
        "session_start must reach the host and run the observe hook",
    )
    .await;

    session
        .shutdown_internal()
        .await
        .expect("session shuts down");
    // session_end 事件 + host shutdown：Observe hook 再跑一次；dispatch
    // task 被 join（无泄漏）。
    wait_for(
        || sink.hook_runs.load(Ordering::SeqCst) >= 2,
        "session_end must fire the observe hook before host shutdown",
    )
    .await;
}

/// 轮询直到条件成立（异步派发的事件处理）。
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

#[tokio::test(flavor = "current_thread")]
async fn tool_gate_decision_is_exposed_to_the_product() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let extension = trusted_extension(
        temp.path(),
        "bash-guard",
        "pre_tool_use",
        "echo '{\"decision\":\"deny\",\"reason\":\"no shell\"}'",
    );
    let sink = Arc::new(CollectingSink::default());
    let options = CodingAgentSessionOptions::new()
        .with_cwd(workspace.clone())
        .with_extension_host_options(host_options(
            &temp.path().join("extensions"),
            &extension,
            sink.clone(),
        ));
    let mut session = CodingAgentSession::non_persistent_internal(options)
        .await
        .expect("session opens");

    let gate = session
        .runtime_host
        .extension_host
        .gate()
        .expect("gate is exposed while a host is wired");
    let event = extension_host::api::ExtensionEvent::new(
        ExtensionEventKind::PreToolUse,
        "s",
        workspace.to_string_lossy(),
        "t",
        ExtensionEventPayload::PreToolUse {
            tool_name: tool_contract::api::definition::ToolId::new("bash").unwrap(),
            tool_input: serde_json::json!({"command": "ls"}),
            tool_input_truncated: false,
            path: None,
        },
    );
    let decision = gate.evaluate_tool(&event).await;
    let denied = match &decision {
        extension_host::api::ToolGateDecision::Deny { reason } => {
            assert_eq!(reason, "no shell");
            true
        }
        other => {
            panic!("Tool gate deny must reach the product, got {other:?}");
        }
    };
    assert!(denied);

    session
        .shutdown_internal()
        .await
        .expect("session shuts down");
}

#[tokio::test(flavor = "current_thread")]
async fn stop_gate_decision_is_exposed_to_the_product() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let extension = trusted_extension(
        temp.path(),
        "keep-going",
        "stop",
        "echo '{\"decision\":\"block\",\"reason\":\"tests pending\"}'",
    );
    let sink = Arc::new(CollectingSink::default());
    let options = CodingAgentSessionOptions::new()
        .with_cwd(workspace.clone())
        .with_extension_host_options(host_options(
            &temp.path().join("extensions"),
            &extension,
            sink.clone(),
        ));
    let mut session = CodingAgentSession::non_persistent_internal(options)
        .await
        .expect("session opens");

    let gate = session
        .runtime_host
        .extension_host
        .gate()
        .expect("gate is exposed while a host is wired");
    let event = extension_host::api::ExtensionEvent::new(
        ExtensionEventKind::Stop,
        "s",
        workspace.to_string_lossy(),
        "t",
        ExtensionEventPayload::Stop {
            reason: "end_turn".into(),
            last_assistant_message: None,
        },
    );
    let decision = gate.evaluate_stop(&event).await;
    assert!(
        decision.wants_continuation(),
        "block 语义 = 产品继续运行（不停止）"
    );
    assert_eq!(decision.blocks, ["tests pending"]);

    session
        .shutdown_internal()
        .await
        .expect("session shuts down");
}

#[tokio::test(flavor = "current_thread")]
async fn first_enable_requests_are_exposed_with_source_and_capabilities() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    // 不信任（NotDecided）→ 首次启用请求。
    let extensions_root = temp.path().join("extensions");
    let dir = extensions_root.join("pending-ext");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("extension.json"),
        serde_json::json!({
            "name": "Pending",
            "version": "0.2.0",
            "capabilities": [{"name": "lint", "description": "run linters", "risk": "process_execution"}]
        })
        .to_string(),
    )
    .unwrap();
    let sink = Arc::new(CollectingSink::default());
    let trust = InMemoryTrustStore::new();
    let options = CodingAgentSessionOptions::new()
        .with_cwd(workspace.clone())
        .with_extension_host_options(ExtensionHostOptions {
            global_dirs: vec![extensions_root],
            trust_store: Arc::new(trust),
            diagnostics: Some(sink.clone()),
            ..Default::default()
        });
    let mut session = CodingAgentSession::non_persistent_internal(options)
        .await
        .expect("session opens");

    let requests = session.runtime_host.extension_host.first_enables();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].extension_id, "pending-ext");
    assert_eq!(requests[0].capabilities[0].name, "lint");
    assert!(
        requests[0]
            .source_dir
            .to_string_lossy()
            .contains("pending-ext"),
        "来源目录必须随请求展示"
    );

    session
        .shutdown_internal()
        .await
        .expect("session shuts down");
}

#[tokio::test(flavor = "current_thread")]
async fn host_without_extensions_keeps_the_product_behavior_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    // 无扩展目录 → 事件提交 no-op 但 session 正常。
    let sink = Arc::new(CollectingSink::default());
    let trust = InMemoryTrustStore::new();
    let options = CodingAgentSessionOptions::new()
        .with_cwd(workspace.clone())
        .with_extension_host_options(ExtensionHostOptions {
            global_dirs: vec![temp.path().join("no-such-extensions")],
            trust_store: Arc::new(trust),
            diagnostics: Some(sink),
            ..Default::default()
        });
    let mut session = CodingAgentSession::non_persistent_internal(options)
        .await
        .expect("session opens with an empty host");
    session
        .shutdown_internal()
        .await
        .expect("session shuts down");
}
