//! Command runner：把 hook 命令作为沙箱子进程执行并解释结果。
//!
//! 执行语义（复用 workspace-runtime 的 [`ProcessSpec`] / [`run`]）：
//!
//! - 事件信封经**环境变量**注入（[`EVENT_ENV_VAR`]，JSON 截断到
//!   [`MAX_HOOK_PAYLOAD_BYTES`]）。与 xai-grok-hooks 的 stdin 注入不同：
//!   Evo 的 `ProcessSpec` 固定 `stdin = null`，env 是唯一不破坏共享进程
//!   契约的通道。
//! - 环境使用白名单（[`HOOK_ENV_KEYS`] + 注入变量），hook 进程看不到
//!   宿主其余环境；注入变量最后写入（覆盖白名单同名值），hook 无法伪造
//!   身份信号。
//! - **每个 hook 进程必须携带 [`SandboxProfile::product_default`]**；
//!   平台能力不足（[`SandboxCapability`] 探测）时**不 spawn**，返回
//!   [`HookRunOutcome::SandboxUnsupported`]（Tool gate 据此 fail-closed）。
//! - 输出按 [`OutputBudget`] 截断（洪泛保护）；截断是显式的
//!   [`HookRunOutcome::OutputLimited`]，不静默丢弃。**截断输出不驱动
//!   gate 决策**：Tool / Stop gate 在 `output_limited` 时直接返回
//!   `OutputLimited`（dispatcher fail-open），残留的 JSON 尾部不能产生
//!   allow / deny / block；Observe 同样报告 `OutputLimited`。
//! - 超时 / 取消 / spawn 失败 / 进程崩溃 / 非法 JSON 都有结构化结果。
//! - 相对命令解析：direct 分支经 [`HookSpec::command_path`] 相对扩展目录
//!   解析；shell 分支经 [`HookSpec::shell_command`] 把第一 token（相对
//!   路径时）绝对化，其余文本不变（详见 hook.rs）。
//!
//! 决策协议（stdout JSON，Tool / Stop gate）：
//!
//! - Tool：`{"decision": "allow" | "deny", "reason": "…"}`；无 JSON 时
//!   exit 0 = allow、exit 2 = deny（stderr 或默认文案作 reason）、其余
//!   退出码 = 失败。
//! - Stop：`{"decision": "block" | "approve", "reason", "continue",
//!   "stopReason", "hookSpecificOutput": {"additionalContext"}}`；无 JSON
//!   时 exit 0 = 无信号、exit 2 = block（stderr 作反馈）、其余 = 失败。
//! - JSON 决策优先于退出码（deny/block JSON 在任何退出码下都生效），
//!   与 xai-grok-hooks 一致；未知 decision 值是错误（typo 显式暴露，
//!   不静默 fail-open）。
//! - Observe gate 忽略输出：只看退出码（exit 0 = 成功）。

// Adapted from xai-grok-hooks, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// command.rs patterns (timeout/output limits/exit-code ladder) ported onto
// Evo's workspace-runtime ProcessSpec; stdin injection replaced by the env
// channel, sandbox enforcement and structured outcomes are Evo additions.
use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use workspace_runtime::api::{
    EnvPolicy, OutputBudget, ProcessOutcome, ProcessSpec, ProgramKind, SandboxCapability,
    SandboxProfile, run,
};

use crate::event::MAX_HOOK_PAYLOAD_BYTES;
use crate::hook::HookSpec;

/// hook 输出捕获预算：单流 64 KB / 2000 行（与 xai-grok-hooks 的
/// `MAX_OUTPUT_BYTES` 同量级）。
pub const HOOK_OUTPUT_MAX_BYTES: usize = 64 * 1024;
pub const HOOK_OUTPUT_MAX_LINES: usize = 2000;

/// 未声明超时时观察类 hook 的默认超时。
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(30);

/// hook 进程可见的环境白名单（宿主 env 白名单收集 + 注入变量）。
const HOOK_ENV_KEYS: &[&str] = &[
    "PATH", "HOME", "SHELL", "LANG", "LC_ALL", "TZ", "PWD", "USER", "LOGNAME",
];

/// 事件信封注入的 env 变量名。
pub const EVENT_ENV_VAR: &str = "EVO_HOOK_EVENT";
pub const HOOK_NAME_ENV_VAR: &str = "EVO_HOOK_NAME";
pub const SESSION_ENV_VAR: &str = "EVO_SESSION_ID";
pub const WORKSPACE_ENV_VAR: &str = "EVO_WORKSPACE_ROOT";

/// gate 分类：决定 hook 输出如何被解释、失败如何影响产品。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    /// 观察：输出被忽略，只记录成败。
    Observe,
    /// Tool：allow / deny 决策。
    Tool,
    /// Stop：block / continue 决策。
    Stop,
}

/// runner 运行上下文。
#[derive(Debug, Clone)]
pub struct RunContext {
    pub session_id: String,
    pub workspace_root: String,
    /// 宿主级取消（session / host shutdown 时触发，杀死在途 hook 进程树）。
    pub cancel: CancellationToken,
    /// 平台 sandbox 能力探测结果；`None` 时现场探测（测试可注入
    /// 能力不足的平台模拟 fail-closed 语义）。
    pub sandbox_capability: Option<SandboxCapability>,
}

/// 单个 Stop hook 的聚合信号。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StopSignals {
    /// `decision: "block"`（+ reason）；空 = 无 block 意见。
    pub block: Option<String>,
    /// `continue: false`（+ stopReason）；`Some` 表示强制停止。
    pub force_stop: Option<String>,
    /// `hookSpecificOutput.additionalContext`（反馈给模型）。
    pub additional_context: Option<String>,
}

impl StopSignals {
    pub fn is_empty(&self) -> bool {
        self.block.is_none() && self.force_stop.is_none() && self.additional_context.is_none()
    }
}

/// 单次 hook 运行的结构化结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookRunOutcome {
    /// 成功完成：Observe 下 exit 0；Tool 下 allow；Stop 下无信号。
    Success,
    /// Tool gate：`allow: false` 表示明确 deny（reason 供展示）。
    ToolDecision { allow: bool, reason: Option<String> },
    /// Stop gate：聚合信号（block / force_stop / additional context）。
    StopSignals(StopSignals),
    /// 超出运行时限，进程树被终止。
    TimedOut,
    /// 被宿主取消（shutdown / session 关闭），进程树被终止。
    Cancelled,
    /// 输出超过 [`HOOK_OUTPUT_MAX_BYTES`]，结果已截断（进程本身完成）。
    OutputLimited,
    /// 进程崩溃 / 非零退出码 / 非法决策 JSON：执行失败（gate 按
    /// fail-open 处理）。
    Failed { reason: String },
    /// 未能 spawn（命令不存在等）。
    SpawnFailed { reason: String },
    /// 平台无法强制 sandbox：未 spawn（Tool gate fail-closed 依据）。
    SandboxUnsupported { reason: String },
}

impl HookRunOutcome {
    /// 是否为「sandbox 环境性失败」：平台无法强制 sandbox，hook 未 spawn。
    /// 这是唯一的 fail-closed 类别（Tool gate）：其余执行失败一律
    /// fail-open（与 xai-grok-hooks 一致）。
    pub fn is_sandbox_failure(&self) -> bool {
        matches!(self, HookRunOutcome::SandboxUnsupported { .. })
    }
}

/// 运行一个 hook 命令并解释结果。
pub async fn run_hook(
    spec: &HookSpec,
    event_json: &str,
    ctx: &RunContext,
    timeout: Duration,
    gate: GateKind,
) -> HookRunOutcome {
    let capability = match ctx.sandbox_capability.clone() {
        Some(capability) => capability,
        None => SandboxCapability::current(),
    };
    if !capability.fs_supported() {
        return HookRunOutcome::SandboxUnsupported {
            reason: format!(
                "sandbox filesystem enforcement unavailable: {}",
                capability.fs.detail
            ),
        };
    }

    let envelope_bytes = event_json.len();
    if envelope_bytes > MAX_HOOK_PAYLOAD_BYTES {
        return HookRunOutcome::Failed {
            reason: format!(
                "event envelope exceeds {MAX_HOOK_PAYLOAD_BYTES} bytes ({envelope_bytes}); \
                 hook not executed"
            ),
        };
    }

    let spec_program = if spec.runs_via_shell() {
        ProgramKind::Shell {
            path: "sh".into(),
            command_arg: "-c".into(),
        }
    } else {
        ProgramKind::Direct {
            program: spec.command_path().to_string_lossy().into_owned(),
            args: Vec::new(),
        }
    };
    let workspace_root = ctx.workspace_root.clone();
    let process_spec = ProcessSpec {
        program: spec_program,
        // shell 分支：第一 token 是相对路径时由 [`HookSpec::shell_command`]
        // 绝对化为扩展目录解析（其余文本不变）；否则原样传给 `sh -c`
        // （PATH 命令 / `$VAR` / `~` / 管道 / 重定向由 shell 语义处理）。
        command: spec.shell_command(),
        cwd: std::path::PathBuf::from(&workspace_root),
        env: EnvPolicy::AllowList(hook_env(event_json, spec, ctx)),
        timeout,
        output_budget: OutputBudget::new(HOOK_OUTPUT_MAX_BYTES, HOOK_OUTPUT_MAX_LINES),
        sandbox: Some(SandboxProfile::product_default(std::path::Path::new(
            &workspace_root,
        ))),
    };

    let outcome = run(process_spec, &ctx.cancel, None).await;
    match outcome {
        ProcessOutcome::Completed { exit_code, output } => {
            let output_limited = output.stdout_bytes > HOOK_OUTPUT_MAX_BYTES
                || output.stderr_bytes > HOOK_OUTPUT_MAX_BYTES;
            let completed = CompletedProcess {
                exit_code,
                stdout: &output.stdout,
                stderr: &output.stderr,
                output_limited,
            };
            match gate {
                GateKind::Observe => interpret_observe(completed, spec),
                GateKind::Tool => interpret_tool(completed, spec),
                GateKind::Stop => interpret_stop(completed, spec),
            }
        }
        ProcessOutcome::TimedOut { .. } => HookRunOutcome::TimedOut,
        ProcessOutcome::Cancelled { .. } => HookRunOutcome::Cancelled,
        ProcessOutcome::Failed { message, .. } => {
            if message.contains("sandbox") {
                HookRunOutcome::SandboxUnsupported { reason: message }
            } else {
                HookRunOutcome::SpawnFailed { reason: message }
            }
        }
    }
}

/// 已完成进程的视图（interpret 函数的输入）。
struct CompletedProcess<'a> {
    exit_code: Option<i32>,
    stdout: &'a str,
    stderr: &'a str,
    output_limited: bool,
}

/// 进程环境：宿主白名单 + 注入变量（注入变量最后写入，覆盖同名
/// 白名单值，hook 无法伪造身份信号）。
fn hook_env(event_json: &str, spec: &HookSpec, ctx: &RunContext) -> HashMap<String, String> {
    let mut env = HashMap::new();
    for key in HOOK_ENV_KEYS {
        if let Some(value) = std::env::var_os(key) {
            env.insert((*key).to_string(), value.to_string_lossy().into_owned());
        }
    }
    env.insert(EVENT_ENV_VAR.to_string(), event_json.to_string());
    env.insert(HOOK_NAME_ENV_VAR.to_string(), spec.name.clone());
    env.insert(SESSION_ENV_VAR.to_string(), ctx.session_id.clone());
    env.insert(WORKSPACE_ENV_VAR.to_string(), ctx.workspace_root.clone());
    env
}

/// Observe gate：忽略输出，只看退出码与输出洪泛。
fn interpret_observe(process: CompletedProcess<'_>, spec: &HookSpec) -> HookRunOutcome {
    let outcome = match process.exit_code {
        Some(0) => HookRunOutcome::Success,
        Some(code) => HookRunOutcome::Failed {
            reason: format!("hook '{}' failed with exit code {code}", spec.name),
        },
        None => HookRunOutcome::Failed {
            reason: format!("hook '{}' was terminated by a signal", spec.name),
        },
    };
    if process.output_limited && matches!(outcome, HookRunOutcome::Success) {
        return HookRunOutcome::OutputLimited;
    }
    outcome
}

/// Tool gate 决策 JSON。
#[derive(Debug, Deserialize)]
struct ToolDecisionJson {
    decision: String,
    #[serde(default)]
    reason: Option<String>,
}

/// Tool gate：JSON allow/deny 优先，退出码兜底。
fn interpret_tool(process: CompletedProcess<'_>, spec: &HookSpec) -> HookRunOutcome {
    // 输出被预算截断（洪泛）时不解析截断内容：残留 JSON 尾部不能驱动
    // 决策。返回 OutputLimited，dispatcher 对 Tool gate 按 fail-open 处理
    // （等同无意见），与 Observe 的截断报告语义一致。
    if process.output_limited {
        return HookRunOutcome::OutputLimited;
    }
    let parsed = parse_tool_json(process.stdout);
    if let Some(parsed) = parsed {
        return match parsed.decision.as_str() {
            "deny" => HookRunOutcome::ToolDecision {
                allow: false,
                reason: Some(
                    parsed
                        .reason
                        .unwrap_or_else(|| format!("denied by hook '{}'", spec.name)),
                ),
            },
            "allow" => {
                if process.exit_code == Some(2) {
                    HookRunOutcome::ToolDecision {
                        allow: false,
                        reason: Some(format!("denied by hook '{}' (exit code 2)", spec.name)),
                    }
                } else {
                    HookRunOutcome::Success
                }
            }
            other => HookRunOutcome::Failed {
                reason: format!("unknown decision value '{other}' from hook '{}'", spec.name),
            },
        };
    }
    match process.exit_code {
        Some(0) => HookRunOutcome::Success,
        Some(2) => HookRunOutcome::ToolDecision {
            allow: false,
            reason: Some(deny_reason(process.stderr, spec)),
        },
        Some(code) => HookRunOutcome::Failed {
            reason: format!("hook '{}' failed with exit code {code}", spec.name),
        },
        None => HookRunOutcome::Failed {
            reason: format!("hook '{}' was terminated by a signal", spec.name),
        },
    }
}

fn parse_tool_json(stdout: &str) -> Option<ToolDecisionJson> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str::<ToolDecisionJson>(trimmed).ok()
}

fn deny_reason(stderr: &str, spec: &HookSpec) -> String {
    let feedback = stderr.trim();
    if feedback.is_empty() {
        format!("denied by hook '{}' (exit code 2)", spec.name)
    } else {
        feedback.to_string()
    }
}

/// Stop gate 决策 JSON。所有字段可选；一次输出可携带多个信号。
#[derive(Debug, Default, Deserialize)]
struct StopDecisionJson {
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default, rename = "continue")]
    continue_: Option<bool>,
    #[serde(default, rename = "stopReason")]
    stop_reason: Option<String>,
    #[serde(default, rename = "hookSpecificOutput")]
    hook_specific_output: Option<StopSpecificOutputJson>,
}

#[derive(Debug, Default, Deserialize)]
struct StopSpecificOutputJson {
    #[serde(default, rename = "additionalContext")]
    additional_context: Option<String>,
}

/// Stop gate：JSON 信号优先，退出码兜底。
fn interpret_stop(process: CompletedProcess<'_>, spec: &HookSpec) -> HookRunOutcome {
    // 输出被预算截断（洪泛）时不解析截断内容：残留 JSON 尾部不能产生
    // block / force_stop 信号。返回 OutputLimited，dispatcher 对 Stop
    // gate 按无信号处理（fail-open），与 Observe 的截断报告语义一致。
    if process.output_limited {
        return HookRunOutcome::OutputLimited;
    }
    let mut signals = StopSignals::default();
    let parsed = parse_stop_json(process.stdout);
    if let Some(json) = parsed {
        match json.decision.as_deref() {
            Some("block") => {
                signals.block = Some(
                    json.reason
                        .filter(|reason| !reason.trim().is_empty())
                        .unwrap_or_else(|| format!("Blocked by stop hook '{}'", spec.name)),
                );
            }
            Some("approve") | None => {}
            Some(other) => {
                return HookRunOutcome::Failed {
                    reason: format!("unknown decision value '{other}' from hook '{}'", spec.name),
                };
            }
        }
        if json.continue_ == Some(false) {
            signals.force_stop = json.stop_reason;
        }
        if let Some(context) = json
            .hook_specific_output
            .and_then(|output| output.additional_context)
            .filter(|text| !text.trim().is_empty())
        {
            signals.additional_context = Some(context);
        }
        if signals.is_empty() {
            return HookRunOutcome::Success;
        }
        return HookRunOutcome::StopSignals(signals);
    }
    match process.exit_code {
        Some(0) => HookRunOutcome::Success,
        Some(2) => HookRunOutcome::StopSignals(StopSignals {
            block: Some(deny_reason(process.stderr, spec)),
            ..Default::default()
        }),
        Some(code) => HookRunOutcome::Failed {
            reason: format!("hook '{}' failed with exit code {code}", spec.name),
        },
        None => HookRunOutcome::Failed {
            reason: format!("hook '{}' was terminated by a signal", spec.name),
        },
    }
}

fn parse_stop_json(stdout: &str) -> Option<StopDecisionJson> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str::<StopDecisionJson>(trimmed).ok()
}

#[cfg(test)]
#[path = "runner/tests_interpret.rs"]
mod tests_interpret;
