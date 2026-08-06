//! 测试辅助：fake MCP stdio server（JSON lines transport）。
//!
//! 由集成测试（`tests/mcp_lifecycle.rs`）经 `env!("CARGO_BIN_EXE_fake_mcp_server")`
//! spawn；行为由 argv 控制，用于钉死 extension-host MCP 适配器的进程级
//! 语义：握手、工具发现、per-tool 超时 / 取消、liveness、断线重连、
//! `tools/list_changed` 热更新、进程崩溃、输出洪泛、非法 JSON。
//!
//! 用法：`fake_mcp_server [options]`
//!
//! - `--tools <json>`：`tools/list` 返回的工具数组（默认两个：echo / slow）。
//! - `--grow-tools`：每次 `tools/list` 追加一个工具（配合 list-changed）。
//! - `--call-delay-ms <n>`：`tools/call` 响应前延迟。
//! - `--list-changed-delay-ms <n>`：首次 `tools/list` 后延迟发
//!   `notifications/tools/list_changed`（0 = 不发）。
//! - `--auth-fail-on-call <n>`：前 n 次 `tools/call` 返回 JSON-RPC
//!   `-32001`（UNAUTHORIZED），之后正常（OAuth 401 refresh 测试）。
//! - `--mode <name>`：行为模式，见 [`Mode`]。

use std::io::{BufRead, Write};

/// stdout 全局锁：通知线程与主线程交错写行时保证整行原子。
static STDOUT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn out_line(line: &str) {
    let _guard = STDOUT_LOCK.lock().unwrap();
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = handle.write_all(line.as_bytes());
    let _ = handle.write_all(b"\n");
    let _ = handle.flush();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut tools = serde_json::json!([
        {"name": "echo", "description": "Echo the arguments back", "inputSchema": {"type": "object"}},
        {"name": "slow", "description": "Slow tool", "inputSchema": {"type": "object"}}
    ]);
    let mut grow_tools = false;
    let mut crash_file: Option<std::path::PathBuf> = None;
    let mut call_delay_ms: u64 = 0;
    let mut list_changed_delay_ms: u64 = 0;
    let mut auth_fail_on_call: u64 = 0;
    let mut mode = Mode::Echo;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            // ProcessSpec 的 Direct 形态会在 args 后追加 command（这里为空串）。
            "" => {}
            "--tools" => {
                index += 1;
                tools = serde_json::from_str(&args[index]).expect("--tools must be a JSON array");
            }
            "--grow-tools" => grow_tools = true,
            "--crash-file" => {
                index += 1;
                crash_file = Some(args[index].clone().into());
            }
            "--call-delay-ms" => {
                index += 1;
                call_delay_ms = args[index]
                    .parse()
                    .expect("--call-delay-ms must be a number");
            }
            "--list-changed-delay-ms" => {
                index += 1;
                list_changed_delay_ms = args[index]
                    .parse()
                    .expect("--list-changed-delay-ms must be a number");
            }
            "--auth-fail-on-call" => {
                index += 1;
                auth_fail_on_call = args[index]
                    .parse()
                    .expect("--auth-fail-on-call must be a number");
            }
            "--mode" => {
                index += 1;
                mode = match args[index].as_str() {
                    "echo" => Mode::Echo,
                    "flood" => Mode::Flood,
                    "bad-json" => Mode::BadJson,
                    "garbage-init" => Mode::GarbageInit,
                    "crash-after-init" => Mode::CrashAfterInit,
                    "crash-on-call" => Mode::CrashOnCall,
                    "ping-drop" => Mode::PingDrop,
                    "list-changed" => Mode::ListChanged,
                    "crash-every-call" => Mode::CrashEveryCall,
                    other => panic!("unknown --mode '{other}'"),
                };
            }
            other => panic!("unknown argument '{other}'"),
        }
        index += 1;
    }

    if matches!(mode, Mode::GarbageInit) {
        for _ in 0..3 {
            out_line("this is not json");
        }
    }

    let stdin = std::io::stdin();
    let mut line = String::new();
    let mut list_count: u64 = 0;
    let mut request_count: u64 = 0;
    let mut initialized = false;

    loop {
        line.clear();
        let read = stdin.lock().read_line(&mut line).expect("read stdin");
        if read == 0 {
            return;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => continue, // 不可解析的行：忽略。
        };
        request_count += 1;

        if matches!(mode, Mode::BadJson) && request_count.is_multiple_of(3) {
            out_line("{\"jsonrpc\": \"2.0\", \"id\": 999, \"result\": \"orphan\"");
        }
        if matches!(mode, Mode::Flood) {
            for _ in 0..200 {
                out_line(&format!("{{not json {request_count}}}"));
            }
        }

        let id = value.get("id").cloned();
        let method = value
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();

        match method {
            "initialize" => {
                respond(
                    &id,
                    serde_json::json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {},
                        "serverInfo": {"name": "fake_mcp_server", "version": "0.1.0"}
                    }),
                );
                if matches!(mode, Mode::CrashAfterInit) {
                    std::process::exit(7);
                }
                if matches!(mode, Mode::ListChanged) && list_changed_delay_ms > 0 {
                    // 首轮 list 后通知工具变化。
                    let delay = list_changed_delay_ms;
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(delay));
                        out_line(
                            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}",
                        );
                    });
                }
            }
            "notifications/initialized" => {
                initialized = true;
            }
            "ping" => {
                if matches!(mode, Mode::PingDrop) {
                    // 静默丢弃：触发 liveness 超时。
                } else {
                    respond(&id, serde_json::json!({}));
                }
            }
            "tools/list" => {
                list_count += 1;
                let mut current = tools.clone();
                // 从第 2 次 list 开始追加（配合 list-changed 通知：
                // 初始发现保持配置集，热更新后才增长）。
                if grow_tools && list_count > 1 {
                    current
                        .as_array_mut()
                        .expect("tools is an array")
                        .push(serde_json::json!({
                            "name": format!("grown_{list_count}"),
                            "description": format!("Grown tool {list_count}"),
                            "inputSchema": {"type": "object"}
                        }));
                }
                respond(&id, serde_json::json!({"tools": current}));
            }
            "tools/call" => {
                if matches!(mode, Mode::CrashOnCall)
                    && crash_file.as_ref().is_none_or(|path| !path.exists())
                {
                    if let Some(path) = &crash_file {
                        let _ = std::fs::write(path, "crashed");
                    }
                    std::process::exit(3);
                }
                if matches!(mode, Mode::CrashEveryCall) {
                    std::process::exit(3);
                }
                if auth_fail_on_call > 0 {
                    auth_fail_on_call -= 1;
                    respond_error(&id, -32001, "authentication required");
                    continue;
                }
                if call_delay_ms > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(call_delay_ms));
                }
                let name = value
                    .get("params")
                    .and_then(|params| params.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown");
                let arguments = value
                    .get("params")
                    .and_then(|params| params.get("arguments"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                respond(
                    &id,
                    serde_json::json!({
                        "content": [{
                            "type": "text",
                            "text": format!("fake:{name}:{arguments}")
                        }],
                        "isError": false
                    }),
                );
            }
            other => {
                respond(
                    &id,
                    serde_json::json!({
                        "error": {"code": -32601, "message": format!("method not found: {other}")}
                    }),
                );
            }
        }
        let _ = initialized;
    }
}

fn respond(id: &Option<serde_json::Value>, result: serde_json::Value) {
    let Some(id) = id else {
        return; // 通知：无响应。
    };
    let payload = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
    out_line(&payload.to_string());
}

fn respond_error(id: &Option<serde_json::Value>, code: i32, message: &str) {
    let Some(id) = id else {
        return; // 通知：无响应。
    };
    let payload = serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}});
    out_line(&payload.to_string());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 默认：正常 echo。
    Echo,
    /// 每次请求先输出 200 行垃圾再响应。
    Flood,
    /// 每第 3 个请求前输出一行坏 JSON。
    BadJson,
    /// 启动时输出 3 行垃圾。
    GarbageInit,
    /// initialize 后立即以退出码 7 崩溃。
    CrashAfterInit,
    /// 首个 tools/call 时以退出码 3 崩溃（`--crash-once` 下只崩一次）。
    CrashOnCall,
    /// 静默丢弃 ping。
    PingDrop,
    /// 首轮 list 后发 tools/list_changed 通知。
    ListChanged,
    /// 每次 tools/call 都以退出码 3 崩溃（重连风暴测试）。
    CrashEveryCall,
}
