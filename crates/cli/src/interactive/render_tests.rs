use base64::Engine;

use super::*;
use crate::interactive::UiEvent;
use crate::interactive::transcript::TranscriptViewState;
use coding_agent::api::settings::CodingAgentThemeSnapshot;

#[test]
fn transcript_styles_fallback_when_no_theme() {
    let styles = TranscriptStyles::from_theme(None);
    // Without a resolved theme we fall back to the built-in palette
    // constants, so the transcript still renders with sensible defaults.
    assert_eq!(styles.user_text, USER);
    assert!(styles.thinking.italic);
    assert_eq!(styles.thinking.fg, Color::Yellow);
    assert_eq!(styles.error, ERROR);
    // Backgrounds collapse to default (no bg fill) in fallback mode.
    assert_eq!(styles.user_bg.bg, Color::Default);
    assert_eq!(styles.tool_pending_bg.bg, Color::Default);
}

#[test]
fn framed_modal_lines_wraps_content_in_a_full_width_border() {
    let framed = framed_modal_lines(
        vec!["a".to_string(), "longer content".to_string()],
        12,
        &Style::default(),
        false,
    );
    assert_eq!(framed.len(), 4);
    assert_eq!(framed[0], "┌──────────┐");
    assert_eq!(framed[1], "│ a        │");
    assert_eq!(framed[2], "│ longer co│");
    assert_eq!(framed[3], "└──────────┘");
}

#[test]
fn framed_modal_lines_falls_back_for_narrow_or_empty_input() {
    let lines = vec!["x".to_string()];
    assert_eq!(
        framed_modal_lines(lines.clone(), 4, &Style::default(), false),
        lines
    );
    assert!(framed_modal_lines(Vec::new(), 12, &Style::default(), false).is_empty());
}

#[test]
fn transcript_styles_resolve_from_dark_theme() {
    let resolved = CodingAgentThemeSnapshot::dark();
    let styles = TranscriptStyles::from_theme(Some(&resolved));

    // userMessageText -> "text" var -> #d4d4d4
    assert_eq!(styles.user_text.fg, Color::Rgb(0xd4, 0xd4, 0xd4));
    // userMessageBg -> #343541
    assert_eq!(styles.user_bg.bg, Color::Rgb(0x34, 0x35, 0x41));
    // thinkingText -> "gray" var -> #808080, italic preserved
    assert_eq!(styles.thinking.fg, Color::Rgb(0x80, 0x80, 0x80));
    assert!(styles.thinking.italic);
    // toolPendingBg -> #282832
    assert_eq!(styles.tool_pending_bg.bg, Color::Rgb(0x28, 0x28, 0x32));
    // toolSuccessBg -> #283228
    assert_eq!(styles.tool_success_bg.bg, Color::Rgb(0x28, 0x32, 0x28));
    // toolErrorBg -> #3c2828
    assert_eq!(styles.tool_error_bg.bg, Color::Rgb(0x3c, 0x28, 0x28));
    // toolTitle bold
    assert!(styles.tool_title.bold);
    // tool diffs + bash + warning tokens
    assert_eq!(styles.tool_diff_added.fg, Color::Rgb(0xb5, 0xbd, 0x68));
    assert_eq!(styles.tool_diff_removed.fg, Color::Rgb(0xcc, 0x66, 0x66));
    assert_eq!(styles.bash_mode.fg, Color::Rgb(0xb5, 0xbd, 0x68));
    assert!(styles.bash_mode.bold);
    assert_eq!(styles.warning.fg, Color::Rgb(0xff, 0xff, 0x00));
}

#[test]
fn transcript_roles_share_one_reserved_content_gutter() {
    fn content_column(lines: &[String], needle: &str) -> usize {
        let line = lines
            .iter()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle}: {lines:#?}"));
        let byte = line.find(needle).unwrap();
        visible_width(&line[..byte])
    }

    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::system("System notice"));
    transcript.push(TranscriptItem::assistant(
        "assistant-gutter",
        "Assistant content",
        true,
    ));
    transcript.push(TranscriptItem::Tool {
        call_id: "tool-gutter".into(),
        name: "read".into(),
        args: serde_json::json!({"path": "src/main.rs"}),
        result: Some("done".into()),
        is_error: false,
    });
    transcript.push(TranscriptItem::error("gutter failure"));
    let mut opts = test_opts(72, false);
    opts.selection_gutter = true;
    opts.selected_block = None;

    let lines = render_transcript_lines(&transcript, &opts);
    for needle in [
        "System notice",
        "Assistant content",
        "read src/main.rs",
        "Error:",
    ] {
        assert_eq!(content_column(&lines, needle), 2, "{needle}: {lines:#?}");
    }
    assert!(
        lines.iter().any(|line| line.starts_with("  System notice")),
        "non-selectable System rows must reserve the same gutter: {lines:#?}"
    );
}

#[test]
fn markdown_theme_uses_resolved_colors() {
    // Regression: `markdown_theme()` must derive its colors from the
    // ResolvedTheme (dark.json), not the tui palette (Ansi16/256 +
    // dim). Before the fix, assistant markdown bodies rendered with
    // `Ansi256(244)` + `dim` while user/tool blocks used vivid RGB from
    // the same dark.json — splitting the transcript into "dim text vs.
    // bright blocks". Now every md* token resolves through the theme,
    // so the whole transcript shares one palette.
    let resolved = CodingAgentThemeSnapshot::dark();
    let md = markdown_theme_from_resolved(&resolved);

    // mdHeading -> #f0c674 (not tui Cyan)
    assert_eq!(md.heading.fg, Color::Rgb(0xf0, 0xc6, 0x74));
    assert!(md.heading.bold);
    // mdCodeBlock -> green #b5bd68 (not Ansi256(244) + dim)
    assert_eq!(md.code_block.fg, Color::Rgb(0xb5, 0xbd, 0x68));
    assert!(!md.code_block.dim);
    // mdQuote -> gray #808080 (not Ansi256(244) + dim)
    assert_eq!(md.quote.fg, Color::Rgb(0x80, 0x80, 0x80));
    assert!(!md.quote.dim);
    // mdCode (inline) -> accent #8abeb7 (not Yellow)
    assert_eq!(md.code.fg, Color::Rgb(0x8a, 0xbe, 0xb7));
    // mdLink -> #81a2be (not Cyan)
    assert_eq!(md.link.fg, Color::Rgb(0x81, 0xa2, 0xbe));
    // mdHr -> gray #808080
    assert_eq!(md.hr.fg, Color::Rgb(0x80, 0x80, 0x80));
    // bold/italic/underline/strikethrough are attribute-only (fg=Default),
    // mirroring TS theme.bold/italic/underline (inherit surrounding fg).
    assert_eq!(md.bold.fg, Color::Default);
    assert!(md.bold.bold);
    assert_eq!(md.italic.fg, Color::Default);
    assert!(md.italic.italic);
    assert_eq!(md.underline.fg, Color::Default);
    assert!(md.underline.underline);
    assert_eq!(md.strikethrough.fg, Color::Default);
    assert!(md.strikethrough.strikethrough);
    // highlight_code is left for the caller to mount.
    assert!(md.highlight_code.is_none());
}

/// Build render options with no resolved theme (fallback palette) and
/// the given color flag, for layout-focused assertions.
fn test_opts(width: usize, color: bool) -> TranscriptRenderOptions<'static> {
    TranscriptRenderOptions {
        width,
        max_tool_result_lines: 3,
        color,
        markdown_theme: MarkdownTheme::default(),
        hide_thinking_block: false,
        hidden_thinking_label: "Thinking...",
        styles: TranscriptStyles::from_theme(None),
        view: None,
        selected_block: None,
        selection_gutter: false,
        show_images: true,
        image_width_cells: 60,
        terminal_capabilities: TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks: false,
        },
    }
}

fn png_base64(width: u32, height: u32) -> String {
    let mut png = vec![0_u8; 24];
    png[0..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    png[16..20].copy_from_slice(&width.to_be_bytes());
    png[20..24].copy_from_slice(&height.to_be_bytes());
    base64::engine::general_purpose::STANDARD.encode(png)
}

#[test]
fn user_message_renders_as_backgrounded_box_not_bare_prefix() {
    // Plan stage 1: user message is a backgrounded box (TS
    // UserMessageComponent), not a bare `user: <text>` prefix. The box
    // has top/bottom padding rows and left/right content padding.
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::user("hello"));

    let lines = render_transcript_lines(&transcript, &test_opts(20, false));
    // Top pad + content + bottom pad = 3 rows.
    assert_eq!(lines.len(), 3, "{lines:?}");
    // Content row carries the text with one-space left padding, no `user:`.
    assert!(
        !lines[1].contains("user:"),
        "bare prefix must go: {lines:?}"
    );
    assert!(lines[1].contains("hello"), "{lines:?}");
    // Every row is padded to the full width (background fill), and none
    // overflow it.
    for line in &lines {
        assert_eq!(visible_width(line), 20, "row must fill width: {lines:?}");
    }
}

#[test]
fn user_message_background_fills_full_width_with_color() {
    // Regression: with color enabled and a real theme, the user-message
    // background must cover the full row width — including the trailing
    // padding after the content. The content's own foreground reset
    // (\x1b[0m) must not bleed into a full reset that drops the
    // background for the rest of the row (TS theme.bg uses \x1b[49m,
    // a background-only reset, so nesting stays clean).
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::user("hi"));

    let resolved = CodingAgentThemeSnapshot::dark();
    let styles = TranscriptStyles::from_theme(Some(&resolved));
    let opts = TranscriptRenderOptions {
        width: 30,
        max_tool_result_lines: 3,
        color: true,
        markdown_theme: MarkdownTheme::default(),
        hide_thinking_block: false,
        hidden_thinking_label: "Thinking...",
        styles,
        view: None,
        selected_block: None,
        selection_gutter: false,
        show_images: true,
        image_width_cells: 60,
        terminal_capabilities: TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks: false,
        },
    };
    let lines = render_transcript_lines(&transcript, &opts);

    // Every row must carry the userMessageBg background escape and end
    // with a reset, so the background spans the whole width.
    for (i, line) in lines.iter().enumerate() {
        assert!(
            line.starts_with("\x1b[48;2;52;53;65m"),
            "row {i} missing bg open: {line:?}"
        );
        assert!(
            line.ends_with("\x1b[0m"),
            "row {i} missing bg close: {line:?}"
        );
        assert_eq!(visible_width(line), 30, "row {i} not full width: {line:?}");
    }

    // The content row's trailing padding must stay inside the
    // background span: the content's inner reset must be a
    // foreground-only reset (\x1b[39m), NOT a full reset (\x1b[0m),
    // so the background opened at the start of the row covers the
    // trailing spaces all the way to the row's final reset.
    let content = &lines[1];
    let hi_pos = content.find("hi").expect("content present");
    let after_hi = &content[hi_pos + 2..];
    assert!(
        after_hi.starts_with("\x1b[39m"),
        "content reset should be foreground-only (\\x1b[39m), got: {content:?}"
    );
    // No full reset appears before the final row reset, so the bg span is
    // unbroken across the trailing padding.
    let inner = &content[..content.len() - "\x1b[0m".len()];
    assert_eq!(
        inner.matches("\x1b[0m").count(),
        0,
        "inner full reset would break the bg span: {content:?}"
    );
    assert_eq!(
        inner.matches("\x1b[48;2;52;53;65m").count(),
        1,
        "bg should open exactly once: {content:?}"
    );
}

#[test]
fn visible_thinking_block_has_label_and_indented_content() {
    // Plan stage 1: thinking uses a `thinking` label and indented content
    // in thinkingText, distinguishing it from the assistant body.
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Assistant {
        id: "a".to_string(),
        markdown: "the answer".to_string(),
        thinking: "need to check".to_string(),
        thinking_seconds: None,
        done: true,
    });

    let lines = render_transcript_lines(&transcript, &test_opts(40, false));
    let joined = lines.join("\n");
    assert!(joined.contains("thinking"), "label missing: {joined}");
    assert!(
        joined.contains("  need to check"),
        "content not indented: {joined}"
    );
    // Body follows, separated by a blank line.
    assert!(joined.contains("the answer"), "body missing: {joined}");
    assert!(
        joined.contains("\n\n"),
        "no blank between thinking and body: {joined}"
    );
}

#[test]
fn hidden_thinking_block_shows_static_label_instead_of_vanishing() {
    // Plan stage 1: when thinking is hidden, show `Thinking...` rather
    // than dropping the block entirely.
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Assistant {
        id: "a".to_string(),
        markdown: String::new(),
        thinking: "secret reasoning".to_string(),
        thinking_seconds: None,
        done: true,
    });

    let mut opts = test_opts(40, false);
    opts.hide_thinking_block = true;
    let lines = render_transcript_lines(&transcript, &opts);
    let joined = lines.join("\n");
    assert!(
        joined.contains("Thinking..."),
        "hidden label missing: {joined}"
    );
    assert!(
        !joined.contains("secret reasoning"),
        "content leaked when hidden: {joined}"
    );
}

#[test]
fn long_thinking_lines_wrap_to_width_instead_of_truncating() {
    // Regression: thinking text must word-wrap at the available width
    // (width − 2 for the indent) rather than being truncated with
    // fit_line.  Before the fix, each source line was passed through
    // fit_line which *cuts* overflow without wrapping, so long thinking
    // content would just get clipped at the right edge.
    let long_thought = "this is a very long thinking line that absolutely must wrap to the available terminal width";
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Assistant {
        id: "a".to_string(),
        markdown: String::new(),
        thinking: long_thought.to_string(),
        thinking_seconds: None,
        done: true,
    });

    for (color, label) in [(false, "colorless"), (true, "colored")] {
        for width in [30, 20] {
            let lines = render_transcript_lines(&transcript, &test_opts(width, color));
            // First line is the "thinking" label.
            assert!(
                lines[0].contains("thinking"),
                "{label} w={width}: label missing"
            );
            let think_lines: Vec<_> = lines[1..].iter().filter(|l| !l.trim().is_empty()).collect();
            // At narrow widths, we should get at least 2 thinking lines
            // (the text wraps), not just 1 truncated line.
            assert!(
                think_lines.len() >= 2,
                "{label} w={width}: expected at least 2 wrapped thinking lines, got {}: {think_lines:?}",
                think_lines.len()
            );
            // Every word of the original must be present (no truncation loss).
            let joined = lines.join("\n");
            for word in long_thought.split_whitespace() {
                assert!(
                    joined.contains(word),
                    "{label} w={width}: word `{word}` lost: {joined}"
                );
            }
            // No line overflows width.
            for line in &lines {
                assert!(
                    visible_width(line) <= width,
                    "{label} w={width} overflow: {:?}",
                    line
                );
            }
        }
    }
}

#[test]
fn blocks_are_separated_by_one_blank_line() {
    // Plan stage 1 spacing policy: every visible block (user, assistant,
    // tool, error) is separated from the previous one by exactly one
    // blank line.
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::user("q"));
    transcript.push(TranscriptItem::assistant("a", "reply", true));

    let lines = render_transcript_lines(&transcript, &test_opts(40, false));
    // user box (3 rows) + blank + assistant body (1 row)
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert_eq!(lines[3], "", "expected blank separator: {lines:?}");
}

#[test]
fn no_line_overflows_render_width() {
    // Plan width contract: every rendered line must satisfy
    // visible_width(line) <= width, across color and narrow widths.
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::user(
        "a fairly long user prompt that needs wrapping",
    ));
    transcript.push(TranscriptItem::Assistant {
        id: "a".to_string(),
        markdown: "# Title\n\nsome *markdown* body with a lot of text in it".to_string(),
        thinking: "thinking line that is also somewhat long".to_string(),
        thinking_seconds: None,
        done: true,
    });
    transcript.push(TranscriptItem::Tool {
        call_id: "c".to_string(),
        name: "read".to_string(),
        args: serde_json::json!({"path": "src/very/deeply/nested/path/file.rs"}),
        result: Some("line content here\nand more".to_string()),
        is_error: false,
    });
    transcript.push(TranscriptItem::Tool {
        call_id: "c".to_string(),
        name: "grep".to_string(),
        args: serde_json::json!({
            "pattern": "someLongRegexPattern",
            "path": "src/very/deep/nested/dir",
            "glob": "*.rs",
            "limit": 100
        }),
        result: Some("src/lib.rs:1: match".to_string()),
        is_error: false,
    });
    transcript.push(TranscriptItem::Tool {
        call_id: "c".to_string(),
        name: "find".to_string(),
        args: serde_json::json!({
            "pattern": "**/*.rs",
            "path": "crates/very/deeply/nested",
            "limit": 1000
        }),
        result: Some("crates/lib.rs".to_string()),
        is_error: false,
    });
    transcript.push(TranscriptItem::Tool {
        call_id: "c".to_string(),
        name: "ls".to_string(),
        args: serde_json::json!({"path": "src/very/deeply/nested/path"}),
        result: Some("file.rs".to_string()),
        is_error: false,
    });

    for (color, label) in [(false, "colorless"), (true, "colored")] {
        for width in [40, 20] {
            let lines = render_transcript_lines(&transcript, &test_opts(width, color));
            for line in &lines {
                assert!(
                    visible_width(line) <= width,
                    "{label} width={width} overflow: {:?}",
                    line
                );
            }
        }
    }
}

#[test]
fn read_header_shows_path_and_line_range() {
    // Plan stage 3 read parity: header is `read <path>:<range>` (no
    // `tool` prefix), with the line range in the warning color.
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Tool {
        call_id: "c".to_string(),
        name: "read".to_string(),
        args: serde_json::json!({"path": "src/lib.rs", "offset": 10, "limit": 5}),
        result: Some("body".to_string()),
        is_error: false,
    });
    let lines = render_transcript_lines(&transcript, &test_opts(60, false));
    assert!(
        lines[0]
            .trim()
            .starts_with("read src/lib.rs:10-14 completed"),
        "{}",
        lines[0]
    );
}

#[test]
fn bash_header_uses_dollar_prefix_and_running_hint() {
    // Plan stage 3 bash parity: header is `$ <command>`; while pending
    // show `Running...`.
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Tool {
        call_id: "c".to_string(),
        name: "bash".to_string(),
        args: serde_json::json!({"command": "cargo test"}),
        result: None,
        is_error: false,
    });
    let lines = render_transcript_lines(&transcript, &test_opts(60, false));
    assert!(
        lines[0].trim().starts_with("$ cargo test running"),
        "{}",
        lines[0]
    );
    assert!(lines[1].trim().starts_with("Running..."), "{}", lines[1]);
}

#[test]
fn bash_result_shows_tail_preview_not_head() {
    // Plan stage 3 bash parity: collapsed view shows the *last* N lines
    // (tail), not the first N, so the most recent output stays visible.
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Tool {
        call_id: "c".to_string(),
        name: "bash".to_string(),
        args: serde_json::json!({"command": "echo"}),
        result: Some("l1\nl2\nl3\nl4\nl5\nl6".to_string()),
        is_error: false,
    });
    let lines = render_transcript_lines(&transcript, &test_opts(60, false));
    let body: Vec<String> = lines
        .iter()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    assert!(
        body.iter().any(|l| l.starts_with("l6")),
        "tail must include l6: {body:?}"
    );
    assert!(
        body.iter().any(|l| l.starts_with("l4")),
        "tail must include l4: {body:?}"
    );
    assert!(
        !body.iter().any(|l| l.starts_with("l1")),
        "head l1 should be hidden: {body:?}"
    );
    assert!(
        body.iter().any(|l| l.contains("3 more lines")),
        "omitted hint missing: {body:?}"
    );
}

#[test]
fn edit_block_self_renders_diff_with_semantic_colors() {
    // Plan stage 3 edit parity: edit self-renders (no tool bg), with
    // added/removed/context lines colored separately.
    let diff = "--- src/lib.rs\n+++ src/lib.rs\n@@ -1,2 +1,2 @@\n context\n-old\n+new";
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Tool {
        call_id: "c".to_string(),
        name: "edit".to_string(),
        args: serde_json::json!({"file_path": "src/lib.rs"}),
        result: Some(diff.to_string()),
        is_error: false,
    });

    let colored = render_transcript_lines(&transcript, &test_opts(60, true));
    let joined = colored.join("\n");
    // Header is `edit <path> done` with no `tool` prefix.
    assert!(joined.contains("src/lib.rs"), "path missing: {joined}");
    assert!(joined.contains("completed"), "status missing: {joined}");
    assert!(
        !joined.contains("tool edit"),
        "should not use generic prefix: {joined}"
    );
    // Added/removed lines carry their semantic color escapes (green/red).
    // toolDiffAdded = green = ANSI 2, toolDiffRemoved = red = ANSI 1.
    assert!(
        joined.contains("\x1b[32m"),
        "added line not green: {joined}"
    );
    assert!(
        joined.contains("\x1b[31m"),
        "removed line not red: {joined}"
    );
    // The `+new` / `-old` markers are preserved, with added/removed
    // content colored green/red respectively.
    assert!(
        joined.contains("\x1b[32mnew"),
        "added content not green: {joined}"
    );
    assert!(
        joined.contains("\x1b[31mold"),
        "removed content not red: {joined}"
    );
    assert!(
        joined.contains("+\x1b[32m"),
        "added marker missing: {joined}"
    );
    assert!(
        joined.contains("-\x1b[31m"),
        "removed marker missing: {joined}"
    );
}

#[test]
fn grep_header_shows_pattern_path_glob_and_limit() {
    // Plan stage 4 grep parity: header surfaces pattern (accent), path,
    // glob, and limit, mirroring TS formatGrepCall.
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Tool {
        call_id: "c".to_string(),
        name: "grep".to_string(),
        args: serde_json::json!({
            "pattern": "TODO",
            "path": "src",
            "glob": "*.rs",
            "limit": 50
        }),
        result: Some("src/lib.rs:1: TODO".to_string()),
        is_error: false,
    });
    let lines = render_transcript_lines(&transcript, &test_opts(80, false));
    let header = lines[0].trim();
    assert!(header.starts_with("grep"), "no grep prefix: {header}");
    assert!(header.contains("/TODO/"), "pattern missing: {header}");
    assert!(header.contains("in src"), "path missing: {header}");
    assert!(header.contains("(*.rs)"), "glob missing: {header}");
    assert!(header.contains("limit 50"), "limit missing: {header}");
    assert!(header.contains("completed"), "status missing: {header}");
}

#[test]
fn find_header_shows_pattern_path_and_limit() {
    // Plan stage 4 find parity: header surfaces pattern (accent), path,
    // and limit, mirroring TS formatFindCall.
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Tool {
        call_id: "c".to_string(),
        name: "find".to_string(),
        args: serde_json::json!({
            "pattern": "**/*.rs",
            "path": "crates",
            "limit": 100
        }),
        result: Some("crates/lib.rs".to_string()),
        is_error: false,
    });
    let lines = render_transcript_lines(&transcript, &test_opts(80, false));
    let header = lines[0].trim();
    assert!(header.starts_with("find"), "no find prefix: {header}");
    assert!(header.contains("**/*.rs"), "pattern missing: {header}");
    assert!(header.contains("in crates"), "path missing: {header}");
    assert!(header.contains("limit 100"), "limit missing: {header}");
}

#[test]
fn ls_header_shows_path_defaulting_to_dot() {
    // Plan stage 4 ls parity: header is `ls <path>`, defaulting to `.`
    // when no path is given, mirroring TS formatLsCall.
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Tool {
        call_id: "c".to_string(),
        name: "ls".to_string(),
        args: serde_json::json!({}),
        result: Some("file.rs".to_string()),
        is_error: false,
    });
    let lines = render_transcript_lines(&transcript, &test_opts(40, false));
    let header = lines[0].trim();
    assert!(header.starts_with("ls ."), "default path missing: {header}");

    let mut transcript2 = Transcript::new();
    transcript2.push(TranscriptItem::Tool {
        call_id: "c".to_string(),
        name: "ls".to_string(),
        args: serde_json::json!({"path": "src"}),
        result: Some("lib.rs".to_string()),
        is_error: false,
    });
    let lines2 = render_transcript_lines(&transcript2, &test_opts(40, false));
    let header2 = lines2[0].trim();
    assert!(
        header2.starts_with("ls src"),
        "explicit path missing: {header2}"
    );
}

#[test]
fn write_header_shows_path() {
    // Plan stage 4 write parity: header is `write <path>`.
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Tool {
        call_id: "c".to_string(),
        name: "write".to_string(),
        args: serde_json::json!({"path": "src/main.rs", "content": "fn main(){}"}),
        result: Some("Successfully wrote 12 bytes to src/main.rs".to_string()),
        is_error: false,
    });
    let lines = render_transcript_lines(&transcript, &test_opts(60, false));
    let header = lines[0].trim();
    assert!(
        header.starts_with("write src/main.rs completed"),
        "{}",
        header
    );
}

// ---- error message wrapping ----

#[test]
fn long_error_wraps_to_multiple_lines() {
    // A long single-line error must wrap to the transcript width instead
    // of being truncated to one line.
    let mut transcript = Transcript::new();
    let long_text = "summarization failed: complete failed: HTTP 400 unexpected provider response payload that is quite long indeed";
    transcript.push(TranscriptItem::error(long_text.to_string()));

    let lines = render_transcript_lines(&transcript, &test_opts(40, false));
    assert!(
        lines.len() > 1,
        "long error should wrap to multiple lines: {lines:?}"
    );
    // First line carries the Error: label.
    assert!(
        lines[0].starts_with("Error: "),
        "first line missing label: {:?}",
        lines[0]
    );
    // No rendered line overflows the width.
    for line in &lines {
        assert!(
            visible_width(line) <= 40,
            "line overflows width: {:?} ({})",
            line,
            visible_width(line)
        );
    }
    // Full text is recoverable: every word of the original appears across
    // the wrapped lines (no ANSI with color=false).
    let recovered = lines
        .iter()
        .map(|l| l.strip_prefix("Error: ").unwrap_or(l))
        .collect::<Vec<_>>()
        .join(" ");
    for word in long_text.split_whitespace() {
        assert!(
            recovered.contains(word),
            "missing word {word:?} in recovered text: {recovered:?}"
        );
    }
}

#[test]
fn multi_line_error_preserves_newlines_and_wraps_each_paragraph() {
    // Explicit newlines in the error are preserved as paragraph breaks,
    // and each paragraph wraps within the width.
    let mut transcript = Transcript::new();
    let text = "first paragraph that is long enough to wrap across several lines here\nsecond paragraph also long enough to wrap nicely within the width";
    transcript.push(TranscriptItem::error(text.to_string()));

    let lines = render_transcript_lines(&transcript, &test_opts(30, false));
    let all = lines.join("\n");
    for word in text.split_whitespace() {
        assert!(all.contains(word), "missing word {word:?}: {all:?}");
    }
    for line in &lines {
        assert!(
            visible_width(line) <= 30,
            "overflow: {:?} ({})",
            line,
            visible_width(line)
        );
    }
    assert!(lines.len() > 2, "both paragraphs should wrap: {lines:?}");
    // Only the very first rendered line carries the Error: label.
    assert!(lines[0].starts_with("Error: "));
    assert!(
        lines.iter().filter(|l| l.starts_with("Error: ")).count() == 1,
        "exactly one label expected: {lines:?}"
    );
}

#[test]
fn colored_error_keeps_style_on_all_wrapped_lines() {
    // With color enabled, the fallback ERROR style (bold red) must be
    // applied to every wrapped line, not just the first.
    let mut transcript = Transcript::new();
    let long_text = "summarization failed: complete failed: HTTP 400 unexpected provider response payload that is quite long";
    transcript.push(TranscriptItem::error(long_text.to_string()));

    let lines = render_transcript_lines(&transcript, &test_opts(40, true));
    assert!(lines.len() > 1, "should wrap: {lines:?}");
    for line in &lines {
        if !line.is_empty() {
            assert!(
                line.contains("\x1b[1;31m"),
                "error style missing on line: {line:?}"
            );
            assert!(line.contains("\x1b[0m"), "reset missing on line: {line:?}");
        }
        assert!(
            visible_width(line) <= 40,
            "overflow with color: {:?} ({})",
            line,
            visible_width(line)
        );
    }
}

#[test]
fn per_block_tool_preview_expands_only_the_selected_tool() {
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Tool {
        call_id: "call-1".into(),
        name: "bash".into(),
        args: serde_json::json!({"command": "printf lines"}),
        result: Some("one\ntwo\nthree\nfour\nfive".into()),
        is_error: false,
    });
    let mut view = TranscriptViewState::default();
    view.sync(&transcript);
    let mut opts = test_opts(50, false);
    opts.view = Some(view.snapshot());
    opts.selected_block = view.selected();
    opts.selection_gutter = true;

    let preview = render_transcript_lines(&transcript, &opts).join("\n");
    assert!(preview.contains("▌ $ printf lines completed"), "{preview}");
    assert!(preview.contains("three"), "{preview}");
    assert!(preview.contains("five"), "{preview}");
    assert!(
        !preview.lines().any(|line| line.trim() == "one"),
        "{preview}"
    );
    assert!(
        preview.contains("2 more lines (disclose block)"),
        "{preview}"
    );

    assert!(view.toggle_selected(&transcript));
    opts.view = Some(view.snapshot());
    let expanded = render_transcript_lines(&transcript, &opts).join("\n");
    for line in ["one", "two", "three", "four", "five"] {
        assert!(expanded.contains(line), "missing {line}: {expanded}");
    }
    assert!(!expanded.contains("more lines"), "{expanded}");
}

#[test]
fn web_search_renders_queries_and_omits_ws_call_id_markers() {
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Tool {
            call_id: "web-1".into(),
            name: "web_search".into(),
            args: serde_json::json!({
                "type": "web_search_call",
                "id": "web-1",
                "status": "completed"
            }),
            result: Some(
                r#"{"status":"completed","action":{"type":"search","queries":["DeepSeek API docs","ws_call_id=ignored"]}}"#
                    .into(),
            ),
            is_error: false,
        });
    let rendered = render_transcript_lines(&transcript, &test_opts(60, false)).join("\n");
    assert!(rendered.contains("search completed"), "{rendered}");
    assert!(rendered.contains("DeepSeek API docs"), "{rendered}");
    assert!(!rendered.contains("ws_call_id=ignored"), "{rendered}");
    assert!(!rendered.contains("\"action\""), "{rendered}");
}

#[test]
fn web_search_renders_opened_page_urls() {
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Tool {
            call_id: "web-2".into(),
            name: "web_search".into(),
            args: serde_json::json!({
                "type": "web_search_call",
                "id": "web-2",
                "status": "completed"
            }),
            result: Some(
                r#"{"status":"completed","action":{"type":"open_page","url":"https://example.com/docs#ws_call_id=xyz"}}"#
                    .into(),
            ),
            is_error: false,
        });
    let rendered = render_transcript_lines(&transcript, &test_opts(60, false)).join("\n");
    assert!(rendered.contains("open page completed"), "{rendered}");
    assert!(rendered.contains("https://example.com/docs"), "{rendered}");
    assert!(!rendered.contains("ws_call_id=xyz"), "{rendered}");
}

#[test]
fn web_search_legacy_summary_falls_back_to_raw_text() {
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Tool {
        call_id: "web-3".into(),
        name: "web_search".into(),
        args: serde_json::json!({"type": "web_search_call", "id": "web-3"}),
        result: Some("completed".into()),
        is_error: false,
    });
    let rendered = render_transcript_lines(&transcript, &test_opts(60, false)).join("\n");
    assert!(rendered.contains("search completed"), "{rendered}");
    assert!(rendered.contains("  completed"), "{rendered}");
}

#[test]
fn thinking_preview_caps_height_while_streaming() {
    let mut transcript = Transcript::new();
    let mut view = TranscriptViewState::default();
    let mut opts = test_opts(50, false);
    let mut heights = Vec::new();
    for _ in 0..8 {
        transcript.apply_event(UiEvent::ThinkingDelta {
            text: "line of thinking content\n".into(),
        });
        view.sync(&transcript);
        opts.view = Some(view.snapshot());
        heights.push(render_transcript_lines(&transcript, &opts).len());
    }
    assert!(
        heights.windows(2).all(|window| window[1] >= window[0]),
        "preview height must grow but never shrink while streaming: {heights:?}"
    );
    assert_eq!(heights.last().copied(), Some(5), "{heights:?}");
    assert!(
        heights[4..].windows(2).all(|window| window[0] == window[1]),
        "preview height must settle once the cap is reached: {heights:?}"
    );
}

#[test]
fn thinking_label_switches_to_thought_duration_once_sealed() {
    let mut transcript = Transcript::new();
    transcript.apply_event(UiEvent::ThinkingDelta {
        text: "think one\nthink two\n".into(),
    });
    transcript.apply_event(UiEvent::AssistantDelta {
        text: "answer".into(),
    });
    let mut view = TranscriptViewState::default();
    view.sync(&transcript);
    let mut opts = test_opts(50, false);
    opts.view = Some(view.snapshot());

    let rendered = render_transcript_lines(&transcript, &opts).join("\n");
    assert!(rendered.contains("Thought for"), "{rendered}");
    assert!(!rendered.contains("thinking · preview"), "{rendered}");
    assert!(rendered.contains("think two"), "{rendered}");
}

#[test]
fn assistant_disclosure_changes_thinking_without_hiding_answer() {
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Assistant {
        id: "assistant-1".into(),
        markdown: "final answer".into(),
        thinking: "think one\nthink two\nthink three\nthink four\nthink five".into(),
        thinking_seconds: None,
        done: true,
    });
    let mut view = TranscriptViewState::default();
    view.sync(&transcript);
    let mut opts = test_opts(50, false);
    opts.selected_block = view.selected();
    opts.selection_gutter = true;
    opts.view = Some(view.snapshot());

    let preview = render_transcript_lines(&transcript, &opts).join("\n");
    assert!(preview.contains("▌ thinking · preview"), "{preview}");
    assert!(!preview.contains("think one"), "{preview}");
    assert!(preview.contains("think five"), "{preview}");
    assert!(preview.contains("final answer"), "{preview}");

    assert!(view.toggle_selected(&transcript));
    opts.view = Some(view.snapshot());
    let expanded = render_transcript_lines(&transcript, &opts).join("\n");
    assert!(expanded.contains("think one"), "{expanded}");
    assert!(expanded.contains("final answer"), "{expanded}");

    assert!(view.toggle_selected(&transcript));
    opts.view = Some(view.snapshot());
    let collapsed = render_transcript_lines(&transcript, &opts).join("\n");
    assert!(collapsed.contains("5 lines hidden"), "{collapsed}");
    assert!(!collapsed.contains("think five"), "{collapsed}");
    assert!(collapsed.contains("final answer"), "{collapsed}");
}

#[test]
fn delegation_preview_identifies_target_task_and_final_summary() {
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Tool {
        call_id: "delegate-1".into(),
        name: "delegation".into(),
        args: serde_json::json!({
            "targetKind": "agent",
            "targetId": "review",
            "task": "review the parser",
            "status": "completed"
        }),
        result: Some("No blocking issues.\nOne follow-up suggestion.".into()),
        is_error: false,
    });
    let mut view = TranscriptViewState::default();
    view.sync(&transcript);
    let mut opts = test_opts(70, false);
    opts.view = Some(view.snapshot());
    opts.selected_block = view.selected();
    opts.selection_gutter = true;

    let preview = render_transcript_lines(&transcript, &opts).join("\n");
    assert!(
        preview.contains("▌ delegate agent review completed"),
        "{preview}"
    );
    assert!(preview.contains("task: review the parser"), "{preview}");
    assert!(preview.contains("No blocking issues."), "{preview}");

    assert!(view.toggle_selected(&transcript));
    assert!(view.toggle_selected(&transcript));
    opts.view = Some(view.snapshot());
    let collapsed = render_transcript_lines(&transcript, &opts).join("\n");
    assert!(collapsed.contains("delegate agent review completed"));
    assert!(!collapsed.contains("review the parser"), "{collapsed}");
    assert!(!collapsed.contains("No blocking issues."), "{collapsed}");
}

#[test]
fn generic_tool_arguments_default_to_bounded_preview() {
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Tool {
        call_id: "plugin-1".into(),
        name: "plugin.inspect".into(),
        args: serde_json::json!({
            "alpha": 1,
            "beta": 2,
            "gamma": 3,
            "delta": 4
        }),
        result: Some("done".into()),
        is_error: false,
    });
    let mut view = TranscriptViewState::default();
    view.sync(&transcript);
    let mut opts = test_opts(60, false);
    opts.view = Some(view.snapshot());
    opts.selected_block = view.selected();
    opts.selection_gutter = true;

    let preview = render_transcript_lines(&transcript, &opts).join("\n");
    assert!(preview.contains("arguments · preview"), "{preview}");
    assert!(preview.contains("\"alpha\": 1"), "{preview}");
    assert!(preview.contains("more argument lines"), "{preview}");

    assert!(view.toggle_selected_arguments(&transcript));
    opts.view = Some(view.snapshot());
    let expanded = render_transcript_lines(&transcript, &opts).join("\n");
    assert!(expanded.contains("arguments · expanded"), "{expanded}");
    assert!(expanded.contains("\"delta\": 4"), "{expanded}");
    assert!(!expanded.contains("more argument lines"), "{expanded}");
}

#[test]
fn transcript_image_honors_visibility_capability_and_width_settings() {
    let mut transcript = Transcript::new();
    transcript.push(TranscriptItem::Image {
        mime_type: "image/png".into(),
        data: png_base64(18, 18),
    });
    let mut opts = test_opts(80, false);
    opts.image_width_cells = 10;
    opts.terminal_capabilities = TerminalCapabilities {
        images: Some(tui::api::terminal::ImageProtocol::Kitty),
        true_color: true,
        hyperlinks: true,
    };

    let rendered = render_transcript_lines(&transcript, &opts);
    assert_eq!(rendered.len(), 5, "{rendered:?}");
    assert!(rendered[0].starts_with("\x1b_G"), "{rendered:?}");
    assert!(rendered[0].contains("c=10,r=5,i="), "{rendered:?}");
    assert!(rendered[1..].iter().all(String::is_empty));

    opts.show_images = false;
    let hidden = render_transcript_lines(&transcript, &opts);
    assert_eq!(hidden, ["[Image: image/png]"]);
    assert!(!hidden[0].contains("\x1b_G"));

    opts.show_images = true;
    opts.terminal_capabilities.images = None;
    let fallback = render_transcript_lines(&transcript, &opts);
    assert_eq!(fallback.len(), 1);
    assert!(fallback[0].contains("image/png"), "{fallback:?}");
    assert!(!fallback[0].contains("\x1b_G"));
}
