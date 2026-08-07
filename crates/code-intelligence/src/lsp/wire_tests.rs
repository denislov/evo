//! `lsp::wire` 测试：Content-Length framing、严格解析、截断 / 超大帧 /
//! 非法头、消息形状判别、错误码。

use std::io::{BufReader, Cursor};

use serde_json::json;

use crate::lsp::wire::{
    Id, Message, Notification, Request, Response, WireError, error_response, is_cancelled,
    parse_message, read_frame, read_frame_sync, write_frame, write_frame_sync,
};

fn parse(input: &str) -> Result<Message, WireError> {
    parse_message(input.as_bytes())
}

/// 同步读一帧（默认上限足够大）。
fn read_sync(bytes: &[u8], max: usize) -> Result<Vec<u8>, WireError> {
    read_frame_sync(&mut BufReader::new(Cursor::new(bytes)), max)
}

#[test]
fn message_shapes_round_trip() {
    let request = Request::new(7, "textDocument/hover", Some(json!({"position": {}})));
    let json = serde_json::to_string(&request).unwrap();
    assert_eq!(
        parse(&json).unwrap(),
        Message::Request(Request {
            jsonrpc: "2.0".into(),
            id: Id::Number(7),
            method: "textDocument/hover".into(),
            params: Some(json!({"position": {}})),
        })
    );

    let response = Response {
        jsonrpc: "2.0".into(),
        id: Id::Number(7),
        result: json!({"contents": "hi"}),
    };
    assert_eq!(
        parse(&serde_json::to_string(&response).unwrap()).unwrap(),
        Message::Response(response)
    );

    let notification = Notification {
        jsonrpc: "2.0".into(),
        method: "textDocument/publishDiagnostics".into(),
        params: Some(json!({"uri": "file:///a.rs"})),
    };
    let parsed = parse(&serde_json::to_string(&notification).unwrap()).unwrap();
    assert!(parsed.is_notification());
    assert_eq!(parsed.response_id(), None);
    assert_eq!(parsed, Message::Notification(notification));
}

#[test]
fn string_ids_are_accepted() {
    let parsed = parse(r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#).unwrap();
    match parsed {
        Message::Request(request) => assert_eq!(request.id, Id::String("abc".into())),
        other => panic!("expected request, got {other:?}"),
    }
}

#[test]
fn malformed_json_is_structured_error() {
    let error = parse("{oops").unwrap_err();
    assert!(matches!(error, WireError::InvalidJson { .. }));
    assert_eq!(error.code(), crate::lsp::wire::PARSE_ERROR);
}

#[test]
fn non_object_top_level_rejected() {
    assert!(matches!(
        parse("[1,2]").unwrap_err(),
        WireError::InvalidMessage { .. }
    ));
    assert!(matches!(
        parse("42").unwrap_err(),
        WireError::InvalidMessage { .. }
    ));
}

#[test]
fn missing_jsonrpc_and_unknown_fields_rejected() {
    assert!(matches!(
        parse(r#"{"id":1,"method":"ping"}"#).unwrap_err(),
        WireError::InvalidMessage { .. }
    ));
    assert!(matches!(
        parse(r#"{"jsonrpc":"2.0","id":1,"method":"ping","bogus":1}"#).unwrap_err(),
        WireError::InvalidMessage { .. }
    ));
}

#[test]
fn unsupported_version_rejected() {
    assert!(matches!(
        parse(r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#).unwrap_err(),
        WireError::UnsupportedVersion { .. }
    ));
}

#[test]
fn ambiguous_shapes_rejected() {
    assert!(matches!(
        parse(r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-1,"message":"x"}}"#)
            .unwrap_err(),
        WireError::InvalidMessage { .. }
    ));
    assert!(matches!(
        parse(r#"{"jsonrpc":"2.0","id":1,"method":42}"#).unwrap_err(),
        WireError::InvalidMessage { .. }
    ));
}

#[test]
fn sync_frame_round_trip() {
    let payload = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let mut bytes = Vec::new();
    write_frame_sync(&mut bytes, payload).unwrap();
    let wire = String::from_utf8(bytes.clone()).unwrap();
    assert!(
        wire.starts_with(&format!("Content-Length: {}\r\n\r\n", payload.len())),
        "wire: {wire}"
    );
    let read = read_sync(&bytes, 1024).unwrap();
    assert_eq!(read, payload);
    let parsed = parse_message(&read).unwrap();
    assert!(matches!(parsed, Message::Request(_)));
}

#[tokio::test]
async fn async_frame_round_trip() {
    let payload = br#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics"}"#;
    let (mut writer, reader) = tokio::io::duplex(4096);
    write_frame(&mut writer, payload).await.unwrap();
    drop(writer);
    let mut reader = tokio::io::BufReader::new(reader);
    let bytes = read_frame(&mut reader, 1024).await.unwrap();
    assert_eq!(bytes, payload);
}

#[test]
fn header_with_extra_lines_and_crlf_is_accepted() {
    let payload = b"{}";
    let mut bytes = Vec::new();
    write_frame_sync(&mut bytes, payload).unwrap();
    // 在 Content-Length 前插入一个自定义头行：应忽略。
    let wire = String::from_utf8(bytes).unwrap();
    let framed = format!("X-Custom: 1\r\n{}\r\n", wire.trim_end_matches("\r\n\r\n"));
    let read = read_sync(framed.as_bytes(), 1024).unwrap();
    assert_eq!(read, payload);
}

#[test]
fn content_length_case_insensitive() {
    let framed = "content-length: 2\r\n\r\n{}";
    assert_eq!(read_sync(framed.as_bytes(), 1024).unwrap(), b"{}");
}

#[test]
fn missing_content_length_rejected() {
    let framed = "Content-Type: application/json\r\n\r\n{}";
    assert!(matches!(
        read_sync(framed.as_bytes(), 1024).unwrap_err(),
        WireError::InvalidHeader { .. }
    ));
}

#[test]
fn non_numeric_content_length_rejected() {
    for header in [
        "Content-Length: abc\r\n\r\n{}",
        "Content-Length: -5\r\n\r\n{}",
        "Content-Length: 1.5\r\n\r\n{}",
    ] {
        assert!(
            matches!(
                read_sync(header.as_bytes(), 1024).unwrap_err(),
                WireError::InvalidHeader { .. }
            ),
            "header: {header}"
        );
    }
}

#[test]
fn duplicate_content_length_rejected() {
    let framed = "Content-Length: 2\r\nContent-Length: 3\r\n\r\n{}";
    assert!(matches!(
        read_sync(framed.as_bytes(), 1024).unwrap_err(),
        WireError::InvalidHeader { .. }
    ));
}

#[test]
fn oversized_frame_rejected() {
    let payload = vec![b'x'; 300];
    let mut bytes = Vec::new();
    write_frame_sync(&mut bytes, &payload).unwrap();
    let error = read_sync(&bytes, 100).unwrap_err();
    match error {
        WireError::FrameTooLarge { bytes, limit } => {
            assert_eq!(bytes, 300);
            assert_eq!(limit, 100);
        }
        other => panic!("expected FrameTooLarge, got {other:?}"),
    }
}

#[test]
fn truncated_frame_rejected() {
    // EOF 于头部中间。
    let error = read_sync(b"Content-Length: 100\r", 1024).unwrap_err();
    assert!(matches!(error, WireError::TruncatedFrame { .. }));
    // EOF 于 payload 中间（头声明 10 字节，只有 3 字节）。
    let framed = b"Content-Length: 10\r\n\r\nabc";
    let error = read_sync(framed, 1024).unwrap_err();
    assert!(matches!(error, WireError::TruncatedFrame { .. }));
    // EOF 于空输入。
    let error = read_sync(b"", 1024).unwrap_err();
    assert!(matches!(error, WireError::TruncatedFrame { .. }));
}

#[test]
fn oversized_header_rejected() {
    let mut framed = "Content-Length: 0\r\n".to_string();
    framed.push_str(&"x".repeat(8192));
    framed.push_str("\r\n\r\n");
    let error = read_sync(framed.as_bytes(), 1024).unwrap_err();
    assert!(matches!(error, WireError::InvalidHeader { .. }));
}

#[test]
fn cancelled_error_detection() {
    let error = crate::lsp::wire::JsonRpcError::new(
        crate::lsp::wire::REQUEST_CANCELLED,
        "request cancelled",
    );
    assert!(is_cancelled(&error));
    let error = crate::lsp::wire::JsonRpcError::new(-32000, "request was canceled by the client");
    assert!(is_cancelled(&error));
    let error = crate::lsp::wire::JsonRpcError::new(-32000, "server exploded");
    assert!(!is_cancelled(&error));
}

#[test]
fn error_response_serializes_and_parses() {
    let json = error_response(
        &Id::Number(3),
        crate::lsp::wire::JsonRpcError::new(crate::lsp::wire::INVALID_PARAMS, "bad params"),
    );
    match parse(&json).unwrap() {
        Message::ErrorResponse(response) => {
            assert_eq!(response.id, Id::Number(3));
            assert_eq!(response.error.code, crate::lsp::wire::INVALID_PARAMS);
        }
        other => panic!("expected error response, got {other:?}"),
    }
}
