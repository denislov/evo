//! Unicode and ANSI-visible width behavior.

use tui::api::render::{truncate_to_width, visible_width};

#[test]
fn visible_width_counts_ascii() {
    assert_eq!(visible_width("hello"), 5);
}

#[test]
fn visible_width_counts_tabs_as_three_columns() {
    assert_eq!(visible_width("a\tb"), 5);
}

#[test]
fn visible_width_counts_cjk_as_wide() {
    assert_eq!(visible_width("你a"), 3);
}

#[test]
fn visible_width_counts_emoji_as_wide() {
    assert_eq!(visible_width("🙂a"), 3);
}

#[test]
fn visible_width_ignores_csi_osc_and_apc_sequences() {
    let styled = "\x1b[31mred\x1b[0m";
    let hyperlink = "\x1b]8;;https://example.com\x07link\x1b]8;;\x07";
    let marker = "\x1b_pi:c\x07x";

    assert_eq!(visible_width(styled), 3);
    assert_eq!(visible_width(hyperlink), 4);
    assert_eq!(visible_width(marker), 1);
}

#[test]
fn truncate_to_width_does_not_split_wide_graphemes() {
    assert_eq!(truncate_to_width("你好吗", 4), "你好");
    assert_eq!(truncate_to_width("🙂🙂a", 2), "🙂");
}

#[test]
fn truncate_to_width_keeps_leading_ansi_sequences() {
    let clipped = truncate_to_width("\x1b[31mhello", 2);
    assert_eq!(clipped, "\x1b[31mhe");
    assert_eq!(visible_width(&clipped), 2);
}
