//! MCP transport：stdio 子进程与 HTTP endpoint 两种通道上的 JSON-RPC
//! 会话（请求-响应 + 通知 + 超时 + 取消）。
//!
//! - **stdio**：子进程经 workspace-runtime 的 [`workspace_runtime::api::PeerProcess`]
//!   spawn（同一 [`SandboxProfile`] 强制边界：平台能力不足 fail-closed，
//!   kill-on-drop + process-group 终止）。stdin 写 JSON-RPC 行、stdout 读行；
//!   **单行解码失败跳过并继续读**（参考 xai-grok-mcp `ResilientRwTransport`：
//!   一个坏行不 collapse 整个 transport）。读循环在后台 task 中按 id 分发
//!   响应、把通知推给订阅者；EOF / 读错误传播为
//!   [`RpcError::TransportClosed`]。
//! - **HTTP**：reqwest 直连用户显式配置的 endpoint（`POST` JSON-RPC 请求、
//!   同步读响应）。**信任边界**：endpoint 来自显式配置（不是任意 URL 的
//!   web_fetch 管线），本模块只校验 scheme 为 http/https；SSE / 服务端
//!   推送不在本任务范围（HTTP 下收不到 `tools/list_changed`，见债务）。
//!   401（HTTP 状态或 JSON-RPC `-32001`）归类为 [`RpcError::Unauthorized`]，
//!   由调用方触发 credential refresh / OAuth。
//!
//! 超时 / 取消：`request()` 内部 `select!` 超时与取消令牌；超时或被取消后
//! 迟到的响应按 id 丢弃（stdin 读循环继续运行），不污染后续请求。

// Adapted from xai-grok-mcp, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// ResilientRwTransport line-skip discipline and McpError classification consulted;
// session design (id-dispatch, notification fan-out, cancel/timeout) is Evo's own.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;
use workspace_runtime::api::{EnvPolicy, PeerProcess, ProcessSpec, ProgramKind, SandboxProfile};

use crate::mcp::wire::{self, Id, Message, Notification, Request, Response};

/// stdio transport 配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioConfig {
    pub command: String,
    pub args: Vec<String>,
    /// 环境白名单（`Inherit` 会继承宿主全部环境，仅允许显式配置）。
    pub env: EnvPolicy,
    /// 工作目录；`None` 取宿主当前目录。
    pub cwd: Option<PathBuf>,
    /// 沙箱配置；`None` 时按 `cwd` 生成 [`SandboxProfile::product_default`]。
    pub sandbox: Option<SandboxProfile>,
}

/// HTTP transport 配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpConfig {
    /// 用户显式配置的 MCP endpoint（仅 http/https）。
    pub url: String,
    /// 附加请求头（如 `Authorization: Bearer …`）。
    pub headers: Vec<(String, String)>,
}

/// 传输形态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportConfig {
    Stdio(StdioConfig),
    Http(HttpConfig),
}

/// MCP JSON-RPC 会话的错误分类。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RpcError {
    #[error("JSON-RPC error {0}: {1}")]
    JsonRpc(i32, String),
    #[error("MCP request timed out after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },
    #[error("MCP request cancelled")]
    Cancelled,
    #[error("MCP transport closed: {reason}")]
    TransportClosed { reason: String },
    #[error("MCP server requires authentication (401)")]
    Unauthorized,
    #[error("MCP wire error: {0}")]
    Wire(#[from] wire::WireError),
    #[error("MCP transport error: {0}")]
    Other(String),
}

impl RpcError {
    pub fn is_unauthorized(&self) -> bool {
        match self {
            RpcError::Unauthorized => true,
            RpcError::JsonRpc(code, message) => {
                *code == wire::UNAUTHORIZED || message.to_ascii_lowercase().contains("unauthorized")
            }
            _ => false,
        }
    }
}

type PendingReply = oneshot::Sender<Result<Response, RpcError>>;

enum SessionInner {
    Stdio {
        /// 子进程句柄（terminate / pid）。
        process: Arc<Mutex<PeerProcess>>,
        /// stdin 独占写句柄。
        stdin: Arc<Mutex<tokio::process::ChildStdin>>,
        read_join: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    },
    Http {
        client: reqwest::Client,
        endpoint: reqwest::Url,
        headers: Vec<(String, String)>,
    },
}

/// 与单个 MCP 服务器的 JSON-RPC 会话。
pub struct RpcSession {
    inner: SessionInner,
    /// 未完成的请求 id → 响应通道。
    pending: Arc<Mutex<BTreeMap<Id, PendingReply>>>,
    next_id: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for RpcSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RpcSession")
    }
}

impl RpcSession {
    /// 建立会话：stdio 为 spawn + 后台读循环；http 为客户端配置（惰性
    /// 连接）。
    ///
    /// `notifications` 由 lifecycle 层传入（收到通知的消费方）。返回
    /// `(session, transport_died)`——`transport_died` 是读循环 / 子进程
    /// 终止信号（EOF、读错误、spawn 失败）。
    pub async fn open(
        transport: TransportConfig,
        notifications: tokio::sync::mpsc::UnboundedSender<Notification>,
    ) -> Result<(Self, tokio::sync::watch::Receiver<bool>), RpcError> {
        let pending: Arc<Mutex<BTreeMap<Id, PendingReply>>> = Arc::new(Mutex::new(BTreeMap::new()));
        let (died_tx, died_rx) = tokio::sync::watch::channel(false);
        let inner = match transport {
            TransportConfig::Stdio(config) => {
                let (process, stdin, read_join) =
                    spawn_stdio(config, Arc::clone(&pending), &notifications, died_tx).await?;
                SessionInner::Stdio {
                    process,
                    stdin,
                    read_join: tokio::sync::Mutex::new(Some(read_join)),
                }
            }
            TransportConfig::Http(config) => {
                let endpoint = reqwest::Url::parse(&config.url).map_err(|error| {
                    RpcError::Other(format!("invalid MCP endpoint '{}': {error}", config.url))
                })?;
                if !matches!(endpoint.scheme(), "http" | "https") {
                    return Err(RpcError::Other(format!(
                        "MCP endpoint scheme must be http or https, got '{}'",
                        endpoint.scheme()
                    )));
                }
                let client = reqwest::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(10))
                    .build()
                    .map_err(|error| RpcError::Other(format!("http client: {error}")))?;
                SessionInner::Http {
                    client,
                    endpoint,
                    headers: config.headers,
                }
            }
        };
        Ok((
            Self {
                inner,
                pending,
                next_id: std::sync::atomic::AtomicU64::new(1),
            },
            died_rx,
        ))
    }

    /// 发送一个请求并等待响应。超时 / 取消后迟到响应按 id 丢弃。
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: std::time::Duration,
        cancel: &CancellationToken,
    ) -> Result<Value, RpcError> {
        let id = Id::Number(
            self.next_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        let request = Request::new(id_number(&id), method, params);
        let (reply_tx, reply_rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), reply_tx);

        let request_bytes = match serde_json::to_vec(&request) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.pending.lock().await.remove(&id);
                return Err(RpcError::Other(format!("serialize request: {error}")));
            }
        };

        match &self.inner {
            SessionInner::Stdio { stdin, .. } => {
                let mut line = request_bytes;
                line.push(b'\n');
                let stdin = stdin.clone();
                let write = async move {
                    let mut stdin = stdin.lock().await;
                    stdin.write_all(&line).await?;
                    stdin.flush().await
                };
                match await_with_controls(write, timeout, cancel).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        // 发送失败立即移除 pending（读循环不会给该 id 回执）。
                        self.pending.lock().await.remove(&id);
                        return Err(RpcError::TransportClosed {
                            reason: format!("stdin write failed: {error}"),
                        });
                    }
                    Err(error) => {
                        self.pending.lock().await.remove(&id);
                        return Err(error);
                    }
                }
            }
            SessionInner::Http {
                client,
                endpoint,
                headers,
            } => {
                let response = await_with_controls(
                    post_json(client, endpoint, headers, &request_bytes),
                    timeout,
                    cancel,
                )
                .await;
                let parsed = match response {
                    Ok(Ok(Ok(value))) => parse_http_response(value),
                    Ok(Ok(Err(error))) => Err(error),
                    Ok(Err(error)) | Err(error) => Err(error),
                };
                // HTTP 同步响应：读循环不存在，立即移除 pending。
                self.pending.lock().await.remove(&id);
                return parsed;
            }
        }

        let reply = await_with_controls(reply_rx, timeout, cancel).await;
        self.pending.lock().await.remove(&id);
        match reply {
            Ok(Ok(Ok(response))) => Ok(response.result),
            // 服务器显式返回错误响应（如 401 Unauthorized）→ 原样上抛，
            // 供调用方触发凭据恢复（不折叠成 TransportClosed）。
            Ok(Ok(Err(error))) => Err(error),
            // 响应通道已消失：读循环终止（transport 关闭）或取消/超时后
            // 迟到响应已被移除。
            Ok(Err(_)) => Err(RpcError::TransportClosed {
                reason: "response channel dropped".into(),
            }),
            Err(error) => Err(error),
        }
    }

    /// 发送通知（无响应期待）。
    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), RpcError> {
        let notification = Notification {
            jsonrpc: wire::JSONRPC_VERSION.into(),
            method: method.to_string(),
            params,
        };
        let bytes = serde_json::to_vec(&notification)
            .map_err(|error| RpcError::Other(format!("serialize notification: {error}")))?;
        match &self.inner {
            SessionInner::Stdio { stdin, .. } => {
                let mut line = bytes;
                line.push(b'\n');
                let mut stdin = stdin.lock().await;
                stdin
                    .write_all(&line)
                    .await
                    .map_err(|error| RpcError::TransportClosed {
                        reason: format!("stdin write failed: {error}"),
                    })?;
                stdin
                    .flush()
                    .await
                    .map_err(|error| RpcError::TransportClosed {
                        reason: format!("stdin flush failed: {error}"),
                    })
            }
            SessionInner::Http {
                client,
                endpoint,
                headers,
            } => {
                post_json(client, endpoint, headers, &bytes)
                    .await
                    .map_err(|error| RpcError::Other(error.to_string()))??;
                Ok(())
            }
        }
    }

    /// 停止会话：stdio 下终止子进程并回收读循环（幂等）。
    pub async fn close(&self) {
        match &self.inner {
            SessionInner::Stdio {
                process, read_join, ..
            } => {
                let mut process = process.lock().await;
                process.terminate().await;
                drop(process);
                let join = read_join.lock().await.take();
                if let Some(join) = join {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), join).await;
                }
            }
            SessionInner::Http { .. } => {}
        }
    }
}

fn id_number(id: &Id) -> u64 {
    match id {
        Id::Number(number) => *number,
        Id::String(_) => unreachable!("session ids are always numbers"),
    }
}

async fn await_with_controls<F, T>(
    future: F,
    timeout: std::time::Duration,
    cancel: &CancellationToken,
) -> Result<T, RpcError>
where
    F: std::future::Future<Output = T>,
{
    tokio::pin!(future);
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(RpcError::Cancelled),
        _ = tokio::time::sleep(timeout) => Err(RpcError::Timeout {
            timeout_secs: timeout.as_secs(),
        }),
        result = future => Ok(result),
    }
}

/// 后台读循环：读行 → 解析 → 响应按 id 分发 / 通知推送 / 坏行跳过。
async fn read_loop(
    stdout: tokio::process::ChildStdout,
    pending: Arc<Mutex<BTreeMap<Id, PendingReply>>>,
    notifications: &tokio::sync::mpsc::UnboundedSender<Notification>,
    died: tokio::sync::watch::Sender<bool>,
) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) => {
                fail_all_pending(&pending, "stdout closed (end of stream)").await;
                report_died(&died, "stdout closed (end of stream)");
                return;
            }
            Ok(_) => {}
            Err(error) => {
                fail_all_pending(&pending, &format!("stdout read failed: {error}")).await;
                report_died(&died, &format!("stdout read failed: {error}"));
                return;
            }
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            continue;
        }
        match wire::parse_message(&line) {
            Ok(message) => match message {
                Message::Response(response) => {
                    let id = response.id.clone();
                    dispatch_response(&pending, &id, Ok(response)).await;
                }
                Message::ErrorResponse(response) => {
                    let error = if response.error.code == wire::UNAUTHORIZED
                        || response
                            .error
                            .message
                            .to_ascii_lowercase()
                            .contains("unauthorized")
                    {
                        RpcError::Unauthorized
                    } else {
                        RpcError::JsonRpc(response.error.code, response.error.message)
                    };
                    dispatch_response(&pending, &response.id, Err(error)).await;
                }
                Message::Notification(notification) => {
                    let _ = notifications.send(notification);
                }
                Message::Request(_) => {
                    // 客户端不应收到请求；忽略（不 collapse transport）。
                }
            },
            Err(_) => {
                // 坏行跳过并继续读，不 collapse transport（与
                // xai-grok-mcp ResilientRwTransport 一致）。跳过是静默的：
                // 后续请求/响应不受影响，服务器仍可用。
            }
        }
    }
}

async fn dispatch_response(
    pending: &Arc<Mutex<BTreeMap<Id, PendingReply>>>,
    id: &Id,
    result: Result<Response, RpcError>,
) {
    if let Some(sender) = pending.lock().await.remove(id) {
        let _ = sender.send(result);
    }
}

/// transport 终止时，把未完成的请求全部以 `TransportClosed` 失败（在途
/// 调用立即返回，不悬挂到超时）。
async fn fail_all_pending(pending: &Arc<Mutex<BTreeMap<Id, PendingReply>>>, reason: &str) {
    let replies = std::mem::take(&mut *pending.lock().await);
    for (_, sender) in replies {
        let _ = sender.send(Err(RpcError::TransportClosed {
            reason: reason.to_string(),
        }));
    }
}

fn report_died(died: &tokio::sync::watch::Sender<bool>, reason: &str) {
    // 死亡信号经 watch 交给 lifecycle（重连 / 诊断由状态机处理）。
    let _ = died.send(true);
    let _ = reason;
}

/// 后台 drain stderr：无人读取的 stderr 写满管道会阻塞子进程，丢弃即可
/// （服务器日志不进入产品路径）。
async fn drain_stderr(mut stderr: tokio::process::ChildStderr) {
    let mut reader = BufReader::new(&mut stderr);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

async fn spawn_stdio(
    config: StdioConfig,
    pending: Arc<Mutex<BTreeMap<Id, PendingReply>>>,
    notifications: &tokio::sync::mpsc::UnboundedSender<Notification>,
    died: tokio::sync::watch::Sender<bool>,
) -> Result<
    (
        Arc<Mutex<PeerProcess>>,
        Arc<Mutex<tokio::process::ChildStdin>>,
        tokio::task::JoinHandle<()>,
    ),
    RpcError,
> {
    let cwd = config
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let process_spec = ProcessSpec {
        program: ProgramKind::Direct {
            program: config.command.clone(),
            args: config.args.clone(),
        },
        command: String::new(),
        cwd: cwd.clone(),
        env: config.env.clone(),
        timeout: std::time::Duration::from_secs(0),
        output_budget: workspace_runtime::api::OutputBudget::new(1 << 20, 100_000),
        sandbox: Some(
            config
                .sandbox
                .clone()
                .unwrap_or_else(|| SandboxProfile::product_default(&cwd)),
        ),
    };
    let mut peer = PeerProcess::spawn(process_spec)
        .await
        .map_err(|reason| RpcError::Other(format!("spawn MCP server: {reason}")))?;
    peer.disarm(); // 生命周期由 RpcSession::close 控制。
    let stdout = peer
        .take_stdout()
        .ok_or_else(|| RpcError::Other("MCP server stdout unavailable".into()))?;
    let stderr = peer
        .take_stderr()
        .ok_or_else(|| RpcError::Other("MCP server stderr unavailable".into()))?;
    let stdin =
        Arc::new(Mutex::new(peer.take_stdin().ok_or_else(|| {
            RpcError::Other("MCP server stdin unavailable".into())
        })?));
    let process = Arc::new(Mutex::new(peer));
    let read_join = {
        let pending = Arc::clone(&pending);
        let notifications = notifications.clone();
        let died = died.clone();
        tokio::spawn(async move {
            read_loop(stdout, pending, &notifications, died).await;
        })
    };
    tokio::spawn(drain_stderr(stderr));
    Ok((process, stdin, read_join))
}

async fn post_json(
    client: &reqwest::Client,
    endpoint: &reqwest::Url,
    headers: &[(String, String)],
    bytes: &[u8],
) -> Result<Result<Value, RpcError>, RpcError> {
    let mut request = client
        .post(endpoint.clone())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request
        .body(bytes.to_vec())
        .send()
        .await
        .map_err(|error| RpcError::Other(format!("http request failed: {error}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| RpcError::Other(format!("http response body: {error}")))?;
    if status.as_u16() == 401 {
        return Ok(Err(RpcError::Unauthorized));
    }
    if !status.is_success() {
        return Ok(Err(RpcError::Other(format!(
            "http status {}: {}",
            status.as_u16(),
            body.chars().take(200).collect::<String>()
        ))));
    }
    Ok(serde_json::from_str(&body)
        .map_err(|error| RpcError::Other(format!("http response is not JSON: {error}"))))
}

fn parse_http_response(value: Value) -> Result<Value, RpcError> {
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| RpcError::Other(format!("reserialize: {error}")))?;
    match wire::parse_message(&bytes)? {
        Message::Response(response) => Ok(response.result),
        Message::ErrorResponse(response) => Err(RpcError::JsonRpc(
            response.error.code,
            response.error.message,
        )),
        _ => Err(RpcError::Other(
            "http endpoint returned a non-response message".into(),
        )),
    }
}
