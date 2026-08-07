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
        budget: None,
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
async fn shell_relative_command_with_args_runs_in_extension_dir() {
    // 带参数的相对命令（shell 路由）：第一 token 必须相对扩展目录解析。
    // 修复前在 workspace_root 下找 bin/tool.sh → 127 失败。
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("tool.sh");
    std::fs::write(&script, "#!/bin/sh\n[ \"$1\" = \"--flag\" ]\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    let hook = spec(
        "rel-args",
        ExtensionEventKind::PreToolUse,
        "bin/tool.sh --flag",
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
        "bin/tool.sh --flag must execute inside the extension dir"
    );
}

#[tokio::test]
async fn shell_path_command_with_args_is_not_rewritten() {
    // PATH 命令 + 参数：第一 token 不是相对路径 → 不替换，shell 正常执行。
    let root = tempfile::tempdir().unwrap();
    let hook = spec(
        "path-cmd",
        ExtensionEventKind::PreToolUse,
        "sh -c 'true'",
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
    assert_eq!(outcome, HookRunOutcome::Success);
}

#[tokio::test]
async fn shell_absolute_command_with_args_is_not_rewritten() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("abs.sh");
    std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    let command = format!("{} --flag", script.to_string_lossy());
    let hook = spec(
        "abs-cmd",
        ExtensionEventKind::PreToolUse,
        &command,
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
    assert_eq!(outcome, HookRunOutcome::Success);
}

#[tokio::test]
async fn shell_tilde_and_env_commands_are_not_rewritten() {
    // `$VAR` / `~` 开头的命令不替换（shell 负责展开）。
    let root = tempfile::tempdir().unwrap();
    for command in ["$SHELL -c 'exit 0'", "echo hi"] {
        let hook = spec(
            "no-rewrite",
            ExtensionEventKind::PreToolUse,
            command,
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
            "{command:?} must run via the shell without rewriting"
        );
    }
}

#[tokio::test]
async fn flooded_tool_output_does_not_drive_deny() {
    // 洪泛（超预算）且截断尾部含完整 deny JSON → OutputLimited（fail-open），
    // 截断输出不产生 deny 决策。
    let root = tempfile::tempdir().unwrap();
    let hook = spec(
        "flood-deny",
        ExtensionEventKind::PreToolUse,
        "(head -c 200000 /dev/zero | tr '\\0' x; echo '{\"decision\":\"deny\",\"reason\":\"flood\"}')",
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
        HookRunOutcome::OutputLimited,
        "truncated stdout must not yield a deny decision"
    );
}

#[tokio::test]
async fn flooded_stop_output_produces_no_signals() {
    let root = tempfile::tempdir().unwrap();
    let hook = spec(
        "flood-block",
        ExtensionEventKind::Stop,
        "(head -c 200000 /dev/zero | tr '\\0' x; echo '{\"decision\":\"block\",\"reason\":\"flood\"}')",
        root.path(),
    );
    let outcome = run_hook(
        &hook,
        &envelope(root.path()),
        &ctx(root.path(), CancellationToken::new()),
        Duration::from_secs(10),
        GateKind::Stop,
    )
    .await;
    assert_eq!(
        outcome,
        HookRunOutcome::OutputLimited,
        "truncated stdout must not produce stop signals"
    );
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

/// 收集 hook 运行的 sink（按 code 计数）。
#[derive(Debug, Clone, Default)]
struct CountingSink {
    hook_runs: Arc<std::sync::atomic::AtomicUsize>,
    records: Arc<std::sync::Mutex<Vec<String>>>,
}

impl DiagnosticSink for CountingSink {
    fn emit(&self, record: DiagnosticRecord) {
        self.records
            .lock()
            .unwrap()
            .push(format!("{}-{}", record.code, record.message));
        if record.code == "hook_run" {
            self.hook_runs
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// ARC-730：untrusted hook 不执行（trust 三态：Trusted 执行 / Untrusted
/// 与 NotDecided 不执行，NotDecided 产首次启用请求）。
#[tokio::test]
async fn untrusted_and_pending_extensions_never_run_hooks() {
    let root = tempfile::tempdir().unwrap();
    let extensions_root = root.path().join("extensions");
    for (id, hooks) in [
        (
            "trusted-ext",
            serde_json::json!([{"name": "trusted-hook", "event": "post_tool_use", "command": "exit 0"}]),
        ),
        (
            "untrusted-ext",
            serde_json::json!([{"name": "untrusted-hook", "event": "post_tool_use", "command": "exit 0"}]),
        ),
        (
            "pending-ext",
            serde_json::json!([{"name": "pending-hook", "event": "post_tool_use", "command": "exit 0"}]),
        ),
    ] {
        let dir = extensions_root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("extension.json"),
            serde_json::json!({
                "name": id,
                "version": "0.1.0",
                "hooks": hooks
            })
            .to_string(),
        )
        .unwrap();
    }
    let trust = Arc::new(InMemoryTrustStore::new());
    trust.trust(extensions_root.join("trusted-ext"));
    trust.distrust(extensions_root.join("untrusted-ext"));
    let sink = Arc::new(CountingSink::default());
    let (host, errors) = ExtensionHost::new(ExtensionHostOptions {
        project_dirs: vec![extensions_root],
        trust_store: trust,
        diagnostics: Some(sink.clone()),
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
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while sink.hook_runs.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "trusted hook must run"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        sink.hook_runs.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "exactly one hook run: only the trusted extension is enabled"
    );
    // Untrusted → extension_untrusted 诊断；NotDecided → 首次启用请求。
    let messages: Vec<_> = sink.records.lock().unwrap().clone();
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("extension_untrusted-")),
        "untrusted extension is diagnosed: {messages:?}"
    );
    let info = host.info();
    let requests = info.first_enables();
    assert_eq!(
        requests.len(),
        1,
        "pending extension yields an enable request"
    );
    assert_eq!(requests[0].extension_id, "pending-ext");

    handle.shutdown("test shutdown");
    let _ = tokio::time::timeout(Duration::from_secs(10), task.join())
        .await
        .expect("host joins promptly");
}

// ---- manifest config 生效（P2）：per-extension enabled / budget ----

fn write_extension(
    dir: &std::path::Path,
    id: &str,
    config: serde_json::Value,
    hooks: serde_json::Value,
) {
    let ext_dir = dir.join(id);
    std::fs::create_dir_all(&ext_dir).unwrap();
    std::fs::write(
        ext_dir.join("extension.json"),
        serde_json::json!({
            "name": id,
            "version": "0.1.0",
            "config": config,
            "hooks": hooks
        })
        .to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn manifest_disabled_extension_is_not_enabled_even_when_trusted() {
    let root = tempfile::tempdir().unwrap();
    write_extension(
        root.path(),
        "off-ext",
        serde_json::json!({"enabled": false}),
        serde_json::json!([{"name": "h", "event": "post_tool_use", "command": "exit 0"}]),
    );
    let trust = Arc::new(InMemoryTrustStore::new());
    trust.trust(root.path().join("off-ext"));
    let (host, errors) = ExtensionHost::new(ExtensionHostOptions {
        project_dirs: vec![root.path().to_path_buf()],
        trust_store: trust,
        ..Default::default()
    });
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    assert!(
        host.info().enabled().is_empty(),
        "manifest config.enabled=false must disable even a trusted extension"
    );
    assert!(
        host.gate().is_none(),
        "no registry for a disabled extension"
    );
    let diags = host.diagnostics();
    let codes: Vec<_> = diags.iter().map(|d| d.code.as_str()).collect();
    assert!(
        codes.contains(&"extension_disabled"),
        "disabled extension must be diagnosed: {codes:?}"
    );
}

#[tokio::test]
async fn manifest_budget_overrides_global_for_hook_timeout() {
    // manifest budget maxRunSecs=2 vs 全局默认 3600：hook（sleep 30）必须在
    // ~2s 被 per-extension 预算杀死（TimedOut，fail-open → Allow），证明
    // manifest 覆盖全局预算。
    let root = tempfile::tempdir().unwrap();
    write_extension(
        root.path(),
        "budget-ext",
        serde_json::json!({"budget": {"maxRunSecs": 2}}),
        serde_json::json!([{"name": "slow", "event": "pre_tool_use", "command": "sleep 30"}]),
    );
    let trust = Arc::new(InMemoryTrustStore::new());
    trust.trust(root.path().join("budget-ext"));
    let (host, errors) = ExtensionHost::new(ExtensionHostOptions {
        project_dirs: vec![root.path().to_path_buf()],
        trust_store: trust,
        ..Default::default()
    });
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    let gate = host.gate().expect("budgeted extension has hooks");

    let event = ExtensionEvent::new(
        ExtensionEventKind::PreToolUse,
        "s1",
        root.path().to_string_lossy(),
        "t",
        ExtensionEventPayload::PreToolUse {
            tool_name: ToolId::new("bash").unwrap(),
            tool_input: json!({"command": "ls"}),
            tool_input_truncated: false,
            path: None,
        },
    );
    let started = std::time::Instant::now();
    let decision = gate.evaluate_tool(&event).await;
    assert_eq!(
        decision,
        ToolGateDecision::Allow,
        "timeout fails open for the Tool gate"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "per-extension budget (2s) must cap the timeout, not the global 3600s"
    );
    let messages: Vec<_> = host
        .diagnostics()
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        messages.iter().any(|m| m.contains("TimedOut")),
        "hook run must record the timeout outcome: {messages:?}"
    );
}

#[tokio::test]
async fn manifest_without_config_behaves_unchanged() {
    // manifest 无 config：per-extension 配置 == 全局默认 → 正常启用执行。
    let root = tempfile::tempdir().unwrap();
    write_extension(
        root.path(),
        "plain-ext",
        serde_json::json!({}),
        serde_json::json!([{"name": "h", "event": "pre_tool_use", "command": "exit 0"}]),
    );
    let trust = Arc::new(InMemoryTrustStore::new());
    trust.trust(root.path().join("plain-ext"));
    let (host, errors) = ExtensionHost::new(ExtensionHostOptions {
        project_dirs: vec![root.path().to_path_buf()],
        trust_store: trust,
        ..Default::default()
    });
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    assert_eq!(host.info().enabled().len(), 1, "plain manifest enables");
    let gate = host.gate().expect("plain extension has hooks");
    let event = ExtensionEvent::new(
        ExtensionEventKind::PreToolUse,
        "s1",
        root.path().to_string_lossy(),
        "t",
        ExtensionEventPayload::PreToolUse {
            tool_name: ToolId::new("bash").unwrap(),
            tool_input: json!({}),
            tool_input_truncated: false,
            path: None,
        },
    );
    assert_eq!(gate.evaluate_tool(&event).await, ToolGateDecision::Allow);
    let diags = host.diagnostics();
    let codes: Vec<_> = diags.iter().map(|d| d.code.as_str()).collect();
    assert!(!codes.contains(&"extension_disabled"), "got {codes:?}");
}

#[tokio::test]
async fn config_layers_override_manifest_enabled_with_and_semantics() {
    // 外部层禁用（高层优先）+ manifest enabled=true → 不启用（AND）。
    let root = tempfile::tempdir().unwrap();
    write_extension(
        root.path(),
        "ext",
        serde_json::json!({"enabled": true}),
        serde_json::json!([{"name": "h", "event": "post_tool_use", "command": "exit 0"}]),
    );
    let trust = Arc::new(InMemoryTrustStore::new());
    trust.trust(root.path().join("ext"));
    let layer = ExtensionConfigLayer::new(
        ExtensionSource::Managed,
        "managed",
        ExtensionConfig {
            enabled: false,
            ..Default::default()
        },
    );
    let (host, errors) = ExtensionHost::new(ExtensionHostOptions {
        project_dirs: vec![root.path().to_path_buf()],
        trust_store: trust,
        config_layers: vec![layer],
        ..Default::default()
    });
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    assert!(
        host.info().enabled().is_empty(),
        "higher-priority config layer must disable the extension"
    );
    let diags = host.diagnostics();
    let codes: Vec<_> = diags.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"extension_disabled"), "got {codes:?}");
}
