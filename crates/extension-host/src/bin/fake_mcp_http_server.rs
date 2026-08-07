//! 测试辅助：fake MCP HTTP server（JSON-RPC over HTTP）。
//!
//! 由集成测试（`tests/mcp_lifecycle.rs`）spawn，验证 HTTP transport 的
//! 请求头注入与 401 → OAuth refresh → 重试携带新 token 的闭环。
//!
//! 用法：`fake_mcp_http_server [options]`
//!
//! - `--listen-addr <ip:port>`：监听地址（默认 `127.0.0.1:0`）；启动后
//!   向 stdout 打印 `LISTENING <实际地址>`。
//! - `--auth-fail-calls <n>`：前 n 次 `tools/call` 返回 HTTP 401。
//! - `--headers-file <path>`：每个 HTTP 请求追加一行
//!   `<method> <path> authorization=<值或(none)>`（服务端记录收到的请求头）。
//! - `--tools <json>`：`tools/list` 返回的工具数组（默认 echo / slow）。
//! - `--huge-content-length`：所有响应声明 `content-length: 1 GiB` 但不
//!   发送 body（客户端应在读取前按 content-length 预检拒绝）。
//! - `--huge-body-bytes <n>`：所有响应以 close-delimited（无
//!   content-length）发送 `n` 字节垃圾 body（客户端流式读取累计超限
//!   拒绝）。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

fn main() {
    let mut listen_addr = "127.0.0.1:0".to_string();
    let mut headers_file: Option<std::path::PathBuf> = None;
    let mut auth_fail_calls: u64 = 0;
    let mut huge_content_length = false;
    let mut huge_body_bytes: usize = 0;
    let mut tools = serde_json::json!([
        {"name": "echo", "description": "Echo the arguments back", "inputSchema": {"type": "object"}},
        {"name": "slow", "description": "Slow tool", "inputSchema": {"type": "object"}}
    ]);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--listen-addr" => {
                index += 1;
                listen_addr = args[index].clone();
            }
            "--auth-fail-calls" => {
                index += 1;
                auth_fail_calls = args[index]
                    .parse()
                    .expect("--auth-fail-calls must be a number");
            }
            "--headers-file" => {
                index += 1;
                headers_file = Some(args[index].clone().into());
            }
            "--tools" => {
                index += 1;
                tools = serde_json::from_str(&args[index]).expect("--tools must be a JSON array");
            }
            "--huge-content-length" => huge_content_length = true,
            "--huge-body-bytes" => {
                index += 1;
                huge_body_bytes = args[index]
                    .parse()
                    .expect("--huge-body-bytes must be a number");
            }
            other => panic!("unknown argument '{other}'"),
        }
        index += 1;
    }

    let listener = TcpListener::bind(&listen_addr).expect("bind");
    let addr = listener.local_addr().unwrap();
    println!("LISTENING {addr}");
    let fail_remaining = std::sync::Arc::new(std::sync::Mutex::new(auth_fail_calls));
    for stream in listener.incoming().flatten() {
        let headers_file = headers_file.clone();
        let tools = tools.clone();
        let fail_remaining = fail_remaining.clone();
        std::thread::spawn(move || {
            let mut stream = stream;
            let _ = handle_connection(
                &mut stream,
                &tools,
                &fail_remaining,
                &headers_file,
                huge_content_length,
                huge_body_bytes,
            );
        });
    }
}

fn handle_connection(
    stream: &mut TcpStream,
    tools: &serde_json::Value,
    auth_fail_calls: &std::sync::Mutex<u64>,
    headers_file: &Option<std::path::PathBuf>,
    huge_content_length: bool,
    huge_body_bytes: usize,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line == "\r\n" || line == "\n" {
            break;
        }
        headers.push(line.trim_end().to_string());
    }
    let mut content_length = 0usize;
    for header in &headers {
        if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = Vec::new();
    reader.take(content_length as u64).read_to_end(&mut body)?;

    // 超大响应模拟：声明 1 GiB 但发送空 body（客户端预检拒绝，不读 body）。
    if huge_content_length {
        let response =
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 1073741824\r\nconnection: close\r\n\r\n";
        stream.write_all(response)?;
        stream.flush()?;
        return Ok(());
    }
    // 超大 body 模拟：close-delimited（无 content-length）发送 n 字节
    // 垃圾（客户端流式累计超限拒绝）。
    if huge_body_bytes > 0 {
        let response =
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n";
        stream.write_all(response)?;
        let chunk = vec![b'x'; 64 * 1024];
        let mut remaining = huge_body_bytes;
        while remaining > 0 {
            let n = remaining.min(chunk.len());
            stream.write_all(&chunk[..n])?;
            remaining -= n;
        }
        stream.flush()?;
        return Ok(());
    }

    let authorization = headers
        .iter()
        .find(|header| header.to_ascii_lowercase().starts_with("authorization:"))
        .and_then(|header| {
            header
                .split_once(':')
                .map(|(_, value)| value.trim().to_string())
        })
        .unwrap_or_else(|| "(none)".to_string());
    let request: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::json!({}));
    let method = request
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if let Some(path) = headers_file
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(
            file,
            "{} method={method} authorization={authorization}",
            request_line.trim_end()
        );
    }

    let id = request.get("id").cloned();
    let result = match method {
        "initialize" => serde_json::json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "serverInfo": {"name": "fake_mcp_http_server", "version": "0.1.0"}
        }),
        "ping" => serde_json::json!({}),
        "tools/list" => serde_json::json!({"tools": tools}),
        "tools/call" => {
            if *auth_fail_calls.lock().unwrap() > 0 {
                *auth_fail_calls.lock().unwrap() -= 1;
                let response =
                    b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
                stream.write_all(response)?;
                stream.flush()?;
                return Ok(());
            }
            let name = request
                .get("params")
                .and_then(|params| params.get("name"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let arguments = request
                .get("params")
                .and_then(|params| params.get("arguments"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            serde_json::json!({
                "content": [{"type": "text", "text": format!("fake:{name}:{arguments}")}],
                "isError": false
            })
        }
        other => serde_json::json!({
            "error": {"code": -32601, "message": format!("method not found: {other}")}
        }),
    };
    let payload = if method == "notifications/initialized" {
        serde_json::json!({"jsonrpc": "2.0"})
    } else {
        serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
    };
    let body = serde_json::to_vec(&payload).unwrap();
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.write_all(&body)?;
    stream.flush()
}
