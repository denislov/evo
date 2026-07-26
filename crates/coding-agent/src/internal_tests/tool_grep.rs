//! Internal owner tests for the grep tool.

use ai::api::conversation::ContentBlock;
use coding_agent::tools::filesystem::grep::grep_execute;
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
async fn grep_literal_and_ignore_case_with_relative_paths() {
    let dir = tempdir().unwrap();
    tokio::fs::create_dir_all(dir.path().join("src"))
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("src/a.txt"), "Hello\nworld")
        .await
        .unwrap();

    let result = grep_execute(
        dir.path(),
        serde_json::json!({"pattern": "hello", "literal": true, "ignoreCase": true}),
    )
    .await
    .unwrap();

    assert_eq!(text(&result), "src/a.txt:1: Hello");
}

#[tokio::test]
async fn grep_regex_glob_context_and_limit() {
    let dir = tempdir().unwrap();
    tokio::fs::create_dir_all(dir.path().join("src"))
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("src/a.rs"), "before\nlet alpha = 1;\nafter")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("src/b.txt"), "let beta = 2;")
        .await
        .unwrap();

    let result = grep_execute(
        dir.path(),
        serde_json::json!({
            "pattern": "alpha|beta",
            "glob": "*.rs",
            "context": 1,
            "limit": 1
        }),
    )
    .await
    .unwrap();

    assert_eq!(
        text(&result),
        "src/a.rs-1- before\nsrc/a.rs:2: let alpha = 1;\nsrc/a.rs-3- after\n\n[1 matches limit reached. Use limit=2 for more, or refine pattern]"
    );
}

#[tokio::test]
async fn grep_reports_no_matches_and_invalid_regex() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("a.txt"), "abc")
        .await
        .unwrap();

    let empty = grep_execute(dir.path(), serde_json::json!({"pattern": "zzz"}))
        .await
        .unwrap();
    assert_eq!(text(&empty), "No matches found");

    let err = grep_execute(dir.path(), serde_json::json!({"pattern": "["}))
        .await
        .unwrap_err();
    assert!(err.starts_with("grep: invalid regex:"), "{err}");
}

#[tokio::test]
async fn capability_walk_does_not_search_gitignored_files() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join(".gitignore"), "ignored.txt\n")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("ignored.txt"), "unique-secret")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("visible.txt"), "unique-secret")
        .await
        .unwrap();

    let result = grep_execute(
        dir.path(),
        serde_json::json!({"pattern": "unique-secret", "literal": true}),
    )
    .await
    .unwrap();

    assert_eq!(text(&result), "visible.txt:1: unique-secret");
}
