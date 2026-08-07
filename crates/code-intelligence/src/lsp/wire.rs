//! LSP wire：JSON-RPC 2.0 + Content-Length framing（stdio）。
//!
//! LSP 的传输帧是 `Content-Length: N\r\n\r\n` 头 + N 字节 JSON 体（**不是**
//! MCP 的 JSON lines）。本模块手写 wire（不引入 async-lsp / lsp-types
//! 框架，见 `docs/refactor/phase8-lsp.md` 的依赖决策），解析纪律参照
//! `extension-host` 的 MCP wire：严格解析、结构化错误、fail closed。
//!
//! 角色差异：MCP 的 wire 是**服务端**（Evo 的 extension-host 解析客户端
//! 请求）；LSP 的 wire 是**客户端**（Evo 的 code-intelligence 解析
//! 语言服务器响应 / 通知 / 服务器请求）。同一 JSON-RPC 2.0 消息形状：
//! 请求（`id`+`method`+`params`）、成功响应（`id`+`result`）、错误响应
//! （`id`+`error`）、通知（`method`+`params`，无 `id`）。
//!
//! 帧纪律（与 MCP 行协议的关键差异）：
//!
//! - 坏帧 = 流不同步：LSP 帧的长度由头决定，一个坏头之后无法再找到帧
//!   边界，所以**任何帧错误都 fail closed**（返回错误，由调用方重启
//!   会话），不像 MCP 坏行那样跳过继续。
//! - 单帧上限 [`DEFAULT_MAX_FRAME_BYTES`]（诊断可以很大，默认 16 MiB）：
//!   防超大帧 / 输出洪泛。
//! - 头解析只认 `Content-Length`（大小写不敏感），缺失 / 非法 / 非数字
//!   都产生 [`WireError::InvalidHeader`]；其他头行忽略。

// Evo 独立设计：JSON-RPC 2.0 消息形状与错误码是协议标准；解析纪律参照
// extension-host 的 MCP wire（`crates/extension-host/src/mcp/wire.rs`），
// 代码按 LSP 帧协议重写，无直接移植。
use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

/// JSON-RPC 2.0 协议版本常量。
pub const JSONRPC_VERSION: &str = "2.0";

/// 解析错误（-32700）。
pub const PARSE_ERROR: i32 = -32700;
/// 无效请求（-32600）。
pub const INVALID_REQUEST: i32 = -32600;
/// 方法不存在（-32601）。
pub const METHOD_NOT_FOUND: i32 = -32601;
/// 参数无效（-32602）。
pub const INVALID_PARAMS: i32 = -32602;
/// 内部错误（-32603）。
pub const INTERNAL_ERROR: i32 = -32603;
/// LSP 约定：请求已取消。
pub const REQUEST_CANCELLED: i32 = -32800;
/// 服务器未初始化（-32002，LSP 3.17）。
pub const SERVER_NOT_INITIALIZED: i32 = -32002;

/// 头部最大长度（Content-Length 行不应超过几十字节；防御坏头洪泛）。
pub const MAX_HEADER_BYTES: usize = 4096;
/// 单帧默认上限（16 MiB：诊断 / 大文档内容可以很大）。
pub const DEFAULT_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// 请求 / 响应 id。LSP 客户端总是发出数字 id；解析时接受数字或字符串。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(u64),
    String(String),
}

/// JSON-RPC 请求（带 `id`，期待响应）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Id,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl Request {
    pub fn new(id: u64, method: impl Into<String>, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Id::Number(id),
            method: method.into(),
            params,
        }
    }
}

/// JSON-RPC 错误对象。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// 成功响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Id,
    pub result: serde_json::Value,
}

/// 错误响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    pub jsonrpc: String,
    pub id: Id,
    pub error: JsonRpcError,
}

/// 通知（无 `id`，不期待响应）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// 一帧内的完整 LSP 消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Request(Request),
    Response(Response),
    ErrorResponse(ErrorResponse),
    Notification(Notification),
}

impl Message {
    pub fn is_notification(&self) -> bool {
        matches!(self, Message::Notification(_))
    }

    pub fn response_id(&self) -> Option<&Id> {
        match self {
            Message::Response(response) => Some(&response.id),
            Message::ErrorResponse(response) => Some(&response.id),
            _ => None,
        }
    }
}

/// 结构化 wire 错误（解析失败分类，fail closed）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("invalid JSON: {detail}")]
    InvalidJson { detail: String },
    #[error("invalid JSON-RPC message: {detail}")]
    InvalidMessage { detail: String },
    #[error("unsupported JSON-RPC version: {version}")]
    UnsupportedVersion { version: String },
    #[error("invalid Content-Length header: {detail}")]
    InvalidHeader { detail: String },
    #[error("frame exceeds the {limit} byte limit (got {bytes})")]
    FrameTooLarge { bytes: u64, limit: u64 },
    #[error("truncated frame: expected {expected} bytes, got {got}")]
    TruncatedFrame { expected: u64, got: u64 },
    #[error("LSP transport io error: {detail}")]
    Io { detail: String },
}

impl WireError {
    /// 该错误对应的 JSON-RPC 错误码（向服务器回错误响应时用）。
    pub fn code(&self) -> i32 {
        match self {
            WireError::InvalidJson { .. } => PARSE_ERROR,
            WireError::InvalidMessage { .. } | WireError::InvalidHeader { .. } => INVALID_REQUEST,
            WireError::UnsupportedVersion { .. } => INVALID_REQUEST,
            _ => INTERNAL_ERROR,
        }
    }
}

/// 从头部字节中解析 `Content-Length`。
///
/// 头由 `\r\n` 分隔的行组成，以空行结束；`Content-Length` 行大小写不敏感，
/// 缺失 / 重复 / 非法数字 / 负数都是 [`WireError::InvalidHeader`]。
fn parse_content_length(header: &[u8]) -> Result<usize, WireError> {
    let text = std::str::from_utf8(header).map_err(|error| WireError::InvalidHeader {
        detail: format!("header is not UTF-8: {error}"),
    })?;
    let mut found: Option<usize> = None;
    for raw_line in text.split("\r\n") {
        let line = raw_line.strip_suffix('\n').unwrap_or(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(WireError::InvalidHeader {
                detail: format!("malformed header line {line:?}"),
            });
        };
        if !name.eq_ignore_ascii_case("Content-Length") {
            continue;
        }
        let parsed = value
            .trim()
            .parse::<u64>()
            .map_err(|error| WireError::InvalidHeader {
                detail: format!("non-numeric Content-Length {value:?}: {error}"),
            })?;
        if found.replace(parsed as usize).is_some() {
            return Err(WireError::InvalidHeader {
                detail: "duplicate Content-Length".into(),
            });
        }
    }
    found.ok_or_else(|| WireError::InvalidHeader {
        detail: "missing Content-Length".into(),
    })
}

/// 读取一帧（同步版本，fake LSP server 测试辅助用）。
pub fn read_frame_sync(reader: &mut impl BufRead, max_bytes: usize) -> Result<Vec<u8>, WireError> {
    let mut header = Vec::new();
    loop {
        let mut line: Vec<u8> = Vec::new();
        let n = reader.read_until(b'\n', &mut line).map_err(io_error)?;
        if n == 0 {
            return Err(WireError::TruncatedFrame {
                expected: 1,
                got: 0,
            });
        }
        header.extend_from_slice(&line);
        if line == b"\n" || line == b"\r\n" {
            break;
        }
        if header.len() > MAX_HEADER_BYTES {
            return Err(WireError::InvalidHeader {
                detail: format!("header exceeds {MAX_HEADER_BYTES} bytes"),
            });
        }
    }
    let length = parse_content_length(&header)?;
    if length > max_bytes {
        return Err(WireError::FrameTooLarge {
            bytes: length as u64,
            limit: max_bytes as u64,
        });
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            WireError::TruncatedFrame {
                expected: length as u64,
                got: payload.len() as u64,
            }
        } else {
            io_error(error)
        }
    })?;
    Ok(payload)
}

/// 写入一帧（同步版本）。
pub fn write_frame_sync(writer: &mut impl Write, payload: &[u8]) -> Result<(), WireError> {
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", payload.len()).as_bytes())
        .map_err(io_error)?;
    writer.write_all(payload).map_err(io_error)?;
    writer.flush().map_err(io_error)
}

/// 读取一帧（async 版本，transport 用）。
pub async fn read_frame(
    reader: &mut (impl AsyncBufRead + Unpin),
    max_bytes: usize,
) -> Result<Vec<u8>, WireError> {
    let mut header = Vec::new();
    loop {
        let mut line: Vec<u8> = Vec::new();
        let n = reader
            .read_until(b'\n', &mut line)
            .await
            .map_err(io_error)?;
        if n == 0 {
            return Err(WireError::TruncatedFrame {
                expected: 1,
                got: 0,
            });
        }
        header.extend_from_slice(&line);
        if line == b"\n" || line == b"\r\n" {
            break;
        }
        if header.len() > MAX_HEADER_BYTES {
            return Err(WireError::InvalidHeader {
                detail: format!("header exceeds {MAX_HEADER_BYTES} bytes"),
            });
        }
    }
    let length = parse_content_length(&header)?;
    if length > max_bytes {
        return Err(WireError::FrameTooLarge {
            bytes: length as u64,
            limit: max_bytes as u64,
        });
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            WireError::TruncatedFrame {
                expected: length as u64,
                got: payload.len() as u64,
            }
        } else {
            io_error(error)
        }
    })?;
    Ok(payload)
}

/// 写入一帧（async 版本）。
pub async fn write_frame(
    writer: &mut (impl AsyncWriteExt + Unpin),
    payload: &[u8],
) -> Result<(), WireError> {
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", payload.len()).as_bytes())
        .await
        .map_err(io_error)?;
    writer.write_all(payload).await.map_err(io_error)?;
    writer.flush().await.map_err(io_error)
}

fn io_error(error: std::io::Error) -> WireError {
    WireError::Io {
        detail: error.to_string(),
    }
}

/// 解析一帧 JSON 字节为 [`Message`]。
///
/// 非法 JSON / 顶层非对象 / 缺 `jsonrpc` / 未知字段 / 类型不符都产生
/// 结构化 [`WireError`]。
pub fn parse_message(bytes: &[u8]) -> Result<Message, WireError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| WireError::InvalidJson {
            detail: error.to_string(),
        })?;
    let object = value.as_object().ok_or_else(|| WireError::InvalidMessage {
        detail: "top-level JSON must be an object".into(),
    })?;
    let jsonrpc = object
        .get("jsonrpc")
        .and_then(|value| value.as_str())
        .ok_or_else(|| WireError::InvalidMessage {
            detail: "missing 'jsonrpc' string field".into(),
        })?;
    if jsonrpc != JSONRPC_VERSION {
        return Err(WireError::UnsupportedVersion {
            version: jsonrpc.to_string(),
        });
    }
    let has_id = object.contains_key("id");
    let has_method = object.contains_key("method");
    let has_result = object.contains_key("result");
    let has_error = object.contains_key("error");
    if has_method && !has_id {
        parse_object::<Notification>(value).map(Message::Notification)
    } else if has_method && has_id {
        parse_object::<Request>(value).map(Message::Request)
    } else if has_id && has_result && !has_error {
        parse_object::<Response>(value).map(Message::Response)
    } else if has_id && has_error && !has_result {
        parse_object::<ErrorResponse>(value).map(Message::ErrorResponse)
    } else {
        Err(WireError::InvalidMessage {
            detail: format!(
                "unrecognized message shape (id: {has_id}, method: {has_method}, \
                 result: {has_result}, error: {has_error})"
            ),
        })
    }
}

fn parse_object<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, WireError> {
    serde_json::from_value(value).map_err(|error| WireError::InvalidMessage {
        detail: error.to_string(),
    })
}

/// 是否「请求已取消」错误：LSP `-32800` 或消息文本含 cancelled 字样。
pub fn is_cancelled(error: &JsonRpcError) -> bool {
    if error.code == REQUEST_CANCELLED {
        return true;
    }
    let message = error.message.to_ascii_lowercase();
    message.contains("cancelled") || message.contains("canceled")
}

/// 标准 JSON-RPC 错误响应序列化（向服务器回执 parse 错误时用）。
pub fn error_response(id: &Id, error: JsonRpcError) -> String {
    let payload = ErrorResponse {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id: id.clone(),
        error,
    };
    match serde_json::to_string(&payload) {
        Ok(json) => json,
        Err(_) => {
            "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32603,\"message\":\"internal error\"}}"
                .to_string()
        }
    }
}
