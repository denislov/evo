//! Extension host：discovery / config merge / trust / lifecycle / budget /
//! diagnostics / shutdown 的组装点。
//!
//! 生命周期：
//!
//! 1. [`ExtensionHost::new`]（同步）：discovery + config merge + trust
//!    判定，产出启用列表、首次启用请求、hook 注册表与诊断。
//! 2. [`ExtensionHost::start`]（async）：启动后台 dispatch task，返回
//!    [`ExtensionHostHandle`]（提交事件 / 触发 shutdown）与
//!    [`ExtensionHostTask`]（join 回收结果）。
//! 3. [`ExtensionHostHandle::shutdown`]：确定性顺序 —— 状态置
//!    `Stopping`（新事件被拒）+ 取消在途 hook 进程 -> dispatch task 退出
//!    select 并 drain 已提交事件（有界，不丢已入队事件）-> 写
//!    `host_shutdown` 诊断 -> 退出；随后 [`ExtensionHostTask::join`]
//!    返回 [`HostExit`]。重复 shutdown 幂等。
//!
//! 事件派发（ARC-710）：
//!
//! - host 通道只派发 Observe gate 事件（session / prompt / post_tool_use /
//!   permission / subagent / compact / merge），串行 + budget 记账强制。
//! - gate 事件（pre_tool_use / stop / subagent_stop）在 host 通道只记账，
//!   hook 执行由产品经 [`ExtensionHost::gate`]（[`HookGate`]）同步调用
//!   驱动——避免一次事件双跑 hook。
//! - 任务内 panic 被捕获（fail closed），join 不传播 panic。

// Evo 独立设计：Grok 的 xai-grok-hooks 无 host 概念（load-and-fire）；
// host 生命周期 / shutdown 顺序 / panic 捕获为本 crate 自研机制。
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::budget::{BudgetTracker, ExtensionBudget};
use crate::config::{ExtensionConfig, ExtensionConfigLayer, ExtensionSource};
use crate::diagnostic::{DiagnosticLevel, DiagnosticRecord, DiagnosticSink, DiagnosticsCollector};
use crate::discovery::{ExtensionRecord, discover_extensions};
use crate::dispatcher::{HookGate, HookRegistry, dispatch_observe, event_gate};
use crate::error::ExtensionError;
use crate::event::{ExtensionEvent, ExtensionEventKind};
use crate::hook::parse_hooks;
use crate::trust::{EnableRequest, TrustDecision, TrustStatus, TrustStore, build_enable_request};

/// dispatch 事件队列容量（有界背压）。
const EVENT_QUEUE_CAPACITY: usize = 64;
/// 诊断环形缓冲容量。
const DEFAULT_DIAGNOSTIC_CAPACITY: usize = 256;

/// host 启动参数。ARC-720 在此扩展新配置字段。
#[derive(Debug, Clone)]
pub struct ExtensionHostOptions {
    /// 用户级扩展目录（来源 `Global`）。
    pub global_dirs: Vec<PathBuf>,
    /// 项目级扩展目录（来源 `Project`）。
    pub project_dirs: Vec<PathBuf>,
    /// 配置层，按高优先级在前排列。
    pub config_layers: Vec<ExtensionConfigLayer>,
    /// folder trust 判定入口。
    pub trust_store: Arc<dyn TrustStore>,
    /// 预算上限（层配置可覆盖）。
    pub budget: ExtensionBudget,
    /// 诊断输出目标（可选）。
    pub diagnostics: Option<Arc<dyn DiagnosticSink>>,
    /// 诊断环形缓冲容量。
    pub diagnostic_capacity: usize,
}

impl Default for ExtensionHostOptions {
    fn default() -> Self {
        Self {
            global_dirs: Vec::new(),
            project_dirs: Vec::new(),
            config_layers: Vec::new(),
            trust_store: Arc::new(crate::trust::InMemoryTrustStore::new()),
            budget: ExtensionBudget::default(),
            diagnostics: None,
            diagnostic_capacity: DEFAULT_DIAGNOSTIC_CAPACITY,
        }
    }
}

/// host 生命周期状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostState {
    Idle,
    Running,
    Stopping,
    Stopped,
    Failed,
}

/// host 的只读信息（new 时确定，此后不变）。
#[derive(Debug)]
struct HostInfo {
    options: ExtensionHostOptions,
    records: Vec<ExtensionRecord>,
    enabled: Vec<ExtensionRecord>,
    config: ExtensionConfig,
    first_enables: Vec<EnableRequest>,
}

/// host / handle 共享的可变状态。
#[derive(Debug)]
pub(crate) struct HostShared {
    state: Mutex<HostState>,
    shutdown_tx: Mutex<Option<watch::Sender<bool>>>,
    collector: Mutex<DiagnosticsCollector>,
    budget: Mutex<BudgetTracker>,
    /// shutdown 时取消在途 hook 进程。
    cancel: CancellationToken,
    /// 已解析的 hook 注册表（只读；gate 事件经 [`HookGate`] 查询）。
    registry: Option<Arc<HookRegistry>>,
}

impl HostShared {
    pub(crate) fn record(&self, record: DiagnosticRecord) {
        self.collector.lock().unwrap().record(record);
    }

    /// 宿主级取消令牌（shutdown 时触发）。
    pub(crate) fn cancel(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// 合并后预算的 `max_run_secs`（runner 超时上限）。
    pub(crate) fn budget_max_run_secs(&self) -> u64 {
        self.budget.lock().unwrap().limits().max_run_secs
    }

    /// 通知所有在途 hook 终止（shutdown 顺序第 0 步）。
    pub(crate) fn cancel_in_flight(&self) {
        self.cancel.cancel();
    }

    pub(crate) fn registry(&self) -> Option<&Arc<HookRegistry>> {
        self.registry.as_ref()
    }

    /// 诊断快照（测试用）。
    #[cfg(test)]
    pub(crate) fn diagnostics(&self) -> Vec<DiagnosticRecord> {
        self.collector.lock().unwrap().snapshot()
    }

    /// 测试 harness：默认运行态共享结构（无 registry、默认预算）。
    #[cfg(test)]
    pub(crate) fn test_harness() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(HostState::Running),
            shutdown_tx: Mutex::new(None),
            collector: Mutex::new(DiagnosticsCollector::new(None, 64)),
            budget: Mutex::new(BudgetTracker::new(ExtensionBudget::default())),
            cancel: CancellationToken::new(),
            registry: None,
        })
    }
}

/// 扩展宿主。
#[derive(Debug, Clone)]
pub struct ExtensionHost {
    info: Arc<HostInfo>,
    shared: Arc<HostShared>,
}

impl ExtensionHost {
    /// 构造 host：discovery + config merge + trust 判定 + hook 注册表。
    ///
    /// 返回 host 与加载期的诊断错误（坏 manifest 等，不影响 host 运行）。
    pub fn new(options: ExtensionHostOptions) -> (Self, Vec<ExtensionError>) {
        let mut errors = Vec::new();
        let mut collector = DiagnosticsCollector::new(
            options.diagnostics.clone(),
            options.diagnostic_capacity.max(1),
        );

        // 1. discovery（global 先，project 后；trust 判定按来源区分）。
        let mut records = Vec::new();
        let global_dirs: Vec<&std::path::Path> =
            options.global_dirs.iter().map(|p| p.as_path()).collect();
        let (global_records, global_errors) =
            discover_extensions(&global_dirs, ExtensionSource::Global);
        records.extend(global_records);
        errors.extend(global_errors);
        let project_dirs: Vec<&std::path::Path> =
            options.project_dirs.iter().map(|p| p.as_path()).collect();
        let (project_records, project_errors) =
            discover_extensions(&project_dirs, ExtensionSource::Project);
        records.extend(project_records);
        errors.extend(project_errors);

        // 2. config merge（层已按高优先级在前排列）；options.budget 作为
        //    最低优先级「host 默认」层参与合并，可被任何层覆盖。
        let mut layers = options.config_layers.clone();
        let host_defaults = ExtensionConfigLayer::new(
            ExtensionSource::Global,
            "host-defaults",
            ExtensionConfig {
                budget: options.budget,
                ..Default::default()
            },
        );
        layers.push(host_defaults);
        let (config, merge_errors) = crate::config::merge_config_layers(&layers);
        errors.extend(merge_errors);

        let budget = config.budget;

        // 3. trust 判定：Trusted 启用；Untrusted / NotDecided 不启用。
        let mut enabled = Vec::new();
        let mut first_enables = Vec::new();
        for record in &records {
            let TrustDecision { status, .. } =
                crate::trust::decide_trust(&record.dir, options.trust_store.as_ref());
            match status {
                TrustStatus::Trusted => enabled.push(record.clone()),
                TrustStatus::Untrusted => {
                    collector.record(DiagnosticRecord {
                        level: DiagnosticLevel::Warning,
                        code: "extension_untrusted".into(),
                        message: format!(
                            "extension '{}' skipped: folder is not trusted",
                            record.id
                        ),
                        extension_id: Some(record.id.clone()),
                        context: Default::default(),
                    });
                }
                TrustStatus::NotDecided => {
                    first_enables.push(build_enable_request(record, budget));
                    collector.record(DiagnosticRecord {
                        level: DiagnosticLevel::Info,
                        code: "extension_first_enable".into(),
                        message: format!("extension '{}' awaits first-enable approval", record.id),
                        extension_id: Some(record.id.clone()),
                        context: Default::default(),
                    });
                }
            }
        }

        // 4. 从启用扩展解析 hook 注册表（容错：坏 hook 记录诊断并跳过）。
        let mut registry = HookRegistry::new();
        for record in &enabled {
            let Some(hooks_value) = record.manifest.hooks.as_ref() else {
                continue;
            };
            let (specs, hook_errors) = parse_hooks(hooks_value, &record.dir);
            for detail in hook_errors {
                collector.record(DiagnosticRecord {
                    level: DiagnosticLevel::Warning,
                    code: "hook_invalid".into(),
                    message: format!("extension '{}': {detail}", record.id),
                    extension_id: Some(record.id.clone()),
                    context: Default::default(),
                });
            }
            registry.add_extension(&record.id, specs);
        }
        let registry = (!registry.is_empty()).then(|| Arc::new(registry));

        let info = Arc::new(HostInfo {
            options,
            records,
            enabled,
            config,
            first_enables,
        });
        let shared = Arc::new(HostShared {
            state: Mutex::new(HostState::Idle),
            shutdown_tx: Mutex::new(None),
            collector: Mutex::new(collector),
            budget: Mutex::new(BudgetTracker::new(budget)),
            cancel: CancellationToken::new(),
            registry,
        });
        (Self { info, shared }, errors)
    }

    /// 只读信息视图（product 查询用）。
    pub fn info(&self) -> HostInfoView<'_> {
        HostInfoView { info: &self.info }
    }

    /// 启动后台 dispatch task。只能启动一次。
    pub fn start(self) -> Result<(ExtensionHostHandle, ExtensionHostTask), ExtensionError> {
        {
            let mut state = self.shared.state.lock().unwrap();
            if *state != HostState::Idle {
                return Err(ExtensionError::NotRunning);
            }
            *state = HostState::Running;
        }
        let (tx, rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        *self.shared.shutdown_tx.lock().unwrap() = Some(shutdown_tx);

        let shared = self.shared.clone();
        let on_event: Arc<DispatchHandler> =
            Arc::new(|shared, event| Box::pin(hooks_on_event(shared, event)));
        let join =
            tokio::spawn(
                async move { dispatch_loop(rx, shutdown_rx, shared.clone(), on_event).await },
            );
        let handle = ExtensionHostHandle {
            shared: self.shared.clone(),
            tx,
        };
        let task = ExtensionHostTask { join };
        Ok((handle, task))
    }

    /// 当前生命周期状态。
    pub fn state(&self) -> HostState {
        self.shared.state.lock().unwrap().clone()
    }

    /// 保留的诊断快照（最老在前）。
    pub fn diagnostics(&self) -> Vec<DiagnosticRecord> {
        self.shared.collector.lock().unwrap().snapshot()
    }

    /// 产品侧 gate 评估入口（Tool / Stop gate）。
    ///
    /// 有启用扩展时返回 `Some`；产品在 agent loop 内同步调用评估并消费
    /// 决策。gate 事件在 host 通道只记账，hook 执行仅发生在这里。
    pub fn gate(&self) -> Option<Arc<HookGate>> {
        self.shared
            .registry
            .as_ref()
            .map(|registry| Arc::new(HookGate::new(registry.clone(), self.shared.clone())))
    }
}

/// host 的只读信息视图。
#[derive(Debug, Clone, Copy)]
pub struct HostInfoView<'a> {
    info: &'a HostInfo,
}

impl HostInfoView<'_> {
    /// 启动参数（ARC-710/720 查询扩展配置用）。
    pub fn options(&self) -> &ExtensionHostOptions {
        &self.info.options
    }

    /// 全部发现记录（含未启用）。
    pub fn records(&self) -> &[ExtensionRecord] {
        &self.info.records
    }

    /// 已启用的扩展（trusted）。
    pub fn enabled(&self) -> &[ExtensionRecord] {
        &self.info.enabled
    }

    /// 合并后的最终配置。
    pub fn config(&self) -> &ExtensionConfig {
        &self.info.config
    }

    /// 首次启用请求（等待产品放行）。
    pub fn first_enables(&self) -> &[EnableRequest] {
        &self.info.first_enables
    }
}

/// 运行时 handle：提交事件 / 触发 shutdown。
#[derive(Debug, Clone)]
pub struct ExtensionHostHandle {
    shared: Arc<HostShared>,
    tx: mpsc::Sender<ExtensionEvent>,
}

impl ExtensionHostHandle {
    /// 提交一个事件给 dispatch。事件必须通过 [`ExtensionEvent::validate_version`]；
    /// host 未运行或正在停止时拒绝。
    pub fn submit_event(&self, event: ExtensionEvent) -> Result<(), ExtensionError> {
        event.validate_version()?;
        let state = self.shared.state.lock().unwrap().clone();
        match state {
            HostState::Running => {}
            HostState::Stopping => {
                return Err(ExtensionError::ShuttingDown {
                    reason: "host shutdown in progress".into(),
                });
            }
            _ => return Err(ExtensionError::NotRunning),
        }
        self.tx
            .try_send(event)
            .map_err(|_| ExtensionError::NotRunning)
    }

    /// 触发确定性 shutdown（幂等）。随后用 [`ExtensionHostTask::join`] 回收。
    ///
    /// 顺序：状态置 `Stopping`（新事件被拒）-> 取消在途 hook 进程（dispatch
    /// 中的事件尽快结束）-> 发 watch 信号（dispatch 退出 select 并 drain）。
    pub fn shutdown(&self, reason: impl Into<String>) {
        let reason = reason.into();
        {
            let mut state = self.shared.state.lock().unwrap();
            if *state == HostState::Running {
                *state = HostState::Stopping;
            }
            // Idle / Stopping / Stopped / Failed：幂等，不再变更。
        }
        self.shared.cancel_in_flight();
        self.shared.record(DiagnosticRecord {
            level: DiagnosticLevel::Info,
            code: "host_shutdown_initiated".into(),
            message: format!("extension host shutdown initiated: {reason}"),
            extension_id: None,
            context: Default::default(),
        });
        if let Some(tx) = self.shared.shutdown_tx.lock().unwrap().as_ref() {
            let _ = tx.send(true);
        }
    }

    pub fn is_running(&self) -> bool {
        *self.shared.state.lock().unwrap() == HostState::Running
    }
}

/// 后台 dispatch task 的 join 句柄。
#[derive(Debug)]
pub struct ExtensionHostTask {
    join: tokio::task::JoinHandle<HostExit>,
}

/// dispatch task 的退出原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    /// handle 显式触发。
    Manual,
    /// 所有 handle 被丢弃（channel 关闭）。
    SendersDropped,
    /// dispatch 处理事件时 panic（fail closed）。
    Panic,
}

/// dispatch task 的退出报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostExit {
    pub reason: ShutdownReason,
    pub handled_events: u64,
    pub panicked: bool,
}

impl ExtensionHostTask {
    /// 等待 dispatch task 结束（panic 不会传播）。调用后 host 进入终态。
    pub async fn join(self) -> HostExit {
        match self.join.await {
            Ok(exit) => exit,
            Err(_) => HostExit {
                reason: ShutdownReason::Panic,
                handled_events: 0,
                panicked: true,
            },
        }
    }
}

/// dispatch 事件处理器签名：`Arc<HostShared>`（owned）+ 事件 -> future。
///
/// 每个事件在独立 task 中执行（panic 被 tokio 捕获为 `JoinError`，
/// fail closed）；`on_event` 必须是 `Send + Sync` 的 `Fn`（可变状态都在
/// `HostShared` 内）。
type DispatchHandler = dyn Fn(Arc<HostShared>, ExtensionEvent) -> Pin<Box<dyn Future<Output = ()> + Send>>
    + Send
    + Sync;

/// dispatch 主循环：消费事件队列直到 shutdown 信号或所有 sender 退出；
/// 退出前 drain 已入队事件（不丢已提交），然后写收尾诊断。
///
/// 每个事件的处理在独立 task 中执行；task 内 panic 被捕获并终止循环
/// （fail closed）。shutdown 先取消在途 hook（[`HostShared::cancel`]），
/// 使正在运行的 hook 进程尽快结束，随后 drain。
async fn dispatch_loop(
    mut rx: mpsc::Receiver<ExtensionEvent>,
    mut shutdown_rx: watch::Receiver<bool>,
    shared: Arc<HostShared>,
    on_event: Arc<DispatchHandler>,
) -> HostExit {
    let mut handled: u64 = 0;
    let mut panicked = false;

    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                let _ = changed; // watch sender 已 drop 或值已变化，都退出。
                break;
            }
            event = rx.recv() => {
                let Some(event) = event else {
                    break; // 所有 sender 已 drop。
                };
                let handler = on_event.clone();
                let shared_for_task = shared.clone();
                let task = tokio::spawn(async move {
                    handler(shared_for_task, event).await
                });
                match task.await {
                    Ok(()) => handled += 1,
                    Err(error) if error.is_panic() => {
                        panicked = true;
                        shared.record(DiagnosticRecord {
                            level: DiagnosticLevel::Error,
                            code: "dispatch_panic".into(),
                            message: "extension dispatch panicked; host stopping (fail closed)"
                                .into(),
                            extension_id: None,
                            context: Default::default(),
                        });
                        break;
                    }
                    Err(_) => break, // task 被 abort：视同 panic，停止派发。
                }
            }
        }
    }

    // 确定性 shutdown：drain 已入队事件（shutdown 后新提交已被拒绝，
    // 此循环有界；在途 hook 已被 cancel，drain 快速完成）。
    while let Ok(event) = rx.try_recv() {
        let handler = on_event.clone();
        let shared_for_task = shared.clone();
        let task = tokio::spawn(async move { handler(shared_for_task, event).await });
        match task.await {
            Ok(()) => handled += 1,
            Err(error) if error.is_panic() => {
                panicked = true;
                break;
            }
            Err(_) => break,
        }
    }

    let reason = if panicked {
        ShutdownReason::Panic
    } else if *shutdown_rx.borrow() {
        ShutdownReason::Manual
    } else {
        ShutdownReason::SendersDropped
    };
    shared.record(DiagnosticRecord {
        level: DiagnosticLevel::Info,
        code: "host_shutdown".into(),
        message: format!(
            "extension host stopped (reason: {}, handled_events: {handled})",
            reason_label(reason)
        ),
        extension_id: None,
        context: Default::default(),
    });
    *shared.state.lock().unwrap() = HostState::Stopped;

    HostExit {
        reason,
        handled_events: handled,
        panicked,
    }
}

fn reason_label(reason: ShutdownReason) -> &'static str {
    match reason {
        ShutdownReason::Manual => "manual",
        ShutdownReason::SendersDropped => "senders_dropped",
        ShutdownReason::Panic => "panic",
    }
}

/// 默认事件处理：budget 记账（超出 → 诊断并丢弃该事件）+ Observe 事件
/// 派发。
///
/// - gate 事件（pre_tool_use / stop / subagent_stop）只记账：hook 执行由
///   产品经 [`ExtensionHost::gate`] 同步驱动，避免双跑。
/// - Observe 事件记账后串行派发到匹配 hook（fail-open：hook 失败只落
///   诊断，不阻断产品）。
async fn hooks_on_event(shared: Arc<HostShared>, event: ExtensionEvent) {
    let session_id = event.session_id.clone();
    let payload_bytes = serde_json::to_vec(&event.payload)
        .map(|bytes| bytes.len())
        .unwrap_or(0);

    // 每次记账用独立作用域：std Mutex guard 必须在下次加锁前 drop，
    // 否则 and_then 闭包内的第二次 lock 会死锁（非重入锁）。
    let call_result = {
        let mut budget = shared.budget.lock().unwrap();
        budget.record_call(&session_id)
    };
    let budget_result = call_result.and_then(|_| {
        let mut budget = shared.budget.lock().unwrap();
        budget.record_output_bytes(&session_id, payload_bytes)
    });
    if let Err(err) = budget_result {
        shared.record(DiagnosticRecord {
            level: DiagnosticLevel::Warning,
            code: "budget_exceeded".into(),
            message: format!("event dropped: {err}"),
            extension_id: event.extension_id.clone(),
            context: Default::default(),
        });
        return;
    }
    // session 终结事件：清零该 session 的记账（预算按 session 计）。
    if event.kind == ExtensionEventKind::SessionEnd {
        shared.budget.lock().unwrap().reset_session(&session_id);
    }

    // 只有非 gate 事件在此派发。
    if event_gate(event.kind).is_some() {
        return;
    }
    let Some(registry) = shared.registry() else {
        return;
    };
    let executed = dispatch_observe(registry, &shared, &event).await;
    if executed > 0 {
        shared.record(DiagnosticRecord {
            level: DiagnosticLevel::Debug,
            code: "hook_dispatched".into(),
            message: format!(
                "dispatched {} hook(s) for event {}",
                executed,
                event.kind.as_str()
            ),
            extension_id: event.extension_id.clone(),
            context: Default::default(),
        });
    }
}

#[cfg(test)]
mod tests_host;
