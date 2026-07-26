//! Internal owner tests for the edit tool.

use crate::internal_tests::product_fixture::resources::builtin_tools;
use agent_core::api::tool::AgentToolOutput;
use ai::api::conversation::ContentBlock;
use coding_agent::tools::filesystem::edit::edit_execute;
use tempfile::tempdir;

fn text(output: &AgentToolOutput) -> String {
    output
        .content
        .iter()
        .filter_map(|x| match x {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn builtin_edit_tool_reports_self_healing_workflow_details() {
    let d = tempdir().unwrap();
    let p = d.path().join("f.txt");
    std::fs::write(&p, "hello world").unwrap();
    let tool = builtin_tools(d.path().to_path_buf())
        .unwrap()
        .into_iter()
        .find(|tool| tool.name == "edit")
        .expect("builtin edit tool should exist");

    let output = (tool.execute)(
        agent_core::api::tool::ToolExecutionContext::standalone(tool.name.clone()),
        serde_json::json!({
            "path":"f.txt",
            "edits":[{"oldText":"world","newText":"rust"}]
        }),
        None,
    )
    .await
    .unwrap();

    assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello rust");
    assert!(text(&output).contains("Successfully replaced 1 block(s) in f.txt."));
    let details = output.details.expect("edit result should include details");
    assert!(details["diff"].as_str().unwrap().contains("+1 hello rust"));
    assert!(details["patch"].as_str().unwrap().contains("+hello rust"));
    assert_eq!(details["firstChangedLine"], 1);
    assert_eq!(details["selfHealingEdit"]["attempts"], 1);
    assert_eq!(
        details["selfHealingEdit"]["diagnostics"],
        serde_json::json!([])
    );
}

#[tokio::test]
async fn builtin_edit_tool_preserves_edit_error_message() {
    let d = tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "abc").unwrap();
    let tool = builtin_tools(d.path().to_path_buf())
        .unwrap()
        .into_iter()
        .find(|tool| tool.name == "edit")
        .expect("builtin edit tool should exist");

    let error = (tool.execute)(
        agent_core::api::tool::ToolExecutionContext::standalone(tool.name.clone()),
        serde_json::json!({
            "path":"f.txt",
            "edits":[{"oldText":"xyz","newText":"q"}]
        }),
        None,
    )
    .await
    .unwrap_err();

    assert_eq!(
        error,
        "Could not find the exact text in f.txt. The old text must match exactly including all whitespace and newlines."
    );
}

#[tokio::test]
async fn exact_replace() {
    let d = tempdir().unwrap();
    let p = d.path().join("f.txt");
    std::fs::write(&p, "hello world").unwrap();
    let r = edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","edits":[{"oldText":"world","newText":"rust"}]}),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello rust");
    assert!(text(&r).contains("Successfully replaced 1 block(s) in f.txt."));
}

#[tokio::test]
async fn returns_diff_details_for_full_file_change() {
    let d = tempdir().unwrap();
    let p = d.path().join("f.txt");
    std::fs::write(&p, "one\ntwo\nthree\n").unwrap();
    let r = edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","edits":[{"oldText":"two","newText":"TWO"}]}),
    )
    .await
    .unwrap();

    let details = r.details.expect("edit result should include details");
    assert_eq!(details["firstChangedLine"], 2);
    assert!(details["diff"].as_str().unwrap().contains("-2 two"));
    assert!(details["diff"].as_str().unwrap().contains("+2 TWO"));
    assert!(details["patch"].as_str().unwrap().contains("--- f.txt"));
    assert!(details["patch"].as_str().unwrap().contains("+++ f.txt"));
    assert!(details["patch"].as_str().unwrap().contains("-two"));
    assert!(details["patch"].as_str().unwrap().contains("+TWO"));
}

#[tokio::test]
async fn multi_edit_success() {
    let d = tempdir().unwrap();
    let p = d.path().join("f.txt");
    std::fs::write(&p, "a\nb\nc").unwrap();
    edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","edits":[{"oldText":"a","newText":"A"},{"oldText":"c","newText":"C"}]}),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "A\nb\nC");
}

#[tokio::test]
async fn not_found_single() {
    let d = tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "abc").unwrap();
    let e = edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","edits":[{"oldText":"xyz","newText":"q"}]}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        e,
        "Could not find the exact text in f.txt. The old text must match exactly including all whitespace and newlines."
    );
}

#[tokio::test]
async fn duplicate_single() {
    let d = tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "x x").unwrap();
    let e = edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","edits":[{"oldText":"x","newText":"y"}]}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        e,
        "Found 2 occurrences of the text in f.txt. The text must be unique. Please provide more context to make it unique."
    );
}

#[tokio::test]
async fn overlap_errors() {
    let d = tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "abcdef").unwrap();
    let e = edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","edits":[{"oldText":"abc","newText":"X"},{"oldText":"bcd","newText":"Y"}]}),
    )
    .await
    .unwrap_err();
    assert!(e.contains("overlap in f.txt"));
}

#[tokio::test]
async fn empty_oldtext_single() {
    let d = tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "abc").unwrap();
    let e = edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","edits":[{"oldText":"","newText":"q"}]}),
    )
    .await
    .unwrap_err();
    assert_eq!(e, "oldText must not be empty in f.txt.");
}

#[tokio::test]
async fn empty_oldtext_multi() {
    let d = tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "abc").unwrap();
    let e = edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","edits":[{"oldText":"a","newText":"A"},{"oldText":"","newText":"q"}]}),
    )
    .await
    .unwrap_err();
    assert_eq!(e, "edits[1].oldText must not be empty in f.txt.");
}

#[tokio::test]
async fn no_change_errors() {
    let d = tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "abc").unwrap();
    let e = edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","edits":[{"oldText":"abc","newText":"abc"}]}),
    )
    .await
    .unwrap_err();
    assert!(e.starts_with("No changes made to f.txt."));
}

#[tokio::test]
async fn fuzzy_smart_quote() {
    let d = tempdir().unwrap();
    let p = d.path().join("f.txt");
    std::fs::write(&p, "say \u{2018}hi\u{2019} now").unwrap();
    let r = edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","edits":[{"oldText":"say 'hi' now","newText":"done"}]}),
    )
    .await;
    assert!(r.is_ok(), "fuzzy match should succeed: {r:?}");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "done");
}

#[tokio::test]
async fn fuzzy_preserves_replacement_text() {
    let d = tempdir().unwrap();
    let p = d.path().join("f.txt");
    std::fs::write(&p, "say \u{2018}hi\u{2019} now").unwrap();
    edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","edits":[{"oldText":"say 'hi' now","newText":"done \u{2013} now"}]}),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "done \u{2013} now");
}

#[tokio::test]
async fn fuzzy_preserves_unmatched_lines_original_bytes() {
    // Safety contract (TS applyReplacementsPreservingUnchangedLines): when an
    // edit matches via fuzzy normalization, only the touched lines are
    // rewritten from the normalized base; unchanged lines keep their original
    // bytes. A smart quote on an untouched line must survive verbatim.
    let d = tempdir().unwrap();
    let p = d.path().join("f.txt");
    // Line 1 has smart quotes (untouched); line 2 is the fuzzy target written
    // with ASCII quotes in oldText.
    std::fs::write(
        &p,
        "keep \u{2018}this\u{2019} line\nreplace \u{2018}me\u{2019} here\n",
    )
    .unwrap();
    edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","edits":[{"oldText":"replace 'me' here","newText":"done"}]}),
    )
    .await
    .unwrap();
    let result = std::fs::read_to_string(&p).unwrap();
    assert_eq!(result, "keep \u{2018}this\u{2019} line\ndone\n");
}

#[tokio::test]
async fn crlf_preserved() {
    let d = tempdir().unwrap();
    let p = d.path().join("f.txt");
    std::fs::write(&p, "a\r\nb\r\nc").unwrap();
    edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","edits":[{"oldText":"b","newText":"B"}]}),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "a\r\nB\r\nc");
}

#[tokio::test]
async fn bom_preserved() {
    let d = tempdir().unwrap();
    let p = d.path().join("f.txt");
    std::fs::write(&p, "\u{feff}a\nb").unwrap();
    edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","edits":[{"oldText":"b","newText":"B"}]}),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "\u{feff}a\nB");
}

#[tokio::test]
async fn legacy_single_edit_args() {
    let d = tempdir().unwrap();
    let p = d.path().join("f.txt");
    std::fs::write(&p, "abc").unwrap();
    edit_execute(
        d.path(),
        serde_json::json!({"path":"f.txt","oldText":"abc","newText":"xyz"}),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "xyz");
}
