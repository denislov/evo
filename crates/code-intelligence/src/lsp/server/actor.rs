//! `LspActor`：生命周期驱动的 actor 实现（server 公共面见
//! `crate::lsp::server` 的 `LspService` / `LspHandle`）。

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Sleep;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::lsp::diagnostics;
use crate::lsp::documents::DocumentStore;
use crate::lsp::edit;
use crate::lsp::state::{LspEvent, LspLifecycleState, apply_event, backoff_for};
use crate::lsp::transport::{LspSession, RpcError, ServerRequestReply};
use crate::lsp::wire::{self, Notification};

/// shutdown 请求超时。
const SHUTDOWN_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

impl LspActor {
    pub(crate) fn new(info: Arc<LspInfo>, shared: Arc<LspShared>) -> Self {
        let config = info.config.clone();
        let workspace_root = config.workspace_root.clone();
        Self {
            config,
            shared,
            documents: DocumentStore::new(workspace_root),
            diagnostics_store: DiagnosticStore::new(),
            session: None,
            cancel: CancellationToken::new(),
            attempt: 0,
        }
    }

    fn state(&self) -> LspLifecycleState {
        self.shared.state.lock().unwrap().clone()
    }

    fn transition(&self, event: LspEvent) {
        let state = self.shared.state.lock().unwrap().clone();
        match apply_event(state.clone(), event) {
            Ok(next) => *self.shared.state.lock().unwrap() = next,
            Err(error) => {
                // 非法转换：记录但不 panic（fail closed，事件被忽略）。
                *self.shared.last_error.lock().unwrap() = Some(error.to_string());
            }
        }
    }

    fn record_error(&self, detail: String) {
        *self.shared.last_error.lock().unwrap() = Some(detail);
    }
}

/// actor 主循环：单 select 驱动状态机 + 命令处理。
pub(crate) async fn run_actor(mut actor: LspActor, mut events: LspEvents) -> LspExit {
    let mut handled: u64 = 0;
    let mut backoff_sleep: Option<Pin<Box<Sleep>>> = None;
    // liveness 周期来自配置；reset() 把 next_tick 设为「构造时刻 + period」，
    // 构造时刻早已过去 → 进入 Ready 时 tick 立即触发一次，之后按周期走。
    let liveness_period = actor.config.liveness.ping_interval;
    let mut liveness_interval = tokio::time::interval(liveness_period);
    liveness_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut liveness_enabled = false;
    let mut previous_state: Option<LspLifecycleState> = None;

    // 首次启动。
    actor.do_start(&mut events).await;
    let mut handshaken = matches!(actor.state(), LspLifecycleState::Ready);

    loop {
        // 状态相关的定时器维护（Reconnecting → backoff；Ready → liveness）。
        let state = actor.state();
        if previous_state.as_ref() != Some(&state) {
            match state {
                LspLifecycleState::Reconnecting { attempt } => {
                    let delay = backoff_for(
                        attempt,
                        actor.config.backoff.initial,
                        actor.config.backoff.max,
                    );
                    backoff_sleep = Some(Box::pin(tokio::time::sleep(delay)));
                    liveness_enabled = false;
                }
                LspLifecycleState::Ready => {
                    backoff_sleep = None;
                    liveness_enabled = true;
                    liveness_interval.reset();
                }
                _ => {
                    backoff_sleep = None;
                    liveness_enabled = false;
                }
            }
            previous_state = Some(state);
        }

        tokio::select! {
            biased;
            changed = events.shutdown_rx.changed() => {
                let _ = changed;
                if *events.shutdown_rx.borrow() {
                    handshaken = matches!(actor.state(), LspLifecycleState::Ready);
                    actor.transition(LspEvent::Shutdown);
                    break;
                }
            }
            command = events.commands_rx.recv() => {
                let Some(command) = command else {
                    break; // 所有 handle 已 drop。
                };
                handled += 1;
                actor.handle_command(command).await;
                if *events.shutdown_rx.borrow() {
                    handshaken = matches!(actor.state(), LspLifecycleState::Ready);
                    actor.transition(LspEvent::Shutdown);
                    break;
                }
            }
            died = async {
                match &mut events.session_died {
                    Some(receiver) => {
                        let changed = receiver.changed().await;
                        changed.is_ok() && *receiver.borrow()
                    }
                    None => std::future::pending().await,
                }
            } => {
                if died {
                    actor.on_transport_died("transport closed", &mut events).await;
                }
            }
            server_request = async {
                match &mut events.server_requests_rx {
                    Some(receiver) => receiver.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some((request, reply)) = server_request {
                    actor.handle_server_request(request, reply).await;
                }
            }
            notification = async {
                match &mut events.notifications_rx {
                    Some(receiver) => receiver.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(notification) = notification {
                    actor.handle_notification(notification).await;
                }
            }
            _ = async {
                if liveness_enabled {
                    liveness_interval.tick().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                actor.check_liveness(&mut events).await;
            }
            _ = async {
                match &mut backoff_sleep {
                    Some(sleep) => sleep.as_mut().await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                backoff_sleep = None;
                actor.on_backoff_elapsed(&mut events).await;
            }
        }
    }

    // 确定性退出：记录握手状态（shutdown 消息只发给握手完成的会话），
    // 再执行关闭序列。
    actor.shutdown_sequence(handshaken).await;
    LspExit {
        reason: if *events.shutdown_rx.borrow() {
            LspShutdownReason::Manual
        } else {
            LspShutdownReason::SendersDropped
        },
        restart_count: *actor.shared.restart_count.lock().unwrap(),
        handled_commands: handled,
        panicked: false,
    }
}

impl LspActor {
    async fn handle_command(&mut self, command: LspCommand) {
        match command {
            LspCommand::Open {
                uri,
                language_id,
                version,
                text,
                reply,
            } => {
                let result = match self.documents.open(&uri, &language_id, version, &text) {
                    Ok(_) => {
                        self.notify_document(
                            "textDocument/didOpen",
                            edit::did_open_params(&uri, &language_id, version, &text),
                        )
                        .await;
                        Ok(())
                    }
                    Err(error) => Err(LspError::Document(error)),
                };
                let _ = reply.send(result);
            }
            LspCommand::Change {
                uri,
                version,
                changes,
                reply,
            } => {
                let result = match self.documents.change(&uri, version, &changes) {
                    Ok(document) => {
                        if let Ok(parsed) = self.documents.parse_uri(&uri) {
                            self.diagnostics_store
                                .document_changed(&parsed, document.version);
                        }
                        self.notify_document(
                            "textDocument/didChange",
                            edit::did_change_params(&uri, document.version, &document.text),
                        )
                        .await;
                        Ok(())
                    }
                    Err(error) => Err(LspError::Document(error)),
                };
                let _ = reply.send(result);
            }
            LspCommand::Close { uri, reply } => {
                let result = match self.documents.close(&uri) {
                    Ok(_document) => {
                        if let Ok(parsed) = self.documents.parse_uri(&uri) {
                            self.diagnostics_store.document_closed(&parsed);
                        }
                        self.notify_document("textDocument/didClose", edit::did_close_params(&uri))
                            .await;
                        Ok(())
                    }
                    Err(error) => Err(LspError::Document(error)),
                };
                let _ = reply.send(result);
            }
            LspCommand::Diagnostics { uri, reply } => {
                let result = match self.documents.parse_uri(&uri) {
                    Ok(parsed) => Ok(self
                        .diagnostics_store
                        .query(&parsed, self.config.stale_policy)
                        .cloned()),
                    Err(error) => Err(LspError::Document(error)),
                };
                let _ = reply.send(result);
            }
            LspCommand::Snapshot { reply } => {
                let state = self.state();
                let pid = self.session.as_ref().and_then(|session| session.pid());
                let _ = reply.send(Ok(LspSnapshot {
                    state,
                    pid,
                    restart_count: *self.shared.restart_count.lock().unwrap(),
                    open_documents: self.documents.replay_list(),
                    diagnostics: self.diagnostics_store.summary(),
                    last_error: self.shared.last_error.lock().unwrap().clone(),
                }));
            }
            LspCommand::PendingEdits { reply } => {
                let _ = reply.send(Ok(self.shared.pending_edits.lock().unwrap().clone()));
            }
            LspCommand::PullDiagnostics { uri, reply } => {
                self.spawn_pull_diagnostics(&uri, reply);
            }
            LspCommand::Query { query, reply } => {
                self.spawn_query(query, reply);
            }
        }
    }

    /// server 就绪时发送文档通知（未就绪时跳过——restart 后 replay 收敛）。
    async fn notify_document(&self, method: &str, params: Value) {
        if self.state() == LspLifecycleState::Ready
            && let Some(session) = &self.session
            && let Err(error) = session.notify(method, Some(params)).await
        {
            self.record_error(format!("{method} notify: {error}"));
        }
    }

    /// 网络命令（pull diagnostics / query）在独立 task 中执行（不阻塞
    /// actor 的命令顺序处理）；shutdown 时共享 cancel 令牌立即失败。
    fn spawn_pull_diagnostics(
        &self,
        uri: &str,
        reply: oneshot::Sender<Result<StoredDiagnostics, LspError>>,
    ) {
        let Some(session) = self.ready_session() else {
            let state = self.state();
            let _ = reply.send(Err(LspError::NotReady {
                state: state.as_str().into(),
            }));
            return;
        };
        let cancel = self.cancel.clone();
        let timeout = self.config.request_timeout;
        let workspace_root = self.config.workspace_root.clone();
        let uri = uri.to_string();
        tokio::spawn(async move {
            let result = async {
                let params = diagnostics::pull_params(&uri, None);
                let parsed = DocumentStore::new(workspace_root)
                    .parse_uri(&uri)
                    .map_err(LspError::Document)?;
                let response = session
                    .request(
                        "textDocument/pullDiagnostics",
                        Some(params),
                        timeout,
                        &cancel,
                    )
                    .await
                    .map_err(LspError::Rpc)?;
                let (items, _result_id) =
                    diagnostics::parse_pull_result(&response).ok_or_else(|| {
                        LspError::Rpc(RpcError::Other(
                            "pullDiagnostics result lacks 'items'".into(),
                        ))
                    })?;
                Ok::<_, LspError>(StoredDiagnostics {
                    uri: parsed,
                    version: None,
                    staleness: diagnostics::DiagnosticStaleness::Unknown,
                    items,
                })
            }
            .await;
            let _ = reply.send(result);
        });
    }

    fn spawn_query(
        &self,
        query: LspQuery,
        reply: oneshot::Sender<Result<LspQueryResult, LspError>>,
    ) {
        let Some(session) = self.ready_session() else {
            let state = self.state();
            let _ = reply.send(Err(LspError::NotReady {
                state: state.as_str().into(),
            }));
            return;
        };
        let cancel = self.cancel.clone();
        let timeout = self.config.request_timeout;
        tokio::spawn(async move {
            let (method, params) = crate::lsp::query::query_request(&query);
            let result = session
                .request(&method, Some(params), timeout, &cancel)
                .await
                .map(|result| LspQueryResult {
                    kind: query.kind,
                    uri: query.uri.clone(),
                    result,
                })
                .map_err(LspError::Rpc);
            let _ = reply.send(result);
        });
    }

    /// 就绪会话快照（非 Ready 返回 None）。
    fn ready_session(&self) -> Option<Arc<LspSession>> {
        if self.state() == LspLifecycleState::Ready {
            self.session.clone()
        } else {
            None
        }
    }

    /// 服务器请求：目前支持 `workspace/applyEdit`；其余回 method not found。
    async fn handle_server_request(&mut self, request: wire::Request, reply: ServerRequestReply) {
        let result = match request.method.as_str() {
            "workspace/applyEdit" => self.handle_apply_edit(request.params.as_ref()).await,
            _ => Err(wire::JsonRpcError::new(
                wire::METHOD_NOT_FOUND,
                format!("method not supported: {}", request.method),
            )),
        };
        let _ = reply.send(result);
    }

    /// `workspace/applyEdit`：校验 → 计划 → 注入 applicator（ChangeReceipt）；
    /// 无 applicator 拒绝并记录计划。
    async fn handle_apply_edit(
        &mut self,
        params: Option<&Value>,
    ) -> Result<Value, wire::JsonRpcError> {
        let params = params.cloned().unwrap_or(Value::Null);
        // LSP 协议：applyEdit 的 params 是 `{"edit": WorkspaceEdit}`。
        let edit_value = params.get("edit").cloned().unwrap_or(params);
        let edit = edit::parse_apply_edit_params(&edit_value).map_err(lsp_edit_error)?;
        let plan = edit::validate_edit(&edit, &self.documents).map_err(lsp_edit_error)?;
        match &self.config.applicator {
            Some(applicator) => match applicator.apply(&plan) {
                Ok(receipts) => {
                    self.shared.change_receipts.lock().unwrap().extend(receipts);
                    Ok(serde_json::json!({"applied": true}))
                }
                Err(error) => Err(lsp_edit_error(error)),
            },
            None => {
                // 无 applicator：拒绝（绝不静默吞掉 edit），计划记录供查询。
                self.shared.pending_edits.lock().unwrap().push(plan);
                Err(lsp_edit_error(EditError::NoApplicator))
            }
        }
    }

    /// `publishDiagnostics` 通知：入库（只存已打开文档的诊断）。
    async fn handle_notification(&mut self, notification: Notification) {
        let Some(params) = notification.params else {
            return;
        };
        if notification.method.as_str() == "textDocument/publishDiagnostics" {
            let Some((uri, version, items)) = diagnostics::parse_publish_params(&params) else {
                return;
            };
            let Ok(parsed) = self.documents.parse_uri(&uri) else {
                return; // 未打开文档的诊断不存储（fail closed）。
            };
            let doc_version = self
                .documents
                .get(&uri)
                .map_or(0, |document| document.version);
            self.diagnostics_store
                .publish(parsed, version, items, doc_version);
        } // 未知通知忽略。
    }

    /// Ready 下 liveness ping 失败 → 传输死亡处理。
    async fn check_liveness(&mut self, events: &mut LspEvents) {
        if self.state() != LspLifecycleState::Ready {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let liveness = self.config.liveness;
        match session
            .request("ping", None, liveness.ping_timeout, &self.cancel)
            .await
        {
            Ok(_) => {}
            Err(error) => {
                self.record_error(format!("liveness ping failed: {error}"));
                self.enter_reconnecting(LspEvent::LivenessFailed);
                self.close_current_session().await;
                self.events_take(events);
            }
        }
    }

    /// 传输死亡（崩溃 / EOF / 坏帧）：关闭会话并进入重启流程。
    async fn on_transport_died(&mut self, reason: &str, events: &mut LspEvents) {
        match self.state() {
            LspLifecycleState::Ready => {
                self.enter_reconnecting(LspEvent::TransportFailed);
            }
            LspLifecycleState::Initializing { .. } => {
                // 握手期间死亡：按握手失败处理（重试）。
                self.enter_reconnecting(LspEvent::HandshakeFailed);
            }
            _ => return,
        }
        self.record_error(format!("server died: {reason}"));
        self.close_current_session().await;
        self.events_take(events);
    }

    /// 进入 Reconnecting：全局 attempt 计数 +1（Ready 崩溃与握手失败都
    /// 累计），重写状态机给出的 attempt（状态机对 Ready 崩溃固定为 1），
    /// 超过上限即 GiveUp（Failed 终态）。
    fn enter_reconnecting(&mut self, event: LspEvent) {
        self.attempt = self.attempt.saturating_add(1);
        self.transition(event);
        if matches!(self.state(), LspLifecycleState::Reconnecting { .. }) {
            *self.shared.state.lock().unwrap() = LspLifecycleState::Reconnecting {
                attempt: self.attempt,
            };
            if self.attempt > self.config.max_restart_attempts {
                self.transition(LspEvent::GiveUp);
            }
        }
    }

    fn events_take(&mut self, events: &mut LspEvents) {
        events.session_died = None;
        events.notifications_rx = None;
        events.server_requests_rx = None;
    }

    /// 退避结束：开始下一次启动（restart_count + 1）。
    async fn on_backoff_elapsed(&mut self, events: &mut LspEvents) {
        if !matches!(self.state(), LspLifecycleState::Reconnecting { .. }) {
            return;
        }
        self.transition(LspEvent::BackoffElapsed);
        {
            let mut count = self.shared.restart_count.lock().unwrap();
            *count = count.saturating_add(1);
        }
        self.do_start(events).await;
    }

    /// 启动流程：spawn → initialize 握手 → initialized → replay documents。
    async fn do_start(&mut self, events: &mut LspEvents) {
        // 进入 Starting（Idle 首次 / Reconnecting 重启）。
        if matches!(self.state(), LspLifecycleState::Idle) {
            self.attempt = 1;
            self.transition(LspEvent::Start);
        }
        self.attempt = match self.state() {
            LspLifecycleState::Starting { attempt } => attempt,
            _ => self.attempt,
        };
        let (notifications_tx, notifications_rx) = mpsc::unbounded_channel();
        let (server_requests_tx, server_requests_rx) = mpsc::unbounded_channel();
        let session_config = self.config.session_config();

        match transport::open_session(session_config, notifications_tx, server_requests_tx).await {
            Ok((session, died_rx)) => {
                events.notifications_rx = Some(notifications_rx);
                events.server_requests_rx = Some(server_requests_rx);
                events.session_died = Some(died_rx);
                let session = Arc::new(session);
                self.session = Some(session.clone());
                self.transition(LspEvent::Spawned);

                let params = serde_json::json!({
                    "processId": std::process::id(),
                    "clientInfo": {"name": "evo", "version": "0.7.2"},
                    "capabilities": {},
                    "rootUri": format!("file://{}", self.config.workspace_root.display()),
                });
                match session
                    .request(
                        "initialize",
                        Some(params),
                        self.config.request_timeout,
                        &self.cancel,
                    )
                    .await
                {
                    Ok(result) => {
                        // 握手成功：验证 result 形状（宽松：对象即可）。
                        if result.is_object() {
                            self.transition(LspEvent::HandshakeDone);
                            let _ = session.notify("initialized", None).await.map_err(|error| {
                                self.record_error(format!("initialized notify: {error}"))
                            });
                            // document replay：按 uri 排序重发 didOpen。
                            for document in self.documents.replay_list() {
                                let _ = session
                                    .notify(
                                        "textDocument/didOpen",
                                        Some(edit::did_open_params(
                                            document.uri.as_str(),
                                            &document.language_id,
                                            document.version,
                                            &document.text,
                                        )),
                                    )
                                    .await
                                    .map_err(|error| {
                                        self.record_error(format!(
                                            "replay didOpen {}: {error}",
                                            document.uri.as_str()
                                        ))
                                    });
                            }
                            // 恢复 diagnostics staleness（文档版本已更新）。
                            diagnostics::refresh_all(&mut self.diagnostics_store, &self.documents);
                        } else {
                            self.record_error("initialize result is not an object".into());
                            self.enter_reconnecting(LspEvent::HandshakeFailed);
                            self.close_current_session().await;
                            self.events_take(events);
                        }
                    }
                    Err(error) => {
                        self.record_error(format!("initialize failed: {error}"));
                        self.enter_reconnecting(LspEvent::HandshakeFailed);
                        self.close_current_session().await;
                        self.events_take(events);
                    }
                }
            }
            Err(error) => {
                // spawn 失败（进程未创建）：不重试（Failed 终态）。
                self.record_error(format!("spawn failed: {error}"));
                self.transition(LspEvent::SpawnFailed);
            }
        }
    }

    /// 关闭当前会话（terminate + 回收读循环）。幂等。
    async fn close_current_session(&mut self) {
        if let Some(session) = self.session.take() {
            session.close().await;
        }
    }

    /// 确定性 shutdown：取消在途 → shutdown 请求 → exit 通知 → 终止进程。
    async fn shutdown_sequence(&mut self, handshaken: bool) {
        // 1. 停新请求（状态 ShuttingDown，submit 拒绝）。
        // 2. 取消在途网络请求（共享令牌）。
        self.cancel.cancel();
        // 3. shutdown/exit 消息用独立令牌（全局令牌已取消）。
        if let Some(session) = self.session.clone() {
            if handshaken {
                let no_cancel = CancellationToken::new();
                let _ = session
                    .request("shutdown", None, SHUTDOWN_REQUEST_TIMEOUT, &no_cancel)
                    .await;
                let _ = session.notify("exit", None).await;
                // 给 server 处理 exit 的窗口（自行退出优先于强杀）。
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            // 4. 终止进程并回收读循环。
            session.close().await;
        }
        self.session = None;
        self.transition(LspEvent::StopComplete);
        *self.shared.state.lock().unwrap() = LspLifecycleState::Stopped;
    }
}

fn lsp_edit_error(error: EditError) -> wire::JsonRpcError {
    wire::JsonRpcError::new(wire::INTERNAL_ERROR, error.to_string())
}
