//! ANSI-preserving wrapping and truncation behavior.

use tui::api::render::{truncate_to_width_with_ellipsis, visible_width, wrap_text_with_ansi};

#[test]
fn wrap_text_with_ansi_preserves_active_sgr_across_wrapped_lines() {
    let lines = wrap_text_with_ansi("\x1b[31mhello world from rust\x1b[0m", 10);

    assert_eq!(lines.len(), 3);
    assert_eq!(visible_width(&lines[0]), 5);
    assert!(lines[0].starts_with("\x1b[31m"));
    assert!(lines[1].starts_with("\x1b[31m"));
    assert!(lines[2].ends_with("\x1b[0m"));
    assert!(lines.iter().all(|line| visible_width(line) <= 10));
}

#[test]
fn wrap_text_with_ansi_keeps_graphemes_and_wide_emoji_inside_width() {
    let lines = wrap_text_with_ansi("alpha 🎉🎉 beta", 8);

    assert_eq!(
        lines,
        vec!["alpha".to_string(), "🎉🎉".to_string(), "beta".to_string()]
    );
    assert!(lines.iter().all(|line| visible_width(line) <= 8));
}

#[test]
fn wrap_text_with_ansi_handles_literal_newlines_and_empty_input() {
    assert_eq!(wrap_text_with_ansi("", 10), vec!["".to_string()]);
    assert_eq!(
        wrap_text_with_ansi("one\ntwo three", 5),
        vec!["one".to_string(), "two".to_string(), "three".to_string()]
    );
}

#[test]
fn truncate_to_width_with_ellipsis_preserves_ansi_prefix_and_visible_width() {
    let truncated = truncate_to_width_with_ellipsis("\x1b[32mabcdef\x1b[0m", 5);

    assert!(truncated.starts_with("\x1b[32m"));
    assert!(truncated.contains("..."));
    assert!(truncated.ends_with("\x1b[0m"));
    assert_eq!(visible_width(&truncated), 5);
}
