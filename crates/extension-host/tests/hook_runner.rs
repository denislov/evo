//! Hook runner 集成测试：真实进程 spawn 路径（超时 / 取消 / 输出洪泛 /
//! 崩溃 / 退出码 / sandbox 携带）+ host shutdown 时在途 hook 处理。
//!
//! 这些测试是 ARC-730 跨域矩阵的基础：进程语义在此钉死，ARC-730 只补
//! hunk 归因与重连风暴等产品级矩阵。

use std::sync::Arc;
use std::time::Duration;

use extension_host::api::*;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tool_contract::api::definition::ToolId;

fn spec(name: &str, event: ExtensionEventKind, command: &str, dir: &std::path::Path) -> HookSpec {
    HookSpec {
        name: name.into(),
        event,
        match_tool: None,
        match_path: None,
        match_profile: None,
        priority: 0,
        command: command.into(),
        source_dir: dir.to_path_buf(),
        timeout_secs: None,
        enabled: true,
        matcher: HookMatcher::match_all(),
    }
}

fn ctx(root: &std::path::Path, cancel: CancellationToken) -> RunContext {
    RunContext {
        session_id: "it-session".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        cancel,
        sandbox_capability: None,
    }
}

fn envelope(root: &std::path::Path) -> String {
    let event = ExtensionEvent::new(
        ExtensionEventKind::PreToolUse,
        "it-session",
        root.to_string_lossy(),
        "2026-08-06T00:00:00Z",
        ExtensionEventPayload::PreToolUse {
            tool_name: ToolId::new("bash").unwrap(),
            tool_input: json!({"command": "ls"}),
            tool_input_truncated: false,
            path: None,
        },
    );
    serde_json::to_string(&event).unwrap()
}

#[tokio::test]
async fn successful_hook_receives_envelope_and_env() {
    let root = tempfile::tempdir().unwrap();
    let hook = spec(
        "env-probe",
        ExtensionEventKind::PreToolUse,
        "test \"$EVO_HOOK_EVENT\" != \"\" && test \"$EVO_HOOK_NAME\" = \"env-probe\" \
         && test \"$EVO_SESSION_ID\" = \"it-session\" && test \"$EVO_WORKSPACE_ROOT\" != \"\" \
         && cat > /dev/null",
        root.path(),
    );
    let outcome = run_hook(
        &hook,
        &envelope(root.path()),
        &ctx(root.path(), CancellationToken::new()),
        Duration::from_secs(10),
        GateKind::Tool,
    )
    .await;
    assert_eq!(
        outcome,
        HookRunOutcome::Success,
        "hook must see injected env vars"
    );
}

#[tokio::test]
async fn tool_gate_deny_via_exit_2() {
    let root = tempfile::tempdir().unwrap();
    let hook = spec(
        "deny",
        ExtensionEventKind::PreToolUse,
        "echo 'blocked by policy' >&2; exit 2",
        root.path(),
    );
    let outcome = run_hook(
        &hook,
        &envelope(root.path()),
        &ctx(root.path(), CancellationToken::new()),
        Duration::from_secs(10),
        GateKind::Tool,
    )
    .await;
    match outcome {
        HookRunOutcome::ToolDecision { allow, reason } => {
            assert!(!allow);
            assert_eq!(reason.as_deref(), Some("blocked by policy"));
        }
        other => panic!("expected deny, got {other:?}"),
    }
}

#[tokio::test]
async fn timeout_kills_the_process_tree() {
    let root = tempfile::tempdir().unwrap();
    let hook = spec("slow", ExtensionEventKind::Stop, "sleep 30", root.path());
    let started = std::time::Instant::now();
    let outcome = run_hook(
        &hook,
        &envelope(root.path()),
        &ctx(root.path(), CancellationToken::new()),
        Duration::from_millis(300),
        GateKind::Observe,
    )
    .await;
    assert_eq!(outcome, HookRunOutcome::TimedOut);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "timeout must terminate the child promptly"
    );
}

#[tokio::test]
async fn cancellation_returns_cancelled() {
    let root = tempfile::tempdir().unwrap();
    let hook = spec("slow", ExtensionEventKind::Stop, "sleep 30", root.path());
    let cancel = CancellationToken::new();
    let task = tokio::spawn({
        let hook = hook.clone();
        let event_json = envelope(root.path());
        let ctx = ctx(root.path(), cancel.clone());
        async move {
            run_hook(
                &hook,
                &event_json,
                &ctx,
                Duration::from_secs(60),
                GateKind::Observe,
            )
            .await
        }
    });
    // 等 spawn 完成再取消（进程树已建立）。
    tokio::time::sleep(Duration::from_millis(150)).await;
    cancel.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancelled hook must return promptly")
        .unwrap();
    assert_eq!(outcome, HookRunOutcome::Cancelled);
}

#[tokio::test]
async fn output_flood_is_truncated_not_lost() {
    let root = tempfile::tempdir().unwrap();
    let hook = spec(
        "flood",
        ExtensionEventKind::PostToolUse,
        "head -c 400000 /dev/zero | tr '\\0' x",
        root.path(),
    );
    let outcome = run_hook(
        &hook,
        &envelope(root.path()),
        &ctx(root.path(), CancellationToken::new()),
        Duration::from_secs(10),
        GateKind::Observe,
    )
    .await;
    assert_eq!(
        outcome,
        HookRunOutcome::OutputLimited,
        "over-budget output is reported as OutputLimited"
    );
}

#[tokio::test]
async fn crashed_hook_reports_failed() {
    let root = tempfile::tempdir().unwrap();
    let hook = spec("crash", ExtensionEventKind::Stop, "exit 7", root.path());
    let outcome = run_hook(
        &hook,
        &envelope(root.path()),
        &ctx(root.path(), CancellationToken::new()),
        Duration::from_secs(10),
        GateKind::Stop,
    )
    .await;
    assert!(
        matches!(&outcome, HookRunOutcome::Failed { reason } if reason.contains("exit code 7")),
        "expected failed with exit code, got {outcome:?}"
    );
}

#[tokio::test]
async fn missing_command_reports_spawn_failed() {
    let root = tempfile::tempdir().unwrap();
    let hook = spec(
        "missing",
        ExtensionEventKind::Stop,
        "no-such-executable-xyz",
        root.path(),
    );
    let outcome = run_hook(
        &hook,
        &envelope(root.path()),
        &ctx(root.path(), CancellationToken::new()),
        Duration::from_secs(10),
        GateKind::Observe,
    )
    .await;
    assert!(
        matches!(outcome, HookRunOutcome::SpawnFailed { .. }),
        "expected spawn failure, got {outcome:?}"
    );
}

#[tokio::test]
async fn relative_command_resolves_against_extension_dir() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("hook.sh");
    std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    let hook = spec("rel", ExtensionEventKind::Stop, "hook.sh", root.path());
    let outcome = run_hook(
        &hook,
        &envelope(root.path()),
        &ctx(root.path(), CancellationToken::new()),
        Duration::from_secs(10),
        GateKind::Observe,
    )
    .await;
    assert_eq!(outcome, HookRunOutcome::Success);
}

#[tokio::test]
async fn host_shutdown_cancels_in_flight_hook_and_drains() {
    let root = tempfile::tempdir().unwrap();
    let ext_dir = root.path().join("hooks-ext");
    std::fs::create_dir_all(&ext_dir).unwrap();
    std::fs::write(
        ext_dir.join("extension.json"),
        serde_json::json!({
            "name": "Slow Hooks",
            "version": "0.1.0",
            "hooks": [
                {"name": "slow-observer", "event": "post_tool_use", "command": "sleep 30"}
            ]
        })
        .to_string(),
    )
    .unwrap();
    let trust = Arc::new(InMemoryTrustStore::new());
    trust.trust(ext_dir.clone());
    let (host, errors) = ExtensionHost::new(ExtensionHostOptions {
        project_dirs: vec![root.path().to_path_buf()],
        trust_store: trust,
        ..Default::default()
    });
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    let (handle, task) = host.clone().start().unwrap();

    let event = ExtensionEvent::new(
        ExtensionEventKind::PostToolUse,
        "s1",
        root.path().to_string_lossy(),
        "t",
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
    handle.submit_event(event).unwrap();
    // 等 dispatch 进入 hook（sleep 30 开始跑）。
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = std::time::Instant::now();
    handle.shutdown("test shutdown");
    let exit = tokio::time::timeout(Duration::from_secs(10), task.join())
        .await
        .expect("shutdown must cancel the in-flight hook and join promptly");
    assert_eq!(exit.reason, ShutdownReason::Manual);
    assert_eq!(
        exit.handled_events, 1,
        "event is handled (hook cancelled, not dropped)"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "in-flight hook must be cancelled at shutdown, not awaited to completion"
    );
    let diagnostics = host.diagnostics();
    let messages: Vec<_> = diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(
        messages.iter().any(|m| m.contains("slow-observer")),
        "the cancelled hook run is still recorded: {messages:?}"
    );
}
