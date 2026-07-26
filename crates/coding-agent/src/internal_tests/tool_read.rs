//! Internal owner tests for the read tool.

use ai::api::conversation::ContentBlock;
use coding_agent::tools::filesystem::read::read_execute;
use tempfile::tempdir;

fn text(b: &[ContentBlock]) -> String {
    b.iter()
        .filter_map(|x| match x {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn write(dir: &std::path::Path, name: &str, body: &str) {
    tokio::fs::write(dir.join(name), body).await.unwrap();
}

#[tokio::test]
async fn reads_full_file() {
    let d = tempdir().unwrap();
    write(d.path(), "a.txt", "l1\nl2\nl3").await;
    let r = read_execute(d.path(), serde_json::json!({"path":"a.txt"}))
        .await
        .unwrap();
    assert_eq!(text(&r), "l1\nl2\nl3");
}

#[tokio::test]
async fn offset_and_limit() {
    let d = tempdir().unwrap();
    let body = (1..=5)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    write(d.path(), "a.txt", &body).await;
    let r = read_execute(
        d.path(),
        serde_json::json!({"path":"a.txt","offset":2,"limit":2}),
    )
    .await
    .unwrap();
    let t = text(&r);
    assert!(t.starts_with("line2\nline3"));
    assert!(t.contains("more lines in file. Use offset=4 to continue."));
}

#[tokio::test]
async fn offset_out_of_bounds() {
    let d = tempdir().unwrap();
    write(d.path(), "a.txt", "x\ny").await;
    let e = read_execute(d.path(), serde_json::json!({"path":"a.txt","offset":99}))
        .await
        .unwrap_err();
    assert_eq!(e, "Offset 99 is beyond end of file (2 lines total)");
}

#[tokio::test]
async fn offset_zero_reads_from_start() {
    let d = tempdir().unwrap();
    write(d.path(), "a.txt", "x\ny").await;
    let r = read_execute(d.path(), serde_json::json!({"path":"a.txt","offset":0}))
        .await
        .unwrap();
    assert_eq!(text(&r), "x\ny");
}

#[tokio::test]
async fn missing_file_errors() {
    let d = tempdir().unwrap();
    assert!(
        read_execute(d.path(), serde_json::json!({"path":"nope.txt"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn image_returns_note() {
    let d = tempdir().unwrap();
    tokio::fs::write(d.path().join("a.png"), b"not really")
        .await
        .unwrap();
    let r = read_execute(d.path(), serde_json::json!({"path":"a.png"}))
        .await
        .unwrap();
    assert_eq!(
        text(&r),
        "Read image file [image/png]\n[Image content is not supported in headless mode yet; omitted.]"
    );
}

#[tokio::test]
async fn limit_larger_than_remaining_clips_to_eof() {
    let d = tempdir().unwrap();
    write(d.path(), "a.txt", "x\ny").await;
    let r = read_execute(
        d.path(),
        serde_json::json!({"path":"a.txt","offset":2,"limit":99}),
    )
    .await
    .unwrap();
    assert_eq!(text(&r), "y");
}

#[tokio::test]
async fn line_truncation_has_continuation() {
    let d = tempdir().unwrap();
    let body = (1..=2005)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    write(d.path(), "a.txt", &body).await;
    let r = read_execute(d.path(), serde_json::json!({"path":"a.txt"}))
        .await
        .unwrap();
    let t = text(&r);
    assert!(t.contains("line2000"));
    assert!(t.contains("[Showing lines 1-2000 of 2005. Use offset=2001 to continue.]"));
}

#[tokio::test]
async fn byte_truncation_has_continuation() {
    let d = tempdir().unwrap();
    let body = (1..=60)
        .map(|i| format!("{i}:{}", "x".repeat(1000)))
        .collect::<Vec<_>>()
        .join("\n");
    write(d.path(), "a.txt", &body).await;
    let r = read_execute(d.path(), serde_json::json!({"path":"a.txt"}))
        .await
        .unwrap();
    let t = text(&r);
    assert!(t.contains("(50.0KB limit). Use offset="), "{t}");
}

#[tokio::test]
async fn first_line_exceeds_limit_has_bash_hint() {
    let d = tempdir().unwrap();
    tokio::fs::write(d.path().join("a.txt"), "x".repeat(51201))
        .await
        .unwrap();
    let r = read_execute(d.path(), serde_json::json!({"path":"a.txt"}))
        .await
        .unwrap();
    assert_eq!(
        text(&r),
        "[Line 1 is 50.0KB, exceeds 50.0KB limit. Use bash: sed -n '1p' a.txt | head -c 51200]"
    );
}
