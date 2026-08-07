//! Hook 派发：registry 构建、Observe 事件派发、Tool/Stop gate 评估。
//!
//! 架构分工（避免 gate 事件双跑）：
//!
//! - **host 事件通道**（[`dispatch_observe`]）：只派发 Observe gate 事件
//!   （session / prompt / post_tool_use / permission / subagent / compact /
//!   merge）。host 的 dispatch loop 串行调用，budget 记账与派发解耦。
//! - **gate 评估**（[`HookGate::evaluate`]）：产品在 agent loop 内同步调用，
//!   处理 gate 事件（pre_tool_use / stop / subagent_stop）并返回决策。
//!   gate 事件在 host 通道只记账、不派发 hook（产品调用是唯一执行者），
//!   保证一次事件只跑一次 hook。
//!
//! 确定优先级：hook 按 `priority` 降序、同优先级按 `name` 升序执行
//! （[`sort_hooks`]）。冲突规则（transition 测试覆盖）：
//!
//! - Tool gate：按序咨询，**首个 deny 短路**（后续 hook 不再执行）。
//!   deny 优先于 allow：无论执行顺序如何，存在任一 deny 即拒绝。
//! - Stop gate：按序**全部执行**，聚合 block / force_stop /
//!   additional_context；首个 `continue: false` 生效（force_stop wins，
//!   后续 force_stop 被丢弃，block 与 context 全部保留）。
//! - Observe gate：按序全部执行，结果只进诊断（不阻断任何产品行为）。
//!
//! gate 失败策略矩阵（[`HookGate`] rustdoc 与 transition 测试）：

// Adapted from xai-grok-hooks, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// dispatcher.rs aggregation semantics (first-deny-short-circuit, stop
// signal folding) ported; priority ordering, gate/host split and the
// failure strategy matrix are Evo's own design.
use std::sync::Arc;

use crate::diagnostic::{DiagnosticLevel, DiagnosticRecord};
use crate::event::{ExtensionEvent, ExtensionEventKind};
use crate::hook::{HookSpec, sort_hooks};
use crate::matcher::MatchContext;
use crate::runner::{DEFAULT_HOOK_TIMEOUT, GateKind, HookRunOutcome, RunContext, run_hook};

/// 事件到 gate 的映射。
pub fn event_gate(kind: ExtensionEventKind) -> Option<GateKind> {
    match kind {
        ExtensionEventKind::PreToolUse => Some(GateKind::Tool),
        ExtensionEventKind::Stop | ExtensionEventKind::SubagentStop => Some(GateKind::Stop),
        _ => None,
    }
}

/// 已解析并排序的 hook 集合（每个启用扩展一份）。
#[derive(Debug, Default)]
pub struct HookRegistry {
    /// 按 `event` 分组，组内已按确定优先级排序。
    by_event: std::collections::BTreeMap<ExtensionEventKind, Vec<HookSpec>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 添加一个扩展的 hooks（跳过禁用的；启用检查由调用方完成）。
    pub fn add_extension(&mut self, _extension_id: &str, mut specs: Vec<HookSpec>) {
        specs.retain(|spec| spec.enabled);
        for spec in specs {
            self.by_event.entry(spec.event).or_default().push(spec);
        }
        for hooks in self.by_event.values_mut() {
            sort_hooks(hooks);
        }
    }

    /// 某事件匹配的 hook（按确定优先级排序）。
    pub fn hooks_for(&self, event: ExtensionEventKind) -> &[HookSpec] {
        self.by_event.get(&event).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn is_empty(&self) -> bool {
        self.by_event.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_event.values().map(Vec::len).sum()
    }
}

/// 记录一条诊断（host 与 gate 共用）。
pub(crate) fn record_diagnostic(
    shared: &crate::host::HostShared,
    level: DiagnosticLevel,
    code: &str,
    message: impl Into<String>,
    extension_id: Option<&str>,
) {
    shared.record(DiagnosticRecord {
        level,
        code: code.into(),
        message: message.into(),
        extension_id: extension_id.map(str::to_owned),
        context: Default::default(),
    });
}

/// 派发一个 Observe gate 事件到全部匹配 hook（串行，按确定优先级）。
///
/// 任何 hook 失败都不阻断其余 hook（fail-open：观察类事件不得影响
/// 产品行为）；结果统一落诊断。每个 hook 执行前后调用注入的
/// [`HookLifecycle`] 观察点（ARC-730 hook 修改归因；观察失败不阻断）。
/// 返回执行的 hook 数。
pub(crate) async fn dispatch_observe(
    registry: &HookRegistry,
    shared: &Arc<crate::host::HostShared>,
    event: &ExtensionEvent,
) -> usize {
    let Some(hooks) = non_gate_hooks(registry, event.kind) else {
        return 0;
    };
    let context = MatchContext::from_event(event);
    let session_id = event.session_id.clone();
    let budget_max_run_secs = shared.budget_max_run_secs();
    let workspace_root = event.workspace_root.clone();
    let cancel = shared.cancel();
    let event_json = serde_json::to_string(event).unwrap_or_default();
    let lifecycle = shared.hook_lifecycle().clone();
    let mut executed = 0;
    for spec in hooks {
        if !spec.matcher.matches(&context) {
            continue;
        }
        lifecycle.before(event, spec).await;
        let ctx = RunContext {
            session_id: session_id.clone(),
            workspace_root: workspace_root.clone(),
            cancel: cancel.clone(),
            sandbox_capability: None,
        };
        let timeout = hook_timeout(spec, budget_max_run_secs);
        let outcome = run_hook(spec, &event_json, &ctx, timeout, GateKind::Observe).await;
        lifecycle.after(event, spec, &outcome).await;
        executed += 1;
        record_hook_outcome(shared, spec, &outcome);
    }
    executed
}

/// gate 事件（pre_tool_use / stop）不在此派发（产品经 [`HookGate`] 调用）。
fn non_gate_hooks(registry: &HookRegistry, kind: ExtensionEventKind) -> Option<&[HookSpec]> {
    if event_gate(kind).is_some() {
        return None;
    }
    Some(registry.hooks_for(kind))
}

fn record_hook_outcome(
    shared: &Arc<crate::host::HostShared>,
    spec: &HookSpec,
    outcome: &HookRunOutcome,
) {
    let level = match outcome {
        HookRunOutcome::Success
        | HookRunOutcome::ToolDecision { .. }
        | HookRunOutcome::StopSignals(_)
        | HookRunOutcome::OutputLimited => DiagnosticLevel::Info,
        _ => DiagnosticLevel::Warning,
    };
    record_diagnostic(
        shared,
        level,
        "hook_run",
        format!(
            "hook '{}' ({}) finished: {outcome:?}",
            spec.name, spec.event
        ),
        None,
    );
}

/// 单次运行超时：spec 声明优先（受预算封顶），否则预算或默认。
fn hook_timeout(spec: &HookSpec, budget_max_run_secs: u64) -> std::time::Duration {
    let budget = if budget_max_run_secs > 0 {
        budget_max_run_secs
    } else {
        u64::MAX
    };
    let declared = spec
        .timeout_secs
        .filter(|secs| *secs > 0)
        .unwrap_or(budget)
        .min(budget);
    if declared == u64::MAX {
        return DEFAULT_HOOK_TIMEOUT;
    }
    std::time::Duration::from_secs(declared)
}

/// Tool gate 评估结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolGateDecision {
    /// 无匹配 hook、全部 allow、或失败按 fail-open 放行。
    Allow,
    /// 某 hook 明确 deny（首个 deny 短路后的决策）。
    Deny { reason: String },
    /// 环境性失败（sandbox 不支持等）：fail-closed 拒绝。
    ClosedByEnvironment { reason: String },
}

/// Stop gate 评估结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopGateDecision {
    /// block 意见（全部保留，按执行顺序）。
    pub blocks: Vec<String>,
    /// 首个 `continue: false`（之后丢弃）。
    pub force_stop: Option<String>,
    /// additional context（全部保留）。
    pub additional_context: Vec<String>,
    /// 每个执行的 hook 的结果（诊断展示）。
    pub outcomes: Vec<(String, HookRunOutcome)>,
}

impl StopGateDecision {
    /// 是否有任何「继续」信号（block / context）。
    pub fn wants_continuation(&self) -> bool {
        self.force_stop.is_none()
            && (!self.blocks.is_empty() || !self.additional_context.is_empty())
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty() && self.force_stop.is_none() && self.additional_context.is_empty()
    }
}

/// 产品侧的 gate 评估入口：持有 registry + 共享状态，串行执行匹配 hook。
#[derive(Clone)]
pub struct HookGate {
    registry: Arc<HookRegistry>,
    shared: Arc<crate::host::HostShared>,
    /// 平台 sandbox 能力探测（测试注入用；生产走现场探测）。
    sandbox_capability: Option<workspace_runtime::api::SandboxCapability>,
}

impl HookGate {
    pub(crate) fn new(registry: Arc<HookRegistry>, shared: Arc<crate::host::HostShared>) -> Self {
        Self {
            registry,
            shared,
            sandbox_capability: None,
        }
    }

    #[cfg(test)]
    fn with_sandbox_capability(
        registry: Arc<HookRegistry>,
        shared: Arc<crate::host::HostShared>,
        capability: workspace_runtime::api::SandboxCapability,
    ) -> Self {
        Self {
            registry,
            shared,
            sandbox_capability: Some(capability),
        }
    }

    /// 评估 Tool gate（`pre_tool_use`）：按确定优先级咨询匹配 hook，
    /// 首个 deny 短路。
    pub async fn evaluate_tool(&self, event: &ExtensionEvent) -> ToolGateDecision {
        let hooks = self.registry.hooks_for(event.kind);
        if hooks.is_empty() {
            return ToolGateDecision::Allow;
        }
        let context = MatchContext::from_event(event);
        let session_id = event.session_id.clone();
        let budget_max_run_secs = self.shared.budget_max_run_secs();
        let workspace_root = event.workspace_root.clone();
        let cancel = self.shared.cancel();
        let event_json = serde_json::to_string(event).unwrap_or_default();
        for spec in hooks {
            if !spec.matcher.matches(&context) {
                continue;
            }
            let ctx = RunContext {
                session_id: session_id.clone(),
                workspace_root: workspace_root.clone(),
                cancel: cancel.clone(),
                sandbox_capability: self.sandbox_capability.clone(),
            };
            let timeout = hook_timeout(spec, budget_max_run_secs);
            let outcome = run_hook(spec, &event_json, &ctx, timeout, GateKind::Tool).await;
            record_hook_outcome(&self.shared, spec, &outcome);
            match outcome {
                HookRunOutcome::Success | HookRunOutcome::OutputLimited => continue,
                HookRunOutcome::ToolDecision {
                    allow: false,
                    reason,
                } => {
                    return ToolGateDecision::Deny {
                        reason: reason.unwrap_or_else(|| format!("denied by hook '{}'", spec.name)),
                    };
                }
                HookRunOutcome::ToolDecision { allow: true, .. } => continue,
                // 执行失败（崩溃 / 非法 JSON / 超时 / 取消 / spawn 失败）
                // fail-open：hook 没有给出意见 = 放行（Grok 同款语义）。
                HookRunOutcome::Failed { reason } => {
                    record_diagnostic(
                        &self.shared,
                        DiagnosticLevel::Warning,
                        "hook_gate_failed",
                        format!("hook '{}' failed; failing open: {reason}", spec.name),
                        None,
                    );
                    continue;
                }
                HookRunOutcome::TimedOut
                | HookRunOutcome::Cancelled
                | HookRunOutcome::SpawnFailed { .. } => {
                    record_diagnostic(
                        &self.shared,
                        DiagnosticLevel::Warning,
                        "hook_gate_failed",
                        format!(
                            "hook '{}' could not run ({}); failing open",
                            spec.name,
                            outcome_label(&outcome)
                        ),
                        None,
                    );
                    continue;
                }
                HookRunOutcome::StopSignals(_) => {
                    record_diagnostic(
                        &self.shared,
                        DiagnosticLevel::Warning,
                        "hook_gate_unexpected",
                        format!("hook '{}' returned stop signals for a tool gate", spec.name),
                        None,
                    );
                    continue;
                }
                // sandbox 无法强制：hook 不能在安全边界内运行 → 拒绝工具
                // 调用（fail closed，ARC-610 安全原则）。
                HookRunOutcome::SandboxUnsupported { reason } => {
                    let message = format!(
                        "hook '{}' could not run inside the sandbox ({reason}); \
                         tool call rejected (fail closed)",
                        spec.name
                    );
                    record_diagnostic(
                        &self.shared,
                        DiagnosticLevel::Error,
                        "hook_gate_closed",
                        message.clone(),
                        None,
                    );
                    return ToolGateDecision::ClosedByEnvironment { reason: message };
                }
            }
        }
        ToolGateDecision::Allow
    }

    /// 评估 Stop gate（`stop` / `subagent_stop`）：全部执行并聚合信号。
    pub async fn evaluate_stop(&self, event: &ExtensionEvent) -> StopGateDecision {
        let mut decision = StopGateDecision {
            blocks: Vec::new(),
            force_stop: None,
            additional_context: Vec::new(),
            outcomes: Vec::new(),
        };
        let hooks = self.registry.hooks_for(event.kind);
        if hooks.is_empty() {
            return decision;
        }
        let context = MatchContext::from_event(event);
        let session_id = event.session_id.clone();
        let budget_max_run_secs = self.shared.budget_max_run_secs();
        let workspace_root = event.workspace_root.clone();
        let cancel = self.shared.cancel();
        let event_json = serde_json::to_string(event).unwrap_or_default();
        for spec in hooks {
            if !spec.matcher.matches(&context) {
                continue;
            }
            let ctx = RunContext {
                session_id: session_id.clone(),
                workspace_root: workspace_root.clone(),
                cancel: cancel.clone(),
                sandbox_capability: self.sandbox_capability.clone(),
            };
            let timeout = hook_timeout(spec, budget_max_run_secs);
            let outcome = run_hook(spec, &event_json, &ctx, timeout, GateKind::Stop).await;
            record_hook_outcome(&self.shared, spec, &outcome);
            match outcome.clone() {
                HookRunOutcome::StopSignals(signals) => {
                    if let Some(block) = signals.block {
                        decision.blocks.push(block);
                    }
                    if decision.force_stop.is_none() {
                        decision.force_stop = signals.force_stop;
                    }
                    if let Some(context) = signals.additional_context {
                        decision.additional_context.push(context);
                    }
                }
                HookRunOutcome::Failed { reason } => {
                    // Stop gate 失败 fail-open：无信号，agent 正常停止。
                    record_diagnostic(
                        &self.shared,
                        DiagnosticLevel::Warning,
                        "hook_gate_failed",
                        format!("stop hook '{}' failed; failing open: {reason}", spec.name),
                        None,
                    );
                }
                HookRunOutcome::Success
                | HookRunOutcome::OutputLimited
                | HookRunOutcome::ToolDecision { .. } => {}
                // 全部失败 fail-open：Stop gate 无阻断义务，sandbox 不可用
                // 也不阻塞正常停止。
                outcome => {
                    record_diagnostic(
                        &self.shared,
                        DiagnosticLevel::Warning,
                        "hook_gate_failed",
                        format!(
                            "stop hook '{}' could not run ({}); failing open",
                            spec.name,
                            outcome_label(&outcome)
                        ),
                        None,
                    );
                }
            }
            decision.outcomes.push((spec.name.clone(), outcome));
        }
        decision
    }
}

fn outcome_label(outcome: &HookRunOutcome) -> &'static str {
    match outcome {
        HookRunOutcome::TimedOut => "timed out",
        HookRunOutcome::Cancelled => "cancelled",
        HookRunOutcome::SpawnFailed { .. } => "spawn failed",
        HookRunOutcome::SandboxUnsupported { .. } => "sandbox unavailable",
        _ => "failed",
    }
}

#[cfg(test)]
#[path = "dispatcher/tests_dispatcher.rs"]
mod tests_dispatcher;
