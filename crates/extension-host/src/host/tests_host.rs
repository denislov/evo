use super::*;
use crate::budget::BudgetKind;
use crate::event::{ExtensionEventKind, ExtensionEventPayload, SubagentStopPhase};
use crate::trust::{CapabilityRisk, InMemoryTrustStore};
use serde_json::json;
use std::time::Duration;
use tool_contract::api::definition::ToolId;

fn manifest_dir(root: &std::path::Path, id: &str, name: &str) -> std::path::PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
            dir.join("extension.json"),
            serde_json::json!({
                "name": name,
                "version": "0.1.0",
                "capabilities": [{"name": "lint", "description": "run linters", "risk": "process_execution"}],
                "config": {"enabled": true}
            })
            .to_string(),
        )
        .unwrap();
    dir
}

fn options_with(root: &std::path::Path, trust: Arc<InMemoryTrustStore>) -> ExtensionHostOptions {
    ExtensionHostOptions {
        global_dirs: vec![root.to_path_buf()],
        project_dirs: Vec::new(),
        config_layers: Vec::new(),
        trust_store: trust,
        ..Default::default()
    }
}

fn event(kind: ExtensionEventKind, session: &str) -> ExtensionEvent {
    ExtensionEvent::new(
        kind,
        session,
        "/ws",
        "2026-08-06T00:00:00Z",
        ExtensionEventPayload::PreToolUse {
            tool_name: ToolId::new("read_file").unwrap(),
            tool_input: json!({}),
            tool_input_truncated: false,
        },
    )
}

#[tokio::test]
async fn new_host_discovers_and_filters_by_trust() {
    let root = tempfile::tempdir().unwrap();
    manifest_dir(root.path(), "trusted-ext", "Trusted");
    manifest_dir(root.path(), "pending-ext", "Pending");

    let trust = Arc::new(InMemoryTrustStore::new());
    trust.trust(root.path().join("trusted-ext"));
    let (host, errors) = ExtensionHost::new(options_with(root.path(), trust.clone()));
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");

    let info = host.info();
    assert_eq!(info.records().len(), 2);
    assert_eq!(info.enabled().len(), 1, "only trusted extension enabled");
    assert_eq!(info.enabled()[0].id, "trusted-ext");
    assert_eq!(info.first_enables().len(), 1);
    assert_eq!(info.first_enables()[0].extension_id, "pending-ext");
    assert_eq!(info.first_enables()[0].capabilities.len(), 1);
    assert_eq!(
        info.first_enables()[0].capabilities[0].risk,
        CapabilityRisk::ProcessExecution
    );
    // untrusted 有诊断，pending 有 info 诊断。
    let diags = host.diagnostics();
    let codes: Vec<_> = diags.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"extension_first_enable"), "got {codes:?}");
}

#[tokio::test]
async fn new_host_marks_untrusted_extensions() {
    let root = tempfile::tempdir().unwrap();
    manifest_dir(root.path(), "bad-ext", "Bad");
    let trust = Arc::new(InMemoryTrustStore::new());
    trust.distrust(root.path().join("bad-ext"));
    let (host, errors) = ExtensionHost::new(options_with(root.path(), trust));
    assert!(errors.is_empty());
    let info = host.info();
    assert!(info.enabled().is_empty());
    assert!(info.first_enables().is_empty());
    let diags = host.diagnostics();
    let codes: Vec<_> = diags.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"extension_untrusted"), "got {codes:?}");
}

#[tokio::test]
async fn new_host_merges_config_layers() {
    let root = tempfile::tempdir().unwrap();
    manifest_dir(root.path(), "ext", "Ext");
    let trust = Arc::new(InMemoryTrustStore::new());
    trust.trust(root.path().join("ext"));

    let global_layer = crate::config::ExtensionConfigLayer::new(
        ExtensionSource::Global,
        "global",
        ExtensionConfig {
            enabled: true,
            ..Default::default()
        },
    );
    let managed_layer = crate::config::ExtensionConfigLayer::new(
        ExtensionSource::Managed,
        "managed",
        ExtensionConfig {
            enabled: false, // 任何层可禁用
            budget: ExtensionBudget {
                max_calls_per_session: 4,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let mut options = options_with(root.path(), trust);
    options.config_layers = vec![managed_layer, global_layer];
    let (host, _) = ExtensionHost::new(options);
    let info = host.info();
    let config = info.config();
    assert!(!config.enabled);
    assert_eq!(config.budget.max_calls_per_session, 4);
}

#[tokio::test]
async fn start_submit_shutdown_join_happy_path() {
    let root = tempfile::tempdir().unwrap();
    manifest_dir(root.path(), "ext", "Ext");
    let trust = Arc::new(InMemoryTrustStore::new());
    trust.trust(root.path().join("ext"));
    let (host, _) = ExtensionHost::new(options_with(root.path(), trust));
    let (handle, task) = host.clone().start().unwrap();

    for i in 0..5 {
        let mut ev = event(ExtensionEventKind::PreToolUse, "s1");
        ev.payload = ExtensionEventPayload::PostToolUse {
            tool_name: ToolId::new("read_file").unwrap(),
            tool_input: json!({}),
            tool_result: json!({"i": i}),
            tool_input_truncated: false,
            tool_result_truncated: false,
            duration_ms: None,
        };
        handle.submit_event(ev).unwrap();
    }
    handle.shutdown("test done");
    assert!(!handle.is_running());

    let exit = task.join().await;
    assert_eq!(exit.reason, ShutdownReason::Manual);
    assert_eq!(exit.handled_events, 5, "all submitted events handled");
    assert!(!exit.panicked);

    let diags = host.diagnostics();
    let codes: Vec<_> = diags.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"host_shutdown"), "got {codes:?}");
}

#[tokio::test]
async fn shutdown_rejects_new_events_but_keeps_queued() {
    let root = tempfile::tempdir().unwrap();
    let trust = Arc::new(InMemoryTrustStore::new());
    let (host, _) = ExtensionHost::new(options_with(root.path(), trust));
    let (handle, task) = host.clone().start().unwrap();

    for _ in 0..3 {
        handle
            .submit_event(event(ExtensionEventKind::PreToolUse, "s1"))
            .unwrap();
    }
    handle.shutdown("now");
    let err = handle
        .submit_event(event(ExtensionEventKind::PreToolUse, "s1"))
        .unwrap_err();
    assert!(matches!(err, ExtensionError::ShuttingDown { .. }));

    let exit = task.join().await;
    assert_eq!(exit.reason, ShutdownReason::Manual);
    assert_eq!(exit.handled_events, 3, "queued events drained before exit");
}

#[tokio::test]
async fn repeated_shutdown_is_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let trust = Arc::new(InMemoryTrustStore::new());
    let (host, _) = ExtensionHost::new(options_with(root.path(), trust));
    let (handle, task) = host.clone().start().unwrap();
    handle.shutdown("one");
    handle.shutdown("two"); // 幂等
    let exit = task.join().await;
    assert_eq!(exit.reason, ShutdownReason::Manual);
}

#[tokio::test]
async fn dropping_all_handles_stops_the_task() {
    let root = tempfile::tempdir().unwrap();
    let trust = Arc::new(InMemoryTrustStore::new());
    let (host, _) = ExtensionHost::new(options_with(root.path(), trust));
    let (handle, task) = host.clone().start().unwrap();
    handle
        .submit_event(event(ExtensionEventKind::PreToolUse, "s1"))
        .unwrap();
    drop(handle); // 唯一 sender 退出 → channel 关闭。
    let exit = task.join().await;
    assert_eq!(exit.reason, ShutdownReason::SendersDropped);
    assert_eq!(exit.handled_events, 1);
    assert_eq!(host.state(), HostState::Stopped);
}

#[tokio::test]
async fn cannot_start_twice() {
    let root = tempfile::tempdir().unwrap();
    let trust = Arc::new(InMemoryTrustStore::new());
    let (host, _) = ExtensionHost::new(options_with(root.path(), trust));
    let (handle, task) = host.clone().start().unwrap();
    assert!(host.clone().start().is_err());
    handle.shutdown("bye");
    task.join().await;
}

#[tokio::test]
async fn cannot_submit_when_idle_or_after_stop() {
    let root = tempfile::tempdir().unwrap();
    let trust = Arc::new(InMemoryTrustStore::new());
    let (host, _) = ExtensionHost::new(options_with(root.path(), trust));
    // Idle：无 handle，submit 不可达（API 上 handle 仅由 start 产生）。
    // Stopped：join 后 submit 被拒。
    let (handle, task) = host.clone().start().unwrap();
    handle.shutdown("x");
    let _ = task.join().await;
    assert!(
        handle
            .submit_event(event(ExtensionEventKind::PreToolUse, "s1"))
            .is_err()
    );
}

#[tokio::test]
async fn unsupported_version_is_rejected_before_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let trust = Arc::new(InMemoryTrustStore::new());
    let (host, _) = ExtensionHost::new(options_with(root.path(), trust));
    let (handle, task) = host.clone().start().unwrap();
    let mut ev = event(ExtensionEventKind::PreToolUse, "s1");
    ev.version = 99;
    let err = handle.submit_event(ev).unwrap_err();
    assert!(matches!(
        err,
        ExtensionError::UnsupportedVersion {
            version: 99,
            supported: 1
        }
    ));
    handle.shutdown("x");
    task.join().await;
}

#[tokio::test]
async fn budget_exceeded_drops_event_with_diagnostic() {
    let root = tempfile::tempdir().unwrap();
    let trust = Arc::new(InMemoryTrustStore::new());
    let mut options = options_with(root.path(), trust);
    options.budget = ExtensionBudget {
        max_calls_per_session: 2,
        ..Default::default()
    };
    let (host, _) = ExtensionHost::new(options);
    let (handle, task) = host.clone().start().unwrap();
    for _ in 0..3 {
        handle
            .submit_event(event(ExtensionEventKind::PreToolUse, "s1"))
            .unwrap();
    }
    handle.shutdown("budget test");
    let exit = task.join().await;
    // 事件都被记账处理（骨架不派发）；第 3 个事件触发 budget 诊断。
    assert_eq!(exit.handled_events, 3);
    let diags = host.diagnostics();
    let codes: Vec<_> = diags.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"budget_exceeded"), "got {codes:?}");
    let exceeded: Vec<_> = diags
        .iter()
        .filter(|d| d.code == "budget_exceeded")
        .collect();
    assert_eq!(exceeded.len(), 1);
    assert!(exceeded[0].message.contains("call_count"));
}

#[tokio::test]
async fn output_bytes_budget_is_checked() {
    let root = tempfile::tempdir().unwrap();
    let trust = Arc::new(InMemoryTrustStore::new());
    let mut options = options_with(root.path(), trust);
    options.budget = ExtensionBudget {
        max_calls_per_session: 0,         // 不限次数
        max_output_bytes_per_session: 10, // 小预算
        ..Default::default()
    };
    let (host, _) = ExtensionHost::new(options);
    let (handle, task) = host.clone().start().unwrap();
    // 每个事件 payload 序列化 > 10 字节 → 第一次记账即超限（诊断、不 panic）。
    for _ in 0..2 {
        handle
            .submit_event(event(ExtensionEventKind::PreToolUse, "s1"))
            .unwrap();
    }
    handle.shutdown("bytes");
    let exit = task.join().await;
    assert_eq!(
        exit.handled_events, 2,
        "byte budget failures do not panic dispatch"
    );
    let diags = host.diagnostics();
    let codes: Vec<_> = diags.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"budget_exceeded"), "got {codes:?}");
    assert!(
        host.diagnostics()
            .iter()
            .any(|d| d.message.contains("output_bytes"))
    );
}

#[tokio::test]
async fn dispatch_panic_fails_closed_without_propagating() {
    // 直接驱动 dispatch_loop 的 panic 路径：task 内部 panic 被捕获，
    // join 正常返回，不向调用方传播 panic。
    let shared = Arc::new(HostShared {
        state: Mutex::new(HostState::Running),
        shutdown_tx: Mutex::new(None),
        collector: Mutex::new(DiagnosticsCollector::new(None, 16)),
        budget: Mutex::new(BudgetTracker::new(ExtensionBudget::default())),
    });
    let (tx, rx) = mpsc::channel(4);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    tx.try_send(event(ExtensionEventKind::PreToolUse, "s1"))
        .unwrap();
    tx.try_send(event(ExtensionEventKind::PreToolUse, "s1"))
        .unwrap();
    drop(tx);

    let task = tokio::spawn(dispatch_loop(
        rx,
        shutdown_rx,
        shared.clone(),
        |_shared, _ev| panic!("boom"),
    ));
    let exit = task.await.unwrap();
    assert_eq!(exit.reason, ShutdownReason::Panic);
    assert!(exit.panicked);
    assert_eq!(exit.handled_events, 0);
    let diagnostics = shared.collector.lock().unwrap().snapshot();
    assert!(diagnostics.iter().any(|d| d.code == "dispatch_panic"));
}

#[tokio::test]
async fn panic_after_partial_handling_reports_count() {
    let shared = Arc::new(HostShared {
        state: Mutex::new(HostState::Running),
        shutdown_tx: Mutex::new(None),
        collector: Mutex::new(DiagnosticsCollector::new(None, 16)),
        budget: Mutex::new(BudgetTracker::new(ExtensionBudget::default())),
    });
    let (tx, rx) = mpsc::channel(8);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut count = 0;
    tx.try_send(event(ExtensionEventKind::PreToolUse, "s1"))
        .unwrap();
    tx.try_send(event(ExtensionEventKind::PreToolUse, "s1"))
        .unwrap();
    drop(tx);
    let task = tokio::spawn(dispatch_loop(
        rx,
        shutdown_rx,
        shared.clone(),
        move |_s, _e| {
            count += 1;
            if count == 2 {
                panic!("boom on second");
            }
        },
    ));
    let exit = task.await.unwrap();
    assert!(exit.panicked);
    assert_eq!(exit.handled_events, 1);
}

#[tokio::test]
async fn shutdown_signal_after_queued_events_drains_in_order() {
    let root = tempfile::tempdir().unwrap();
    let trust = Arc::new(InMemoryTrustStore::new());
    let (host, _) = ExtensionHost::new(options_with(root.path(), trust));
    let (handle, task) = host.clone().start().unwrap();

    // 提交事件后立刻 shutdown：select 可能先收到事件也可能先收到 shutdown，
    // 但 drain 阶段保证全部已入队事件被处理。
    for _ in 0..8 {
        handle
            .submit_event(event(ExtensionEventKind::PreToolUse, "s1"))
            .unwrap();
    }
    handle.shutdown("drain");
    let exit = task.join().await;
    assert_eq!(exit.handled_events, 8, "drain must not lose queued events");
    assert_eq!(exit.reason, ShutdownReason::Manual);
}

#[tokio::test]
async fn submit_is_rejected_after_stop_with_timeout_guard() {
    let root = tempfile::tempdir().unwrap();
    let trust = Arc::new(InMemoryTrustStore::new());
    let (host, _) = ExtensionHost::new(options_with(root.path(), trust));
    let (handle, task) = host.clone().start().unwrap();
    handle.shutdown("stop");
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("join must complete promptly");
    assert!(
        handle
            .submit_event(event(ExtensionEventKind::PreToolUse, "s1"))
            .is_err()
    );
}

#[test]
fn budget_kind_as_str_matches_serialization() {
    for kind in [
        BudgetKind::CallCount,
        BudgetKind::OutputBytes,
        BudgetKind::RunDurationSecs,
        BudgetKind::ConcurrentExtensions,
    ] {
        assert_eq!(
            serde_json::to_value(kind).unwrap(),
            serde_json::Value::from(kind.as_str()),
            "{kind:?}"
        );
    }
}

#[test]
fn first_enable_flow_dto_shape() {
    // 端到端形状：NotDecided → EnableRequest 携带来源 + 能力 + 预算。
    let root = tempfile::tempdir().unwrap();
    manifest_dir(root.path(), "pending", "Pending");
    let trust = Arc::new(InMemoryTrustStore::new());
    let (host, errors) = ExtensionHost::new(options_with(root.path(), trust));
    assert!(errors.is_empty());
    let info = host.info();
    let requests = info.first_enables();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.source, ExtensionSource::Global);
    assert_eq!(request.capabilities[0].name, "lint");
    let json = serde_json::to_value(request).unwrap();
    assert_eq!(json["source"], "global");
    assert!(json.get("sourceDir").is_some());
}

#[tokio::test]
async fn subagent_stop_phase_round_trip() {
    let payload = ExtensionEventPayload::SubagentStop {
        subagent_type: "explore".into(),
        phase: SubagentStopPhase::Observe,
        stop_reason: None,
    };
    let value = serde_json::to_value(&payload).unwrap();
    assert_eq!(value["phase"], "observe");
    let back: ExtensionEventPayload = serde_json::from_value(value).unwrap();
    assert_eq!(back, payload);
}
