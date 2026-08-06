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

async fn wait_ready(host: &McpHost, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if host.server_state("fake").is_ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "fake server did not become Ready; state={:?}",
        host.server_state("fake")
    );
}

#[tokio::test]
async fn initialize_handshake_and_tool_discovery() {
    let host = host_for("echo", &[], |_| {});
    host.start().unwrap();
    wait_ready(&host, Duration::from_secs(10)).await;

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
    wait_ready(&host, Duration::from_secs(10)).await;

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
    wait_ready(&host, Duration::from_secs(10)).await;

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
    wait_ready(&host, Duration::from_secs(10)).await;

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
    wait_ready(&host, Duration::from_secs(10)).await;
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
    wait_ready(&host, Duration::from_secs(10)).await;

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
    wait_ready(&host, Duration::from_secs(10)).await;
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
    wait_ready(&host, Duration::from_secs(10)).await;

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
    wait_ready(&host, Duration::from_secs(10)).await;

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
    wait_ready(&host, Duration::from_secs(10)).await;

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
