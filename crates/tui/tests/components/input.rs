//! Generic input component editing behavior.

use tui::api::component::{Component, Input};
use tui::api::input::{KeybindingsManager, StdinBuffer, TUI_KEYBINDINGS};

fn feed(input: &mut Input, data: &str) {
    let mut buffer = StdinBuffer::new();
    for event in buffer.process(data) {
        input.handle_input(&event);
    }
}

#[test]
fn input_edits_unicode_graphemes() {
    let mut input = Input::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    feed(&mut input, "a好");
    assert_eq!(input.value(), "a好");
    feed(&mut input, "\x7f");
    assert_eq!(input.value(), "a");
}

#[test]
fn input_paste_inserts_literal_content() {
    let mut input = Input::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    feed(&mut input, "\x1b[200~hello\nworld\x1b[201~");
    assert_eq!(input.value(), "hello\nworld");
}

#[test]
fn focused_input_renders_cursor_marker() {
    let mut input = Input::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    input.set_focused(true);
    feed(&mut input, "abc");
    let line = input.render(10).join("");
    assert!(line.contains(tui::api::component::CURSOR_MARKER));
}
