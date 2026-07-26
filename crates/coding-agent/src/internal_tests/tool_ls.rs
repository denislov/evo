//! Internal owner tests for the directory-listing tool.

use ai::api::conversation::ContentBlock;
use coding_agent::tools::filesystem::ls::ls_execute;
use tempfile::tempdir;

fn text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn lists_entries_sorted_with_directory_suffix_and_dotfiles() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("b.txt"), "b")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join(".env"), "x")
        .await
        .unwrap();
    tokio::fs::create_dir(dir.path().join("Alpha"))
        .await
        .unwrap();

    let result = ls_execute(dir.path(), serde_json::json!({})).await.unwrap();

    assert_eq!(text(&result), ".env\nAlpha/\nb.txt");
}

#[tokio::test]
async fn limit_adds_notice() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("a.txt"), "a")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("b.txt"), "b")
        .await
        .unwrap();

    let result = ls_execute(dir.path(), serde_json::json!({"limit": 1}))
        .await
        .unwrap();

    let output = text(&result);
    assert_eq!(
        output,
        "a.txt\n\n[1 entries limit reached. Use limit=2 for more]"
    );
}

#[tokio::test]
async fn empty_directory_message() {
    let dir = tempdir().unwrap();

    let result = ls_execute(dir.path(), serde_json::json!({})).await.unwrap();

    assert_eq!(text(&result), "(empty directory)");
}

#[tokio::test]
async fn missing_and_file_paths_error() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("file.txt"), "x")
        .await
        .unwrap();

    let missing = ls_execute(dir.path(), serde_json::json!({"path": "missing"}))
        .await
        .unwrap_err();
    assert!(missing.starts_with("ls: path not found:"), "{missing}");

    let file = ls_execute(dir.path(), serde_json::json!({"path": "file.txt"}))
        .await
        .unwrap_err();
    assert!(file.starts_with("ls: not a directory:"), "{file}");
}
