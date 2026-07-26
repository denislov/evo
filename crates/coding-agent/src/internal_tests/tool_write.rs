//! Internal owner tests for the write tool.

use ai::api::conversation::ContentBlock;
use coding_agent::tools::filesystem::write::write_execute;
use tempfile::tempdir;

fn text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn writes_and_creates_parents() {
    let dir = tempdir().unwrap();
    let args = serde_json::json!({ "path": "sub/dir/out.txt", "content": "héllo" });
    let r = write_execute(dir.path(), args).await.unwrap();
    let written = std::fs::read_to_string(dir.path().join("sub/dir/out.txt")).unwrap();
    assert_eq!(written, "héllo");
    assert!(text(&r).contains("Successfully wrote 6 bytes to sub/dir/out.txt"));
}

#[tokio::test]
async fn missing_args_error() {
    let dir = tempdir().unwrap();
    let err = write_execute(dir.path(), serde_json::json!({ "path": "x" }))
        .await
        .unwrap_err();
    assert!(err.contains("content"));
}
