use super::*;
use crate::event::ExtensionEventKind;
use crate::hook::HookSpec;
use crate::matcher::HookMatcher;

fn spec(name: &str) -> HookSpec {
    HookSpec {
        name: name.into(),
        event: ExtensionEventKind::PreToolUse,
        match_tool: None,
        match_path: None,
        match_profile: None,
        priority: 0,
        command: "run.sh".into(),
        source_dir: std::path::PathBuf::from("/ext"),
        timeout_secs: None,
        budget: None,
        enabled: true,
        matcher: HookMatcher::match_all(),
    }
}

fn completed<'a>(
    exit_code: Option<i32>,
    stdout: &'a str,
    stderr: &'a str,
    output_limited: bool,
) -> CompletedProcess<'a> {
    CompletedProcess {
        exit_code,
        stdout,
        stderr,
        output_limited,
    }
}

fn deny_reason_of(outcome: HookRunOutcome) -> String {
    match outcome {
        HookRunOutcome::ToolDecision { allow, reason } => {
            assert!(!allow);
            reason.unwrap_or_default()
        }
        other => panic!("expected ToolDecision deny, got {other:?}"),
    }
}

#[test]
fn observe_accepts_exit_zero_ignores_output() {
    let outcome = interpret_observe(
        completed(Some(0), "{\"decision\":\"deny\"}", "", false),
        &spec("o"),
    );
    assert_eq!(outcome, HookRunOutcome::Success, "observe ignores stdout");
    let outcome = interpret_observe(completed(Some(1), "", "boom", false), &spec("o"));
    assert!(matches!(outcome, HookRunOutcome::Failed { reason } if reason.contains("exit code 1")));
    let outcome = interpret_observe(completed(None, "", "", false), &spec("o"));
    assert!(matches!(outcome, HookRunOutcome::Failed { .. }));
}

#[test]
fn observe_marks_output_limited() {
    let outcome = interpret_observe(completed(Some(0), "", "", true), &spec("o"));
    assert_eq!(outcome, HookRunOutcome::OutputLimited);
    let outcome = interpret_observe(completed(Some(1), "", "", true), &spec("o"));
    assert!(matches!(outcome, HookRunOutcome::Failed { .. }));
}

#[test]
fn tool_json_decision_wins_over_exit_code() {
    let outcome = interpret_tool(
        completed(
            Some(0),
            r#"{"decision":"deny","reason":"blocked"}"#,
            "",
            false,
        ),
        &spec("t"),
    );
    assert_eq!(deny_reason_of(outcome), "blocked");

    let outcome = interpret_tool(
        completed(Some(2), r#"{"decision":"deny"}"#, "", false),
        &spec("t"),
    );
    assert!(deny_reason_of(outcome).contains("t"));

    // JSON deny 在任何退出码下生效。
    let outcome = interpret_tool(
        completed(Some(1), r#"{"decision":"deny","reason":"nope"}"#, "", false),
        &spec("t"),
    );
    assert_eq!(deny_reason_of(outcome), "nope");

    // JSON allow + exit 2 → deny（退出码阶梯兜底）。
    let outcome = interpret_tool(
        completed(Some(2), r#"{"decision":"allow"}"#, "", false),
        &spec("t"),
    );
    assert!(deny_reason_of(outcome).contains("exit code 2"));
}

#[test]
fn tool_falls_back_to_exit_code() {
    let outcome = interpret_tool(completed(Some(0), "not json at all", "", false), &spec("t"));
    assert_eq!(outcome, HookRunOutcome::Success);
    let outcome = interpret_tool(completed(Some(0), "", "", false), &spec("t"));
    assert_eq!(outcome, HookRunOutcome::Success);
    let outcome = interpret_tool(completed(Some(2), "", "no network", false), &spec("t"));
    assert_eq!(deny_reason_of(outcome), "no network");
    let outcome = interpret_tool(completed(Some(2), "", "", false), &spec("t"));
    assert!(deny_reason_of(outcome).contains("exit code 2"));
    let outcome = interpret_tool(completed(Some(3), "", "", false), &spec("t"));
    assert!(matches!(outcome, HookRunOutcome::Failed { reason } if reason.contains("exit code 3")));
    let outcome = interpret_tool(completed(None, "", "", false), &spec("t"));
    assert!(matches!(outcome, HookRunOutcome::Failed { reason } if reason.contains("signal")));
}

#[test]
fn tool_unknown_decision_is_an_error() {
    let outcome = interpret_tool(
        completed(Some(0), r#"{"decision":"maybe"}"#, "", false),
        &spec("t"),
    );
    assert!(matches!(outcome, HookRunOutcome::Failed { reason } if reason.contains("maybe")));
}

#[test]
fn stop_json_signals_aggregate() {
    let outcome = interpret_stop(
        completed(
            Some(0),
            r#"{"decision":"block","reason":"tests failing","continue":false,"stopReason":"user said stop","hookSpecificOutput":{"additionalContext":"run tests"}}"#,
            "",
            false,
        ),
        &spec("s"),
    );
    match outcome {
        HookRunOutcome::StopSignals(signals) => {
            assert_eq!(signals.block.as_deref(), Some("tests failing"));
            assert_eq!(signals.force_stop.as_deref(), Some("user said stop"));
            assert_eq!(signals.additional_context.as_deref(), Some("run tests"));
        }
        other => panic!("expected StopSignals, got {other:?}"),
    }
}

#[test]
fn stop_block_requires_reason_fallback() {
    let outcome = interpret_stop(
        completed(Some(0), r#"{"decision":"block"}"#, "", false),
        &spec("s"),
    );
    match outcome {
        HookRunOutcome::StopSignals(signals) => {
            assert_eq!(signals.block.as_deref(), Some("Blocked by stop hook 's'"));
            assert!(signals.force_stop.is_none());
        }
        other => panic!("expected StopSignals, got {other:?}"),
    }
}

#[test]
fn stop_force_stop_and_context_only() {
    let outcome = interpret_stop(
        completed(
            Some(0),
            r#"{"continue":false,"stopReason":"budget"}"#,
            "",
            false,
        ),
        &spec("s"),
    );
    match outcome {
        HookRunOutcome::StopSignals(signals) => {
            assert!(signals.block.is_none());
            assert_eq!(signals.force_stop.as_deref(), Some("budget"));
        }
        other => panic!("expected StopSignals, got {other:?}"),
    }
    let outcome = interpret_stop(
        completed(
            Some(0),
            r#"{"hookSpecificOutput":{"additionalContext":"note"}}"#,
            "",
            false,
        ),
        &spec("s"),
    );
    match outcome {
        HookRunOutcome::StopSignals(signals) => {
            assert!(signals.block.is_none());
            assert!(signals.force_stop.is_none());
            assert_eq!(signals.additional_context.as_deref(), Some("note"));
        }
        other => panic!("expected StopSignals, got {other:?}"),
    }
}

#[test]
fn stop_exit_code_ladder() {
    let outcome = interpret_stop(completed(Some(0), "all done!", "", false), &spec("s"));
    assert_eq!(outcome, HookRunOutcome::Success);
    let outcome = interpret_stop(completed(Some(0), "", "", false), &spec("s"));
    assert_eq!(outcome, HookRunOutcome::Success);
    let outcome = interpret_stop(completed(Some(2), "", "fix the build\n", false), &spec("s"));
    match outcome {
        HookRunOutcome::StopSignals(signals) => {
            assert_eq!(signals.block.as_deref(), Some("fix the build"));
        }
        other => panic!("expected StopSignals, got {other:?}"),
    }
    let outcome = interpret_stop(completed(Some(1), "", "boom", false), &spec("s"));
    assert!(matches!(outcome, HookRunOutcome::Failed { .. }));
}

#[test]
fn stop_unknown_decision_is_an_error() {
    let outcome = interpret_stop(
        completed(Some(0), r#"{"decision":"deny"}"#, "", false),
        &spec("s"),
    );
    assert!(matches!(outcome, HookRunOutcome::Failed { reason } if reason.contains("deny")));
    // approve 是显式 no-op。
    let outcome = interpret_stop(
        completed(Some(0), r#"{"decision":"approve"}"#, "", false),
        &spec("s"),
    );
    assert_eq!(outcome, HookRunOutcome::Success);
}

#[test]
fn stop_json_wins_over_exit_2() {
    let outcome = interpret_stop(
        completed(
            Some(2),
            r#"{"continue":false,"stopReason":"enough"}"#,
            "blocked",
            false,
        ),
        &spec("s"),
    );
    match outcome {
        HookRunOutcome::StopSignals(signals) => {
            assert!(
                signals.block.is_none(),
                "JSON wins over stderr exit-2 block"
            );
            assert_eq!(signals.force_stop.as_deref(), Some("enough"));
        }
        other => panic!("expected StopSignals, got {other:?}"),
    }
}

#[test]
fn malformed_json_falls_back_to_exit_code() {
    // JSON 形似但解析失败 → 回退退出码。
    let outcome = interpret_stop(
        completed(Some(0), "{\"decision\":\"block\"", "", false),
        &spec("s"),
    );
    assert_eq!(outcome, HookRunOutcome::Success);
    let outcome = interpret_tool(
        completed(Some(2), "{\"decision\":\"deny\"", "", false),
        &spec("t"),
    );
    assert!(
        deny_reason_of(outcome).contains("exit code 2"),
        "malformed JSON falls back to the exit-code ladder"
    );
}

#[test]
fn truncated_tool_output_never_drives_a_decision() {
    // 洪泛（output_limited）且截断内容含完整 deny JSON：不解析、不 deny，
    // 返回 OutputLimited（dispatcher 按 fail-open 放行）。
    let outcome = interpret_tool(
        completed(Some(0), r#"{"decision":"deny","reason":"flood"}"#, "", true),
        &spec("t"),
    );
    assert_eq!(
        outcome,
        HookRunOutcome::OutputLimited,
        "truncated stdout must not produce a deny decision"
    );
    // 截断 + exit 2 + stderr 也一样：退出码兜底不跨过 output_limited。
    let outcome = interpret_tool(
        completed(Some(2), r#"{"decision":"allow"}"#, "stderr reason", true),
        &spec("t"),
    );
    assert_eq!(outcome, HookRunOutcome::OutputLimited);
}

#[test]
fn truncated_stop_output_never_produces_signals() {
    // 洪泛且截断内容含完整 block JSON：无信号（fail-open，agent 正常
    // 停止），与 Observe 的 OutputLimited 语义一致。
    let outcome = interpret_stop(
        completed(
            Some(0),
            r#"{"decision":"block","continue":false,"stopReason":"flood"}"#,
            "",
            true,
        ),
        &spec("s"),
    );
    assert_eq!(
        outcome,
        HookRunOutcome::OutputLimited,
        "truncated stdout must not produce block/force_stop signals"
    );
    let outcome = interpret_stop(
        completed(Some(2), r#"{"decision":"approve"}"#, "blocked", true),
        &spec("s"),
    );
    assert_eq!(outcome, HookRunOutcome::OutputLimited);
}

#[test]
fn sandbox_failure_classification() {
    assert!(HookRunOutcome::SandboxUnsupported { reason: "x".into() }.is_sandbox_failure());
    assert!(!HookRunOutcome::TimedOut.is_sandbox_failure());
    assert!(!HookRunOutcome::Cancelled.is_sandbox_failure());
    assert!(!HookRunOutcome::SpawnFailed { reason: "x".into() }.is_sandbox_failure());
    assert!(!HookRunOutcome::Failed { reason: "x".into() }.is_sandbox_failure());
    assert!(!HookRunOutcome::Success.is_sandbox_failure());
    assert!(!HookRunOutcome::OutputLimited.is_sandbox_failure());
    assert!(!HookRunOutcome::StopSignals(StopSignals::default()).is_sandbox_failure());
    assert!(
        !HookRunOutcome::ToolDecision {
            allow: true,
            reason: None
        }
        .is_sandbox_failure()
    );
}
