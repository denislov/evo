//! MCP server 生命周期：连接 → initialize 握手 → 工具发现 → liveness →
//! 断线重连 → 确定性 shutdown。
//!
//! 每个启用的 [`McpServerConfig`] 由 [`McpServerTask`] 驱动下面的状态机
//! （transition 表测试见 `lifecycle/tests_state.rs`）：
//!
//! ```text
//!                 ┌──────────────┐
//!                 ▼              │ (liveness 失败 / transport 死 / 进程退出)
//! Disconnected ─► Connecting ─► Initializing ─► Ready ──────────────┐
//!                  ▲               │                                  │
//!                  │               │ (initialize/tools.list 失败)     │
//!                  └─────── Reconnecting ◄── (backoff, 重试) ─────────┘
//!  任何状态 ──► ShuttingDown ──► Terminated
//! ```
//!
//! - **initialize 握手**：`initialize`（协议版本协商，接受服务器返回的
//!   `protocolVersion`）→ `notifications/initialized` → `tools/list` →
//!   缓存工具并 bump 版本号。
//! - **liveness**：`Ready` 下按 [`LivenessConfig::ping_interval`] 发
//!   `ping`，`ping_timeout` 内无响应判定死亡，进入重连。
//! - **reconnect**：指数退避（`initial_backoff` 起、`max_backoff` 封顶），
//!   重连后**重新 initialize + 重新发现工具**（工具集以服务器为准）。
//! - **tools/list_changed**：`Ready` 下收到通知 → 重新 `tools/list` →
//!   更新缓存 + bump 版本号（meta tools 的 `mcp_search` 读到最新）。
//! - **per-tool 调用**：经 [`McpServerHandle::call_tool`] 转发
//!   `tools/call`；401 触发单次 refresh（或 device flow）后重试一次，
//!   见 [`crate::mcp::oauth`]。
//! - **shutdown**：状态置 `Stopping` → 全局取消令牌（在途调用立即失败）
//!   → 关闭会话（stdio 终止子进程）→ task 自行退出 → `Stopped`。

// Adapted from xai-grok-mcp, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// ClientStateKind discipline and McpError classification consulted;
// ping-based liveness, backoff reconnect, and tools_changed refresh are Evo's own.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::diagnostic::{DiagnosticLevel, DiagnosticRecord, DiagnosticSink, NoopDiagnosticSink};
use crate::mcp::credentials::McpCredentialStore;
use crate::mcp::oauth::{
    OAuthConfig, OAuthRuntime, device_flow_authenticate, refresh_access_token,
};
use crate::mcp::state::{LifecycleEvent, ServerLifecycleState, apply_event, backoff_for};
use crate::mcp::transport::{RpcError, RpcSession, TransportConfig};
use crate::mcp::wire::Notification;

/// 默认 per-tool 调用超时。
pub const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(60);
/// 默认 liveness ping 间隔。
pub const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(30);
/// 默认 liveness ping 超时。
pub const DEFAULT_PING_TIMEOUT: Duration = Duration::from_secs(10);
/// 默认重连初始退避。
pub const DEFAULT_RECONNECT_BASE: Duration = Duration::from_millis(500);
/// 默认重连最大退避。
pub const DEFAULT_RECONNECT_MAX: Duration = Duration::from_secs(30);

/// liveness 配置。
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// 重连退避配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectConfig {
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            initial_backoff: DEFAULT_RECONNECT_BASE,
            max_backoff: DEFAULT_RECONNECT_MAX,
        }
    }
}

/// 单个 MCP server 的声明配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    /// server 名（工具归属与 credential 键控）。
    pub name: String,
    pub transport: TransportConfig,
    /// `false` 时跳过（不 spawn）。
    pub enabled: bool,
    /// per-tool 调用超时。
    pub tool_timeout: Duration,
    pub liveness: LivenessConfig,
    pub reconnect: ReconnectConfig,
    /// 配置了 OAuth device flow 时，401 且 refresh 失败会触发一次。
    pub oauth: Option<OAuthConfig>,
}

impl McpServerConfig {
    pub fn new(name: impl Into<String>, transport: TransportConfig) -> Self {
        Self {
            name: name.into(),
            transport,
            enabled: true,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
            liveness: LivenessConfig::default(),
            reconnect: ReconnectConfig::default(),
            oauth: None,
        }
    }
}

/// 已发现并缓存的一个 MCP 工具。
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredTool {
    pub server: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// 从 `tools/list` 结果转换工具：非法条目（非对象 / 缺 name / 空名）返回
/// 结构化错误，由调用方决定跳过并诊断。
pub fn convert_tool(server: &str, raw: &Value) -> Result<DiscoveredTool, String> {
    let object = raw
        .as_object()
        .ok_or_else(|| "tool entry is not an object".to_string())?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "tool entry lacks string 'name'".to_string())?;
    if name.is_empty() || name.len() > 128 {
        return Err(format!("invalid tool name '{name}'"));
    }
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let input_schema = object
        .get("inputSchema")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"type": "object"}));
    Ok(DiscoveredTool {
        server: server.to_string(),
        name: name.to_string(),
        description,
        input_schema,
    })
}

/// server 的调用句柄（`McpHost` 暴露给 meta tools / 产品）。
#[derive(Debug, Clone)]
pub struct McpServerHandle {
    config: McpServerConfig,
    /// 当前会话（`Ready` 时 `Some`；重连时被替换）。
    session: Arc<tokio::sync::Mutex<Option<Arc<RpcSession>>>>,
    /// host shutdown 时触发：在途调用立即失败。
    host_cancel: CancellationToken,
    credential_store: Arc<dyn McpCredentialStore>,
    oauth_runtime: OAuthRuntime,
    diagnostics: Arc<dyn DiagnosticSink>,
}

impl McpServerHandle {
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// 结构化诊断（坏工具 / 重连 / OAuth 恢复失败等）。
    fn record(&self, code: &str, message: impl Into<String>) {
        self.diagnostics.emit(DiagnosticRecord {
            level: DiagnosticLevel::Warning,
            code: code.into(),
            message: message.into(),
            extension_id: Some(self.config.name.clone()),
            context: Default::default(),
        });
    }

    /// 调用服务器工具。401 → 单次 refresh（有 refresh token）或
    /// device flow（配置了 OAuth）后重试一次；失败返回结构化错误。
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> Result<Value, RpcError> {
        let timeout = self.config.tool_timeout;
        let params = serde_json::json!({
            "name": tool_name,
            "arguments": arguments,
        });
        let first = self
            .request("tools/call", Some(params.clone()), timeout, cancel)
            .await;
        match first {
            Ok(result) => Ok(result),
            Err(error) if error.is_unauthorized() => {
                if self.try_credential_retry(cancel).await {
                    self.request("tools/call", Some(params), timeout, cancel)
                        .await
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(error),
        }
    }

    /// 请求转发：服务器不在 `Ready`（无会话）时给出结构化错误；host
    /// shutdown（`host_cancel`）时在途调用立即失败。
    async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Value, RpcError> {
        let session_guard = self.session.lock().await;
        let Some(session) = session_guard.as_ref() else {
            return Err(RpcError::Other(
                "MCP server is not connected (reconnecting or failed)".into(),
            ));
        };
        // 取 Arc 后立即释放锁：请求等待期间不阻塞重连替换会话。
        let session = Arc::clone(session);
        drop(session_guard);
        tokio::select! {
            biased;
            _ = self.host_cancel.cancelled() => Err(RpcError::Cancelled),
            result = session.request(method, params, timeout, cancel) => result,
        }
    }
}

impl McpServerHandle {
    /// 401 后的凭据恢复：refresh 优先，device flow 兜底；成功返回 `true`。
    async fn try_credential_retry(&self, cancel: &CancellationToken) -> bool {
        let Some(oauth) = self.config.oauth.as_ref() else {
            return false;
        };
        if let Some(credentials) = self.credential_store.get(&self.config.name)
            && let Some(refresh_token) = credentials.refresh_token
        {
            match refresh_access_token(oauth, &refresh_token, &self.oauth_runtime, cancel).await {
                Ok(credentials) => {
                    let _ = self.credential_store.set(&self.config.name, credentials);
                    return true;
                }
                Err(error) => {
                    self.record(
                        "mcp_refresh_failed",
                        format!("token refresh failed: {error}; falling back to device flow"),
                    );
                }
            }
        }
        match device_flow_authenticate(oauth, &self.oauth_runtime, cancel).await {
            Ok(credentials) => {
                let _ = self.credential_store.set(&self.config.name, credentials);
                true
            }
            Err(error) => {
                self.record(
                    "mcp_oauth_recovery_failed",
                    format!("OAuth recovery failed: {error}"),
                );
                false
            }
        }
    }
}

/// host 级共享状态。
#[derive(Debug)]
struct McpHostShared {
    servers: Vec<McpServerHandle>,
    /// server 名 → 已发现工具（`Ready` 时填充，`tools/list_changed` 刷新）。
    tools: RwLock<BTreeMap<String, Vec<DiscoveredTool>>>,
    /// 工具集版本号（每次发现 / 热更新 +1）。
    tools_version: watch::Sender<u64>,
    /// 保活 receiver：无订阅者时 `send` 会失败，版本号必须始终递增。
    _tools_version_rx: watch::Receiver<u64>,
    state: Mutex<HostMcpState>,
    cancel: CancellationToken,
    diagnostics: Arc<dyn DiagnosticSink>,
    /// server 名 → 生命周期状态（任务内更新，产品查询）。
    server_states: RwLock<BTreeMap<String, ServerLifecycleState>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostMcpState {
    Idle,
    Running,
    Stopping,
    Stopped,
}

/// MCP host：装配多个 MCP server 的生命周期与工具缓存。
///
/// [`McpHost::new`] 同步构造（校验配置）；[`McpHost::start`] 启动各
/// server task；[`McpHost::shutdown`] 确定性关闭。`Clone` 共享同一实例。
#[derive(Debug, Clone)]
pub struct McpHost {
    inner: Arc<McpHostShared>,
}

impl McpHost {
    pub fn new(
        configs: Vec<McpServerConfig>,
        credential_store: Arc<dyn McpCredentialStore>,
    ) -> Self {
        Self::with_runtime(configs, credential_store, OAuthRuntime::default())
    }

    /// 测试 / 产品注入 OAuth 运行时（mock HTTP、快速轮询）。
    pub fn with_runtime(
        configs: Vec<McpServerConfig>,
        credential_store: Arc<dyn McpCredentialStore>,
        oauth_runtime: OAuthRuntime,
    ) -> Self {
        Self::with_diagnostics(
            configs,
            credential_store,
            oauth_runtime,
            Arc::new(NoopDiagnosticSink),
        )
    }

    /// 注入诊断输出目标（坏工具条目 / 重连 / OAuth 恢复失败等落诊断）。
    pub fn with_diagnostics(
        configs: Vec<McpServerConfig>,
        credential_store: Arc<dyn McpCredentialStore>,
        oauth_runtime: OAuthRuntime,
        diagnostics: Arc<dyn DiagnosticSink>,
    ) -> Self {
        let (tools_version, tools_version_rx) = watch::channel(0u64);
        let cancel = CancellationToken::new();
        let servers = configs
            .into_iter()
            .filter(|config| config.enabled)
            .map(|config| McpServerHandle {
                config,
                session: Arc::new(tokio::sync::Mutex::new(None)),
                host_cancel: cancel.clone(),
                credential_store: Arc::clone(&credential_store),
                oauth_runtime: oauth_runtime.clone(),
                diagnostics: diagnostics.clone(),
            })
            .collect();
        Self {
            inner: Arc::new(McpHostShared {
                servers,
                tools: RwLock::new(BTreeMap::new()),
                tools_version,
                _tools_version_rx: tools_version_rx,
                state: Mutex::new(HostMcpState::Idle),
                cancel,
                diagnostics,
                server_states: RwLock::new(BTreeMap::new()),
            }),
        }
    }

    /// 启动所有启用的 server task（幂等）。
    pub fn start(&self) -> Result<(), String> {
        let mut state = self.inner.state.lock().unwrap();
        match *state {
            HostMcpState::Running => return Ok(()),
            HostMcpState::Idle => *state = HostMcpState::Running,
            _ => return Err("MCP host is stopping or stopped".into()),
        }
        let shared = self.inner.clone();
        for handle in self.inner.servers.clone() {
            let task_shared = shared.clone();
            tokio::spawn(async move {
                McpServerTask::run(handle, task_shared).await;
            });
        }
        Ok(())
    }

    /// 确定性 shutdown：状态置 `Stopping` → 取消在途调用 → 关闭会话
    /// （stdio 终止子进程、HTTP 释放连接）→ task 自行退出。
    pub async fn shutdown(&self) {
        {
            let mut state = self.inner.state.lock().unwrap();
            match *state {
                HostMcpState::Idle => {
                    *state = HostMcpState::Stopped;
                    return;
                }
                HostMcpState::Stopping | HostMcpState::Stopped => return,
                HostMcpState::Running => *state = HostMcpState::Stopping,
            }
        }
        self.inner.cancel.cancel();
        // 关闭所有会话（task 观察到取消后不再重建新会话）。
        for handle in &self.inner.servers {
            let mut session = handle.session.lock().await;
            if let Some(session) = session.take() {
                session.close().await;
            }
        }
        tokio::task::yield_now().await;
        {
            let mut state = self.inner.state.lock().unwrap();
            *state = HostMcpState::Stopped;
        }
    }

    /// 是否运行中。
    pub fn is_running(&self) -> bool {
        *self.inner.state.lock().unwrap() == HostMcpState::Running
    }

    /// server 句柄（meta tools 与产品查询）。
    pub fn servers(&self) -> &[McpServerHandle] {
        &self.inner.servers
    }

    /// 已发现工具（server 名 → 工具列表）。
    pub fn tools(&self) -> BTreeMap<String, Vec<DiscoveredTool>> {
        self.inner.tools.read().unwrap().clone()
    }

    /// 工具集版本号（每次发现 / 热更新 +1）。
    pub fn tools_version(&self) -> u64 {
        *self.inner.tools_version.borrow()
    }

    /// 订阅工具集变化（`notifications/tools/list_changed` 或重连后重新
    /// 发现时版本号递增）。
    pub fn subscribe_tools_changed(&self) -> watch::Receiver<u64> {
        self.inner.tools_version.subscribe()
    }

    /// 某 server 的当前生命周期状态（未装配 / 未启动时为
    /// `Disconnected`）。
    pub fn server_state(&self, server: &str) -> ServerLifecycleState {
        self.inner
            .server_states
            .read()
            .unwrap()
            .get(server)
            .cloned()
            .unwrap_or(ServerLifecycleState::Disconnected)
    }
}

/// 单个 server 的状态机驱动。
struct McpServerTask;

impl McpServerTask {
    /// 应用生命周期事件并发布状态；非法转换记录诊断（fail closed）。
    fn transition(
        shared: &McpHostShared,
        handle: &McpServerHandle,
        event: LifecycleEvent,
        pending_reason: Option<String>,
    ) -> Option<ServerLifecycleState> {
        let current = shared
            .server_states
            .read()
            .unwrap()
            .get(&handle.config.name)
            .cloned()
            .unwrap_or(ServerLifecycleState::Disconnected);
        let next = match apply_event(current.clone(), event) {
            Ok(next) => next,
            Err(error) => {
                shared.diagnostics.emit(DiagnosticRecord {
                    level: DiagnosticLevel::Warning,
                    code: "mcp_state_transition".into(),
                    message: error.to_string(),
                    extension_id: Some(handle.config.name.clone()),
                    context: Default::default(),
                });
                return None;
            }
        };
        let next = match next {
            ServerLifecycleState::Failed { .. } => ServerLifecycleState::Failed {
                reason: pending_reason.unwrap_or_default(),
            },
            other => other,
        };
        shared
            .server_states
            .write()
            .unwrap()
            .insert(handle.config.name.clone(), next.clone());
        Some(next)
    }

    fn record(
        shared: &McpHostShared,
        handle: &McpServerHandle,
        code: &str,
        message: impl Into<String>,
    ) {
        shared.diagnostics.emit(DiagnosticRecord {
            level: DiagnosticLevel::Warning,
            code: code.into(),
            message: message.into(),
            extension_id: Some(handle.config.name.clone()),
            context: Default::default(),
        });
    }

    async fn run(handle: McpServerHandle, shared: Arc<McpHostShared>) {
        let cancel = shared.cancel.clone();
        let mut attempt: u32 = 0;
        Self::transition(&shared, &handle, LifecycleEvent::Connect, None);
        loop {
            if cancel.is_cancelled() {
                break;
            }
            let outcome = Self::connect_and_serve(&handle, &shared, &cancel).await;
            match outcome {
                ServeOutcome::Healthy | ServeOutcome::NotReady => {
                    if cancel.is_cancelled() || attempt == 0 {
                        break;
                    }
                    // 前一轮已失败（attempt > 0）：等待退避后重连。
                    if !Self::wait_for_reconnect(&handle, &mut attempt, &cancel).await {
                        break;
                    }
                    Self::transition(&shared, &handle, LifecycleEvent::Connect, None);
                }
                ServeOutcome::Failed { reason } => {
                    // 曾到过 Ready（liveness / 运行期失败）→ 退避重连；
                    // 从未 Ready（初始握手失败）→ fail early 不重试。
                    let was_ready = shared
                        .server_states
                        .read()
                        .unwrap()
                        .get(&handle.config.name)
                        .is_some_and(ServerLifecycleState::is_ready);
                    Self::transition(
                        &shared,
                        &handle,
                        LifecycleEvent::ConnectFailed,
                        Some(reason.clone()),
                    );
                    if !was_ready {
                        Self::record(shared.as_ref(), &handle, "mcp_connect_failed", reason);
                        break;
                    }
                    Self::record(shared.as_ref(), &handle, "mcp_reconnecting", reason);
                    if !Self::wait_for_reconnect(&handle, &mut attempt, &cancel).await {
                        break;
                    }
                    Self::transition(&shared, &handle, LifecycleEvent::Connect, None);
                }
            }
        }
        // 收尾：清理残留会话。
        let mut session = handle.session.lock().await;
        if let Some(session) = session.take() {
            session.close().await;
        }
        let _ = Self::transition(&shared, &handle, LifecycleEvent::Shutdown, None);
    }

    /// 一次完整的连接 + 服务周期。返回后由调用方决定重连或退出。
    async fn connect_and_serve(
        handle: &McpServerHandle,
        shared: &Arc<McpHostShared>,
        cancel: &CancellationToken,
    ) -> ServeOutcome {
        let (notifications, mut notification_rx) =
            tokio::sync::mpsc::unbounded_channel::<Notification>();
        let (session, mut died_rx) =
            match RpcSession::open(handle.config.transport.clone(), notifications).await {
                Ok(opened) => opened,
                Err(error) => {
                    return ServeOutcome::Failed {
                        reason: error.to_string(),
                    };
                }
            };

        Self::transition(shared, handle, LifecycleEvent::HandshakeStarted, None);

        // initialize 握手（capability 协商：接受服务器返回的协议版本）。
        let initialize_params = serde_json::json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "evo", "version": env!("CARGO_PKG_VERSION")},
        });
        if let Err(error) = session
            .request(
                "initialize",
                Some(initialize_params),
                Duration::from_secs(30),
                cancel,
            )
            .await
        {
            session.close().await;
            return ServeOutcome::Failed {
                reason: format!("initialize: {error}"),
            };
        }
        if session
            .notify("notifications/initialized", None)
            .await
            .is_err()
        {
            session.close().await;
            return ServeOutcome::Failed {
                reason: "initialized notification failed".into(),
            };
        }

        // 工具发现。
        if let Err(reason) = Self::discover_tools(handle, shared, &session, cancel).await {
            session.close().await;
            return ServeOutcome::Failed { reason };
        }

        *handle.session.lock().await = Some(Arc::new(session));
        Self::transition(shared, handle, LifecycleEvent::Ready, None);

        // Ready 服务循环：liveness ping / 通知 / transport 死亡 / 取消。
        let liveness = handle.config.liveness.clone();
        let mut ping = tokio::time::interval(liveness.ping_interval);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let session = {
                let guard = handle.session.lock().await;
                guard.as_ref().cloned()
            };
            let Some(session) = session else {
                return ServeOutcome::NotReady;
            };
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    return ServeOutcome::Healthy;
                }
                changed = died_rx.changed() => {
                    let _ = changed;
                    if *died_rx.borrow() {
                        return ServeOutcome::Failed {
                            reason: "transport closed".into(),
                        };
                    }
                }
                notification = notification_rx.recv() => {
                    let Some(notification) = notification else {
                        return ServeOutcome::Failed {
                            reason: "notification channel closed".into(),
                        };
                    };
                    if notification.method == "notifications/tools/list_changed"
                        && let Err(reason) =
                            Self::discover_tools(handle, shared, &session, cancel).await
                    {
                        Self::record(shared.as_ref(), handle, "mcp_tools_refresh_failed", reason);
                    }
                }
                _ = ping.tick() => {
                    match session.request(
                        "ping",
                        None,
                        liveness.ping_timeout,
                        cancel,
                    ).await {
                        Ok(_) => {}
                        Err(error) => {
                            return ServeOutcome::Failed {
                                reason: format!("liveness ping: {error}"),
                            };
                        }
                    }
                }
            }
        }
    }

    /// `tools/list` → 转换 → 更新缓存 + bump 版本。
    async fn discover_tools(
        handle: &McpServerHandle,
        shared: &Arc<McpHostShared>,
        session: &RpcSession,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let result = session
            .request("tools/list", None, Duration::from_secs(30), cancel)
            .await
            .map_err(|error| format!("tools/list: {error}"))?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| "tools/list result lacks 'tools' array".to_string())?;
        let mut discovered = Vec::new();
        for raw in tools {
            match convert_tool(&handle.config.name, raw) {
                Ok(tool) => discovered.push(tool),
                Err(reason) => {
                    // 非法条目拒绝（fail closed）并诊断，其余继续。
                    Self::record(shared.as_ref(), handle, "mcp_tool_rejected", reason);
                }
            }
        }
        shared
            .tools
            .write()
            .unwrap()
            .insert(handle.config.name.clone(), discovered);
        let version = shared.tools_version.borrow().saturating_add(1);
        let _ = shared.tools_version.send(version);
        Ok(())
    }

    /// 指数退避等待重连。返回 `false` 表示应退出（shutdown）。
    async fn wait_for_reconnect(
        handle: &McpServerHandle,
        attempt: &mut u32,
        cancel: &CancellationToken,
    ) -> bool {
        *attempt = attempt.saturating_add(1);
        let config = &handle.config.reconnect;
        let backoff = backoff_for(*attempt, config.initial_backoff, config.max_backoff);
        tokio::select! {
            _ = cancel.cancelled() => false,
            _ = tokio::time::sleep(backoff) => true,
        }
    }
}

enum ServeOutcome {
    /// 服务周期正常结束（shutdown）。
    Healthy,
    /// 未到 Ready 就退出。
    NotReady,
    /// 传输 / 握手 / liveness 失败。
    Failed { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_config(name: &str) -> McpServerConfig {
        McpServerConfig::new(
            name,
            TransportConfig::Stdio(crate::mcp::transport::StdioConfig {
                command: "true".into(),
                args: vec![],
                env: workspace_runtime::api::EnvPolicy::AllowList(Default::default()),
                cwd: None,
                sandbox: None,
            }),
        )
    }

    fn empty_host() -> McpHost {
        McpHost::new(
            vec![],
            Arc::new(crate::mcp::credentials::FileCredentialStore::new(
                tempfile::tempdir().unwrap().path(),
            )),
        )
    }

    #[test]
    fn convert_tool_accepts_valid_entries_and_rejects_invalid() {
        let valid = serde_json::json!({
            "name": "read_file",
            "description": "Read a file",
            "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}}
        });
        let tool = convert_tool("fs", &valid).unwrap();
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.server, "fs");
        assert_eq!(tool.description, "Read a file");

        assert!(convert_tool("s", &serde_json::json!([])).is_err());
        assert!(convert_tool("s", &serde_json::json!({})).is_err());
        assert!(convert_tool("s", &serde_json::json!({"name": 42})).is_err());
        assert!(convert_tool("s", &serde_json::json!({"name": ""})).is_err());
    }

    #[test]
    fn convert_tool_defaults_schema_when_missing() {
        let tool = convert_tool("s", &serde_json::json!({"name": "x"})).unwrap();
        assert_eq!(tool.input_schema, serde_json::json!({"type": "object"}));
    }

    #[test]
    fn disabled_servers_are_not_assembled() {
        let mut config = server_config("off");
        config.enabled = false;
        let host = McpHost::new(
            vec![config],
            Arc::new(crate::mcp::credentials::FileCredentialStore::new(
                tempfile::tempdir().unwrap().path(),
            )),
        );
        assert!(host.servers().is_empty());
    }

    #[test]
    fn tools_version_starts_at_zero_and_can_be_subscribed() {
        let host = empty_host();
        assert_eq!(host.tools_version(), 0);
        assert_eq!(*host.subscribe_tools_changed().borrow(), 0);
    }

    #[test]
    fn start_is_idempotent() {
        let host = empty_host();
        host.start().unwrap();
        assert!(host.start().is_ok());
        assert!(host.is_running());
    }

    #[tokio::test]
    async fn shutdown_from_idle_is_quick_and_stopped() {
        let host = empty_host();
        host.shutdown().await;
        assert!(!host.is_running());
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let host = empty_host();
        host.start().unwrap();
        host.shutdown().await;
        host.shutdown().await;
        assert!(!host.is_running());
    }
}
