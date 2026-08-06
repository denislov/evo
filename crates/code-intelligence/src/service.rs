//! `CodeIntelligenceService`：Arc handle + actor 的服务 API 边界。
//!
//! 生命周期（参照 `extension-host` 的 `ExtensionHost` / `Handle` / `Task`
//! 模式，Phase 7）：
//!
//! 1. [`CodeIntelligenceService::new`]（同步）：探测索引缓存（失败投影为
//!    `CacheStatus::RebuildRequired`，不 panic）。
//! 2. [`CodeIntelligenceService::start`]（async 能力就绪后返回 handle /
//!    task）：启动后台 dispatch task，顺序处理查询请求。
//! 3. [`CodeIntelligenceHandle::submit`]：提交查询（`Status` 由 actor 直接
//!    回答；其余 kind 委托 [`QueryBackend`]——ARC-810/820 实现该 trait
//!    即可接入，无需改动 actor）。
//! 4. [`CodeIntelligenceHandle::shutdown`]：确定性顺序 —— 状态置
//!    `Stopping`（新提交被拒）+ 发 watch 信号 -> actor 退出并拒绝队列中
//!    未处理请求（`ShuttingDown`，cancel 语义）-> 状态置 `Stopped`；
//!    随后 [`CodeIntelligenceTask::join`] 返回 [`ServiceExit`]。重复
//!    shutdown 幂等。
//!
//! 查询处理在独立 task 中执行：backend panic 被捕获（fail closed），
//! join 不传播 panic。所有 handle 被 drop（无 shutdown）时 channel 关闭，
//! actor 自行退出（`SendersDropped`）。

// Evo 独立设计：Grok 的 IndexManager 无服务生命周期概念（channel actor 只
// 处理文件事件）；Evo 的 handle/task/shutdown 顺序/panic 捕获参照
// extension-host 的 host 模式（Phase 7）自研。
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};

use crate::budget::{BudgetSnapshot, IndexBudget, IndexBudgetTracker};
use crate::cache::{CacheStatus, probe_cache};
use crate::error::CodeIntelligenceError;
use crate::identity::CacheIdentity;
use crate::languages::LanguageRegistry;

/// 查询队列容量（有界背压）。
const QUERY_QUEUE_CAPACITY: usize = 32;

/// 服务生命周期状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Idle,
    Running,
    Stopping,
    Stopped,
}

impl ServiceState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
        }
    }
}

/// 查询类型。骨架只实现 `Status`；其余 kind 预留给 ARC-810（graph）与
/// ARC-820（LSP），提交后返回 `Unimplemented` 错误（kind 带 phase 归属）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryKind {
    /// 索引状态（骨架实现）：identity / 缓存状态 / 预算快照。
    Status,
    /// ARC-810：按路径查询文件内 symbol。
    FileSymbols,
    /// ARC-810：全局 symbol 定义查询。
    Definition,
    /// ARC-810：symbol 引用查询。
    Reference,
    /// ARC-820：文档诊断查询。
    Diagnostics,
}

impl QueryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::FileSymbols => "file_symbols",
            Self::Definition => "definition",
            Self::Reference => "reference",
            Self::Diagnostics => "diagnostics",
        }
    }

    /// 未实现 kind 归属的 phase（错误信息用）。
    fn phase(&self) -> &'static str {
        match self {
            Self::Status => "ARC-800",
            Self::FileSymbols | Self::Definition | Self::Reference => "ARC-810",
            Self::Diagnostics => "ARC-820",
        }
    }
}

/// 查询请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryRequest {
    pub kind: QueryKind,
    /// 请求上下文（文件路径、symbol 名等）。骨架不解释字段；
    /// ARC-810/820 定义各自的上下文结构。
    #[serde(default)]
    pub context: serde_json::Value,
}

impl QueryRequest {
    pub fn new(kind: QueryKind, context: serde_json::Value) -> Self {
        Self { kind, context }
    }

    /// `Status` 查询。
    pub fn status() -> Self {
        Self::new(QueryKind::Status, serde_json::Value::Null)
    }
}

/// 索引状态（所有查询响应都携带）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexStatus {
    pub state: ServiceState,
    pub identity: CacheIdentity,
    pub cache: CacheStatus,
    pub budget: BudgetSnapshot,
}

/// 查询响应。骨架只有 `status` 字段；ARC-810/820 在此追加各自的结果字段
/// （新增字段必须带 `#[serde(default)]` 保持兼容）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryResponse {
    pub kind: QueryKind,
    pub status: IndexStatus,
}

/// 查询后端 trait：ARC-810（graph）与 ARC-820（LSP diagnostics）各自实现
/// 并注入 [`CodeIntelligenceServiceOptions::backend`]；骨架默认
/// [`SkeletonQueryBackend`] 对未实现 kind 返回 `Unimplemented`。
pub trait QueryBackend: Send + Sync {
    fn query(&self, request: &QueryRequest) -> Result<QueryResponse, CodeIntelligenceError>;
}

/// 骨架默认后端：全部非 `Status` kind 返回 `Unimplemented`。
#[derive(Debug, Clone, Default)]
pub struct SkeletonQueryBackend;

impl QueryBackend for SkeletonQueryBackend {
    fn query(&self, request: &QueryRequest) -> Result<QueryResponse, CodeIntelligenceError> {
        Err(CodeIntelligenceError::Unimplemented {
            kind: request.kind.as_str().to_string(),
            phase: request.kind.phase(),
        })
    }
}

/// 服务启动参数。
#[derive(Clone)]
pub struct CodeIntelligenceServiceOptions {
    /// 缓存 identity（workspace / revision / parser-version）。
    pub identity: CacheIdentity,
    /// 缓存文件路径；`None` = 纯内存（不持久化）。
    pub cache_path: Option<PathBuf>,
    /// 索引预算上限。
    pub budget: IndexBudget,
    /// 语言注册表（ARC-810 的 backend 使用；骨架仅持有）。
    pub languages: LanguageRegistry,
    /// 查询后端；`None` = [`SkeletonQueryBackend`]。
    pub backend: Option<Arc<dyn QueryBackend>>,
}

impl Default for CodeIntelligenceServiceOptions {
    fn default() -> Self {
        Self {
            identity: CacheIdentity {
                workspace: workspace_runtime::api::WorkspaceId::parse("source-skeleton").unwrap(),
                revision: crate::identity::RevisionId::parse("skeleton").unwrap(),
                parser_version: crate::identity::ParserVersion::Legacy,
            },
            cache_path: None,
            budget: IndexBudget::default(),
            languages: LanguageRegistry::builtin(),
            backend: None,
        }
    }
}

/// 服务只读信息（new 时确定，此后不变）。
struct ServiceInfo {
    options: CodeIntelligenceServiceOptions,
}

/// 服务 / handle 共享的可变状态。
#[derive(Debug)]
struct ServiceShared {
    state: Mutex<ServiceState>,
    shutdown_tx: Mutex<Option<watch::Sender<bool>>>,
    budget: Mutex<IndexBudgetTracker>,
    cache_status: Mutex<CacheStatus>,
}

/// 代码智能服务。
#[derive(Clone)]
pub struct CodeIntelligenceService {
    info: Arc<ServiceInfo>,
    shared: Arc<ServiceShared>,
}

impl CodeIntelligenceService {
    /// 构造服务并探测索引缓存（探测失败投影为 `RebuildRequired`，不 panic）。
    pub fn new(options: CodeIntelligenceServiceOptions) -> Self {
        let cache_status = probe_cache(options.cache_path.as_deref(), &options.identity);
        let shared = Arc::new(ServiceShared {
            state: Mutex::new(ServiceState::Idle),
            shutdown_tx: Mutex::new(None),
            budget: Mutex::new(IndexBudgetTracker::new(options.budget)),
            cache_status: Mutex::new(cache_status),
        });
        Self {
            info: Arc::new(ServiceInfo { options }),
            shared,
        }
    }

    /// 启动后台 dispatch task。只能启动一次。
    pub fn start(
        self,
    ) -> Result<(CodeIntelligenceHandle, CodeIntelligenceTask), CodeIntelligenceError> {
        {
            let mut state = self.shared.state.lock().unwrap();
            if *state != ServiceState::Idle {
                return Err(CodeIntelligenceError::AlreadyRunning);
            }
            *state = ServiceState::Running;
        }
        let (tx, rx) = mpsc::channel(QUERY_QUEUE_CAPACITY);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        *self.shared.shutdown_tx.lock().unwrap() = Some(shutdown_tx);

        let backend: Arc<dyn QueryBackend> = self
            .info
            .options
            .backend
            .clone()
            .unwrap_or_else(|| Arc::new(SkeletonQueryBackend));
        let identity = self.info.options.identity.clone();
        let shared = self.shared.clone();
        let join =
            tokio::spawn(
                async move { dispatch_loop(rx, shutdown_rx, shared, backend, identity).await },
            );
        let handle = CodeIntelligenceHandle {
            shared: self.shared.clone(),
            tx,
        };
        let task = CodeIntelligenceTask { join };
        Ok((handle, task))
    }

    /// 当前生命周期状态。
    pub fn state(&self) -> ServiceState {
        self.shared.state.lock().unwrap().clone()
    }

    /// 启动参数视图（ARC-810/820 的 backend 查询用）。
    pub fn options(&self) -> &CodeIntelligenceServiceOptions {
        &self.info.options
    }
}

/// 运行时 handle：提交查询 / 触发 shutdown。
#[derive(Debug, Clone)]
pub struct CodeIntelligenceHandle {
    shared: Arc<ServiceShared>,
    tx: mpsc::Sender<QueryEnvelope>,
}

/// 带响应通道的查询信封。
struct QueryEnvelope {
    request: QueryRequest,
    reply: oneshot::Sender<Result<QueryResponse, CodeIntelligenceError>>,
}

impl CodeIntelligenceHandle {
    /// 提交一个查询并等待响应。服务未运行或正在停止时拒绝；
    /// 处理 task panic 时响应丢失，返回 [`CodeIntelligenceError::QueryPanicked`]。
    pub async fn submit(
        &self,
        request: QueryRequest,
    ) -> Result<QueryResponse, CodeIntelligenceError> {
        let state = self.shared.state.lock().unwrap().clone();
        match state {
            ServiceState::Running => {}
            ServiceState::Stopping => {
                return Err(CodeIntelligenceError::ShuttingDown {
                    reason: "service shutdown in progress".into(),
                });
            }
            _ => return Err(CodeIntelligenceError::NotRunning),
        }
        let (reply, response) = oneshot::channel();
        self.tx
            .send(QueryEnvelope { request, reply })
            .await
            .map_err(|_| CodeIntelligenceError::NotRunning)?;
        response
            .await
            .map_err(|_| CodeIntelligenceError::QueryPanicked)?
    }

    /// 触发确定性 shutdown（幂等）。随后用 [`CodeIntelligenceTask::join`]
    /// 回收：顺序为状态置 `Stopping`（新提交被拒）-> 发 watch 信号
    /// （actor 退出并拒绝队列中未处理请求）。
    pub fn shutdown(&self, reason: impl Into<String>) {
        let _ = reason.into();
        {
            let mut state = self.shared.state.lock().unwrap();
            if *state == ServiceState::Running {
                *state = ServiceState::Stopping;
            }
            // Idle / Stopping / Stopped：幂等，不再变更。
        }
        if let Some(tx) = self.shared.shutdown_tx.lock().unwrap().as_ref() {
            let _ = tx.send(true);
        }
    }

    pub fn is_running(&self) -> bool {
        *self.shared.state.lock().unwrap() == ServiceState::Running
    }
}

/// 后台 dispatch task 的 join 句柄。
#[derive(Debug)]
pub struct CodeIntelligenceTask {
    join: tokio::task::JoinHandle<ServiceExit>,
}

/// dispatch task 的退出原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceShutdownReason {
    /// handle 显式触发。
    Manual,
    /// 所有 handle 被丢弃（channel 关闭）。
    SendersDropped,
    /// 处理查询时 backend panic（fail closed）。
    Panic,
}

/// dispatch task 的退出报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceExit {
    pub reason: ServiceShutdownReason,
    pub handled_queries: u64,
    pub panicked: bool,
}

impl CodeIntelligenceTask {
    /// 等待 dispatch task 结束（panic 不会传播）。调用后服务进入终态。
    pub async fn join(self) -> ServiceExit {
        match self.join.await {
            Ok(exit) => exit,
            Err(_) => ServiceExit {
                reason: ServiceShutdownReason::Panic,
                handled_queries: 0,
                panicked: true,
            },
        }
    }
}

/// dispatch 主循环：顺序处理查询直到 shutdown 信号或所有 sender 退出；
/// 退出时拒绝队列中未处理请求（cancel 语义）。shutdown 信号到达后，
/// 当前 in-flight 请求仍完成（其响应照常返回），之后确定性退出——队列
/// 中未处理的请求统一收到 `ShuttingDown`。
///
/// `Status` 由 actor 直接回答（identity 来自构造期的 info）；其余 kind 在
/// 独立 task 中执行 backend（panic 被捕获为 `JoinError`，fail closed）。
async fn dispatch_loop(
    mut rx: mpsc::Receiver<QueryEnvelope>,
    mut shutdown_rx: watch::Receiver<bool>,
    shared: Arc<ServiceShared>,
    backend: Arc<dyn QueryBackend>,
    identity: CacheIdentity,
) -> ServiceExit {
    let mut handled: u64 = 0;
    let mut panicked = false;

    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                let _ = changed; // watch sender 已 drop 或值已变化，都退出。
                break;
            }
            envelope = rx.recv() => {
                let Some(envelope) = envelope else {
                    break; // 所有 sender 已 drop。
                };
                if envelope.request.kind == QueryKind::Status {
                    let state = shared.state.lock().unwrap().clone();
                    let budget = shared.budget.lock().unwrap().snapshot();
                    let cache = shared.cache_status.lock().unwrap().clone();
                    let response = QueryResponse {
                        kind: QueryKind::Status,
                        status: IndexStatus { state, identity: identity.clone(), cache, budget },
                    };
                    let _ = envelope.reply.send(Ok(response));
                    handled += 1;
                    if *shutdown_rx.borrow() {
                        break;
                    }
                    continue;
                }
                let backend = backend.clone();
                let task = tokio::spawn(async move {
                    let result = backend.query(&envelope.request);
                    let _ = envelope.reply.send(result);
                });
                match task.await {
                    Ok(()) => {
                        handled += 1;
                        // shutdown 信号已到：完成当前请求后确定性退出
                        // （避免 select 公平性导致已排队的请求被继续处理）。
                        if *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    Err(error) if error.is_panic() => {
                        panicked = true;
                        break;
                    }
                    Err(_) => break, // task 被 abort：视同 panic，停止派发。
                }
            }
        }
    }

    // 确定性 shutdown：拒绝队列中未处理请求（cancel 语义，有界）。
    while let Ok(envelope) = rx.try_recv() {
        let _ = envelope
            .reply
            .send(Err(CodeIntelligenceError::ShuttingDown {
                reason: "service shutdown in progress".into(),
            }));
    }

    let reason = if panicked {
        ServiceShutdownReason::Panic
    } else if *shutdown_rx.borrow() {
        ServiceShutdownReason::Manual
    } else {
        ServiceShutdownReason::SendersDropped
    };
    *shared.state.lock().unwrap() = ServiceState::Stopped;

    ServiceExit {
        reason,
        handled_queries: handled,
        panicked,
    }
}
