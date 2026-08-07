use super::{
    ConversationBlockKind, ConversationPane, MAX_MARKDOWN_PARSE_STATES,
    conversation_copy_footer_visible, conversation_identity_header_visible,
    delegation_task_summary, edit_diff_text, parse_grep_context, parse_grep_match,
    strip_ws_call_id, tool_detail_copy_text, tool_disclosure_icon, tool_display_label,
    tool_name_from_title, tool_summary, user_message_width, write_diff_text,
};
use gpui::{AppContext as _, Entity, TestAppContext};
use gpui_component::{Theme, ThemeMode, text::TextViewState};
use std::sync::Arc;

#[test]
fn user_message_width_wraps_content_and_caps_long_lines() {
    assert!(user_message_width("Short prompt") < 320.);
    assert!(user_message_width("中文提示") >= user_message_width("test"));
    assert_eq!(
        user_message_width(&"long wrapping prompt ".repeat(200)),
        desktop::ui::shell::USER_MESSAGE_MAX_WIDTH as f32
    );
}

#[test]
fn web_search_tools_render_action_specific_summaries() {
    assert_eq!(tool_display_label("web_search"), "Web search");
    // Search action: queries carry the internal `ws_call_id` marker that
    // must be hidden before counting.
    assert_eq!(
        tool_summary(
            "web_search",
            r#"{"type":"web_search_call","id":"call_1","status":"in_progress"}"#,
            r#"{"status":"completed","action":{"type":"search","queries":["2025年诺贝尔物理学奖 获奖者","ws_call_id=call_1"]}}"#,
        ),
        "搜索：2025年诺贝尔物理学奖 获奖者"
    );
    assert_eq!(
        tool_summary(
            "web_search",
            "{}",
            r#"{"status":"completed","action":{"type":"search","queries":["2025年诺贝尔物理学奖 获奖者","Nobel Prize Physics 2025"]}}"#,
        ),
        "搜索 2 个查询：2025年诺贝尔物理学奖 获奖者"
    );
    // Open-page action strips the trailing `#ws_call_id` fragment.
    assert_eq!(
        tool_summary(
            "web_search",
            "{}",
            r#"{"status":"completed","action":{"type":"open_page","url":"https://nobelprize.org/prizes/physics/2025/summary/#ws_call_id=call_2"}}"#,
        ),
        "打开页面：https://nobelprize.org/prizes/physics/2025/summary/"
    );
    // Legacy items without an action fall back to no summary.
    assert_eq!(tool_summary("web_search", "{}", "completed"), "");
    assert_eq!(
        strip_ws_call_id("https://x/y#ws_call_id=abc"),
        "https://x/y"
    );
}

#[test]
fn tool_titles_and_summaries_use_structured_arguments() {
    assert_eq!(tool_name_from_title("Tool · bash · 320 ms"), "bash");
    assert_eq!(
        tool_summary("bash", r#"{"command":"git status --short"}"#, ""),
        "git status --short"
    );
    assert_eq!(
        tool_summary(
            "read",
            r#"{"path":"src/main.rs","offset":40,"limit":80}"#,
            ""
        ),
        "src/main.rs [40,80]"
    );
    assert_eq!(
        tool_summary(
            "edit",
            r#"{"path":"src/main.rs","oldText":"one\ntwo","newText":"three\nfour\nfive"}"#,
            ""
        ),
        "src/main.rs +3 -2"
    );
    assert_eq!(
        tool_summary(
            "write",
            r#"{"path":"src/lib.rs","content":"line one\nline two\n"}"#,
            ""
        ),
        "src/lib.rs +2"
    );
    assert_eq!(
        tool_summary("ls", r#"{"path":"src"}"#, "a.rs\nb.rs\nlib/\n"),
        "src · 3 entries"
    );
    assert_eq!(
        tool_summary("ls", r#"{"path":"."}"#, "(empty directory)"),
        ". · empty"
    );
    assert_eq!(
        tool_summary("find", r#"{"pattern":"*.rs"}"#, "a.rs\nb.rs"),
        "*.rs · 2 matches"
    );
    assert_eq!(
        tool_summary(
            "find",
            r#"{"pattern":"*.rs"}"#,
            "No files found matching pattern"
        ),
        "*.rs · no matches"
    );
    assert_eq!(
        tool_summary(
            "grep",
            r#"{"pattern":"foo"}"#,
            "src/a.rs:3: foo\nsrc/b.rs:1: foo\n\n[2 matches limit reached]"
        ),
        "foo · 2 matches"
    );
    // grep context lines around a match do not inflate the count.
    assert_eq!(
        tool_summary(
            "grep",
            r#"{"pattern":"fn","context":1}"#,
            "src/lib.rs-3- use std::io;\nsrc/lib.rs:4: fn main() {}"
        ),
        "fn · 1 match"
    );
    // A match whose content starts with '[' or contains ': ' still counts
    // and is not mistaken for the trailing notice block.
    assert_eq!(
        tool_summary(
            "grep",
            r#"{"pattern":"a"}"#,
            "src/a.rs:2: [foo]\nsrc/b.rs:5: let m = {a: 1}\n\n[3 matches limit reached]"
        ),
        "a · 2 matches"
    );
    assert_eq!(
        tool_summary("grep", r#"{"pattern":"nope"}"#, "No matches found"),
        "nope · no matches"
    );
}

#[test]
fn grep_lines_parse_paths_line_numbers_and_context() {
    assert_eq!(
        parse_grep_match("src/a.rs:12: let x = 1"),
        Some(("src/a.rs", "12", "let x = 1"))
    );
    // A path containing ':' or content containing ': ' must not confuse
    // the final `: <digits>: ` anchor.
    assert_eq!(
        parse_grep_match("src/a.rs:3: url = \"http://x:8080\""),
        Some(("src/a.rs", "3", "url = \"http://x:8080\""))
    );
    assert_eq!(
        parse_grep_match("src/a.rs:3: let m = {a: 1, b: 2}"),
        Some(("src/a.rs", "3", "let m = {a: 1, b: 2}"))
    );
    assert_eq!(
        parse_grep_context("src/lib.rs-3- use std::io;"),
        Some(("src/lib.rs", "3", "use std::io;"))
    );
    // A hyphenated basename still splits at the final `- <digits>- `.
    assert_eq!(
        parse_grep_context("my-file.rs-5- let y = 2"),
        Some(("my-file.rs", "5", "let y = 2"))
    );
    // Content containing '- ' must not confuse the context anchor.
    assert_eq!(
        parse_grep_context("src/lib.rs-2- let y = a - b"),
        Some(("src/lib.rs", "2", "let y = a - b"))
    );
    assert_eq!(parse_grep_match("not a match line"), None);
    assert_eq!(parse_grep_match("src/a.rs:12"), None);
    assert_eq!(parse_grep_context("src/a.rs:12: content"), None);
}

#[test]
fn tool_detail_copy_matches_the_expanded_shell_edit_and_write_views() {
    assert_eq!(
        tool_detail_copy_text(
            "Tool · shell · 1.2 s",
            r#"{"command":"git status"}"#,
            "M src/main.rs\n"
        ),
        "$ git status\nM src/main.rs\n"
    );
    let edit = r#"{"path":"src/main.rs","oldText":"old one\nold two","newText":"new one"}"#;
    assert_eq!(edit_diff_text(edit, ""), "- old one\n- old two\n+ new one");
    assert_eq!(
        tool_detail_copy_text("Tool · edit · 90 ms", edit, "done"),
        "- old one\n- old two\n+ new one"
    );
    let write = r#"{"path":"src/lib.rs","content":"line one\nline two\n"}"#;
    assert_eq!(write_diff_text(write, ""), "+ line one\n+ line two");
    assert_eq!(
        tool_detail_copy_text("Tool · write · 15 ms", write, "Wrote 18 bytes"),
        "+ line one\n+ line two"
    );
    // A write whose args were truncated mid-JSON (no parseable content)
    // falls back to copying the tool result text.
    let truncated = r#"{"path":"src/lib.rs","content":"trunc"#;
    assert_eq!(write_diff_text(truncated, ""), "");
    assert_eq!(
        tool_detail_copy_text("Tool · write · 15 ms", truncated, "Wrote 18 bytes"),
        "Wrote 18 bytes"
    );
    // Delegation copy joins the task and the result summary.
    assert_eq!(
        tool_detail_copy_text("Delegation · Agent", "summary text", "task text"),
        "task text\nsummary text"
    );
}

#[test]
fn identity_headers_hide_for_user_and_continue_across_tool_group_rows() {
    assert!(!conversation_identity_header_visible(
        ConversationBlockKind::User,
        Some(ConversationBlockKind::Assistant)
    ));
    assert!(!conversation_identity_header_visible(
        ConversationBlockKind::Assistant,
        Some(ConversationBlockKind::Tool)
    ));
    assert!(!conversation_identity_header_visible(
        ConversationBlockKind::Assistant,
        Some(ConversationBlockKind::Delegation)
    ));
    assert!(conversation_identity_header_visible(
        ConversationBlockKind::Assistant,
        Some(ConversationBlockKind::User)
    ));
    assert!(conversation_identity_header_visible(
        ConversationBlockKind::Tool,
        Some(ConversationBlockKind::Assistant)
    ));
}

#[test]
fn assistant_copy_waits_until_the_tool_group_finishes() {
    assert!(!conversation_copy_footer_visible(
        ConversationBlockKind::Assistant,
        Some(ConversationBlockKind::Tool)
    ));
    assert!(!conversation_copy_footer_visible(
        ConversationBlockKind::Assistant,
        Some(ConversationBlockKind::Delegation)
    ));
    assert!(conversation_copy_footer_visible(
        ConversationBlockKind::Assistant,
        None
    ));
    assert!(!conversation_copy_footer_visible(
        ConversationBlockKind::Tool,
        Some(ConversationBlockKind::Assistant)
    ));
}

#[test]
fn delegation_summary_uses_the_first_task_line() {
    assert_eq!(
        delegation_task_summary("Implement the auth flow\nsecond line"),
        "Implement the auth flow"
    );
    assert_eq!(delegation_task_summary("single line"), "single line");
    assert_eq!(delegation_task_summary(""), "");
}

#[test]
fn delegation_rows_hide_the_generic_copy_footer_like_tools() {
    assert!(!conversation_copy_footer_visible(
        ConversationBlockKind::Delegation,
        None
    ));
    assert!(!conversation_copy_footer_visible(
        ConversationBlockKind::Delegation,
        Some(ConversationBlockKind::Assistant)
    ));
    // An assistant row followed by a delegation is still mid-turn: the
    // copy affordance waits for the delegation like it does for a tool.
    assert!(!conversation_copy_footer_visible(
        ConversationBlockKind::Assistant,
        Some(ConversationBlockKind::Delegation)
    ));
    assert!(conversation_copy_footer_visible(
        ConversationBlockKind::Assistant,
        None
    ));
}

#[test]
fn tool_disclosure_rotates_down_when_expanded() {
    assert_eq!(
        tool_disclosure_icon(false),
        super::DesktopIcon::ChevronRight
    );
    assert_eq!(tool_disclosure_icon(true), super::DesktopIcon::ChevronDown);
}

fn measure(cx: &mut gpui::VisualTestContext, state: &Entity<TextViewState>) -> f32 {
    use gpui::{ParentElement as _, Styled as _, px, size};
    use gpui_component::{ElementExt as _, text::TextView};
    use std::cell::RefCell;
    use std::rc::Rc;

    let observed = Rc::new(RefCell::new(0.0f32));
    let sink = Rc::clone(&observed);
    let state = state.clone();
    cx.draw(
        gpui::point(px(0.), px(0.)),
        size(px(900.), px(4_000.)),
        move |_, _| {
            gpui::div().w(px(900.)).child(
                gpui::div()
                    .w_full()
                    .on_prepaint(move |bounds, _, _| {
                        *sink.borrow_mut() = f32::from(bounds.size.height);
                    })
                    .child(TextView::new(&state)),
            )
        },
    );
    *observed.borrow()
}

struct PaneRoot;
impl gpui::Render for PaneRoot {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div()
    }
}

/// Streaming a row in chunks must land on the same document as parsing it in
/// one shot.
///
/// The pane feeds a reused `TextViewState` the smallest update that gets it
/// to the current text, so a delta becomes an incremental background append.
/// If the suffix arithmetic or the append path were wrong the row would
/// silently render a truncated or duplicated document, which the rendered
/// height catches.
#[gpui::test]
fn streamed_chunks_and_a_single_parse_reach_the_same_document(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
    });
    let (_, visual_cx) = cx.add_window_view(|_, _| PaneRoot);
    visual_cx.run_until_parked();

    let pane = visual_cx.update(|_, cx| cx.new(|_| ConversationPane::new()));
    let chunks = [
        "# Heading\n\nfirst paragraph with **bold**\n\n",
        "- alpha\n- beta\n\n",
        "```rust\nfn main() {}\n```\n\n",
        "closing paragraph\n",
    ];
    let full: String = chunks.concat();

    let streamed_key: Arc<str> = Arc::from("transcript-markdown:row:streaming");
    let oneshot_key: Arc<str> = Arc::from("transcript-markdown:other:streaming");

    let mut accumulated = String::new();
    let mut streamed = None;
    for chunk in chunks {
        accumulated.push_str(chunk);
        let text: Arc<str> = Arc::from(accumulated.as_str());
        streamed = Some(pane.update(visual_cx, |pane, cx| {
            pane.markdown_state(&streamed_key, &text, cx)
        }));
        visual_cx.run_until_parked();
    }
    let streamed = streamed.expect("the streamed row resolved a parse state");

    let oneshot_text: Arc<str> = Arc::from(full.as_str());
    let oneshot = pane.update(visual_cx, |pane, cx| {
        pane.markdown_state(&oneshot_key, &oneshot_text, cx)
    });
    visual_cx.run_until_parked();

    let streamed_height = measure(visual_cx, &streamed);
    let oneshot_height = measure(visual_cx, &oneshot);
    assert!(oneshot_height > 100., "the fixture must be substantial");
    assert_eq!(
        streamed_height, oneshot_height,
        "incrementally appended chunks must render the same document as one parse"
    );

    // One state per row body, reused across every delta rather than rebuilt.
    let (state_count, reused) = pane.read_with(visual_cx, |pane, _| {
        (
            pane.markdown_states.len(),
            pane.markdown_states
                .get(&streamed_key)
                .map(|entry| entry.state.entity_id()),
        )
    });
    assert_eq!(state_count, 2);
    assert_eq!(reused, Some(streamed.entity_id()));
}

/// A revision that is not an extension has to replace, not append.
#[gpui::test]
fn a_rewritten_row_replaces_its_document_in_place(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
    });
    let (_, visual_cx) = cx.add_window_view(|_, _| PaneRoot);
    visual_cx.run_until_parked();

    let pane = visual_cx.update(|_, cx| cx.new(|_| ConversationPane::new()));
    let key: Arc<str> = Arc::from("transcript-markdown:rewound:streaming");

    let long: Arc<str> = Arc::from("paragraph\n\n".repeat(12).as_str());
    let long_state = pane.update(visual_cx, |pane, cx| pane.markdown_state(&key, &long, cx));
    visual_cx.run_until_parked();
    let long_height = measure(visual_cx, &long_state);

    // Completion swaps in sanitised text, and a rewind or branch can shorten
    // a row outright; neither is a suffix of what came before.
    let short: Arc<str> = Arc::from("paragraph\n");
    let short_state = pane.update(visual_cx, |pane, cx| pane.markdown_state(&key, &short, cx));
    visual_cx.run_until_parked();
    let short_height = measure(visual_cx, &short_state);

    assert_eq!(
        long_state.entity_id(),
        short_state.entity_id(),
        "the row keeps one parse state across a rewrite"
    );
    assert!(
        short_height < long_height,
        "a rewrite must replace the document, not append to it: \
             {long_height} -> {short_height}"
    );
}

#[gpui::test]
fn the_parse_state_pool_stays_bounded(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
    });
    let (_, visual_cx) = cx.add_window_view(|_, _| PaneRoot);
    visual_cx.run_until_parked();

    let pane = visual_cx.update(|_, cx| cx.new(|_| ConversationPane::new()));
    let text: Arc<str> = Arc::from("body");
    pane.update(visual_cx, |pane, cx| {
        for index in 0..(MAX_MARKDOWN_PARSE_STATES * 3) {
            pane.markdown_generation = pane.markdown_generation.wrapping_add(1);
            let key: Arc<str> = Arc::from(format!("transcript-markdown:row-{index}:streaming"));
            pane.markdown_state(&key, &text, cx);
        }
        pane.evict_markdown_states();
    });

    let remaining = pane.read_with(visual_cx, |pane, _| pane.markdown_states.len());
    assert_eq!(remaining, MAX_MARKDOWN_PARSE_STATES);
}
