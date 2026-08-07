//! MCP adapter 集成测试：真实 stdio 子进程（`fake_mcp_server` 辅助二进制，
//! 经 `env!("CARGO_BIN_EXE_fake_mcp_server")` 定位）。
//!
//! 覆盖：initialize 握手、tools/list 转换、tools/call 转发、per-tool
//! timeout、取消、liveness 超时重连、进程崩溃重连、tools/list_changed
//! 热更新、输出洪泛、非法 JSON、初始失败不重试、shutdown 顺序与在途
//! 调用取消、未启动/已关闭 server 不可用。

use std::sync::Arc;
use std::time::Duration;

use extension_host::api::*;
use serde_json::json;
use tokio_util::sync::CancellationToken;

const FAKE_SERVER: &str = env!("CARGO_BIN_EXE_fake_mcp_server");
const FAKE_HTTP_SERVER: &str = env!("CARGO_BIN_EXE_fake_mcp_http_server");

fn stdio_config(mode: &str, extra: &[&str]) -> TransportConfig {
    let mut args = vec!["--mode".to_string(), mode.to_string()];
    args.extend(extra.iter().map(|arg| arg.to_string()));
    TransportConfig::Stdio(StdioConfig {
        command: FAKE_SERVER.to_string(),
        args,
        env: workspace_runtime::api::EnvPolicy::AllowList(Default::default()),
        cwd: None,
        sandbox: None,
    })
}

fn host_for(mode: &str, extra: &[&str], patch: impl FnOnce(&mut McpServerConfig)) -> McpHost {
    let mut config = McpServerConfig::new("fake", stdio_config(mode, extra));
    config.tool_timeout = Duration::from_secs(5);
    config.liveness = LivenessConfig {
        ping_interval: Duration::from_millis(150),
        ping_timeout: Duration::from_millis(100),
    };
    config.reconnect = ReconnectConfig {
        initial_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_millis(200),
    };
    patch(&mut config);
    McpHost::new(
        vec![config],
        Arc::new(FileCredentialStore::new(
            tempfile::tempdir().unwrap().path(),
        )),
    )
}

async fn wait_ready(host: &McpHost, server: &str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if host.server_state(server).is_ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "fake server '{server}' did not become Ready; state={:?}",
        host.server_state(server)
    );
}

#[tokio::test]
async fn initialize_handshake_and_tool_discovery() {
    let host = host_for("echo", &[], |_| {});
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;

    let tools = host.tools();
    let fake = tools.get("fake").expect("fake server tools discovered");
    let names: Vec<_> = fake.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, ["echo", "slow"]);
    assert_eq!(fake[0].description, "Echo the arguments back");
    assert!(host.tools_version() >= 1);
    host.shutdown().await;
}

#[tokio::test]
async fn tool_call_forwards_arguments_and_returns_content() {
    let host = host_for("echo", &[], |_| {});
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;

    let handle = host.servers().first().unwrap().clone();
    let result = handle
        .call_tool("echo", json!({"hello": "world"}), &CancellationToken::new())
        .await
        .expect("tool call succeeds");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        text, r#"fake:echo:{"hello":"world"}"#,
        "arguments forwarded verbatim"
    );
    host.shutdown().await;
}

#[tokio::test]
async fn per_tool_timeout_applies() {
    let host = host_for("echo", &["--call-delay-ms", "2000"], |config| {
        config.tool_timeout = Duration::from_millis(150)
    });
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;

    let handle = host.servers().first().unwrap().clone();
    let error = handle
        .call_tool("echo", json!({}), &CancellationToken::new())
        .await
        .expect_err("call must time out");
    assert!(
        matches!(error, RpcError::Timeout { .. }),
        "expected timeout, got {error:?}"
    );
    host.shutdown().await;
}

#[tokio::test]
async fn call_cancellation_applies() {
    let host = host_for("echo", &["--call-delay-ms", "2000"], |config| {
        config.tool_timeout = Duration::from_secs(10)
    });
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;

    let handle = host.servers().first().unwrap().clone();
    let cancel = CancellationToken::new();
    let task = tokio::spawn({
        let handle = handle.clone();
        let cancel = cancel.clone();
        async move { handle.call_tool("echo", json!({}), &cancel).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();
    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancelled call returns promptly")
        .expect("no panic")
        .expect_err("call must be cancelled");
    assert!(matches!(error, RpcError::Cancelled));
    host.shutdown().await;
}

#[tokio::test]
async fn liveness_ping_timeout_triggers_reconnect_and_rediscovery() {
    let host = host_for("ping-drop", &[], |_| {});
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;
    let first_version = host.tools_version();

    // ping 被静默丢弃 → liveness 失败 → 重连（重新 initialize + 重新发现）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut reconnected = false;
    while tokio::time::Instant::now() < deadline {
        if host.server_state("fake").is_ready() && host.tools_version() > first_version {
            reconnected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        reconnected,
        "server must reconnect after liveness loss and rediscover tools"
    );
    host.shutdown().await;
}

#[tokio::test]
async fn process_crash_triggers_reconnect_and_recovers() {
    let crash_dir = tempfile::tempdir().unwrap();
    let crash_file = crash_dir.path().join("crashed.marker");
    let host = host_for(
        "crash-on-call",
        &["--crash-file", crash_file.to_str().unwrap()],
        |_| {},
    );
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;

    let handle = host.servers().first().unwrap().clone();
    let first_call = handle
        .call_tool("echo", json!({}), &CancellationToken::new())
        .await;
    assert!(
        first_call.is_err(),
        "crash-on-call server must fail the in-flight call"
    );

    // 进程死亡 → 重连 → 工具重新发现 → 后续调用成功。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut recovered = false;
    while tokio::time::Instant::now() < deadline {
        if handle
            .call_tool("echo", json!({"round": 2}), &CancellationToken::new())
            .await
            .is_ok()
        {
            recovered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(recovered, "crashed server must reconnect and recover");
    host.shutdown().await;
}

#[tokio::test]
async fn tools_list_changed_refreshes_cache() {
    let host = host_for(
        "list-changed",
        &["--grow-tools", "--list-changed-delay-ms", "300"],
        |_| {},
    );
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;
    let before = host.tools();
    assert_eq!(
        before.get("fake").map(Vec::len),
        Some(2),
        "initial discovery lists the configured tools"
    );

    // 收到 notifications/tools/list_changed → 重新 list（grow-tools 追加）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut refreshed = false;
    while tokio::time::Instant::now() < deadline {
        let after = host.tools();
        if after.get("fake").map(Vec::len) == Some(3) {
            refreshed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(
        refreshed,
        "tools/list_changed must refresh the cached tool list"
    );
    host.shutdown().await;
}

#[tokio::test]
async fn output_flood_does_not_kill_transport() {
    let host = host_for("flood", &[], |_| {});
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;

    let handle = host.servers().first().unwrap().clone();
    for round in 0..3 {
        let result = handle
            .call_tool("echo", json!({"round": round}), &CancellationToken::new())
            .await
            .expect("flooding server must still answer");
        assert!(result["content"][0]["text"].is_string());
    }
    host.shutdown().await;
}

#[tokio::test]
async fn bad_json_lines_are_skipped_not_fatal() {
    let host = host_for("bad-json", &[], |_| {});
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;

    let handle = host.servers().first().unwrap().clone();
    // 每第 3 个请求前输出坏 JSON 行：transport 必须继续可用。
    for round in 0..6 {
        handle
            .call_tool("echo", json!({"round": round}), &CancellationToken::new())
            .await
            .expect("bad-json lines must be skipped, transport stays alive");
    }
    host.shutdown().await;
}

#[tokio::test]
async fn initial_failure_does_not_retry() {
    let host = host_for("crash-after-init", &[], |_| {});
    host.start().unwrap();
    // initialize 成功后进程退出 → 初始连接失败（attempt 0）→ Failed。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let state = host.server_state("fake");
        if matches!(
            state,
            ServerLifecycleState::Failed { .. } | ServerLifecycleState::Terminated
        ) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("expected Failed state, got {state:?}");
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let handle = host.servers().first().unwrap().clone();
    let error = handle
        .call_tool("echo", json!({}), &CancellationToken::new())
        .await
        .expect_err("failed server must not accept calls");
    assert!(error.to_string().contains("not connected"));
    host.shutdown().await;
}

#[tokio::test]
async fn shutdown_cancels_in_flight_call_and_terminates_child() {
    let host = host_for("echo", &["--call-delay-ms", "2000"], |config| {
        config.tool_timeout = Duration::from_secs(30)
    });
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;

    let handle = host.servers().first().unwrap().clone();
    let cancel = CancellationToken::new();
    let task = tokio::spawn({
        let handle = handle.clone();
        let cancel = cancel.clone();
        async move { handle.call_tool("echo", json!({}), &cancel).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let started = std::time::Instant::now();
    host.shutdown().await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "shutdown must not wait for the in-flight call"
    );
    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("in-flight call returns promptly after shutdown")
        .expect("no panic")
        .expect_err("in-flight call must be cancelled by shutdown");
    assert!(matches!(error, RpcError::Cancelled));
}

#[tokio::test]
async fn unstarted_and_shutdown_hosts_are_unavailable() {
    // 未 start：call 返回结构化错误（server 未连接）。
    let host = host_for("echo", &[], |_| {});
    let handle = host.servers().first().unwrap().clone();
    let error = handle
        .call_tool("echo", json!({}), &CancellationToken::new())
        .await
        .expect_err("unstarted host must reject calls");
    assert!(error.to_string().contains("not connected"));

    // 已 shutdown：同一语义（在途调用被取消 / 新调用拒绝）。
    host.start().unwrap();
    host.shutdown().await;
    let error = handle
        .call_tool("echo", json!({}), &CancellationToken::new())
        .await
        .expect_err("shutdown host must reject calls");
    assert!(error.to_string().contains("not connected"));
}

#[tokio::test]
async fn disabled_server_is_not_assembled_or_callable() {
    let mut config = McpServerConfig::new("off", stdio_config("echo", &[]));
    config.enabled = false;
    let host = McpHost::new(
        vec![config],
        Arc::new(FileCredentialStore::new(
            tempfile::tempdir().unwrap().path(),
        )),
    );
    host.start().unwrap();
    assert!(host.servers().is_empty());
    assert_eq!(host.server_state("off"), ServerLifecycleState::Disconnected);
    host.shutdown().await;
}

/// mock OAuth 端点：`/token` 走 `token_behavior`，`/device` 返回设备码。
/// 返回 `(device_authorization_endpoint, token_endpoint)`。
fn start_oauth_mock(
    token_behavior: impl Fn() -> (u16, serde_json::Value) + Send + Sync + 'static,
) -> (String, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            use std::io::{Read, Write};
            let mut stream = stream;
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        request.extend_from_slice(&buf[..n]);
                        if request.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let request = String::from_utf8_lossy(&request);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split(' ').nth(1))
                .unwrap_or("/");
            let (status, body) = if path.starts_with("/device") {
                (
                    200,
                    json!({
                        "device_code": "dc-1",
                        "user_code": "UC-123",
                        "verification_uri": "http://localhost/verify",
                        "expires_in": 900,
                        "interval": 1,
                    }),
                )
            } else {
                token_behavior()
            };
            let body = serde_json::to_vec(&body).unwrap();
            let reason = if status == 200 { "OK" } else { "Bad Request" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    (
        format!("http://{addr}/device"),
        format!("http://{addr}/token"),
    )
}

fn oauth_config(device_endpoint: &str, token_endpoint: &str) -> OAuthConfig {
    OAuthConfig {
        client_id: "evo-test".into(),
        scopes: Vec::new(),
        device_authorization_endpoint: device_endpoint.into(),
        token_endpoint: token_endpoint.into(),
    }
}

fn fast_oauth_runtime() -> OAuthRuntime {
    OAuthRuntime {
        poll_interval: Duration::from_millis(10),
        flow_timeout: Duration::from_secs(10),
        present_verification: Some(Arc::new(|_, _| {})),
        ..Default::default()
    }
}

/// 收集 MCP host 诊断的 sink。
#[derive(Debug, Default, Clone)]
struct CollectingDiagnostics {
    records: Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

impl DiagnosticSink for CollectingDiagnostics {
    fn emit(&self, record: DiagnosticRecord) {
        self.records
            .lock()
            .unwrap()
            .push((record.code, record.message));
    }
}

#[tokio::test]
async fn oauth_401_refreshes_token_and_retries_once() {
    // 服务器对首个 tools/call 返回 -32001（UNAUTHORIZED），之后正常。
    // 预置过期 access token + refresh token；401 → refresh → retry 成功。
    let (device, token) = start_oauth_mock(|| {
        (
            200,
            json!({"access_token": "at-refreshed", "refresh_token": "rt-rotated", "expires_in": 3600}),
        )
    });
    let store = Arc::new(FileCredentialStore::new(
        tempfile::tempdir().unwrap().path(),
    ));
    store
        .set(
            "fake",
            McpCredentials {
                access_token: "at-stale".into(),
                refresh_token: Some("rt-1".into()),
                expires_at: None,
            },
        )
        .unwrap();
    let mut config =
        McpServerConfig::new("fake", stdio_config("echo", &["--auth-fail-on-call", "1"]));
    config.oauth = Some(oauth_config(&device, &token));
    let host = McpHost::new(vec![config], store.clone());
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;

    let handle = host.servers().first().unwrap().clone();
    let result = handle
        .call_tool("echo", json!({"round": 1}), &CancellationToken::new())
        .await
        .expect("401 → refresh → retry must succeed");
    assert!(result["content"][0]["text"].is_string());
    let refreshed = store.get("fake").expect("refreshed credentials stored");
    assert_eq!(refreshed.access_token, "at-refreshed");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("rt-rotated"));
    host.shutdown().await;
}

#[tokio::test]
async fn oauth_401_refresh_failure_falls_back_to_device_flow_and_surfaces_error() {
    // token 端点 400（refresh 失败）→ device flow 也失败 → 结构化错误。
    let (device, token) = start_oauth_mock(|| (400, json!({"error": "invalid_grant"})));
    let diagnostics = Arc::new(CollectingDiagnostics::default());
    let mut config =
        McpServerConfig::new("fake", stdio_config("echo", &["--auth-fail-on-call", "1"]));
    config.oauth = Some(oauth_config(&device, &token));
    let store = Arc::new(FileCredentialStore::new(
        tempfile::tempdir().unwrap().path(),
    ));
    store
        .set(
            "fake",
            McpCredentials {
                access_token: "at-stale".into(),
                refresh_token: Some("rt-1".into()),
                expires_at: None,
            },
        )
        .unwrap();
    let host = McpHost::with_diagnostics(
        vec![config],
        store,
        fast_oauth_runtime(),
        diagnostics.clone(),
    );
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;

    let handle = host.servers().first().unwrap().clone();
    let error = handle
        .call_tool("echo", json!({}), &CancellationToken::new())
        .await
        .expect_err("401 with failing recovery must surface a structured error");
    assert!(
        error.is_unauthorized(),
        "recovery failure keeps the original unauthorized error, got {error:?}"
    );
    let records = diagnostics.records.lock().unwrap().clone();
    assert!(
        records.iter().any(|(code, _)| code == "mcp_refresh_failed"),
        "refresh failure must be recorded: {records:?}"
    );
    host.shutdown().await;
}

/// 启动 fake HTTP MCP server，返回监听地址（从 stdout 的 LISTENING 行）。
async fn start_fake_http_server(extra: &[&str]) -> (String, tokio::process::Child) {
    let mut child = tokio::process::Command::new(FAKE_HTTP_SERVER)
        .args(extra)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn fake http server");
    let stdout = child.stdout.take().unwrap();
    let mut reader = tokio::io::BufReader::new(stdout);
    let mut line = String::new();
    tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line)
        .await
        .expect("fake http server prints LISTENING");
    assert!(
        line.starts_with("LISTENING "),
        "expected LISTENING line, got: {line}"
    );
    let addr = line.trim().trim_start_matches("LISTENING ").to_string();
    (addr, child)
}

fn http_config(addr: &str, patch: impl FnOnce(&mut McpServerConfig)) -> McpServerConfig {
    let mut config = McpServerConfig::new(
        "fake",
        TransportConfig::Http(HttpConfig {
            url: format!("http://{addr}/mcp"),
            headers: Vec::new(),
        }),
    );
    config.tool_timeout = Duration::from_secs(5);
    config.liveness = LivenessConfig {
        ping_interval: Duration::from_millis(500),
        ping_timeout: Duration::from_millis(200),
    };
    patch(&mut config);
    config
}

fn read_headers_file(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn http_oauth_refresh_injects_refreshed_token_into_retry() {
    // fake HTTP server：首个 tools/call 返回 401，之后正常；记录每个
    // 请求收到的 Authorization。store 预置过期 token → 401 → refresh →
    // 重试请求必须携带 `Bearer <new-token>`（服务端记录的 header 断言）。
    let (device, token) = start_oauth_mock(|| {
        (
            200,
            json!({"access_token": "at-refreshed", "refresh_token": "rt-rotated", "expires_in": 3600}),
        )
    });
    let headers_dir = tempfile::tempdir().unwrap();
    let headers_file = headers_dir.path().join("headers.log");
    let (addr, mut server) = start_fake_http_server(&[
        "--auth-fail-calls",
        "1",
        "--headers-file",
        headers_file.to_str().unwrap(),
    ])
    .await;
    let store = Arc::new(FileCredentialStore::new(
        tempfile::tempdir().unwrap().path(),
    ));
    store
        .set(
            "fake",
            McpCredentials {
                access_token: "at-stale".into(),
                refresh_token: Some("rt-1".into()),
                expires_at: None,
            },
        )
        .unwrap();
    let mut config = http_config(&addr, |_| {});
    config.oauth = Some(oauth_config(&device, &token));
    let host = McpHost::with_diagnostics(
        vec![config],
        store,
        fast_oauth_runtime(),
        Arc::new(NoopDiagnosticSink),
    );
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;

    let handle = host.servers().first().unwrap().clone();
    let result = handle
        .call_tool("echo", json!({"round": 1}), &CancellationToken::new())
        .await
        .expect("401 → refresh → retry must succeed");
    assert_eq!(result["content"][0]["text"], "fake:echo:{\"round\":1}");
    let lines = read_headers_file(&headers_file);
    let calls: Vec<_> = lines
        .iter()
        .filter(|line| line.contains("method=tools/call"))
        .collect();
    assert_eq!(
        calls.len(),
        2,
        "tools/call must be attempted exactly twice: {lines:?}"
    );
    assert!(
        calls[0].contains("authorization=Bearer at-stale"),
        "first call carries the stale token: {lines:?}"
    );
    assert!(
        calls[1].contains("authorization=Bearer at-refreshed"),
        "retry must carry the refreshed token: {lines:?}"
    );
    host.shutdown().await;
    server.kill().await.ok();
}

#[tokio::test]
async fn http_static_authorization_is_used_without_store_token() {
    // 静态配置 Authorization + store 无 token → 请求带静态 header。
    let headers_dir = tempfile::tempdir().unwrap();
    let headers_file = headers_dir.path().join("headers.log");
    let (addr, mut server) =
        start_fake_http_server(&["--headers-file", headers_file.to_str().unwrap()]).await;
    let store = Arc::new(FileCredentialStore::new(
        tempfile::tempdir().unwrap().path(),
    ));
    let config = http_config(&addr, |config| {
        if let TransportConfig::Http(http) = &mut config.transport {
            http.headers = vec![("authorization".into(), "Bearer static-token".into())];
        }
    });
    let host = McpHost::with_diagnostics(
        vec![config],
        store,
        fast_oauth_runtime(),
        Arc::new(NoopDiagnosticSink),
    );
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;

    let handle = host.servers().first().unwrap().clone();
    handle
        .call_tool("echo", json!({}), &CancellationToken::new())
        .await
        .expect("call succeeds");
    let lines = read_headers_file(&headers_file);
    let calls: Vec<_> = lines
        .iter()
        .filter(|line| line.contains("method=tools/call"))
        .collect();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].contains("authorization=Bearer static-token"),
        "static authorization must be used: {lines:?}"
    );
    host.shutdown().await;
    server.kill().await.ok();
}

#[tokio::test]
async fn http_dynamic_credentials_override_static_authorization() {
    // store 有 token（refresh 后）→ 动态 Authorization 覆盖静态配置。
    let headers_dir = tempfile::tempdir().unwrap();
    let headers_file = headers_dir.path().join("headers.log");
    let (addr, mut server) =
        start_fake_http_server(&["--headers-file", headers_file.to_str().unwrap()]).await;
    let store = Arc::new(FileCredentialStore::new(
        tempfile::tempdir().unwrap().path(),
    ));
    store
        .set(
            "fake",
            McpCredentials {
                access_token: "at-dynamic".into(),
                refresh_token: None,
                expires_at: None,
            },
        )
        .unwrap();
    let config = http_config(&addr, |config| {
        if let TransportConfig::Http(http) = &mut config.transport {
            http.headers = vec![("authorization".into(), "Bearer static-token".into())];
        }
    });
    let host = McpHost::with_diagnostics(
        vec![config],
        store,
        fast_oauth_runtime(),
        Arc::new(NoopDiagnosticSink),
    );
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;

    let handle = host.servers().first().unwrap().clone();
    handle
        .call_tool("echo", json!({}), &CancellationToken::new())
        .await
        .expect("call succeeds");
    let lines = read_headers_file(&headers_file);
    let calls: Vec<_> = lines
        .iter()
        .filter(|line| line.contains("method=tools/call"))
        .collect();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].contains("authorization=Bearer at-dynamic"),
        "dynamic credential must override the static header: {lines:?}"
    );
    host.shutdown().await;
    server.kill().await.ok();
}

/// stdio transport：credential store 有 token 时不注入 header 也不崩溃。
#[tokio::test]
async fn stdio_path_ignores_credential_headers() {
    let store = Arc::new(FileCredentialStore::new(
        tempfile::tempdir().unwrap().path(),
    ));
    store
        .set("fake", McpCredentials::new("at-for-stdio"))
        .unwrap();
    let host = McpHost::new(
        vec![McpServerConfig::new("fake", stdio_config("echo", &[]))],
        store,
    );
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;
    let handle = host.servers().first().unwrap().clone();
    let result = handle
        .call_tool("echo", json!({"round": 1}), &CancellationToken::new())
        .await
        .expect("stdio call succeeds without header injection");
    assert!(result["content"][0]["text"].is_string());
    host.shutdown().await;
}

#[tokio::test]
async fn mcp_concurrency_limit_starts_only_the_first_servers() {
    // 上限 2、3 个 enabled server：只 spawn 前 2 个（按 configs 顺序），
    // 第 3 个保持 Disconnected，且发出 mcp_concurrency_limit 诊断。
    let diagnostics = Arc::new(CollectingDiagnostics::default());
    let configs = ["a", "b", "c"]
        .iter()
        .map(|name| McpServerConfig::new(*name, stdio_config("echo", &[])))
        .collect();
    let host = McpHost::with_diagnostics(
        configs,
        Arc::new(FileCredentialStore::new(
            tempfile::tempdir().unwrap().path(),
        )),
        fast_oauth_runtime(),
        diagnostics.clone(),
    );
    host.set_max_concurrent_extensions(2);
    host.start().unwrap();

    wait_ready(&host, "a", Duration::from_secs(10)).await;
    assert!(host.server_state("a").is_ready());
    assert!(host.server_state("b").is_ready());
    // c 未 spawn：状态保持初始 Disconnected（不进入 Connecting/Ready）。
    let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    while tokio::time::Instant::now() < deadline {
        assert_eq!(
            host.server_state("c"),
            ServerLifecycleState::Disconnected,
            "c must never start"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(host.server_state("c"), ServerLifecycleState::Disconnected);
    let records = diagnostics.records.lock().unwrap().clone();
    assert!(
        records
            .iter()
            .any(|(code, _)| code == "mcp_concurrency_limit"),
        "limit violation must be diagnosed: {records:?}"
    );
    host.shutdown().await;
}

#[tokio::test]
async fn mcp_concurrency_limit_zero_means_unlimited() {
    let configs = ["a", "b", "c"]
        .iter()
        .map(|name| McpServerConfig::new(*name, stdio_config("echo", &[])))
        .collect();
    let host = McpHost::new(
        configs,
        Arc::new(FileCredentialStore::new(
            tempfile::tempdir().unwrap().path(),
        )),
    );
    host.set_max_concurrent_extensions(0);
    host.start().unwrap();
    wait_ready(&host, "a", Duration::from_secs(10)).await;
    assert!(host.server_state("a").is_ready());
    assert!(host.server_state("b").is_ready());
    assert!(host.server_state("c").is_ready());
    host.shutdown().await;
}

#[tokio::test]
async fn reconnect_storm_stays_bounded_and_recovers() {
    // 每次 tools/call 都崩溃：Ready → crash → 退避重连 → Ready 循环。
    // 断言：退避封顶（max_backoff 约束单次重连间隔）、总时长有界、
    // shutdown 不被风暴阻塞。
    let host = host_for("crash-every-call", &[], |config| {
        config.tool_timeout = Duration::from_secs(5);
        config.liveness = LivenessConfig {
            ping_interval: Duration::from_millis(200),
            ping_timeout: Duration::from_millis(100),
        };
        config.reconnect = ReconnectConfig {
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(60),
        };
    });
    host.start().unwrap();
    wait_ready(&host, "fake", Duration::from_secs(10)).await;

    let handle = host.servers().first().unwrap().clone();
    let started = std::time::Instant::now();
    let mut longest_cycle = Duration::ZERO;
    let mut cycles = 0;
    for _ in 0..5 {
        let cycle_started = std::time::Instant::now();
        let call = handle
            .call_tool("echo", json!({}), &CancellationToken::new())
            .await;
        assert!(
            call.is_err(),
            "crash-every-call server must fail the in-flight call"
        );
        // 等重连完成（tools 版本每次重新发现 +1）。
        let version = host.tools_version();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if host.server_state("fake").is_ready() && host.tools_version() > version {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "reconnect storm must recover each cycle"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        longest_cycle = longest_cycle.max(cycle_started.elapsed());
        cycles += 1;
    }
    assert_eq!(cycles, 5);
    assert!(
        longest_cycle < Duration::from_secs(2),
        "backoff must be capped by max_backoff (longest cycle {longest_cycle:?})"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "storm cycles must stay bounded in total"
    );

    let shutdown_started = std::time::Instant::now();
    host.shutdown().await;
    assert!(
        shutdown_started.elapsed() < Duration::from_secs(5),
        "shutdown must not be blocked by the reconnect storm"
    );
}
