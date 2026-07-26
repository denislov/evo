//! Generic style and color-level rendering behavior.

use tui::api::render::{
    Color, ColorLevel, Style, detect_color_level_from_env, paint_with, paint_with_level,
};

#[test]
fn paint_with_disabled_returns_plain_text() {
    let style = Style::fg(Color::Red).bold();
    assert_eq!(paint_with("hi", &style, false), "hi");
}

#[test]
fn paint_with_enabled_single_fg() {
    let style = Style::fg(Color::Red);
    assert_eq!(paint_with("hi", &style, true), "\x1b[31mhi\x1b[0m");
}

#[test]
fn paint_with_enabled_bold_and_fg_merge_into_single_sgr() {
    let style = Style::fg(Color::Red).bold();
    assert_eq!(paint_with("hi", &style, true), "\x1b[1;31mhi\x1b[0m");
}

#[test]
fn paint_with_enabled_bold_reverse_and_fg_merge() {
    let style = Style::fg(Color::Red).bold().reverse();
    assert_eq!(paint_with("hi", &style, true), "\x1b[1;7;31mhi\x1b[0m");
}

#[test]
fn paint_with_default_color_emits_no_fg_sequence() {
    let style = Style::fg(Color::Default).bold();
    assert_eq!(paint_with("hi", &style, true), "\x1b[1mhi\x1b[0m");
}

#[test]
fn paint_with_default_style_emits_nothing() {
    let style = Style::default();
    assert_eq!(paint_with("hi", &style, true), "hi");
}

#[test]
fn paint_with_enabled_dim() {
    let style = Style::fg(Color::Default).dim();
    assert_eq!(paint_with("hi", &style, true), "\x1b[2mhi\x1b[0m");
}

#[test]
fn paint_with_enabled_bg() {
    let style = Style {
        bg: Color::Blue,
        ..Default::default()
    };
    assert_eq!(paint_with("hi", &style, true), "\x1b[44mhi\x1b[0m");
}

#[test]
fn paint_with_enabled_ansi256_fg_and_bg() {
    let mut style = Style::fg(Color::Ansi256(202));
    style.bg = Color::Ansi256(17);
    assert_eq!(
        paint_with("hi", &style, true),
        "\x1b[38;5;202;48;5;17mhi\x1b[0m"
    );
}

#[test]
fn paint_with_enabled_rgb_fg_and_bg() {
    let mut style = Style::fg(Color::Rgb(1, 2, 3));
    style.bg = Color::Rgb(4, 5, 6);
    assert_eq!(
        paint_with("hi", &style, true),
        "\x1b[38;2;1;2;3;48;2;4;5;6mhi\x1b[0m"
    );
}

#[test]
fn paint_with_level_downgrades_when_color_disabled() {
    let style = Style::fg(Color::Rgb(1, 2, 3)).bold();
    assert_eq!(paint_with_level("hi", &style, ColorLevel::None), "hi");
}

#[test]
fn detect_color_level_honors_no_color_and_dumb() {
    assert_eq!(
        detect_color_level_from_env([("NO_COLOR", "1"), ("TERM", "xterm-256color")]),
        ColorLevel::None
    );
    assert_eq!(
        detect_color_level_from_env([("TERM", "dumb")]),
        ColorLevel::None
    );
}

#[test]
fn detect_color_level_detects_truecolor_and_ansi256() {
    assert_eq!(
        detect_color_level_from_env([("COLORTERM", "truecolor"), ("TERM", "xterm-256color")]),
        ColorLevel::TrueColor
    );
    assert_eq!(
        detect_color_level_from_env([("TERM", "screen-256color")]),
        ColorLevel::Ansi256
    );
    assert_eq!(
        detect_color_level_from_env([("TERM", "xterm")]),
        ColorLevel::Ansi16
    );
}

#[test]
fn paint_with_enabled_italic_underline_strikethrough() {
    let style = Style::fg(Color::Red).italic().underline().strikethrough();
    // SGR codes: italic=3, underline=4, strikethrough=9, red fg=31
    assert_eq!(paint_with("hi", &style, true), "\x1b[3;4;9;31mhi\x1b[0m");
}

#[test]
fn paint_with_ansi256_level_quantizes_rgb_to_256_color() {
    // In a 256-color terminal, RGB colors are downgraded to the nearest
    // 256 palette index (mirrors TS rgbTo256). Pure red (255,0,0) -> index 196.
    let style = Style::fg(Color::Rgb(255, 0, 0));
    let painted = paint_with_level("hi", &style, ColorLevel::Ansi256);
    assert!(
        painted.starts_with("\x1b[38;5;"),
        "expected 256-color sequence, got: {painted:?}"
    );
    assert!(
        painted.contains("196"),
        "red(255,0,0) should map near 196: {painted:?}"
    );
}

#[test]
fn paint_with_truecolor_level_keeps_rgb() {
    // TrueColor terminals keep the full RGB sequence.
    let style = Style::fg(Color::Rgb(255, 0, 0));
    let painted = paint_with_level("hi", &style, ColorLevel::TrueColor);
    assert_eq!(painted, "\x1b[38;2;255;0;0mhi\x1b[0m");
}
