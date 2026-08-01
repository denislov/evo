//! Markdown rendering behavior.

use tui::api::component::{Component, DefaultTextStyle, Markdown};
use tui::api::render::{Color, Style, color_enabled, paint_with, visible_width};
use tui::api::theme::MarkdownTheme;

fn bold() -> Style {
    Style::fg(Color::Default).bold()
}

fn reverse() -> Style {
    Style::default().reverse()
}

fn dim() -> Style {
    Style::fg(Color::Default).dim()
}

#[test]
fn markdown_renders_common_blocks() {
    let mut markdown = Markdown::new("# Title\n\n- one\n- two\n\n```rust\nfn main() {}\n```");
    let lines = markdown.render(40);
    let joined = lines.join("\n");
    assert!(joined.contains("Title"));
    assert!(joined.contains("one"));
    assert!(joined.contains("fn main() {}"));
}

#[test]
fn markdown_lines_do_not_exceed_width() {
    let mut markdown =
        Markdown::new("A long paragraph with **bold** text and `inline code` that must wrap.");
    for line in markdown.render(18) {
        assert!(
            visible_width(&line) <= 18,
            "line exceeded width: {:?}",
            line
        );
    }
}

#[test]
fn markdown_heading_is_bold_when_color_enabled() {
    let mut markdown = Markdown::new("# Title");
    let lines = markdown.render(40);
    let expected = paint_with("Title", &bold(), color_enabled());
    assert_eq!(lines, vec![expected]);
}

#[test]
fn markdown_inline_code_is_reverse_when_color_enabled() {
    let mut markdown = Markdown::new("see `code` here");
    let lines = markdown.render(40);
    let joined = lines.join("\n");
    assert!(
        joined.contains(&paint_with("code", &reverse(), color_enabled())),
        "expected reverse-styled inline code in: {joined:?}"
    );
}

#[test]
fn markdown_strong_text_is_bold_when_color_enabled() {
    let mut markdown = Markdown::new("A **bold** word");
    let lines = markdown.render(40);
    let joined = lines.join("\n");

    assert!(
        joined.contains(&paint_with("bold", &bold(), color_enabled())),
        "expected bold-styled strong text in: {joined:?}"
    );
}

#[test]
fn markdown_link_uses_osc8_when_hyperlinks_are_enabled() {
    let mut markdown = Markdown::new("Open [docs](https://example.com/docs).");
    markdown.set_hyperlinks_enabled(true);

    let joined = markdown.render(80).join("\n");

    assert!(
        joined.contains("\x1b]8;;https://example.com/docs\x1b\\"),
        "expected OSC 8 opener in: {joined:?}"
    );
    assert!(
        joined.contains("\x1b]8;;\x1b\\"),
        "expected OSC 8 closer after link text in: {joined:?}"
    );
    assert_eq!(visible_width(&joined), "Open docs.".len());
}

#[test]
fn markdown_link_falls_back_to_url_when_hyperlinks_are_disabled() {
    let mut markdown = Markdown::new("Open [docs](https://example.com/docs).");
    markdown.set_hyperlinks_enabled(false);

    let joined = markdown.render(80).join("\n");

    assert!(joined.contains("docs"));
    assert!(joined.contains("(https://example.com/docs)"));
    assert!(!joined.contains("\x1b]8;;"));
}

#[test]
fn markdown_blockquote_is_dim_when_color_enabled() {
    let mut markdown = Markdown::new("> quoted text");
    let lines = markdown.render(40);
    let joined = lines.join("\n");
    assert!(
        joined.contains(&paint_with("> quoted text", &dim(), color_enabled())),
        "expected dim-styled blockquote in: {joined:?}"
    );
}

#[test]
fn markdown_rule_is_dim_when_color_enabled() {
    let mut markdown = Markdown::new("---");
    let lines = markdown.render(40);
    let joined = lines.join("\n");
    let dim_rule = paint_with(&"-".repeat(20), &dim(), color_enabled());
    assert!(
        joined.contains(&dim_rule),
        "expected dim-styled rule in: {joined:?}"
    );
}

#[test]
fn markdown_preserves_inline_punctuation_spacing() {
    let mut markdown = Markdown::new("A paragraph with **bold** text and `code`.");
    let lines = markdown.render(80);
    let joined = lines.join("\n");
    // The visible text (ignoring ANSI) must still read correctly.
    assert!(joined.contains("A paragraph with "));
    assert!(joined.contains("bold"));
    assert!(joined.contains(" text and"));
    assert!(joined.contains(&paint_with("code", &reverse(), color_enabled())));
}

#[test]
fn markdown_code_block_has_dim_fence_rows_and_dim_content() {
    let mut markdown = Markdown::new("```rust\nfn main() {}\n```");
    let lines = markdown.render(40);
    let joined = lines.join("\n");

    let dim_fence = paint_with("```", &dim(), color_enabled());
    let dim_content = paint_with("   fn main() {}", &dim(), color_enabled());

    assert!(
        joined.contains(&dim_fence),
        "expected dim fence row in: {joined:?}"
    );
    assert!(
        joined.contains(&dim_content),
        "expected dim indented content in: {joined:?}"
    );
    // Two fence rows (open + close).
    assert_eq!(
        joined.matches(&dim_fence).count(),
        2,
        "expected two fence rows in: {joined:?}"
    );
}

#[test]
fn markdown_code_block_multiline_content_each_line_indented_and_dim() {
    let mut markdown = Markdown::new("```\nlet a = 1;\nlet b = 2;\n```");
    let lines = markdown.render(40);
    let joined = lines.join("\n");
    assert!(
        joined.contains(&paint_with("   let a = 1;", &dim(), color_enabled())),
        "expected dim indented first line in: {joined:?}"
    );
    assert!(
        joined.contains(&paint_with("   let b = 2;", &dim(), color_enabled())),
        "expected dim indented second line in: {joined:?}"
    );
}

#[test]
fn renders_emphasis_with_italic_style() {
    let style = Style::fg(Color::Cyan).italic();
    let theme = MarkdownTheme {
        italic: style,
        ..MarkdownTheme::default()
    };
    let mut md = Markdown::new("*italic*").with_theme(theme);
    let rendered = md.render(40).join("\n");
    assert!(rendered.contains("italic"), "{rendered}");
    // Default strong handler: the text should be painted with italic style.
    let expected = paint_with("italic", &style, color_enabled());
    assert!(rendered.contains(&expected), "{rendered}");
}

#[test]
fn renders_strikethrough_with_strikethrough_style() {
    let style = Style::fg(Color::Magenta).strikethrough();
    let theme = MarkdownTheme {
        strikethrough: style,
        ..MarkdownTheme::default()
    };
    let mut md = Markdown::new("~~deleted~~").with_theme(theme);
    let rendered = md.render(40).join("\n");
    assert!(rendered.contains("deleted"), "{rendered}");
    let expected = paint_with("deleted", &style, color_enabled());
    assert!(rendered.contains(&expected), "{rendered}");
}

// ── Table rendering ─────────────────────────────────────────────────┈

#[test]
fn renders_simple_table() {
    let mut md = Markdown::new("| Name | Age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 25 |");
    let lines = md.render(80);
    let joined = lines.join("\n");

    // Basic content
    assert!(joined.contains("Name"), "{joined}");
    assert!(joined.contains("Age"), "{joined}");
    assert!(joined.contains("Alice"), "{joined}");
    assert!(joined.contains("Bob"), "{joined}");

    // Box-drawing borders
    assert!(joined.contains("┌"), "{joined}");
    assert!(joined.contains("┐"), "{joined}");
    assert!(joined.contains("└"), "{joined}");
    assert!(joined.contains("┘"), "{joined}");
    assert!(joined.contains("│"), "{joined}");
    assert!(
        joined.contains("├"),
        "header/body separator missing: {joined}"
    );
    assert!(
        joined.contains("┤"),
        "header/body separator missing: {joined}"
    );
    assert!(
        joined.contains("┼"),
        "header/body separator missing: {joined}"
    );
}

#[test]
fn renders_table_with_inline_formatting() {
    let mut md = Markdown::new("| **Name** | `Age` |\n| --- | --- |\n| Alice | 30 |");
    let lines = md.render(80);
    let joined = lines.join("\n");

    assert!(joined.contains("Name"), "bold text should render: {joined}");
    assert!(joined.contains("Age"), "code text should render: {joined}");

    // Box-drawing structure
    assert!(joined.contains("│"), "{joined}");
    assert!(joined.contains("─"), "{joined}");
}

#[test]
fn renders_table_with_cell_wrapping() {
    // Narrow width forces cell content to wrap
    let mut md =
        Markdown::new("| Header |\n| --- |\n| This is a long cell content that must wrap |");
    let lines = md.render(30);
    let joined = lines.join("\n");

    // Content should be preserved (wrapped across lines)
    assert!(
        joined.contains("long"),
        "content should be visible: {joined}"
    );
    assert!(
        joined.contains("wrap"),
        "content should be visible: {joined}"
    );

    // Each line should fit within width
    for line in &lines {
        assert!(
            visible_width(line) <= 30,
            "line exceeds width 30: {:?}",
            line
        );
    }
}

#[test]
fn renders_table_as_part_of_document() {
    let mut md =
        Markdown::new("# Report\n\nHere is data:\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n\nDone.");
    let lines = md.render(80);
    let joined = lines.join("\n");

    assert!(joined.contains("Report"), "heading: {joined}");
    assert!(joined.contains("Here is data"), "paragraph: {joined}");
    assert!(joined.contains("Done"), "text after table: {joined}");
    assert!(joined.contains("┌"), "table top border: {joined}");
    assert!(joined.contains("│"), "table cell border: {joined}");
    assert!(joined.contains("└"), "table bottom border: {joined}");
}

#[test]
fn table_rows_separated_by_dividers() {
    let mut md = Markdown::new("| A | B |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |");
    let lines = md.render(80);
    let joined = lines.join("\n");
    // Two data rows should have a row separator between them
    assert!(
        joined.contains("├"),
        "should have header/body separator: {joined}"
    );
    // Count the ┼ character — it should appear in the inter-row separator
    let separator_count = joined.matches("┼").count();
    assert_eq!(
        separator_count, 2,
        "expected 2 separators (one for header/body, one between rows), got {separator_count}: {joined}"
    );
}

#[test]
fn table_geometry_is_atomic_aligned_unicode_safe_and_has_a_narrow_fallback() {
    fn boundaries(line: &str) -> Vec<usize> {
        line.char_indices()
            .filter(|(_, ch)| "┌┬┐│├┼┤└┴┘".contains(*ch))
            .map(|(byte, _)| visible_width(&line[..byte]))
            .collect()
    }

    let source = "| Left | 中🙂 | Right |\n| :--- | :---: | ---: |\n| alpha beta | e\u{301}\t中 | 7 |\n| longer-value | 🙂 | 123 |";
    let mut md = Markdown::new(source);
    let lines = md.render(34);
    let plain = lines
        .iter()
        .map(|line| strip_ansi_line(line))
        .collect::<Vec<_>>();
    let table_lines = plain
        .iter()
        .filter(|line| line.chars().any(|ch| "┌│├└".contains(ch)))
        .collect::<Vec<_>>();
    assert!(!table_lines.is_empty(), "{plain:#?}");
    let expected_boundaries = boundaries(table_lines[0]);
    for line in table_lines {
        assert_eq!(boundaries(line), expected_boundaries, "{plain:#?}");
        assert!(visible_width(line) <= 34, "table overflow: {line:?}");
    }
    assert!(plain.iter().all(|line| !line.contains('\t')), "{plain:#?}");
    assert!(
        plain
            .iter()
            .any(|line| line.contains(" 7 ") || line.contains("  7 ")),
        "right alignment should add leading padding: {plain:#?}"
    );

    let mut narrow = Markdown::new("| A | B | C |\n| --- | --- | --- |\n| one | two | three |");
    let narrow_lines = narrow.render(8);
    assert!(
        narrow_lines.iter().all(|line| visible_width(line) <= 8),
        "narrow fallback overflow: {narrow_lines:#?}"
    );
    let narrow_text = narrow_lines.join("\n");
    assert!(narrow_text.contains("one"), "{narrow_text}");
    assert!(narrow_text.contains("three"), "{narrow_text}");
    assert!(
        !narrow_text.contains('┬'),
        "fallback should not clip a box: {narrow_text}"
    );
}

#[test]
fn default_text_style_applies_fg_color_to_paragraph() {
    let style = DefaultTextStyle {
        fg: Some(Color::Ansi256(244)), // gray
        ..DefaultTextStyle::default()
    };
    let mut md = Markdown::new("Hello world").with_default_style(Some(style));
    let lines = md.render(80);
    let joined = lines.join("\n");
    // The ANSI prefix for gray (244 in 256-color) should be emitted
    assert!(joined.contains("Hello world"), "{joined}");
    if color_enabled() {
        assert!(
            joined.contains("\x1b[38;5;244m"),
            "should have gray color: {joined}"
        );
    } else {
        assert!(
            !joined.contains("\x1b["),
            "color-disabled output should be plain: {joined:?}"
        );
    }
}

#[test]
fn default_text_style_combines_italic_and_color() {
    // Simulate "thinking traces" — gray + italic
    let style = DefaultTextStyle {
        fg: Some(Color::Ansi256(244)),
        italic: true,
        ..DefaultTextStyle::default()
    };
    let mut md = Markdown::new("thinking text").with_default_style(Some(style));
    let lines = md.render(80);
    let joined = lines.join("\n");
    assert!(joined.contains("thinking text"), "{joined}");
    // The style params are merged into one ANSI sequence: \x1b[3;38;5;244m
    if color_enabled() {
        assert!(
            joined.contains("\x1b[3;38;5;244m"),
            "should have italic+gray: {joined}"
        );
    } else {
        assert!(
            !joined.contains("\x1b["),
            "color-disabled output should be plain: {joined:?}"
        );
    }
}

#[test]
fn default_style_inline_code_still_uses_theme_colors() {
    let style = DefaultTextStyle {
        fg: Some(Color::Ansi256(244)),
        italic: true,
        ..DefaultTextStyle::default()
    };
    let mut md = Markdown::new("text with `code` inside").with_default_style(Some(style));
    let lines = md.render(80);
    let joined = lines.join("\n");
    assert!(joined.contains("text with "), "{joined}");
    assert!(joined.contains("code"), "{joined}");
    assert!(joined.contains("inside"), "{joined}");
    // The code should have its own theme styling (default theme.code = reverse / \x1b[7m)
    if color_enabled() {
        assert!(
            joined.contains("\x1b[7m"),
            "code should have reverse: {joined}"
        );
        // Afterwards the default style should be restored (italic+gray reapplied after \x1b[0m)
        let after_code_idx = joined.find("inside").unwrap();
        let before_inside = &joined[..after_code_idx];
        assert!(
            before_inside.contains("\x1b[3;38;5;244m"),
            "should restore default style after code: {before_inside}"
        );
    } else {
        assert!(
            !joined.contains("\x1b["),
            "color-disabled output should be plain: {joined:?}"
        );
    }
}

#[test]
fn default_style_does_not_leak_into_headings() {
    let style = DefaultTextStyle {
        fg: Some(Color::Ansi256(244)),
        ..DefaultTextStyle::default()
    };
    let mut md = Markdown::new("# Title\n\nParagraph").with_default_style(Some(style));
    let lines = md.render(80);
    let joined = lines.join("\n");
    assert!(joined.contains("Title"), "{joined}");
    assert!(joined.contains("Paragraph"), "{joined}");
    // The heading line should NOT have the default gray
    let heading_line = lines.iter().find(|l| l.contains("Title")).unwrap();
    if color_enabled() {
        assert!(
            !heading_line.contains("\x1b[38;5;244m"),
            "heading should NOT have default gray: {heading_line}"
        );
        // The heading should still have bold (from theme.heading)
        assert!(
            heading_line.contains("\x1b[1m"),
            "heading should be bold: {heading_line}"
        );
    } else {
        assert!(
            !heading_line.contains("\x1b["),
            "color-disabled heading should be plain: {heading_line:?}"
        );
    }
}

#[test]
fn default_style_does_not_leak_into_blockquotes() {
    let style = DefaultTextStyle {
        fg: Some(Color::Ansi256(244)),
        italic: true,
        ..DefaultTextStyle::default()
    };
    let mut md = Markdown::new("> quoted\n\nnormal").with_default_style(Some(style));
    let lines = md.render(80);
    let joined = lines.join("\n");
    assert!(joined.contains("quoted"), "{joined}");
    assert!(joined.contains("normal"), "{joined}");
    // Blockquotes use theme.quote (dim) — the default style's color should NOT
    // appear inside the blockquote content.
    // The quote line should use dim (\x1b[2m) rather than default gray
    if color_enabled() {
        assert!(
            joined.contains("\x1b[2m"),
            "quote should use dim style: {joined}"
        );
        assert!(
            !joined.contains("\x1b[38;5;244m"),
            "quote should NOT have default gray: {joined}"
        );
    } else {
        assert!(
            !joined.contains("\x1b["),
            "color-disabled output should be plain: {joined:?}"
        );
    }
}

#[test]
fn default_text_bg_fills_full_line_width() {
    let style = DefaultTextStyle {
        bg: Some(Color::Blue),
        ..DefaultTextStyle::default()
    };
    let mut md = Markdown::new("short").with_default_style(Some(style));
    let lines = md.render(20);
    let joined = lines.join("\n");
    // The line should have blue background padding to width 20
    // paint_with adds \x1b[44m..\x1b[0m around the padded line
    if color_enabled() {
        assert!(joined.contains("\x1b[44m"), "should have blue bg: {joined}");
    } else {
        assert!(
            !joined.contains("\x1b["),
            "color-disabled output should be plain: {joined:?}"
        );
    }
    // Visible width of each line should be exactly 20
    for line in &lines {
        assert_eq!(
            visible_width(line),
            20,
            "line should fill full width: {:?}",
            line
        );
    }
}

// ── Streaming partial closing fence stabilization ────────────────────

#[test]
fn partial_closing_fence_variants_are_stabilized() {
    for (source, content, partial, expected_lines) in [
        ("```ts\nconst x = 1;\n`", "const x = 1;", "\n   `", None),
        ("```ts\nconst x = 1;\n``", "const x = 1;", "\n   ``", None),
        ("~~~\ncontent\n~~", "content", "\n   ~~", Some(3)),
    ] {
        let mut markdown = Markdown::new(source);
        let lines = markdown.render(80);
        let joined = lines.join("\n");
        assert!(
            joined.contains(content),
            "source: {source:?}, output: {joined}"
        );
        assert!(
            !joined.contains(partial),
            "partial fence leaked for {source:?}: {joined}"
        );
        if let Some(expected_lines) = expected_lines {
            assert_eq!(lines.len(), expected_lines, "source: {source:?}");
        }
    }
}

#[test]
fn full_closing_fence_is_not_stripped() {
    let mut md = Markdown::new("```ts\nconst x = 1;\n```");
    let lines = md.render(80);
    let joined = lines.join("\n");
    assert!(
        joined.contains("const x = 1;"),
        "should keep code content: {joined}"
    );
    // Two fence rows: opening ```ts and closing ```
    assert_eq!(
        joined.matches("```").count(),
        2,
        "should have two fence rows: {joined}"
    );
}

#[test]
fn partial_fence_multiline_content_preserves_inner_fence_like_text() {
    let mut md = Markdown::new("```md\nnot a closing fence:\n``\n```");
    let lines = md.render(80);
    let joined = lines.join("\n");
    assert!(
        joined.contains("not a closing fence:"),
        "should keep first content line: {joined}"
    );
    assert!(
        joined.contains("``"),
        "should keep the `` code line (not a fence): {joined}"
    );
}

#[test]
fn partial_fence_only_line_is_stripped() {
    let mut md = Markdown::new("```ts\n``");
    let lines = md.render(80);
    // The code block should have no visible content between the fence rows
    // The partial fence `` should be removed, leaving the block empty.
    // Render should produce exactly 3 lines: fence, empty content, fence
    // and the "content" line should be blank (just whitespace/ANSI).
    assert_eq!(
        lines.len(),
        3,
        "expected 3 lines (fence, empty, fence), got {}: {:?}",
        lines.len(),
        lines
    );
    // The middle line should be visually empty (ANSI codes for dim are OK)
    // Remove ANSI escape sequences: strip everything from ESC to 'm'
    let ansi_stripped: String = {
        let mut result = String::new();
        let mut in_escape = false;
        for c in lines[1].chars() {
            match c {
                '\x1b' => in_escape = true,
                'm' if in_escape => in_escape = false,
                _ if !in_escape => result.push(c),
                _ => {}
            }
        }
        result
    };
    assert!(
        ansi_stripped.trim().is_empty(),
        "middle line should be empty, got: {:?}",
        ansi_stripped
    );
}

fn strip_ansi(s: &str) -> String {
    let mut result = String::new();
    let mut in_escape = false;
    for c in s.chars() {
        match c {
            '\x1b' => in_escape = true,
            'm' if in_escape => in_escape = false,
            _ if !in_escape => result.push(c),
            _ => {}
        }
    }
    result
}

fn plain_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| strip_ansi(line).trim_end().to_string())
        .collect()
}

#[test]
fn block_classes_have_one_blank_line_before_following_paragraph() {
    for (source, trailing_text) in [
        ("# Hello\n\nThis is a paragraph", "This is a paragraph"),
        ("# Hello\nThis is a paragraph", "This is a paragraph"),
        (
            "```js\nconst x = 1;\n```\n\nParagraph after",
            "Paragraph after",
        ),
        ("> A quote\n\nNext paragraph", "Next paragraph"),
        ("---\n\nNext paragraph", "Next paragraph"),
    ] {
        let mut markdown = Markdown::new(source);
        let plain = plain_lines(&markdown.render(80));
        let trailing_index = plain
            .iter()
            .position(|line| line.contains(trailing_text))
            .unwrap_or_else(|| panic!("missing {trailing_text:?}: {plain:?}"));
        let blank_lines = plain[..trailing_index]
            .iter()
            .rev()
            .take_while(|line| line.is_empty())
            .count();
        assert_eq!(blank_lines, 1, "source: {source:?}, output: {plain:?}");
    }
}

#[test]
fn terminal_block_classes_have_no_trailing_blank_line() {
    for source in [
        "# Hello",
        "```\ncode\n```",
        "> A quote",
        "---",
        "| A | B |\n| --- | --- |\n| 1 | 2 |",
    ] {
        let mut markdown = Markdown::new(source);
        let plain = plain_lines(&markdown.render(80));
        assert!(
            plain.last().is_some_and(|line| !line.is_empty()),
            "source: {source:?}, output: {plain:?}"
        );
    }
}

#[test]
fn adjacent_paragraphs_have_one_blank_line() {
    let mut md = Markdown::new("First paragraph.\n\nSecond paragraph.");
    let lines = md.render(80);
    let plain = plain_lines(&lines);
    let first_idx = plain.iter().position(|l| l.contains("First")).unwrap();
    let after_first = &plain[first_idx + 1..];
    let empty_count = after_first.iter().position(|l| !l.is_empty()).unwrap();
    assert_eq!(
        empty_count, 1,
        "expected 1 blank line between paragraphs: {plain:?}"
    );
    assert!(
        after_first[empty_count].contains("Second"),
        "second paragraph should follow: {plain:?}"
    );
}

// ── List nesting (blockquote / code inside list items) ───────────────

fn strip_ansi_line(s: &str) -> String {
    let mut result = String::new();
    let mut in_escape = false;
    for c in s.chars() {
        match c {
            '\x1b' => in_escape = true,
            'm' if in_escape => in_escape = false,
            _ if !in_escape => result.push(c),
            _ => {}
        }
    }
    result.trim_end().to_string()
}

#[test]
fn list_item_containing_blockquote_renders_bullet_and_quote_content() {
    let mut md = Markdown::new("- > alpha beta gamma delta epsilon zeta");
    let lines = md.render(24);
    let plain: Vec<String> = lines.iter().map(|l| strip_ansi_line(l)).collect();
    // The first line should start with the list bullet
    assert!(
        plain[0].starts_with("- "),
        "first line should have bullet: {plain:?}"
    );
    // The content should be visible
    assert!(
        plain.iter().any(|l| l.contains("alpha")),
        "should show content: {plain:?}"
    );
    // The bullet and "alpha" should be on the same line
    assert!(
        plain[0].contains("alpha"),
        "bullet and content should be on same line: {plain:?}"
    );
    // No line should exceed the render width
    for (i, l) in plain.iter().enumerate() {
        assert!(
            l.len() <= 24,
            "line {i} exceeds width 24: {l:?} (len={})",
            l.len()
        );
    }
}

#[test]
fn list_item_containing_code_block_renders_bullet_and_code_fence() {
    let mut md = Markdown::new("- ```ts\n  alpha beta gamma delta epsilon zeta\n  ```");
    let lines = md.render(80);
    let plain: Vec<String> = lines.iter().map(|l| strip_ansi_line(l)).collect();
    // The first line should have bullet + fence
    assert!(
        plain[0].starts_with("- "),
        "first line should have bullet: {plain:?}"
    );
    // Bullet and fence should be on the same line
    assert!(
        plain[0].contains("```"),
        "bullet and fence should be on same line: {plain:?}"
    );
    // Code content should be visible
    assert!(
        plain.iter().any(|l| l.contains("alpha")),
        "should show code content: {plain:?}"
    );
}

#[test]
fn list_item_code_block_wraps_content_properly() {
    let mut md = Markdown::new("- ```ts\n  alpha beta gamma delta epsilon zeta\n  ```");
    let lines = md.render(24);
    let plain: Vec<String> = lines.iter().map(|l| strip_ansi_line(l)).collect();
    // The first line should contain bullet + fence on one line
    assert!(
        plain[0].starts_with("- "),
        "first line should have bullet: {plain:?}"
    );
    assert!(
        plain[0].contains("```"),
        "first line should contain fence: {plain:?}"
    );
    // Code content should be visible
    assert!(
        plain.iter().any(|l| l.contains("alpha")),
        "should show code content: {plain:?}"
    );
}

// ── Render cache ─────────────────────────────────────────────────────

#[test]
fn render_cache_returns_same_lines_on_repeated_call() {
    let mut md = Markdown::new("**bold** and `code`");
    let first = md.render(40);
    let second = md.render(40);
    assert_eq!(
        first.len(),
        second.len(),
        "cache should produce same line count"
    );
    for (a, b) in first.iter().zip(second.iter()) {
        assert_eq!(a, b, "cache should produce identical lines");
    }
}

#[test]
fn render_cache_invalidates_on_text_change() {
    let mut md = Markdown::new("First");
    let first = md.render(40);
    md.set_text("Second");
    let second = md.render(40);
    // The content should be different
    assert_ne!(first, second, "cache should invalidate on text change");
    assert!(
        second.join("\n").contains("Second"),
        "should render new text"
    );
}

#[test]
fn render_cache_invalidates_on_width_change() {
    let mut md = Markdown::new(
        "Hello world this is a longer text that will wrap differently at different widths",
    );
    let first = md.render(30);
    let second = md.render(80);
    assert_ne!(
        first, second,
        "different widths should give different results"
    );
    // Render at original width again — should match the first call
    let third = md.render(30);
    assert_eq!(
        first, third,
        "same width after width change should re-cache"
    );
}

// ── Inline style recovery (heading/quote with inline formatting) ─────

#[test]
fn heading_with_inline_code_restores_heading_style_after() {
    for (markdown, trailing) in [
        (
            "### Why `sourceInfo` should not be optional",
            "should not be optional",
        ),
        ("# Title with `code` inside", "inside"),
    ] {
        let mut md = Markdown::new(markdown);
        let lines = md.render(80);
        let joined = lines.join("\n");
        // The heading theme uses bold. Inline code uses theme.code styling.
        // After the code span's reset, bold must be re-applied.
        assert!(
            joined.contains("sourceInfo") || joined.contains("Title with"),
            "{joined}"
        );
        if color_enabled() {
            assert!(joined.contains("\x1b[1m"), "should have bold: {joined}");
            let after_code_idx = joined.find(trailing).unwrap();
            let before_text = &joined[..after_code_idx];
            assert!(
                before_text.contains("\x1b[1m"),
                "bold should be re-applied after inline code: {before_text:?}"
            );
        } else {
            assert!(
                !joined.contains("\x1b["),
                "color-disabled output should be plain: {joined:?}"
            );
        }
    }
}

#[test]
fn blockquote_with_bold_restores_quote_style() {
    let mut md = Markdown::new("> Quote with **bold** and more");
    let lines = md.render(80);
    let joined = lines.join("\n");
    // blockquote uses dim (\x1b[2m). After bold, dim should be restored.
    assert!(joined.contains("Quote with"), "{joined}");
    assert!(joined.contains("and more"), "{joined}");
    if color_enabled() {
        assert!(joined.contains("\x1b[2m"), "should have dim: {joined}");
        let after_bold = joined.find("and more").unwrap();
        let before_more = &joined[..after_bold];
        assert!(
            before_more.contains("\x1b[2m"),
            "dim should be re-applied after bold: {before_more:?}"
        );
    } else {
        assert!(
            !joined.contains("\x1b["),
            "color-disabled output should be plain: {joined:?}"
        );
    }
}
