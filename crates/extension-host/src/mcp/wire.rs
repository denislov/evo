//! MCP JSON-RPC 2.0 wire 类型与解析。
//!
//! MCP 协议以 JSON-RPC 2.0 + MCP spec（2025-06-18 及之后常用能力）为
//! 准。本模块手写 wire（不引入 rpc 框架，参考 xai-grok-mcp 的
//! `wire.rs`/`servers.rs` 中 `ResilientRwTransport` 的解析纪律）：
//!
//! - 消息信封三种：请求（`id` + `method` + `params`）、响应（`id` +
//!   `result` 或 `error`）、通知（`method` + `params`，无 `id`）。
//! - 解析**严格**：非法 JSON、顶层非对象、缺 `jsonrpc` 字段、未知字段、
//!   类型不符都产生结构化 [`WireError`]（fail closed，不静默吞掉）。
//!   第三方服务器加未知顶层字段属于 spec 违反；宁可显式拒绝也不猜。
//! - `params` / `result` 保持原始 [`serde_json::Value`]，不做深度校验
//!   （业务层按需取字段）。
//!
//! 错误码沿用 JSON-RPC 2.0 保留码；MCP 约定 `-32001` 为
//! `UNAUTHORIZED`（服务器要求认证），见 [`wire_error_unauthorized`]。

// Adapted from xai-grok-mcp, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// wire shape and error-code conventions consulted; strict parsing is Evo's own.
use serde::{Deserialize, Serialize};

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
/// MCP 约定：未授权（服务器要求认证）。
pub const UNAUTHORIZED: i32 = -32001;

/// 请求 / 响应 id：数字或字符串。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(u64),
    String(String),
}

impl Id {
    pub fn as_string(&self) -> String {
        match self {
            Id::Number(number) => number.to_string(),
            Id::String(string) => string.clone(),
        }
    }
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

/// 一行 / 一帧内的完整 MCP 消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Request(Request),
    Response(Response),
    ErrorResponse(ErrorResponse),
    Notification(Notification),
}

impl Message {
    /// 是否「忽略即可」的通知形状（有 `method`、无 `id`）。
    pub fn is_notification(&self) -> bool {
        matches!(self, Message::Notification(_))
    }

    /// 该消息携带的响应 id（响应 / 错误响应）。
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
}

impl WireError {
    /// 该错误对应的 JSON-RPC 错误码（用于向服务器回错误响应）。
    pub fn code(&self) -> i32 {
        match self {
            WireError::InvalidJson { .. } => PARSE_ERROR,
            WireError::InvalidMessage { .. } => INVALID_REQUEST,
            WireError::UnsupportedVersion { .. } => INVALID_REQUEST,
        }
    }
}

/// 解析一段 JSON 字节为 [`Message`]。
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

/// 是否 MCP「未授权」错误：JSON-RPC `-32001`（MCP spec 约定）或
/// 消息文本含认证字样。
pub fn is_unauthorized(error: &JsonRpcError) -> bool {
    if error.code == UNAUTHORIZED {
        return true;
    }
    let message = error.message.to_ascii_lowercase();
    message.contains("unauthorized")
        || message.contains("auth required")
        || message.contains("authentication")
}

/// 标准 JSON-RPC 错误响应构造（供 host 侧解析失败时回执）。
pub fn error_response(id: &Id, error: JsonRpcError) -> String {
    let payload = ErrorResponse {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id: id.clone(),
        error,
    };
    match serde_json::to_string(&payload) {
        Ok(json) => json,
        Err(_) => {
            "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32603,\"message\":\"internal error\"}}"
                .to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(input: &str) -> Result<Message, WireError> {
        parse_message(input.as_bytes())
    }

    #[test]
    fn request_round_trips() {
        let request = Request::new(7, "tools/call", Some(json!({"name": "x"})));
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            parse(&json).unwrap(),
            Message::Request(Request {
                jsonrpc: "2.0".into(),
                id: Id::Number(7),
                method: "tools/call".into(),
                params: Some(json!({"name": "x"})),
            })
        );
    }

    #[test]
    fn response_round_trips() {
        let response = Response {
            jsonrpc: JSONRPC_VERSION.into(),
            id: Id::Number(7),
            result: json!({"content": []}),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(parse(&json).unwrap(), Message::Response(response));
    }

    #[test]
    fn error_response_round_trips_and_unauthorized_detection() {
        let response = ErrorResponse {
            jsonrpc: JSONRPC_VERSION.into(),
            id: Id::String("a".into()),
            error: JsonRpcError::new(UNAUTHORIZED, "authentication required"),
        };
        let json = serde_json::to_string(&response).unwrap();
        let parsed = parse(&json).unwrap();
        assert_eq!(parsed, Message::ErrorResponse(response));
        match &parsed {
            Message::ErrorResponse(err) => assert!(is_unauthorized(&err.error)),
            other => panic!("expected error response, got {other:?}"),
        }
    }

    #[test]
    fn notification_round_trips() {
        let notification = Notification {
            jsonrpc: JSONRPC_VERSION.into(),
            method: "notifications/tools/list_changed".into(),
            params: None,
        };
        let json = serde_json::to_string(&notification).unwrap();
        let parsed = parse(&json).unwrap();
        assert!(parsed.is_notification());
        assert_eq!(parsed, Message::Notification(notification));
        assert_eq!(parsed.response_id(), None);
    }

    #[test]
    fn golden_wire_shapes_are_stable() {
        assert_eq!(
            serde_json::to_value(Request::new(
                1,
                "initialize",
                Some(json!({"protocolVersion": "2025-06-18", "capabilities": {}}))
            ))
            .unwrap(),
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "capabilities": {},
                    "protocolVersion": "2025-06-18"
                }
            })
        );
        assert_eq!(
            serde_json::to_value(JsonRpcError::new(-32602, "bad params")).unwrap(),
            json!({"code": -32602, "message": "bad params"})
        );
        assert_eq!(
            serde_json::to_value(Notification {
                jsonrpc: JSONRPC_VERSION.into(),
                method: "notifications/initialized".into(),
                params: None,
            })
            .unwrap(),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        );
    }

    #[test]
    fn malformed_json_is_structured_error() {
        let error = parse("{oops").unwrap_err();
        assert!(matches!(error, WireError::InvalidJson { .. }));
        assert_eq!(error.code(), PARSE_ERROR);
    }

    #[test]
    fn non_object_top_level_rejected() {
        assert!(matches!(
            parse("[1,2,3]").unwrap_err(),
            WireError::InvalidMessage { .. }
        ));
        assert!(matches!(
            parse("42").unwrap_err(),
            WireError::InvalidMessage { .. }
        ));
    }

    #[test]
    fn missing_jsonrpc_rejected() {
        assert!(matches!(
            parse(r#"{"id":1,"method":"ping"}"#).unwrap_err(),
            WireError::InvalidMessage { .. }
        ));
    }

    #[test]
    fn unknown_top_level_field_rejected() {
        let error = parse(r#"{"jsonrpc":"2.0","id":1,"method":"ping","bogus":1}"#).unwrap_err();
        assert!(matches!(error, WireError::InvalidMessage { .. }));
    }

    #[test]
    fn unsupported_version_rejected() {
        assert!(matches!(
            parse(r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#).unwrap_err(),
            WireError::UnsupportedVersion { version } if version == "1.0"
        ));
    }

    #[test]
    fn ambiguous_shapes_rejected() {
        // 既有 id 又有 result 和 error：无法判别。
        assert!(matches!(
            parse(r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-1,"message":"x"}}"#)
                .unwrap_err(),
            WireError::InvalidMessage { .. }
        ));
        // 通知带 id 判定为请求；method 类型不符拒绝。
        assert!(matches!(
            parse(r#"{"jsonrpc":"2.0","id":1,"method":42}"#).unwrap_err(),
            WireError::InvalidMessage { .. }
        ));
    }

    #[test]
    fn error_response_fallback_serializes_even_on_failure() {
        let json = error_response(&Id::Number(1), JsonRpcError::new(INTERNAL_ERROR, "boom"));
        assert!(parse(&json).is_ok());
    }
}
