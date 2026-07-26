//! JSONL framing behavior shared by machine-readable protocols.

use super::jsonl::{JsonlFrame, JsonlLineReader, read_jsonl_lines, serialize_json_line};
use serde_json::json;
use tokio::io::AsyncWriteExt;

#[test]
fn serialize_json_line_appends_exactly_one_lf() {
    let line = serialize_json_line(&json!({"type": "agent_start"})).unwrap();
    assert_eq!(line, "{\"type\":\"agent_start\"}\n");
}

#[tokio::test]
async fn jsonl_reader_splits_only_on_lf_and_strips_cr() {
    let input = b"{\"type\":\"a\"}\r\n{\"message\":\"line\\u2028inside\"}\n{\"type\":\"c\"}";
    let lines = read_jsonl_lines(&input[..]).await.unwrap();
    assert_eq!(
        lines,
        vec![
            "{\"type\":\"a\"}".to_string(),
            "{\"message\":\"line\\u2028inside\"}".to_string(),
            "{\"type\":\"c\"}".to_string(),
        ]
    );
}

#[tokio::test]
async fn jsonl_reader_handles_chunk_boundaries() {
    let (mut writer, reader) = tokio::io::duplex(8);
    let task = tokio::spawn(async move { read_jsonl_lines(reader).await.unwrap() });
    writer.write_all(b"{\"type\"").await.unwrap();
    writer
        .write_all(b":\"a\"}\n{\"type\":\"b\"}")
        .await
        .unwrap();
    drop(writer);
    let lines = task.await.unwrap();
    assert_eq!(
        lines,
        vec![
            "{\"type\":\"a\"}".to_string(),
            "{\"type\":\"b\"}".to_string()
        ]
    );
}

#[tokio::test]
async fn jsonl_reader_accepts_the_exact_frame_limit() {
    let mut reader = JsonlLineReader::with_max_frame_bytes(&b"12345678\n"[..], 8);
    assert_eq!(
        reader.read_next_frame().await.unwrap(),
        Some(JsonlFrame::Line("12345678".into()))
    );
    assert_eq!(reader.read_next_frame().await.unwrap(), None);
}

#[tokio::test]
async fn jsonl_reader_discards_one_byte_over_and_recovers_at_lf() {
    let mut reader = JsonlLineReader::with_max_frame_bytes(&b"123456789\nok\n"[..], 8);
    assert_eq!(
        reader.read_next_frame().await.unwrap(),
        Some(JsonlFrame::TooLarge { max_bytes: 8 })
    );
    assert_eq!(
        reader.read_next_frame().await.unwrap(),
        Some(JsonlFrame::Line("ok".into()))
    );
    assert_eq!(reader.read_next_frame().await.unwrap(), None);
}

#[tokio::test]
async fn jsonl_reader_reports_one_bounded_error_for_oversized_eof_frame() {
    let mut reader = JsonlLineReader::with_max_frame_bytes(&b"1234567890123456"[..], 8);
    assert_eq!(
        reader.read_next_frame().await.unwrap(),
        Some(JsonlFrame::TooLarge { max_bytes: 8 })
    );
    assert_eq!(reader.read_next_frame().await.unwrap(), None);
}
