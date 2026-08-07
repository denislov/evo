//! LSP stdio transport：Content-Length 帧会话（spawn + 读循环 + 分发）。
//!
//! 参照 `extension-host` 的 MCP `RpcSession` 设计（id 分发 / 通知 fan-out /
//! 取消 / 超时），按 LSP 的帧协议重写，差异：
//!
//! - **帧协议**：`Content-Length` 帧（见 [`crate::lsp::wire`]）；**坏帧
//!   fail closed**（帧流不同步，读循环终止并上报死亡），不像 MCP 坏行
//!   跳过——行协议可跳过单行恢复，帧协议无法。
//! - **服务器请求**：LSP 服务器会主动发请求给客户端（`workspace/applyEdit`
//!   等）；读循环把请求经 `server_requests` 通道交给 actor，actor 处理后
//!   经 [`LspSession::respond_to_server`] 回执。
//! - **进程治理**：`PeerProcess::spawn`（SandboxProfile 强制，能力不足
//!   平台 fail-closed，kill-on-drop + process-group 终止）；输出预算 =
//!   单帧上限（防超大帧 / 输出洪泛）+ stderr drain（不阻塞子进程）。
//! - **取消**：`request` 超时 / 取消后 pending 移除，迟到响应按 id 丢弃。
//!   礼貌性 `$/cancelRequest` 通知未实现（见债务）。

// Evo 独立设计：会话形状参照 extension-host 的 MCP RpcSession（Phase 7，
// id 分发 / 通知 fan-out / cancel/timeout）；LSP 帧协议、服务器请求回执
// 与进程治理为 Evo 自研，无直接移植。
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;
use workspace_runtime::api::{EnvPolicy, PeerProcess, ProcessSpec, ProgramKind, SandboxProfile};

use crate::lsp::wire::{self, Id, Message, Notification, Request, Response, write_frame};

/// 会话死亡原因（读循环 / 子进程终止）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionDeath {
    #[error("LSP transport closed: {reason}")]
    TransportClosed { reason: String },
    #[error("LSP wire error: {0}")]
    Wire(#[from] wire::WireError),
}

/// RPC 调用错误分类。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RpcError {
    #[error("LSP request timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },
    #[error("LSP request cancelled")]
    Cancelled,
    #[error("LSP transport closed: {reason}")]
    TransportClosed { reason: String },
    #[error("LSP server returned error {code}: {message}")]
    ServerError { code: i32, message: String },
    #[error("LSP transport error: {0}")]
    Other(String),
}

/// 会话配置（spawn 边界；sandbox 已由 server 层解析为具体 profile）。
#[derive(Debug, Clone)]
pub struct LspSessionConfig {
    pub command: String,
    pub args: Vec<String>,
    /// 环境白名单（`Inherit` 会继承宿主全部环境，仅允许显式配置）。
    pub env: EnvPolicy,
    /// 工作目录。
    pub cwd: PathBuf,
    /// 沙箱配置（必填；能力不足平台 spawn fail closed）。
    pub sandbox: SandboxProfile,
    /// 单帧上限（防输出洪泛 / 超大帧）。
    pub max_frame_bytes: usize,
}

type PendingReply = oneshot::Sender<Result<Response, RpcError>>;
/// 服务器 → 客户端请求的回执通道（actor 处理后回执）。
pub type ServerRequestReply = oneshot::Sender<Result<Value, wire::JsonRpcError>>;

/// 与单个 LSP 服务器的 stdio 会话。
pub struct LspSession {
    process: Arc<Mutex<PeerProcess>>,
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    pending: Arc<Mutex<BTreeMap<Id, PendingReply>>>,
    next_id: std::sync::atomic::AtomicU64,
    read_join: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for LspSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LspSession")
    }
}

/// spawn + 后台读循环。
///
/// `notifications`：服务器通知（`publishDiagnostics` 等）推送目标；
/// `server_requests`：服务器请求（`workspace/applyEdit` 等）推送目标。
/// 返回 `(session, died)`——`died` 是读循环 / 子进程终止信号
/// （EOF、帧错误、spawn 失败）。
pub async fn open_session(
    config: LspSessionConfig,
    notifications: tokio::sync::mpsc::UnboundedSender<Notification>,
    server_requests: tokio::sync::mpsc::UnboundedSender<(Request, ServerRequestReply)>,
) -> Result<(LspSession, tokio::sync::watch::Receiver<bool>), RpcError> {
    let pending: Arc<Mutex<BTreeMap<Id, PendingReply>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let (died_tx, died_rx) = tokio::sync::watch::channel(false);

    let process_spec = ProcessSpec {
        program: ProgramKind::Direct {
            program: config.command.clone(),
            args: config.args.clone(),
        },
        command: String::new(),
        cwd: config.cwd.clone(),
        env: config.env.clone(),
        timeout: std::time::Duration::from_secs(0),
        output_budget: workspace_runtime::api::OutputBudget::new(1 << 20, 100_000),
        sandbox: Some(config.sandbox.clone()),
    };
    let mut peer = PeerProcess::spawn(process_spec)
        .await
        .map_err(|reason| RpcError::Other(format!("spawn LSP server: {reason}")))?;
    peer.disarm(); // 生命周期由 LspSession::close 控制。
    let stdout = peer
        .take_stdout()
        .ok_or_else(|| RpcError::Other("LSP server stdout unavailable".into()))?;
    let stderr = peer
        .take_stderr()
        .ok_or_else(|| RpcError::Other("LSP server stderr unavailable".into()))?;
    let stdin =
        Arc::new(Mutex::new(peer.take_stdin().ok_or_else(|| {
            RpcError::Other("LSP server stdin unavailable".into())
        })?));
    let process = Arc::new(Mutex::new(peer));

    let read_join = {
        let pending = Arc::clone(&pending);
        let died = died_tx.clone();
        let max_frame_bytes = config.max_frame_bytes;
        let stdin = Arc::clone(&stdin);
        tokio::spawn(async move {
            read_loop(
                stdout,
                stdin,
                pending,
                &notifications,
                &server_requests,
                died,
                max_frame_bytes,
            )
            .await;
        })
    };
    tokio::spawn(drain_stderr(stderr));
    Ok((
        LspSession {
            process,
            stdin,
            pending,
            next_id: std::sync::atomic::AtomicU64::new(1),
            read_join: Mutex::new(Some(read_join)),
        },
        died_rx,
    ))
}

impl LspSession {
    /// 发送请求并等待响应。超时 / 取消后迟到响应按 id 丢弃。
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
        {
            let mut stdin = self.stdin.lock().await;
            if let Err(error) = write_frame(&mut *stdin, &request_bytes).await {
                self.pending.lock().await.remove(&id);
                return Err(RpcError::TransportClosed {
                    reason: format!("stdin write failed: {error}"),
                });
            }
        }

        let reply = await_with_controls(reply_rx, timeout, cancel).await;
        self.pending.lock().await.remove(&id);
        match reply {
            Ok(Ok(Ok(response))) => Ok(response.result),
            Ok(Ok(Err(error))) => Err(error),
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
        let mut stdin = self.stdin.lock().await;
        write_frame(&mut *stdin, &bytes)
            .await
            .map_err(|error| RpcError::TransportClosed {
                reason: format!("stdin write failed: {error}"),
            })
    }

    /// 向服务器回执一个请求的响应（成功 result 或错误）。
    pub async fn respond_to_server(
        &self,
        id: &Id,
        result: Result<Value, wire::JsonRpcError>,
    ) -> Result<(), RpcError> {
        let bytes = match result {
            Ok(result) => serde_json::to_vec(&Response {
                jsonrpc: wire::JSONRPC_VERSION.into(),
                id: id.clone(),
                result,
            }),
            Err(error) => serde_json::to_vec(&wire::ErrorResponse {
                jsonrpc: wire::JSONRPC_VERSION.into(),
                id: id.clone(),
                error,
            }),
        }
        .map_err(|error| RpcError::Other(format!("serialize response: {error}")))?;
        let mut stdin = self.stdin.lock().await;
        write_frame(&mut *stdin, &bytes)
            .await
            .map_err(|error| RpcError::TransportClosed {
                reason: format!("stdin write failed: {error}"),
            })
    }

    /// 子进程 pid（spawn 后可能 None）。
    pub fn pid(&self) -> Option<u32> {
        self.process
            .try_lock()
            .ok()
            .and_then(|process| process.pid())
    }

    /// 当前在途请求数（诊断用）。
    pub async fn in_flight(&self) -> usize {
        self.pending.lock().await.len()
    }

    /// 停止会话：终止子进程并回收读循环（幂等）。
    pub async fn close(&self) {
        {
            let mut process = self.process.lock().await;
            process.terminate().await;
        }
        let join = self.read_join.lock().await.take();
        if let Some(join) = join {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), join).await;
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
            timeout_ms: timeout.as_millis() as u64,
        }),
        result = future => Ok(result),
    }
}

/// 后台读循环：帧 → 解析 → 响应按 id 分发 / 通知与服务器请求 fan-out /
/// 坏帧 fail closed。
async fn read_loop(
    stdout: tokio::process::ChildStdout,
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    pending: Arc<Mutex<BTreeMap<Id, PendingReply>>>,
    notifications: &tokio::sync::mpsc::UnboundedSender<Notification>,
    server_requests: &tokio::sync::mpsc::UnboundedSender<(Request, ServerRequestReply)>,
    died: tokio::sync::watch::Sender<bool>,
    max_frame_bytes: usize,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        match wire::read_frame(&mut reader, max_frame_bytes).await {
            Ok(bytes) => match wire::parse_message(&bytes) {
                Ok(message) => match message {
                    Message::Response(response) => {
                        let id = response.id.clone();
                        dispatch_response(&pending, &id, Ok(response)).await;
                    }
                    Message::ErrorResponse(response) => {
                        let id = response.id.clone();
                        let error = RpcError::ServerError {
                            code: response.error.code,
                            message: response.error.message.clone(),
                        };
                        dispatch_response(&pending, &id, Err(error)).await;
                    }
                    Message::Notification(notification) => {
                        let _ = notifications.send(notification);
                    }
                    Message::Request(request) => {
                        // 服务器请求：交给 actor 处理；回执在独立 task 中
                        // 写回（不阻塞读循环）。actor 未回执（通道关闭）
                        // 时回内部错误。
                        let request_id = request.id.clone();
                        let (reply_tx, reply_rx) = oneshot::channel();
                        let _ = server_requests.send((request, reply_tx));
                        let stdin = stdin.clone();
                        tokio::spawn(async move {
                            let bytes = match reply_rx.await {
                                Ok(Ok(result)) => serde_json::to_vec(&Response {
                                    jsonrpc: wire::JSONRPC_VERSION.into(),
                                    id: request_id.clone(),
                                    result,
                                }),
                                Ok(Err(error)) => serde_json::to_vec(&wire::ErrorResponse {
                                    jsonrpc: wire::JSONRPC_VERSION.into(),
                                    id: request_id.clone(),
                                    error,
                                }),
                                Err(_) => serde_json::to_vec(&wire::ErrorResponse {
                                    jsonrpc: wire::JSONRPC_VERSION.into(),
                                    id: request_id.clone(),
                                    error: wire::JsonRpcError::new(
                                        wire::INTERNAL_ERROR,
                                        "client did not respond",
                                    ),
                                }),
                            };
                            let Ok(bytes) = bytes else {
                                return;
                            };
                            let mut stdin = stdin.lock().await;
                            let _ = write_frame(&mut *stdin, &bytes).await;
                        });
                    }
                },
                Err(error) => {
                    // 坏帧 = 帧流不同步，无法恢复边界：fail closed。
                    fail_all_pending(&pending, &format!("invalid frame: {error}")).await;
                    report_died(&died, &format!("invalid frame: {error}"));
                    return;
                }
            },
            Err(error) => {
                fail_all_pending(&pending, &error.to_string()).await;
                report_died(&died, &error.to_string());
                return;
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

/// 会话终止时把未完成请求全部以 `TransportClosed` 失败（不悬挂）。
async fn fail_all_pending(pending: &Arc<Mutex<BTreeMap<Id, PendingReply>>>, reason: &str) {
    let replies = std::mem::take(&mut *pending.lock().await);
    for (_, sender) in replies {
        let _ = sender.send(Err(RpcError::TransportClosed {
            reason: reason.to_string(),
        }));
    }
}

fn report_died(died: &tokio::sync::watch::Sender<bool>, reason: &str) {
    // 死亡信号经 watch 交给 server actor（restart 状态机处理）。
    let _ = died.send(true);
    let _ = reason;
}

/// 后台 drain stderr：无人读取的 stderr 写满管道会阻塞子进程。
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
