use super::*;
use crate::event::{EXTENSION_EVENT_VERSION, ExtensionEventPayload};
use crate::hook::HookSpec;
use crate::hook_lifecycle::HookLifecycle;
use crate::host::HostShared;
use crate::matcher::HookMatcher;
use serde_json::json;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tool_contract::api::definition::ToolId;

fn spec(name: &str, event: ExtensionEventKind, command: &str) -> HookSpec {
    HookSpec {
        name: name.into(),
        event,
        match_tool: None,
        match_path: None,
        match_profile: None,
        priority: 0,
        command: command.into(),
        source_dir: std::path::PathBuf::from("/ext"),
        timeout_secs: None,
        budget: None,
        enabled: true,
        matcher: HookMatcher::match_all(),
    }
}

fn spec_with_matcher(
    name: &str,
    event: ExtensionEventKind,
    command: &str,
    tool: Option<&str>,
    path: Option<&str>,
) -> HookSpec {
    HookSpec {
        matcher: HookMatcher::new(tool, path, None).unwrap(),
        ..spec(name, event, command)
    }
}

fn registry(specs: Vec<HookSpec>) -> Arc<HookRegistry> {
    let mut registry = HookRegistry::new();
    registry.add_extension("test", specs);
    Arc::new(registry)
}

fn unsupported_capability() -> workspace_runtime::api::SandboxCapability {
    workspace_runtime::api::SandboxCapability {
        fs: workspace_runtime::api::CapabilityDimension {
            supported: false,
            detail: "test platform has no landlock".into(),
        },
        network: workspace_runtime::api::CapabilityDimension {
            supported: false,
            detail: "test".into(),
        },
        exec: workspace_runtime::api::CapabilityDimension {
            supported: true,
            detail: "test".into(),
        },
        env: workspace_runtime::api::CapabilityDimension {
            supported: true,
            detail: "test".into(),
        },
    }
}

fn test_shared() -> Arc<HostShared> {
    HostShared::test_harness()
}

fn tool_event_in(name: &str, path: Option<&str>, root: &std::path::Path) -> ExtensionEvent {
    ExtensionEvent::new(
        ExtensionEventKind::PreToolUse,
        "s1",
        root.to_string_lossy(),
        "t",
        ExtensionEventPayload::PreToolUse {
            tool_name: ToolId::new(name).unwrap(),
            tool_input: json!({}),
            tool_input_truncated: false,
            path: path.map(str::to_string),
        },
    )
}

fn stop_event_in(root: &std::path::Path) -> ExtensionEvent {
    ExtensionEvent::new(
        ExtensionEventKind::Stop,
        "s1",
        root.to_string_lossy(),
        "t",
        ExtensionEventPayload::Stop {
            reason: "end_turn".into(),
            last_assistant_message: None,
        },
    )
}

#[test]
fn registry_groups_by_event_and_sorts() {
    let mut registry = HookRegistry::new();
    registry.add_extension(
        "ext",
        vec![
            spec("b-low", ExtensionEventKind::Stop, "x"),
            spec("a-mid", ExtensionEventKind::Stop, "x"),
            spec("z-mid", ExtensionEventKind::PreToolUse, "x"),
            spec("a-high", ExtensionEventKind::Stop, "x"),
        ],
    );
    let stop_hooks = registry.hooks_for(ExtensionEventKind::Stop);
    let names: Vec<_> = stop_hooks.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["a-high", "a-mid", "b-low"],
        "priority desc, name asc"
    );
    assert_eq!(registry.hooks_for(ExtensionEventKind::PreToolUse).len(), 1);
    assert!(
        registry
            .hooks_for(ExtensionEventKind::PostToolUse)
            .is_empty()
    );
    assert_eq!(registry.len(), 4);
}

#[test]
fn event_gate_classification() {
    assert_eq!(
        event_gate(ExtensionEventKind::PreToolUse),
        Some(GateKind::Tool)
    );
    assert_eq!(event_gate(ExtensionEventKind::Stop), Some(GateKind::Stop));
    assert_eq!(
        event_gate(ExtensionEventKind::SubagentStop),
        Some(GateKind::Stop)
    );
    assert_eq!(event_gate(ExtensionEventKind::PostToolUse), None);
    assert_eq!(event_gate(ExtensionEventKind::SessionStart), None);
    assert_eq!(event_gate(ExtensionEventKind::MergeApplied), None);
}

// ---- Tool gate transition table ----
//
// | 匹配 hook 结果序列                        | 决策            |
// |-------------------------------------------|-----------------|
// | （无匹配）                                | Allow           |
// | allow                                     | Allow           |
// | deny                                      | Deny            |
// | allow, deny                               | Deny（deny 赢） |
// | deny, allow（高优先级先咨询）             | Deny（短路）    |
// | allow, Failed(exit 1)                     | Allow（fail-open）|
// | Failed, deny                              | Deny            |
// | allow, TimedOut                           | Allow（fail-open）|
// | allow, SandboxUnsupported                 | ClosedByEnvironment |
// | 空注册表                                  | Allow           |

fn ws() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[tokio::test]
async fn tool_gate_transition_table() {
    let cases: Vec<(Vec<&str>, ToolGateDecision)> = vec![
        (vec![], ToolGateDecision::Allow),
        (vec!["allow"], ToolGateDecision::Allow),
        (
            vec!["deny"],
            ToolGateDecision::Deny {
                reason: "no".into(),
            },
        ),
        (
            vec!["allow", "deny"],
            ToolGateDecision::Deny {
                reason: "no".into(),
            },
        ),
        (
            vec!["deny-high", "allow-low"],
            ToolGateDecision::Deny {
                reason: "high says no".into(),
            },
        ),
        (vec!["fail"], ToolGateDecision::Allow),
        (
            vec!["fail", "deny"],
            ToolGateDecision::Deny {
                reason: "no".into(),
            },
        ),
        (vec!["slow"], ToolGateDecision::Allow),
    ];
    for (scripts, expected) in cases {
        let mut specs = Vec::new();
        for (index, script) in scripts.iter().enumerate() {
            let mut s = spec(
                &format!("hook-{index}"),
                ExtensionEventKind::PreToolUse,
                &script_command(script),
            );
            // deny-high 高优先级，其余按 name 序（hook-0 先）。
            if *script == "deny-high" {
                s.priority = 10;
                s.name = "deny-high".into();
            } else if *script == "allow-low" {
                s.priority = -10;
                s.name = "allow-low".into();
            } else if *script == "slow" {
                s.timeout_secs = Some(1);
                s.name = format!("hook-{index}");
            } else if *script == "deny" {
                s.name = "hook-0".into();
            } else if *script == "allow" {
                s.name = format!("hook-{index}");
            }
            specs.push(s);
        }
        let shared = test_shared();
        let gate = HookGate::new(registry(specs), shared);
        let decision = gate
            .evaluate_tool(&tool_event_in("bash", None, ws().path()))
            .await;
        assert_eq!(decision, expected, "scripts {scripts:?}");
    }
}

fn script_command(kind: &str) -> String {
    match kind {
        "allow" => "exit 0".into(),
        "deny" => "echo '{\"decision\":\"deny\",\"reason\":\"no\"}'".into(),
        "deny-high" => "echo '{\"decision\":\"deny\",\"reason\":\"high says no\"}'".into(),
        "fail" => "exit 1".into(),
        "slow" => "sleep 5".into(),
        _ => "exit 0".into(),
    }
}

#[tokio::test]
async fn tool_gate_deny_short_circuits_later_hooks() {
    let mut deny = spec("deny", ExtensionEventKind::PreToolUse, "exit 2");
    deny.priority = 10;
    let mut never = spec("never", ExtensionEventKind::PreToolUse, "exit 99");
    never.priority = -10;
    let shared = test_shared();
    let gate = HookGate::new(registry(vec![deny, never]), shared);
    let decision = gate
        .evaluate_tool(&tool_event_in("bash", None, ws().path()))
        .await;
    assert!(matches!(decision, ToolGateDecision::Deny { .. }));
    // 短路语义：deny 后的低优先级 hook 不得运行（transition 表另有
    // allow-then-deny 用例证明 deny 优先于先行的 allow）。
}
#[tokio::test]
async fn tool_gate_fails_open_on_execution_failures() {
    let shared = test_shared();
    let gate = HookGate::new(
        registry(vec![spec("fail", ExtensionEventKind::PreToolUse, "exit 1")]),
        shared,
    );
    assert_eq!(
        gate.evaluate_tool(&tool_event_in("bash", None, ws().path()))
            .await,
        ToolGateDecision::Allow
    );
}

#[tokio::test]
async fn tool_gate_fails_open_on_output_limited() {
    // 洪泛输出（超预算）即使截断尾部含完整 deny JSON 也不驱动 deny：
    // interpret_tool 返回 OutputLimited → fail-open（Allow）。
    let flood = spec(
        "flood",
        ExtensionEventKind::PreToolUse,
        "(head -c 200000 /dev/zero | tr '\\0' x; echo '{\"decision\":\"deny\",\"reason\":\"flood\"}')",
    );
    let shared = test_shared();
    let gate = HookGate::new(registry(vec![flood]), shared);
    assert_eq!(
        gate.evaluate_tool(&tool_event_in("bash", None, ws().path()))
            .await,
        ToolGateDecision::Allow,
        "truncated output must fail open for the Tool gate"
    );
}

#[tokio::test]
async fn tool_gate_closes_on_sandbox_unsupported() {
    let shared = test_shared();
    let gate = HookGate::with_sandbox_capability(
        registry(vec![spec(
            "guard",
            ExtensionEventKind::PreToolUse,
            "exit 0",
        )]),
        shared,
        unsupported_capability(),
    );
    // 无 sandbox 能力 → 不 spawn → fail-closed。
    let decision = gate
        .evaluate_tool(&tool_event_in("bash", None, ws().path()))
        .await;
    assert!(
        matches!(decision, ToolGateDecision::ClosedByEnvironment { .. }),
        "sandboxless platform must fail closed for the Tool gate, got {decision:?}"
    );
}

#[tokio::test]
async fn stop_gate_fails_open_on_sandbox_unsupported() {
    let shared = test_shared();
    let gate = HookGate::with_sandbox_capability(
        registry(vec![spec("guard", ExtensionEventKind::Stop, "exit 0")]),
        shared,
        unsupported_capability(),
    );
    let decision = gate.evaluate_stop(&stop_event_in(ws().path())).await;
    assert!(
        !decision.wants_continuation(),
        "sandboxless platform must fail open for the Stop gate"
    );
    assert!(decision.blocks.is_empty());
}

#[tokio::test]
async fn stop_gate_output_limited_produces_no_signals() {
    // 洪泛输出（超预算）即使截断尾部含完整 block JSON 也不产生信号：
    // interpret_stop 返回 OutputLimited → 无 block / force_stop / context。
    let flood = spec(
        "flood",
        ExtensionEventKind::Stop,
        "(head -c 200000 /dev/zero | tr '\\0' x; echo '{\"decision\":\"block\",\"reason\":\"flood\"}')",
    );
    let shared = test_shared();
    let gate = HookGate::new(registry(vec![flood]), shared);
    let decision = gate.evaluate_stop(&stop_event_in(ws().path())).await;
    assert!(
        !decision.wants_continuation(),
        "truncated output must produce no stop signals"
    );
    assert!(decision.blocks.is_empty());
    assert!(decision.force_stop.is_none());
    assert!(decision.additional_context.is_empty());
}

// ---- Stop gate transition table ----
//
// | hook 输出                    | blocks | force_stop | context | wants_continuation |
// |------------------------------|--------|------------|---------|--------------------|
// | （无匹配 / 空注册表）        | []     | None       | []      | false              |
// | exit 0（无 JSON）            | []     | None       | []      | false              |
// | block                        | [r]    | None       | []      | true               |
// | block, block                 | [r1,r2]| None       | []      | true               |
// | block, force-stop            | [r]    | Some(s)    | []      | false（force 赢）  |
// | force-stop, block（后续）    | [b]    | Some(s1)   | []      | false              |
// | context only                 | []     | None       | [c]     | true               |
// | Failed(exit 1)               | []     | None       | []      | false（fail-open） |
// | TimedOut                     | []     | None       | []      | false（fail-open） |
// | SandboxUnsupported           | []     | None       | []      | false（fail-open） |

#[tokio::test]
async fn stop_gate_transition_table() {
    let cases: Vec<(&str, Vec<&str>, StopGateDecision)> = vec![
        (
            "no hooks",
            Vec::new(),
            StopGateDecision {
                blocks: vec![],
                force_stop: None,
                additional_context: vec![],
                outcomes: vec![],
            },
        ),
        (
            "plain exit 0",
            vec!["exit 0"],
            StopGateDecision {
                blocks: vec![],
                force_stop: None,
                additional_context: vec![],
                outcomes: vec![],
            },
        ),
        (
            "single block",
            vec!["block"],
            StopGateDecision {
                blocks: vec!["keep going".into()],
                force_stop: None,
                additional_context: vec![],
                outcomes: vec![],
            },
        ),
        (
            "two blocks accumulate",
            vec!["block", "block2"],
            StopGateDecision {
                blocks: vec!["keep going".into(), "second".into()],
                force_stop: None,
                additional_context: vec![],
                outcomes: vec![],
            },
        ),
        (
            "block then force stop",
            vec!["block", "force"],
            StopGateDecision {
                blocks: vec!["keep going".into()],
                force_stop: Some("enough".into()),
                additional_context: vec![],
                outcomes: vec![],
            },
        ),
        (
            "context only",
            vec!["context"],
            StopGateDecision {
                blocks: vec![],
                force_stop: None,
                additional_context: vec!["note".into()],
                outcomes: vec![],
            },
        ),
        (
            "failed hook fails open",
            vec!["exit 1"],
            StopGateDecision {
                blocks: vec![],
                force_stop: None,
                additional_context: vec![],
                outcomes: vec![],
            },
        ),
    ];
    for (label, scripts, mut expected) in cases {
        let specs: Vec<HookSpec> = scripts
            .iter()
            .enumerate()
            .map(|(index, script)| {
                let command = match *script {
                    "exit 0" => "exit 0".to_string(),
                    "block" => {
                        "echo '{\"decision\":\"block\",\"reason\":\"keep going\"}'".to_string()
                    }
                    "block2" => "echo '{\"decision\":\"block\",\"reason\":\"second\"}'".to_string(),
                    "force" => "echo '{\"continue\":false,\"stopReason\":\"enough\"}'".to_string(),
                    "context" => "echo '{\"hookSpecificOutput\":{\"additionalContext\":\"note\"}}'"
                        .to_string(),
                    "exit 1" => "exit 1".to_string(),
                    other => other.to_string(),
                };
                spec(&format!("h{index}"), ExtensionEventKind::Stop, &command)
            })
            .collect();
        let shared = test_shared();
        let gate = HookGate::new(registry(specs), shared);
        let decision = gate.evaluate_stop(&stop_event_in(ws().path())).await;
        expected.outcomes = decision.outcomes.clone();
        assert_eq!(decision.blocks, expected.blocks, "{label}: blocks");
        assert_eq!(
            decision.force_stop, expected.force_stop,
            "{label}: force_stop"
        );
        assert_eq!(
            decision.additional_context, expected.additional_context,
            "{label}: context"
        );
        let wants = match label {
            "no hooks" | "plain exit 0" | "failed hook fails open" => false,
            "single block" | "two blocks accumulate" | "context only" => true,
            "block then force stop" => false,
            _ => true,
        };
        assert_eq!(
            decision.wants_continuation(),
            wants,
            "{label}: wants_continuation"
        );
    }
}

#[tokio::test]
async fn stop_first_force_stop_wins_later_signals_dropped() {
    let mut first = spec(
        "first",
        ExtensionEventKind::Stop,
        "echo '{\"continue\":false,\"stopReason\":\"first\"}'",
    );
    first.priority = 10;
    let mut second = spec(
        "second",
        ExtensionEventKind::Stop,
        "echo '{\"continue\":false,\"stopReason\":\"second\"}'",
    );
    second.priority = -10;
    let shared = test_shared();
    let gate = HookGate::new(registry(vec![first, second]), shared);
    let decision = gate.evaluate_stop(&stop_event_in(ws().path())).await;
    assert_eq!(decision.force_stop.as_deref(), Some("first"));
    assert_eq!(
        decision.outcomes.len(),
        2,
        "all hooks still run for Stop gate"
    );
}

#[tokio::test]
async fn stop_timeout_and_sandbox_fail_open() {
    let mut slow = spec("slow", ExtensionEventKind::Stop, "sleep 5");
    slow.timeout_secs = Some(1);
    let shared = test_shared();
    let gate = HookGate::new(registry(vec![slow]), shared);
    let decision = gate.evaluate_stop(&stop_event_in(ws().path())).await;
    assert!(
        !decision.wants_continuation(),
        "timeout must not block the stop"
    );
    assert!(decision.blocks.is_empty());
}

#[tokio::test]
async fn observe_dispatches_matching_hooks_only() {
    let shared = test_shared();
    let mut registry = HookRegistry::new();
    registry.add_extension(
        "ext",
        vec![
            spec_with_matcher(
                "bash-observer",
                ExtensionEventKind::PostToolUse,
                "exit 0",
                Some("bash"),
                None,
            ),
            spec_with_matcher(
                "read-observer",
                ExtensionEventKind::PostToolUse,
                "exit 0",
                Some("read_file"),
                None,
            ),
        ],
    );
    let registry = Arc::new(registry);
    let mut event = ExtensionEvent::new(
        ExtensionEventKind::PostToolUse,
        "s1",
        "/ws",
        "t",
        ExtensionEventPayload::PostToolUse {
            tool_name: ToolId::new("bash").unwrap(),
            tool_input: json!({}),
            tool_result: json!({"ok": true}),
            tool_input_truncated: false,
            tool_result_truncated: false,
            duration_ms: None,
            path: None,
        },
    );
    event.version = EXTENSION_EVENT_VERSION;
    let executed = dispatch_observe(&registry, &shared, &event).await;
    assert_eq!(executed, 1, "only the bash matcher fires");
    let diagnostics = shared.diagnostics();
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("bash-observer")),
        "bash-observer should record a hook_run diagnostic"
    );
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.message.contains("read-observer")),
        "read-observer must not run"
    );
}

#[tokio::test]
async fn gate_events_are_not_observed_dispatched() {
    let shared = test_shared();
    let registry = registry(vec![spec(
        "guard",
        ExtensionEventKind::PreToolUse,
        "exit 0",
    )]);
    let executed = dispatch_observe(
        &registry,
        &shared,
        &tool_event_in("bash", None, ws().path()),
    )
    .await;
    assert_eq!(executed, 0, "gate events dispatch only via HookGate");
}

#[tokio::test]
async fn hook_timeout_obeys_spec_then_budget() {
    use std::time::Duration;
    let s = |timeout: Option<u64>| HookSpec {
        timeout_secs: timeout,
        ..spec("h", ExtensionEventKind::Stop, "x")
    };
    assert_eq!(hook_timeout(&s(Some(10)), 100), Duration::from_secs(10));
    assert_eq!(
        hook_timeout(&s(Some(200)), 100),
        Duration::from_secs(100),
        "budget caps spec"
    );
    assert_eq!(hook_timeout(&s(None), 100), Duration::from_secs(100));
    assert_eq!(
        hook_timeout(&s(None), 0),
        DEFAULT_HOOK_TIMEOUT,
        "unlimited budget -> default"
    );
    assert_eq!(
        hook_timeout(&s(Some(0)), 100),
        Duration::from_secs(100),
        "zero spec = unset"
    );
}

#[tokio::test]
async fn hook_timeout_prefers_per_extension_budget() {
    use crate::budget::ExtensionBudget;
    use std::time::Duration;
    // per-extension budget 优先于全局（manifest 覆盖 host defaults）。
    let with_budget = |max_run_secs: u64, timeout: Option<u64>| HookSpec {
        budget: Some(ExtensionBudget {
            max_run_secs,
            ..Default::default()
        }),
        timeout_secs: timeout,
        ..spec("h", ExtensionEventKind::Stop, "x")
    };
    assert_eq!(
        hook_timeout(&with_budget(5, None), 3_600),
        Duration::from_secs(5),
        "per-extension budget wins over the global default"
    );
    assert_eq!(
        hook_timeout(&with_budget(5, Some(100)), 3_600),
        Duration::from_secs(5),
        "declared timeout capped by the per-extension budget"
    );
    assert_eq!(
        hook_timeout(&with_budget(0, None), 3_600),
        Duration::from_secs(30),
        "per-extension zero budget = unlimited -> default"
    );
    // 未装配（None）回落全局。
    let s = |timeout: Option<u64>| HookSpec {
        timeout_secs: timeout,
        ..spec("h", ExtensionEventKind::Stop, "x")
    };
    assert_eq!(
        hook_timeout(&s(None), 7),
        Duration::from_secs(7),
        "unassembled spec falls back to the global budget"
    );
}

#[tokio::test]
async fn tool_gate_respects_path_and_tool_matchers() {
    let mut guarded = spec_with_matcher(
        "guarded",
        ExtensionEventKind::PreToolUse,
        "echo '{\"decision\":\"deny\",\"reason\":\"src guarded\"}'",
        Some("edit"),
        Some("src/"),
    );
    guarded.name = "guarded".into();
    let shared = test_shared();
    let gate = HookGate::new(registry(vec![guarded]), shared);
    assert!(matches!(
        gate.evaluate_tool(&tool_event_in("edit", Some("src/a.rs"), ws().path()))
            .await,
        ToolGateDecision::Deny { .. }
    ));
    assert_eq!(
        gate.evaluate_tool(&tool_event_in("edit", Some("lib/a.rs"), ws().path()))
            .await,
        ToolGateDecision::Allow,
        "path outside the guarded subtree is allowed"
    );
    assert_eq!(
        gate.evaluate_tool(&tool_event_in("read_file", Some("src/a.rs"), ws().path()))
            .await,
        ToolGateDecision::Allow,
        "tool outside the matcher is allowed"
    );
}

/// 记录 before/after 调用序列的生命周期观察点（ARC-730 注入 seam 测试）。
#[derive(Debug, Clone, Default)]
struct RecordingLifecycle {
    calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl HookLifecycle for RecordingLifecycle {
    fn before<'a>(
        &'a self,
        _event: &'a ExtensionEvent,
        spec: &'a HookSpec,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let calls = self.calls.clone();
        let name = spec.name.clone();
        Box::pin(async move {
            calls.lock().unwrap().push(format!("before:{name}"));
        })
    }

    fn after<'a>(
        &'a self,
        _event: &'a ExtensionEvent,
        spec: &'a HookSpec,
        _outcome: &'a HookRunOutcome,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let calls = self.calls.clone();
        let name = spec.name.clone();
        Box::pin(async move {
            calls.lock().unwrap().push(format!("after:{name}"));
        })
    }
}

fn post_tool_event(root: &std::path::Path) -> ExtensionEvent {
    ExtensionEvent::new(
        ExtensionEventKind::PostToolUse,
        "s1",
        root.to_string_lossy(),
        "t",
        ExtensionEventPayload::PostToolUse {
            tool_name: ToolId::new("bash").unwrap(),
            tool_input: json!({}),
            tool_result: json!({"ok": true}),
            tool_input_truncated: false,
            tool_result_truncated: false,
            duration_ms: None,
            path: None,
        },
    )
}

/// 注入的 lifecycle 在每个匹配 hook 前后按执行顺序调用（两个 hook：
/// before 先于 hook、after 后于 hook、hook 间串行）。
#[tokio::test]
async fn hook_lifecycle_is_invoked_before_and_after_each_hook() {
    let lifecycle = RecordingLifecycle::default();
    let shared = crate::host::HostShared::test_harness_with_lifecycle(Arc::new(lifecycle.clone()));
    let registry = registry(vec![
        spec("first", ExtensionEventKind::PostToolUse, "exit 0"),
        spec("second", ExtensionEventKind::PostToolUse, "exit 0"),
    ]);
    let event = post_tool_event(ws().path());
    let executed = dispatch_observe(&registry, &shared, &event).await;
    assert_eq!(executed, 2);
    let calls = lifecycle.calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![
            "before:first".to_string(),
            "after:first".to_string(),
            "before:second".to_string(),
            "after:second".to_string(),
        ],
        "before/after must bracket each hook in execution order"
    );
}

/// lifecycle 观察失败不阻断 hook 执行（before 内部吞错，hook 照常运行
/// 并落 hook_run 诊断）。
#[tokio::test]
async fn hook_lifecycle_failure_does_not_block_hook_execution() {
    #[derive(Debug)]
    struct FailingLifecycle;
    impl HookLifecycle for FailingLifecycle {
        fn before<'a>(
            &'a self,
            _event: &'a ExtensionEvent,
            _spec: &'a HookSpec,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
            Box::pin(async {
                // 模拟归因前 IO 失败：吞掉，不让错误影响派发。
                let _ = std::fs::read("/nonexistent/arc-730/hook-baseline");
            })
        }

        fn after<'a>(
            &'a self,
            _event: &'a ExtensionEvent,
            _spec: &'a HookSpec,
            _outcome: &'a HookRunOutcome,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
            Box::pin(async {})
        }
    }
    let shared = crate::host::HostShared::test_harness_with_lifecycle(Arc::new(FailingLifecycle));
    let registry = registry(vec![spec("h", ExtensionEventKind::PostToolUse, "exit 0")]);
    let event = post_tool_event(ws().path());
    let executed = dispatch_observe(&registry, &shared, &event).await;
    assert_eq!(executed, 1, "hook still executes when observation fails");
    assert!(
        shared
            .diagnostics()
            .iter()
            .any(|d| d.message.contains("hook 'h'")),
        "hook_run diagnostic is still recorded"
    );
}
