//! `LspService`：LSP server 生命周期治理（Arc handle + actor，与
//! [`crate::service::CodeIntelligenceService`] 平级，共用 `api.rs` 出口）。
//!
//! 生命周期（状态机见 [`crate::lsp::state`]）：
//!
//! ```text
//! Idle ─► Starting ─► Initializing ─► Ready ──(崩溃/传输死/liveness 失败)──┐
//!          │            │              │                                    │
//!          │(spawn 失败)│(握手失败)     └────────► Reconnecting ─► Starting ─┘
//!          ▼            ▼                          │ (指数退避 + 文档 replay)
//!        Failed        Reconnecting(attempt+1)     ▼ (次数用尽)
//!                                                Failed
//! 任意状态 ─► ShuttingDown ─► Stopped
//! ```
//!
//! - **document replay**：文档状态（open/change/close + 最新文本）由 actor
//!   独占维护；server 重启后按 uri 排序重发 `didOpen`（最新文本 + 版本）。
//!   文档操作在 server 未就绪时仍更新本地状态，恢复后自动收敛。
//! - **诊断**：push（`publishDiagnostics` 通知）入库 + stale policy（见
//!   [`crate::lsp::diagnostics`]）；pull（`textDocument/pullDiagnostics`）
//!   经网络请求。只存储已打开文档的诊断。
//! - **查询**（hover/definition/references）在独立 task 中执行（不阻塞
//!   actor 的命令顺序处理），经共享 session 快照转发。
//! - **edit**：`workspace/applyEdit` 请求 → 校验 → [`EditPlan`] → 注入的
//!   [`EditApplicator`] 受限应用（ChangeReceipt）；无 applicator 时拒绝
//!   并记录计划（`pending_edits`）。**本模块绝不直接写磁盘。**
//! - **shutdown**（确定性）：状态 `ShuttingDown`（新命令被拒）→ 取消令牌
//!   （在途网络请求立即失败）→ `shutdown` 请求（2s 超时）→ `exit` 通知 →
//!   终止子进程并回收读循环 → `Stopped`。重复 shutdown 幂等。
//! - **panic 纪律**：命令处理在 actor 内执行，panic 使 actor 任务失败，
//!   [`LspTask::join`] 捕获（fail closed，join 不传播）；网络 task panic
//!   独立捕获（回执 `QueryPanicked`）。

// Evo 独立设计：Grok 的 LSP 客户端生命周期由 async-lsp 的 ClientState
// 驱动（无 document replay / 无 sandbox / 无 task ownership）；Evo 的
// actor 模式、replay、restart/backoff、edit 转换层为自研（参照
// extension-host 的 handle/task 模式）。
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use workspace_runtime::api::{EnvPolicy, SandboxProfile, TaskOwner};

use crate::lsp::diagnostics::{DiagnosticStore, StalePolicy, StoredDiagnostics};
use crate::lsp::documents::{ContentChange, DocumentError, DocumentStore, OpenDocument};
use crate::lsp::edit::{EditApplicator, EditError, EditPlan};
use crate::lsp::query::{LspQuery, LspQueryResult};
use crate::lsp::state::LspLifecycleState;
use crate::lsp::transport::{self, LspSession, RpcError, ServerRequestReply};
use crate::lsp::wire::{self, Notification};
use change_tracker::ChangeReceipt;

/// actor 实现（生命周期驱动 / 命令处理），见 `actor` 子模块。
mod actor;
use actor::run_actor;

/// 默认请求超时。
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// 默认 liveness ping 间隔。
pub const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(30);
/// 默认 liveness ping 超时。
pub const DEFAULT_PING_TIMEOUT: Duration = Duration::from_secs(10);
/// 默认重连初始退避。
pub const DEFAULT_BACKOFF_INITIAL: Duration = Duration::from_millis(500);
/// 默认重连最大退避。
pub const DEFAULT_BACKOFF_MAX: Duration = Duration::from_secs(30);
/// 默认最大重启尝试次数（超过后 Failed 终态）。
pub const DEFAULT_MAX_RESTART_ATTEMPTS: u32 = 10;
/// 默认单帧上限。
pub const DEFAULT_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// 命令通道容量（有界背压）。
const COMMAND_QUEUE_CAPACITY: usize = 64;

/// liveness 配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LivenessConfig {
    pub ping_interval: Duration,
    pub ping_timeout: Duration,
}

impl Default for LivenessConfig {
    fn default() -> Self {
        Self {
            ping_interval: DEFAULT_PING_INTERVAL,
            ping_timeout: DEFAULT_PING_TIMEOUT,
        }
    }
}

/// 重启退避配置（指数退避，封顶）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffConfig {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial: DEFAULT_BACKOFF_INITIAL,
            max: DEFAULT_BACKOFF_MAX,
        }
    }
}

/// LSP server 配置。
#[derive(Clone)]
pub struct LspServerConfig {
    pub command: String,
    pub args: Vec<String>,
    /// 环境白名单（`Inherit` 会继承宿主全部环境，仅允许显式配置）。
    pub env: EnvPolicy,
    /// 工作目录；`None` = workspace_root。
    pub cwd: Option<PathBuf>,
    /// workspace 根（uri 校验 / document 定位的基准）。
    pub workspace_root: PathBuf,
    /// 沙箱配置；`None` = [`SandboxProfile::product_default`]（能力不足
    /// 平台 spawn fail closed）。
    pub sandbox: Option<SandboxProfile>,
    /// background task ownership（ARC-610 原则：进程归属 owner，shutdown
    /// 按 owner 终止）。
    pub task_owner: TaskOwner,
    pub backoff: BackoffConfig,
    /// 重启尝试上限（超过后 Failed 终态，需显式 shutdown）。
    pub max_restart_attempts: u32,
    pub liveness: LivenessConfig,
    pub request_timeout: Duration,
    /// 单帧上限（防输出洪泛 / 超大帧）。
    pub max_frame_bytes: usize,
    pub stale_policy: StalePolicy,
    /// edit 应用器（授权边界注入点；`None` 时 applyEdit 被拒绝并记录）。
    pub applicator: Option<Arc<dyn EditApplicator>>,
}

impl LspServerConfig {
    pub fn new(command: impl Into<String>, workspace_root: PathBuf, owner: TaskOwner) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            env: EnvPolicy::AllowList(Default::default()),
            cwd: None,
            workspace_root,
            sandbox: None,
            task_owner: owner,
            backoff: BackoffConfig::default(),
            max_restart_attempts: DEFAULT_MAX_RESTART_ATTEMPTS,
            liveness: LivenessConfig::default(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            stale_policy: StalePolicy::Mark,
            applicator: None,
        }
    }

    fn session_config(&self) -> transport::LspSessionConfig {
        transport::LspSessionConfig {
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            cwd: self
                .cwd
                .clone()
                .unwrap_or_else(|| self.workspace_root.clone()),
            sandbox: self
                .sandbox
                .clone()
                .unwrap_or_else(|| SandboxProfile::product_default(&self.workspace_root)),
            max_frame_bytes: self.max_frame_bytes,
        }
    }
}

/// LSP 服务的错误分类。
#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("lsp service is not running")]
    NotRunning,
    #[error("lsp service is shutting down: {reason}")]
    ShuttingDown { reason: String },
    #[error("lsp server is not ready (state: {state})")]
    NotReady { state: String },
    #[error("lsp service is already running")]
    AlreadyRunning,
    #[error("lsp network task panicked")]
    QueryPanicked,
    #[error(transparent)]
    Document(#[from] DocumentError),
    #[error(transparent)]
    Edit(#[from] EditError),
    #[error(transparent)]
    Rpc(#[from] RpcError),
}

/// 服务只读信息。
struct LspInfo {
    config: LspServerConfig,
}

/// 服务 / handle 共享的可变状态。
#[derive(Debug)]
struct LspShared {
    state: Mutex<LspLifecycleState>,
    shutdown_tx: Mutex<Option<watch::Sender<bool>>>,
    restart_count: Mutex<u32>,
    last_error: Mutex<Option<String>>,
    /// 被拒绝的 edit 计划（无 applicator 时记录，供调用方查询）。
    pending_edits: Mutex<Vec<EditPlan>>,
    /// 已应用的 edit 的 ChangeReceipt（ARC-830 消费）。
    change_receipts: Mutex<Vec<ChangeReceipt>>,
}

/// LSP 服务（Arc handle + actor）。
#[derive(Clone)]
pub struct LspService {
    info: Arc<LspInfo>,
    shared: Arc<LspShared>,
}

/// 服务快照（`snapshot()` 查询）。
#[derive(Debug, Clone)]
pub struct LspSnapshot {
    pub state: LspLifecycleState,
    pub pid: Option<u32>,
    pub restart_count: u32,
    pub open_documents: Vec<OpenDocument>,
    pub diagnostics: Vec<StoredDiagnostics>,
    pub last_error: Option<String>,
}

/// 运行时 handle。
#[derive(Debug, Clone)]
pub struct LspHandle {
    shared: Arc<LspShared>,
    tx: mpsc::Sender<LspCommand>,
}

/// actor 退出报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspExit {
    pub reason: LspShutdownReason,
    pub restart_count: u32,
    pub handled_commands: u64,
    pub panicked: bool,
}

/// actor 退出原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspShutdownReason {
    /// handle 显式触发。
    Manual,
    /// 所有 handle 被 drop（channel 关闭）。
    SendersDropped,
    /// 命令处理 panic（fail closed）。
    Panic,
}

/// task join 句柄。
#[derive(Debug)]
pub struct LspTask {
    join: tokio::task::JoinHandle<LspExit>,
}

/// 命令（经 handle 提交，actor 顺序处理）。
enum LspCommand {
    Open {
        uri: String,
        language_id: String,
        version: i64,
        text: String,
        reply: oneshot::Sender<Result<(), LspError>>,
    },
    Change {
        uri: String,
        version: i64,
        changes: Vec<ContentChange>,
        reply: oneshot::Sender<Result<(), LspError>>,
    },
    Close {
        uri: String,
        reply: oneshot::Sender<Result<(), LspError>>,
    },
    Diagnostics {
        uri: String,
        reply: oneshot::Sender<Result<Option<StoredDiagnostics>, LspError>>,
    },
    PullDiagnostics {
        uri: String,
        reply: oneshot::Sender<Result<StoredDiagnostics, LspError>>,
    },
    Query {
        query: LspQuery,
        reply: oneshot::Sender<Result<LspQueryResult, LspError>>,
    },
    Snapshot {
        reply: oneshot::Sender<Result<LspSnapshot, LspError>>,
    },
    PendingEdits {
        reply: oneshot::Sender<Result<Vec<EditPlan>, LspError>>,
    },
}

/// actor 独占状态。
struct LspActor {
    config: LspServerConfig,
    shared: Arc<LspShared>,
    documents: DocumentStore,
    diagnostics_store: DiagnosticStore,
    session: Option<Arc<LspSession>>,
    cancel: CancellationToken,
    attempt: u32,
}

/// actor 的事件源（与 actor 状态分离，select 可字段级借用）。
struct LspEvents {
    commands_rx: mpsc::Receiver<LspCommand>,
    shutdown_rx: watch::Receiver<bool>,
    session_died: Option<watch::Receiver<bool>>,
    notifications_rx: Option<mpsc::UnboundedReceiver<Notification>>,
    server_requests_rx: Option<mpsc::UnboundedReceiver<(wire::Request, ServerRequestReply)>>,
}

impl LspService {
    /// 构造服务（不启动；`start()` 后才有句柄）。
    pub fn new(config: LspServerConfig) -> Self {
        let shared = Arc::new(LspShared {
            state: Mutex::new(LspLifecycleState::Idle),
            shutdown_tx: Mutex::new(None),
            restart_count: Mutex::new(0),
            last_error: Mutex::new(None),
            pending_edits: Mutex::new(Vec::new()),
            change_receipts: Mutex::new(Vec::new()),
        });
        Self {
            info: Arc::new(LspInfo { config }),
            shared,
        }
    }

    /// 启动后台 actor。只能启动一次。
    pub fn start(self) -> Result<(LspHandle, LspTask), LspError> {
        {
            let mut state = self.shared.state.lock().unwrap();
            if *state != LspLifecycleState::Idle {
                return Err(LspError::AlreadyRunning);
            }
            // 同步进入 Starting（start 返回后 is_running 立即为真；
            // 状态机推进由 actor 负责）。
            *state = LspLifecycleState::Starting { attempt: 1 };
        }
        let (tx, rx) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        *self.shared.shutdown_tx.lock().unwrap() = Some(shutdown_tx);

        let shared = self.shared.clone();
        let info = self.info.clone();
        let join = tokio::spawn(async move {
            let actor = LspActor::new(info, shared);
            let events = LspEvents {
                commands_rx: rx,
                shutdown_rx,
                session_died: None,
                notifications_rx: None,
                server_requests_rx: None,
            };
            run_actor(actor, events).await
        });
        let handle = LspHandle {
            shared: self.shared.clone(),
            tx,
        };
        Ok((handle, LspTask { join }))
    }

    pub fn state(&self) -> LspLifecycleState {
        self.shared.state.lock().unwrap().clone()
    }
}

impl LspHandle {
    /// 提交门禁：非只读命令在 Failed 终态拒绝（只读命令仍可用作诊断）。
    fn gate(state: &LspLifecycleState, allow_failed: bool) -> Result<(), LspError> {
        match state {
            LspLifecycleState::Idle | LspLifecycleState::Stopped => Err(LspError::NotRunning),
            LspLifecycleState::ShuttingDown => Err(LspError::ShuttingDown {
                reason: "lsp shutdown in progress".into(),
            }),
            LspLifecycleState::Failed { .. } if !allow_failed => Err(LspError::NotReady {
                state: state.as_str().into(),
            }),
            _ => Ok(()),
        }
    }

    async fn dispatch<T>(
        &self,
        send: impl FnOnce(oneshot::Sender<Result<T, LspError>>) -> LspCommand,
    ) -> Result<T, LspError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(send(reply))
            .await
            .map_err(|_| LspError::NotRunning)?;
        response.await.map_err(|_| LspError::QueryPanicked)?
    }

    async fn submit<T>(
        &self,
        send: impl FnOnce(oneshot::Sender<Result<T, LspError>>) -> LspCommand,
    ) -> Result<T, LspError> {
        Self::gate(&self.shared.state.lock().unwrap().clone(), false)?;
        self.dispatch(send).await
    }

    async fn submit_readonly<T>(
        &self,
        send: impl FnOnce(oneshot::Sender<Result<T, LspError>>) -> LspCommand,
    ) -> Result<T, LspError> {
        Self::gate(&self.shared.state.lock().unwrap().clone(), true)?;
        self.dispatch(send).await
    }

    /// 打开文档（本地状态立即更新；server 就绪时发 `didOpen`）。
    pub async fn open(
        &self,
        uri: &str,
        language_id: &str,
        version: i64,
        text: &str,
    ) -> Result<(), LspError> {
        self.submit(|reply| LspCommand::Open {
            uri: uri.to_string(),
            language_id: language_id.to_string(),
            version,
            text: text.to_string(),
            reply,
        })
        .await
    }

    /// 应用内容变更（版本单调校验 + 同版本合并；就绪时发全量 `didChange`）。
    pub async fn change(
        &self,
        uri: &str,
        version: i64,
        changes: Vec<ContentChange>,
    ) -> Result<(), LspError> {
        self.submit(|reply| LspCommand::Change {
            uri: uri.to_string(),
            version,
            changes,
            reply,
        })
        .await
    }

    /// 关闭文档（就绪时发 `didClose`）。
    pub async fn close(&self, uri: &str) -> Result<(), LspError> {
        self.submit(|reply| LspCommand::Close {
            uri: uri.to_string(),
            reply,
        })
        .await
    }

    /// 查询本地诊断存储（push 结果，按 stale policy 投影）。
    pub async fn diagnostics(&self, uri: &str) -> Result<Option<StoredDiagnostics>, LspError> {
        self.submit(|reply| LspCommand::Diagnostics {
            uri: uri.to_string(),
            reply,
        })
        .await
    }

    /// 发起 `textDocument/pullDiagnostics` 请求并入库。
    pub async fn pull_diagnostics(&self, uri: &str) -> Result<StoredDiagnostics, LspError> {
        self.submit(|reply| LspCommand::PullDiagnostics {
            uri: uri.to_string(),
            reply,
        })
        .await
    }

    /// read-only 查询（hover / definition / references）。
    pub async fn query(&self, query: LspQuery) -> Result<LspQueryResult, LspError> {
        self.submit(|reply| LspCommand::Query { query, reply })
            .await
    }

    /// 服务快照（状态 / pid / 重启数 / 打开文档 / 诊断 / 最近错误）。
    /// 只读：Failed 终态下仍可用（诊断）。
    pub async fn snapshot(&self) -> Result<LspSnapshot, LspError> {
        self.submit_readonly(|reply| LspCommand::Snapshot { reply })
            .await
    }

    /// 被拒绝的 edit 计划（无 applicator 时的记录）。只读。
    pub async fn pending_edits(&self) -> Result<Vec<EditPlan>, LspError> {
        self.submit_readonly(|reply| LspCommand::PendingEdits { reply })
            .await
    }

    /// 触发确定性 shutdown（幂等）。
    pub fn shutdown(&self) {
        if let Some(tx) = self.shared.shutdown_tx.lock().unwrap().as_ref() {
            let _ = tx.send(true);
        }
    }

    pub fn state(&self) -> LspLifecycleState {
        self.shared.state.lock().unwrap().clone()
    }

    pub fn is_running(&self) -> bool {
        !matches!(
            self.state(),
            LspLifecycleState::Idle | LspLifecycleState::Stopped | LspLifecycleState::ShuttingDown
        )
    }
}

impl LspTask {
    /// 等待 actor 结束（panic 不传播）。
    pub async fn join(self) -> LspExit {
        match self.join.await {
            Ok(exit) => exit,
            Err(_) => LspExit {
                reason: LspShutdownReason::Panic,
                restart_count: 0,
                handled_commands: 0,
                panicked: true,
            },
        }
    }
}
