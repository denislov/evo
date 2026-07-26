//! Internal owner tests for the find tool.

use ai::api::conversation::ContentBlock;
use coding_agent::tools::filesystem::find::find_execute;
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
async fn finds_basename_matches_recursively_and_skips_common_dirs() {
    let dir = tempdir().unwrap();
    tokio::fs::create_dir_all(dir.path().join("src/nested"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(dir.path().join(".git"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(dir.path().join("node_modules/pkg"))
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("src/lib.rs"), "")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("src/nested/main.rs"), "")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join(".git/hidden.rs"), "")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("node_modules/pkg/index.rs"), "")
        .await
        .unwrap();

    let result = find_execute(dir.path(), serde_json::json!({"pattern": "*.rs"}))
        .await
        .unwrap();

    assert_eq!(text(&result), "src/lib.rs\nsrc/nested/main.rs");
}

#[tokio::test]
async fn path_pattern_matches_relative_path() {
    let dir = tempdir().unwrap();
    tokio::fs::create_dir_all(dir.path().join("src"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(dir.path().join("tests"))
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("src/app.spec.ts"), "")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("tests/app.spec.ts"), "")
        .await
        .unwrap();

    let result = find_execute(
        dir.path(),
        serde_json::json!({"pattern": "src/**/*.spec.ts"}),
    )
    .await
    .unwrap();

    assert_eq!(text(&result), "src/app.spec.ts");
}

#[tokio::test]
async fn limit_and_empty_results_are_reported() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("a.txt"), "")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("b.txt"), "")
        .await
        .unwrap();

    let limited = find_execute(
        dir.path(),
        serde_json::json!({"pattern": "*.txt", "limit": 1}),
    )
    .await
    .unwrap();
    assert_eq!(
        text(&limited),
        "a.txt\n\n[1 results limit reached. Use limit=2 for more, or refine pattern]"
    );

    let empty = find_execute(dir.path(), serde_json::json!({"pattern": "*.rs"}))
        .await
        .unwrap();
    assert_eq!(text(&empty), "No files found matching pattern");
}

#[tokio::test]
async fn capability_walk_preserves_root_and_nested_gitignore_semantics() {
    let dir = tempdir().unwrap();
    tokio::fs::create_dir_all(dir.path().join("nested"))
        .await
        .unwrap();
    tokio::fs::write(dir.path().join(".gitignore"), "ignored.txt\nnested/*.log\n")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("nested/.gitignore"), "!keep.log\n")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("visible.txt"), "")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("ignored.txt"), "")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("nested/drop.log"), "")
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("nested/keep.log"), "")
        .await
        .unwrap();

    let text_files = find_execute(dir.path(), serde_json::json!({"pattern": "*.txt"}))
        .await
        .unwrap();
    assert_eq!(text(&text_files), "visible.txt");

    let log_files = find_execute(dir.path(), serde_json::json!({"pattern": "*.log"}))
        .await
        .unwrap();
    assert_eq!(text(&log_files), "nested/keep.log");
}
