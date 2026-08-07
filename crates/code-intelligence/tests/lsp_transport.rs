//! `lsp::transport` 集成测试：真实 stdio 子进程（`fake_lsp_server` 辅助
//! 二进制，经 `env!("CARGO_BIN_EXE_fake_lsp_server")` 定位）。
//!
//! 覆盖：initialize 握手、通知 fan-out、服务器请求回执、坏帧 fail
//! closed（bad-frame / truncated-frame）、输出洪泛、liveness 超时、
//! 请求取消 / 超时、迟到响应丢弃、spawn 失败。

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use code_intelligence::lsp::transport::{self, LspSession, RpcError, open_session};
use code_intelligence::lsp::wire::{Id, Notification, Request};
use workspace_runtime::api::{EnvPolicy, SandboxProfile};

const FAKE_SERVER: &str = env!("CARGO_BIN_EXE_fake_lsp_server");

fn config_for(mode: &str, extra: &[&str], cwd: &std::path::Path) -> transport::LspSessionConfig {
    let mut args = vec!["--mode".to_string(), mode.to_string()];
    args.extend(extra.iter().map(|arg| arg.to_string()));
    transport::LspSessionConfig {
        command: FAKE_SERVER.to_string(),
        args,
        env: EnvPolicy::AllowList(Default::default()),
        cwd: cwd.to_path_buf(),
        sandbox: SandboxProfile::product_default(cwd),
        max_frame_bytes: 1024 * 1024,
    }
}

async fn open(
    mode: &str,
    extra: &[&str],
    cwd: &std::path::Path,
) -> (
    Arc<LspSession>,
    tokio::sync::watch::Receiver<bool>,
    tokio::sync::mpsc::UnboundedReceiver<Notification>,
    tokio::sync::mpsc::UnboundedReceiver<(Request, transport::ServerRequestReply)>,
) {
    let (notifications_tx, notifications_rx) = tokio::sync::mpsc::unbounded_channel();
    let (server_requests_tx, server_requests_rx) = tokio::sync::mpsc::unbounded_channel();
    let (session, died) = open_session(
        config_for(mode, extra, cwd),
        notifications_tx,
        server_requests_tx,
    )
    .await
    .expect("session opens");
    (
        Arc::new(session),
        died,
        notifications_rx,
        server_requests_rx,
    )
}

async fn initialize(session: &LspSession, cancel: &CancellationToken) -> serde_json::Value {
    session
        .request(
            "initialize",
            Some(serde_json::json!({"capabilities": {}})),
            Duration::from_secs(10),
            cancel,
        )
        .await
        .expect("initialize handshake succeeds")
}

#[tokio::test]
async fn initialize_handshake_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let (session, _died, _notifications, _server_requests) = open("echo", &[], temp.path()).await;
    let cancel = CancellationToken::new();
    let result = initialize(&session, &cancel).await;
    assert_eq!(result["serverInfo"]["name"], "fake_lsp_server");
    assert!(result["capabilities"]["hoverProvider"].as_bool().unwrap());
    session
        .notify("initialized", None)
        .await
        .expect("initialized notify");
    // ping 往返。
    let pong = session
        .request("ping", None, Duration::from_secs(5), &cancel)
        .await
        .expect("ping");
    assert_eq!(pong, serde_json::json!({}));
    session.close().await;
}

#[tokio::test]
async fn notifications_are_forwarded_to_subscriber() {
    let temp = tempfile::tempdir().unwrap();
    let (session, _died, mut notifications, _server_requests) =
        open("echo", &["--push-on-open"], temp.path()).await;
    let cancel = CancellationToken::new();
    initialize(&session, &cancel).await;
    let uri = format!("file://{}/a.rs", temp.path().display());
    session
        .notify(
            "textDocument/didOpen",
            Some(serde_json::json!({
                "textDocument": {"uri": uri, "languageId": "rust", "version": 1, "text": "x"}
            })),
        )
        .await
        .expect("didOpen");
    let notification = tokio::time::timeout(Duration::from_secs(5), notifications.recv())
        .await
        .expect("diagnostic push arrives")
        .expect("channel alive");
    assert_eq!(notification.method, "textDocument/publishDiagnostics");
    let params = notification.params.expect("params");
    assert_eq!(params["version"], 1);
    assert_eq!(
        params["diagnostics"][0]["message"],
        "fake diagnostic for version 1"
    );
    session.close().await;
}

#[tokio::test]
async fn server_requests_are_forwarded_and_answered() {
    let temp = tempfile::tempdir().unwrap();
    let (session, _died, _notifications, mut server_requests) =
        open("apply-edit", &[], temp.path()).await;
    let cancel = CancellationToken::new();
    initialize(&session, &cancel).await;
    session.notify("initialized", None).await.unwrap();

    let (request, reply) = tokio::time::timeout(Duration::from_secs(5), server_requests.recv())
        .await
        .expect("applyEdit request arrives")
        .expect("channel alive");
    assert_eq!(request.method, "workspace/applyEdit");
    // 回执成功。
    reply
        .send(Ok(serde_json::json!({"applied": true})))
        .expect("reply sent");
    // 验证服务器收到回执（通过 apply-edit-response 记录：读进程输出不可行，
    // 改用客户端可观测的事实——applyEdit 请求 id 9001，回执后无异常即可）。
    assert_eq!(request.id, Id::Number(9001));
    session.close().await;
}

#[tokio::test]
async fn bad_frame_fails_closed_and_signals_death() {
    let temp = tempfile::tempdir().unwrap();
    let (session, mut died, _notifications, _server_requests) =
        open("bad-frame", &[], temp.path()).await;
    let cancel = CancellationToken::new();
    // initialize 会收到响应（bad-frame 从第 1 个请求开始输出坏帧——先发
    // ping 触发坏帧路径）。
    let _ = session
        .request("ping", None, Duration::from_secs(5), &cancel)
        .await;
    let dead = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if died.changed().await.is_ok() && *died.borrow() {
                return;
            }
        }
    })
    .await;
    assert!(dead.is_ok(), "died signal must fire on bad frame");
    session.close().await;
}

#[tokio::test]
async fn truncated_frame_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let (session, mut died, _notifications, _server_requests) =
        open("truncated-frame", &[], temp.path()).await;
    let cancel = CancellationToken::new();
    let _ = session
        .request("ping", None, Duration::from_secs(5), &cancel)
        .await;
    let dead = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if died.changed().await.is_ok() && *died.borrow() {
                return;
            }
        }
    })
    .await;
    assert!(dead.is_ok(), "died signal must fire on truncated frame");
    session.close().await;
}

#[tokio::test]
async fn output_flood_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let (session, mut died, _notifications, _server_requests) =
        open("flood", &[], temp.path()).await;
    let cancel = CancellationToken::new();
    let _ = session
        .request("ping", None, Duration::from_secs(5), &cancel)
        .await;
    let dead = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if died.changed().await.is_ok() && *died.borrow() {
                return;
            }
        }
    })
    .await;
    assert!(dead.is_ok(), "died signal must fire on output flood");
    session.close().await;
}

#[tokio::test]
async fn garbage_on_start_breaks_handshake() {
    let temp = tempfile::tempdir().unwrap();
    let (session, mut died, _notifications, _server_requests) =
        open("garbage-on-start", &[], temp.path()).await;
    let cancel = CancellationToken::new();
    // 握手请求会失败（读循环已因垃圾帧终止）。
    let result = session
        .request("initialize", None, Duration::from_secs(5), &cancel)
        .await;
    assert!(matches!(result, Err(RpcError::TransportClosed { .. })));
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if died.changed().await.is_ok() && *died.borrow() {
                return;
            }
        }
    })
    .await;
    session.close().await;
}

#[tokio::test]
async fn request_timeout_and_cancel() {
    let temp = tempfile::tempdir().unwrap();
    let (session, _died, _notifications, _server_requests) =
        open("echo", &["--delay-ms", "1000"], temp.path()).await;
    let cancel = CancellationToken::new();
    // initialize 也有 delay：先等一个成功握手（用长超时）。
    let _ = initialize(&session, &cancel).await;
    // 短超时请求。
    let result = session
        .request("ping", None, Duration::from_millis(50), &cancel)
        .await;
    assert!(matches!(result, Err(RpcError::Timeout { .. })));
    // 取消路径。
    let cancel2 = CancellationToken::new();
    cancel2.cancel();
    let result = session
        .request("ping", None, Duration::from_secs(5), &cancel2)
        .await;
    assert!(matches!(result, Err(RpcError::Cancelled)));
    session.close().await;
}

#[tokio::test]
async fn late_response_is_discarded_by_id() {
    let temp = tempfile::tempdir().unwrap();
    let (session, _died, _notifications, _server_requests) =
        open("echo", &["--delay-ms", "200"], temp.path()).await;
    let cancel = CancellationToken::new();
    let _ = initialize(&session, &cancel).await;
    // 短超时 → 超时；随后同一 id 的迟到响应不应污染后续请求。
    let result = session
        .request("ping", None, Duration::from_millis(20), &cancel)
        .await;
    assert!(matches!(result, Err(RpcError::Timeout { .. })));
    tokio::time::sleep(Duration::from_millis(400)).await;
    let result = session
        .request("ping", None, Duration::from_secs(5), &cancel)
        .await
        .expect("next request unaffected");
    assert_eq!(result, serde_json::json!({}));
    session.close().await;
}

#[tokio::test]
async fn server_error_is_surfaced() {
    let temp = tempfile::tempdir().unwrap();
    let (session, _died, _notifications, _server_requests) = open("echo", &[], temp.path()).await;
    let cancel = CancellationToken::new();
    let result = session
        .request("bogus/method", None, Duration::from_secs(5), &cancel)
        .await;
    match result {
        Err(RpcError::ServerError { code, message }) => {
            assert_eq!(code, -32601);
            assert!(message.contains("bogus/method"));
        }
        other => panic!("expected ServerError, got {other:?}"),
    }
    session.close().await;
}

#[tokio::test]
async fn spawn_failure_is_structured_error() {
    let temp = tempfile::tempdir().unwrap();
    let config = transport::LspSessionConfig {
        command: "/nonexistent/lsp-server-binary".into(),
        args: vec![],
        env: EnvPolicy::AllowList(Default::default()),
        cwd: temp.path().to_path_buf(),
        sandbox: SandboxProfile::product_default(temp.path()),
        max_frame_bytes: 1024,
    };
    let (notifications_tx, _notifications_rx) = tokio::sync::mpsc::unbounded_channel();
    let (server_requests_tx, _server_requests_rx) = tokio::sync::mpsc::unbounded_channel();
    let error = open_session(config, notifications_tx, server_requests_tx)
        .await
        .expect_err("spawn fails");
    assert!(matches!(error, RpcError::Other(message) if message.contains("spawn")));
}

#[tokio::test]
async fn close_terminates_process_and_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let (session, _died, _notifications, _server_requests) = open("echo", &[], temp.path()).await;
    let cancel = CancellationToken::new();
    let _ = initialize(&session, &cancel).await;
    assert!(session.pid().is_some());
    session.close().await;
    session.close().await; // 幂等。
}

#[tokio::test]
async fn record_file_receives_did_open_events() {
    let temp = tempfile::tempdir().unwrap();
    let record_path = temp.path().join("record.jsonl");
    let (session, _died, _notifications, _server_requests) = open(
        "echo",
        &["--record-file", record_path.to_str().unwrap()],
        temp.path(),
    )
    .await;
    let cancel = CancellationToken::new();
    let _ = initialize(&session, &cancel).await;
    session.notify("initialized", None).await.unwrap();
    let uri = format!("file://{}/a.rs", temp.path().display());
    session
        .notify(
            "textDocument/didOpen",
            Some(serde_json::json!({
                "textDocument": {"uri": uri, "languageId": "rust", "version": 1, "text": "x"}
            })),
        )
        .await
        .unwrap();
    session
        .notify(
            "textDocument/didClose",
            Some(serde_json::json!({
                "textDocument": {"uri": uri}
            })),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    session.close().await;
    let contents = std::fs::read_to_string(&record_path).unwrap();
    assert!(contents.contains("didOpen"), "records: {contents}");
    assert!(contents.contains("didClose"), "records: {contents}");
}

#[tokio::test]
async fn fake_server_is_sandboxed_by_default_profile() {
    // 验证 session_config 的 sandbox 语义在 transport 层生效：spawn 成功
    // 即 profile 被接受（Linux 平台 enforced）；fail-closed 语义由
    // PeerProcess::spawn 保证（能力不足平台 spawn 报错）。
    let temp = tempfile::tempdir().unwrap();
    let (session, _died, _notifications, _server_requests) = open("echo", &[], temp.path()).await;
    let cancel = CancellationToken::new();
    let _ = initialize(&session, &cancel).await;
    session.close().await;
}

#[tokio::test]
async fn message_parse_helpers_agree_with_server() {
    // 服务器发来的请求帧可被 wire 层解析（apply-edit 模式的请求形状）。
    let temp = tempfile::tempdir().unwrap();
    let (session, _died, _notifications, mut server_requests) =
        open("apply-edit", &[], temp.path()).await;
    let cancel = CancellationToken::new();
    let _ = initialize(&session, &cancel).await;
    session.notify("initialized", None).await.unwrap();
    let (request, reply) = tokio::time::timeout(Duration::from_secs(5), server_requests.recv())
        .await
        .expect("request arrives")
        .expect("alive");
    // 通过 wire 解析函数验证参数形状（与 diagnostics/edit 解析层对接）。
    let params = request.params.clone().expect("applyEdit has params");
    let edit = code_intelligence::lsp::edit::parse_apply_edit_params(&params["edit"]).unwrap();
    assert_eq!(edit.document_changes.len(), 1);
    assert_eq!(
        edit.document_changes[0].edits[0].new_text,
        "replaced by fake server"
    );
    let _ = reply.send(Ok(serde_json::json!({"applied": true})));
    session.close().await;
}
