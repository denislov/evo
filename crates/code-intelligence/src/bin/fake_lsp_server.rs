//! 测试辅助：fake LSP stdio server（Content-Length framing）。
//!
//! 由集成测试（`crates/code-intelligence/src/lsp/{transport,server}_tests.rs`）
//! 经 `env!("CARGO_BIN_EXE_fake_lsp_server")` spawn；行为由 argv 控制，
//! 用于钉死 LSP 生命周期 / 重启 / replay / 诊断 / edit / 传输语义。
//!
//! 用法：`fake_lsp_server [options]`
//!
//! - `--mode <name>`：行为模式，见 [`Mode`]。
//! - `--record-file <path>`：事件记录（JSON lines：initialize / didOpen
//!   uri / didChange version / didClose uri / shutdown / exit /
//!   apply-edit-response <applied>），断言用。
//! - `--delay-ms <n>`：请求响应前延迟。
//! - `--query-delay-ms <n>`：仅 hover/definition/references 响应前延迟
//!   （in-flight 取消测试：不影响握手时序）。
//! - `--push-on-open`：每次 didOpen 后推送 `publishDiagnostics`（带
//!   didOpen 的版本）。
//! - `--push-on-change`：每次 didChange 后推送诊断（带新版本）。
//! - `--diagnostic-delay-ms <n>`：didOpen 后延迟推送（0 = 立即）。
//! - `--crash-request <n>`：第 n 个请求后崩溃（1-indexed，不含 initialize）。
//! - `--crash-after-open <n>`：第 n 次 didOpen 后延迟 50ms 崩溃（document
//!   replay 测试：每轮 initialize + initialized + didOpen + crash）。
//! - `--edit-uri <uri>`：apply-edit 模式的目标 uri（默认 = 第一个
//!   didOpen 的文档）。

use std::io::{BufWriter, Write};

use code_intelligence::lsp::wire::{self, Request};

static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// stdout 全局锁：通知线程与主线程交错写帧时保证整帧原子。
static STDOUT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const MAX_FRAME: usize = 16 * 1024 * 1024;

fn out_frame(payload: &str) {
    let _guard = STDOUT_LOCK.lock().unwrap();
    let stdout = std::io::stdout();
    let mut handle = BufWriter::new(stdout.lock());
    let _ = wire::write_frame_sync(&mut handle, payload.as_bytes());
}

fn record(file: &Option<std::path::PathBuf>, event: &str) {
    let Some(path) = file else {
        return;
    };
    // 时间戳：自进程启动的毫秒数（断言 backoff 间隔用）。
    let elapsed = START.get().unwrap().elapsed().as_millis();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{event} @{elapsed}ms");
    }
}

fn respond(id: &Option<serde_json::Value>, result: serde_json::Value) {
    let Some(id) = id else {
        return; // 通知：无响应。
    };
    let payload = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
    out_frame(&payload.to_string());
}

fn respond_error(id: &Option<serde_json::Value>, code: i32, message: &str) {
    let Some(id) = id else {
        return;
    };
    let payload = serde_json::json!({"jsonrpc": "2.0", "id": id,
        "error": {"code": code, "message": message}});
    out_frame(&payload.to_string());
}

fn main() {
    START.get_or_init(std::time::Instant::now);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut mode = Mode::Echo;
    let mut record_file: Option<std::path::PathBuf> = None;
    let mut delay_ms: u64 = 0;
    let mut query_delay_ms: u64 = 0;
    let mut push_on_open = false;
    let mut push_on_change = false;
    let mut diagnostic_delay_ms: u64 = 0;
    let mut crash_request: u64 = 0;
    let mut crash_after_open: Option<u64> = None;
    let mut edit_uri: Option<String> = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            // ProcessSpec 的 Direct 形态会在 args 后追加 command（这里为空串）。
            "" => {}
            "--mode" => {
                index += 1;
                mode = match args[index].as_str() {
                    "echo" => Mode::Echo,
                    "crash-after-init" => Mode::CrashAfterInit,
                    "flood" => Mode::Flood,
                    "bad-frame" => Mode::BadFrame,
                    "truncated-frame" => Mode::TruncatedFrame,
                    "ping-drop" => Mode::PingDrop,
                    "no-initialize-response" => Mode::NoInitializeResponse,
                    "garbage-on-start" => Mode::GarbageOnStart,
                    "apply-edit" => Mode::ApplyEdit,
                    other => panic!("unknown --mode '{other}'"),
                };
            }
            "--record-file" => {
                index += 1;
                record_file = Some(args[index].clone().into());
            }
            "--delay-ms" => {
                index += 1;
                delay_ms = args[index].parse().expect("--delay-ms must be a number");
            }
            "--query-delay-ms" => {
                index += 1;
                query_delay_ms = args[index]
                    .parse()
                    .expect("--query-delay-ms must be a number");
            }
            "--push-on-open" => push_on_open = true,
            "--push-on-change" => push_on_change = true,
            "--diagnostic-delay-ms" => {
                index += 1;
                diagnostic_delay_ms = args[index]
                    .parse()
                    .expect("--diagnostic-delay-ms must be a number");
            }
            "--crash-request" => {
                index += 1;
                crash_request = args[index]
                    .parse()
                    .expect("--crash-request must be a number");
            }
            "--crash-after-open" => {
                index += 1;
                crash_after_open = Some(
                    args[index]
                        .parse()
                        .expect("--crash-after-open must be a number"),
                );
            }
            "--edit-uri" => {
                index += 1;
                edit_uri = Some(args[index].clone());
            }
            other => panic!("unknown argument '{other}'"),
        }
        index += 1;
    }

    if matches!(mode, Mode::GarbageOnStart) {
        // 启动即输出垃圾字节（非法帧）：fail closed 测试。
        let _guard = STDOUT_LOCK.lock().unwrap();
        let stdout = std::io::stdout();
        let mut handle = BufWriter::new(stdout.lock());
        let _ = handle.write_all(b"this is not a frame\n");
        let _ = handle.flush();
    }

    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let mut request_count: u64 = 0;
    let mut did_open_count: u64 = 0;
    let mut last_open_uri: Option<String> = None;
    let mut last_open_version: Option<i64> = None;

    loop {
        let payload = match wire::read_frame_sync(&mut reader, MAX_FRAME) {
            Ok(payload) => payload,
            Err(_) => return, // 客户端关闭 / 坏帧：退出。
        };
        let value: serde_json::Value = match serde_json::from_slice(&payload) {
            Ok(value) => value,
            Err(_) => continue, // 客户端坏 JSON 帧：跳过。
        };
        let id = value.get("id").cloned();
        let method = value
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        if method.is_empty() {
            // 客户端 → 服务器的响应帧（如 apply-edit 的回执）。
            if let Some(id) = id {
                let applied = value
                    .get("result")
                    .and_then(|result| result.get("applied"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                record(&record_file, &format!("apply-edit-response {applied} {id}"));
            }
            continue;
        }
        request_count += 1;

        if matches!(mode, Mode::Flood) {
            for _ in 0..50 {
                out_frame(&format!("{{not json {request_count}}}"));
            }
        }
        if matches!(mode, Mode::BadFrame) {
            // 超大帧声明：fail closed。
            let _guard = STDOUT_LOCK.lock().unwrap();
            let stdout = std::io::stdout();
            let mut handle = BufWriter::new(stdout.lock());
            let _ = handle.write_all(b"Content-Length: 999999999\r\n\r\n");
            let _ = handle.flush();
        }
        if matches!(mode, Mode::TruncatedFrame) {
            let _guard = STDOUT_LOCK.lock().unwrap();
            let stdout = std::io::stdout();
            let mut handle = BufWriter::new(stdout.lock());
            let _ = handle.write_all(b"Content-Length: 100\r\n\r\nabc");
            let _ = handle.flush();
            // 截断帧后退出：客户端读到 EOF 触发 TruncatedFrame。
            return;
        }

        if delay_ms > 0 && !method.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }

        if crash_request > 0 && request_count > crash_request {
            record(&record_file, "crash");
            std::process::exit(3);
        }

        match method.as_str() {
            "initialize" => {
                record(&record_file, "initialize");
                if matches!(mode, Mode::NoInitializeResponse) {
                    continue; // 静默丢弃：握手超时 → 重启。
                }
                respond(
                    &id,
                    serde_json::json!({
                        "protocolVersion": "3.17.0",
                        "capabilities": {
                            "textDocumentSync": 1,
                            "hoverProvider": true,
                            "definitionProvider": true,
                            "referencesProvider": true,
                            "diagnosticProvider": {"interFileDependencies": false,
                                                   "workspaceDiagnostics": false}
                        },
                        "serverInfo": {"name": "fake_lsp_server", "version": "0.1.0"}
                    }),
                );
                if matches!(mode, Mode::CrashAfterInit) {
                    record(&record_file, "crash");
                    std::process::exit(7);
                }
            }
            "initialized" => {
                record(&record_file, "initialized");
                if matches!(mode, Mode::ApplyEdit) {
                    // 向客户端发 workspace/applyEdit 请求。
                    let target = edit_uri
                        .clone()
                        .unwrap_or_else(|| last_open_uri.clone().unwrap_or_default());
                    let request = Request::new(
                        9001,
                        "workspace/applyEdit",
                        Some(serde_json::json!({
                            "edit": {
                                "documentChanges": [{
                                    "textDocument": {"uri": target, "version": last_open_version.unwrap_or(1)},
                                    "edits": [{"range": null, "newText": "replaced by fake server"}]
                                }]
                            }
                        })),
                    );
                    out_frame(&serde_json::to_string(&request).unwrap());
                }
            }
            "ping" => {
                if matches!(mode, Mode::PingDrop) {
                    // 静默丢弃：liveness 超时。
                } else {
                    respond(&id, serde_json::json!({}));
                }
            }
            "shutdown" => {
                record(&record_file, "shutdown");
                respond(&id, serde_json::Value::Null);
            }
            "exit" => {
                record(&record_file, "exit");
                return;
            }
            "textDocument/didOpen" => {
                let uri = value
                    .get("params")
                    .and_then(|params| params.get("textDocument"))
                    .and_then(|doc| doc.get("uri"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let version = value
                    .get("params")
                    .and_then(|params| params.get("textDocument"))
                    .and_then(|doc| doc.get("version"))
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                last_open_uri = Some(uri.clone());
                last_open_version = Some(version);
                record(&record_file, &format!("didOpen {uri} v{version}"));
                did_open_count += 1;
                if crash_after_open == Some(did_open_count) {
                    // 记录后延迟崩溃：保证客户端已读到 didOpen。
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    record(&record_file, "crash");
                    std::process::exit(3);
                }
                if push_on_open {
                    push_diagnostics(&uri, version, diagnostic_delay_ms);
                }
            }
            "textDocument/didChange" => {
                let uri = value
                    .get("params")
                    .and_then(|params| params.get("textDocument"))
                    .and_then(|doc| doc.get("uri"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let version = value
                    .get("params")
                    .and_then(|params| params.get("textDocument"))
                    .and_then(|doc| doc.get("version"))
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                last_open_version = Some(version);
                record(&record_file, &format!("didChange {uri} v{version}"));
                if push_on_change {
                    push_diagnostics(&uri, version, diagnostic_delay_ms);
                }
            }
            "textDocument/didClose" => {
                let uri = value
                    .get("params")
                    .and_then(|params| params.get("textDocument"))
                    .and_then(|doc| doc.get("uri"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                record(&record_file, &format!("didClose {uri}"));
            }
            "textDocument/hover" => {
                if query_delay_ms > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(query_delay_ms));
                }
                respond(
                    &id,
                    serde_json::json!({"contents": {"kind": "markdown", "value": "fake hover"}}),
                );
            }
            "textDocument/definition" => {
                respond(
                    &id,
                    serde_json::json!([{
                        "uri": last_open_uri.clone().unwrap_or_default(),
                        "range": {"start": {"line": 0, "character": 0},
                                  "end": {"line": 0, "character": 2}}
                    }]),
                );
            }
            "textDocument/references" => {
                respond(
                    &id,
                    serde_json::json!([{
                        "uri": last_open_uri.clone().unwrap_or_default(),
                        "range": {"start": {"line": 0, "character": 0},
                                  "end": {"line": 0, "character": 2}}
                    }]),
                );
            }
            "textDocument/pullDiagnostics" => {
                let uri = value
                    .get("params")
                    .and_then(|params| params.get("textDocument"))
                    .and_then(|doc| doc.get("uri"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                record(&record_file, &format!("pullDiagnostics {uri}"));
                respond(
                    &id,
                    serde_json::json!({
                        "items": [{
                            "range": {"start": {"line": 0, "character": 0},
                                      "end": {"line": 0, "character": 2}},
                            "severity": 2,
                            "message": "pulled diagnostic"
                        }],
                        "resultId": "pull-1"
                    }),
                );
            }
            other => {
                respond_error(&id, -32601, &format!("method not found: {other}"));
            }
        }
    }
}

/// 推送 `publishDiagnostics`（带版本；延迟可选）。
fn push_diagnostics(uri: &str, version: i64, delay_ms: u64) {
    let uri = uri.to_string();
    if delay_ms > 0 {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            send_diagnostics(&uri, version);
        });
    } else {
        send_diagnostics(&uri, version);
    }
}

fn send_diagnostics(uri: &str, version: i64) {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {
            "uri": uri,
            "version": version,
            "diagnostics": [{
                "range": {"start": {"line": 0, "character": 0},
                          "end": {"line": 0, "character": 4}},
                "severity": 1,
                "message": format!("fake diagnostic for version {version}")
            }]
        }
    });
    out_frame(&payload.to_string());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 默认：正常 echo + 标准能力。
    Echo,
    /// initialize 后立即以退出码 7 崩溃。
    CrashAfterInit,
    /// 每个请求前输出 50 帧垃圾 JSON。
    Flood,
    /// 每个请求前输出超大帧声明（fail closed）。
    BadFrame,
    /// 每个请求前输出截断帧（fail closed）。
    TruncatedFrame,
    /// 静默丢弃 ping（liveness 超时）。
    PingDrop,
    /// 不响应 initialize（握手超时 → 重启）。
    NoInitializeResponse,
    /// 启动时输出垃圾字节。
    GarbageOnStart,
    /// initialized 后发 workspace/applyEdit 请求。
    ApplyEdit,
}
