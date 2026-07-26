//! Basic component composition behavior.

use tui::api::component::{Box as TuiBox, Component, Container, Spacer, Text, TruncatedText};
use tui::api::render::visible_width;

#[test]
fn spacer_renders_empty_lines() {
    let mut spacer = Spacer::new(3);
    assert_eq!(spacer.render(10), vec!["", "", ""]);
}

#[test]
fn container_renders_children_in_order() {
    let mut container = Container::new();
    container.add_child(Box::new(Text::new("alpha")));
    container.add_child(Box::new(Spacer::new(1)));
    container.add_child(Box::new(Text::new("beta")));

    assert_eq!(
        container.render(20),
        vec!["alpha".to_string(), "".to_string(), "beta".to_string()]
    );
}

#[test]
fn text_wraps_words_to_width() {
    let mut text = Text::new("alpha beta gamma");
    assert_eq!(
        text.render(10),
        vec!["alpha beta".to_string(), "gamma".to_string()]
    );
}

#[test]
fn text_splits_long_words_without_exceeding_width() {
    let mut text = Text::new("abcdefghij");
    let lines = text.render(4);

    assert_eq!(
        lines,
        vec!["abcd".to_string(), "efgh".to_string(), "ij".to_string()]
    );
    assert!(lines.iter().all(|line| visible_width(line) <= 4));
}

#[test]
fn text_handles_cjk_width_when_wrapping() {
    let mut text = Text::new("你好 world");
    let lines = text.render(6);

    assert_eq!(lines, vec!["你好".to_string(), "world".to_string()]);
    assert!(lines.iter().all(|line| visible_width(line) <= 6));
}

#[test]
fn truncated_text_renders_first_line_padded_to_width() {
    let mut text = TruncatedText::new("alpha\nbeta");
    assert_eq!(text.render(8), vec!["alpha   ".to_string()]);
}

#[test]
fn truncated_text_applies_padding_and_truncates_to_available_width() {
    let mut text = TruncatedText::with_padding("abcdef", 1, 1);
    let lines = text.render(6);

    assert_eq!(
        lines,
        vec![
            "      ".to_string(),
            " abcd ".to_string(),
            "      ".to_string(),
        ]
    );
    assert!(lines.iter().all(|line| visible_width(line) <= 6));
}

#[test]
fn box_component_adds_padding_around_children() {
    let mut panel = TuiBox::with_padding(1, 1);
    panel.add_child(std::boxed::Box::new(TruncatedText::new("alpha")));

    assert_eq!(
        panel.render(8),
        vec![
            "        ".to_string(),
            " alpha  ".to_string(),
            "        ".to_string(),
        ]
    );
}

#[test]
fn box_component_applies_background_to_padded_lines() {
    let mut panel = TuiBox::with_padding(1, 0);
    panel.set_background_fn(Some(std::boxed::Box::new(|line| format!("<{line}>"))));
    panel.add_child(std::boxed::Box::new(TruncatedText::new("ok")));

    assert_eq!(panel.render(6), vec!["< ok   >".to_string()]);
}

#[test]
fn box_component_clear_removes_children() {
    let mut panel = TuiBox::new();
    panel.add_child(std::boxed::Box::new(TruncatedText::new("alpha")));
    panel.clear();

    assert!(panel.render(8).is_empty());
}
