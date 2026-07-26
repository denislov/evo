//! Editor state and input behavior.

use std::sync::{Arc, Mutex};
use tui::api::component::{CURSOR_MARKER, Component, Editor, extract_cursor_marker};
use tui::api::input::{
    CombinedAutocompleteProvider, KeybindingsManager, SlashCommand, StdinBuffer, TUI_KEYBINDINGS,
};

fn feed(editor: &mut Editor, data: &str) {
    let mut buffer = StdinBuffer::new();
    for event in buffer.process(data) {
        editor.handle_input(&event);
    }
}

fn editor() -> Editor {
    Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ))
}

#[test]
fn editor_shift_enter_inserts_newline_and_enter_submits() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    feed(&mut editor, "hello");
    feed(&mut editor, "\x1b[13;2u");
    feed(&mut editor, "world");
    assert_eq!(editor.text(), "hello\nworld");

    let submitted = Arc::new(Mutex::new(None));
    let submitted_for_callback = Arc::clone(&submitted);
    editor.set_on_submit(Box::new(move |text| {
        *submitted_for_callback.lock().unwrap() = Some(text.to_string());
    }));
    feed(&mut editor, "\r");
    assert_eq!(submitted.lock().unwrap().as_deref(), Some("hello\nworld"));
    assert_eq!(editor.text(), "");
}

#[test]
fn editor_wraps_to_width_and_keeps_lines_bounded() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    feed(&mut editor, "abcdef");
    for line in editor.render(4) {
        assert!(tui::api::render::visible_width(&line) <= 4);
    }
}

#[test]
fn editor_treats_cursor_marker_as_zero_width_when_wrapping() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    editor.set_focused(true);
    feed(&mut editor, "abcd");

    let lines = editor.render(4);

    assert_eq!(lines, vec![format!("abcd{CURSOR_MARKER}")]);
    assert_eq!(tui::api::render::visible_width(&lines[0]), 4);
}

#[test]
fn editor_left_and_right_move_cursor_marker_by_grapheme() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    editor.set_focused(true);
    feed(&mut editor, "a好");
    feed(&mut editor, "\x1b[D");
    let mut lines = editor.render(10);
    let cursor = extract_cursor_marker(&mut lines, 10).unwrap();
    assert_eq!(cursor.col, 1);
    assert_eq!(lines, vec!["a好".to_string()]);

    feed(&mut editor, "\x1b[C");
    let mut lines = editor.render(10);
    let cursor = extract_cursor_marker(&mut lines, 10).unwrap();
    assert_eq!(cursor.col, 3);
    assert_eq!(lines, vec!["a好".to_string()]);
}

#[test]
fn editor_home_end_and_forward_delete_update_cursor_and_text() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    editor.set_focused(true);
    feed(&mut editor, "hello");
    feed(&mut editor, "\x01");
    assert_eq!(editor.cursor(), 0);

    feed(&mut editor, "\x1b[3~");
    assert_eq!(editor.text(), "ello");
    assert_eq!(editor.cursor(), 0);

    feed(&mut editor, "\x05");
    assert_eq!(editor.cursor(), editor.text().len());

    feed(&mut editor, "\x1b[3~");
    assert_eq!(editor.text(), "ello");
}

#[test]
fn editor_word_navigation_and_deletion_are_grapheme_safe() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    feed(&mut editor, "foo 好😀 bar");

    feed(&mut editor, "\x1b[1;5D");
    assert_eq!(editor.cursor(), "foo 好😀 ".len());
    feed(&mut editor, "\x17");
    assert_eq!(editor.text(), "foo bar");
    assert_eq!(editor.cursor(), "foo ".len());

    feed(&mut editor, "\x1b[1;5C");
    assert_eq!(editor.cursor(), "foo bar".len());
}

#[test]
fn editor_kill_line_and_yank_round_trip_multiline_text() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    feed(&mut editor, "alpha");
    feed(&mut editor, "\x1b[13;2u");
    feed(&mut editor, "beta");

    feed(&mut editor, "\x01");
    feed(&mut editor, "\x0b");
    assert_eq!(editor.text(), "alpha\n");

    feed(&mut editor, "\x19");
    assert_eq!(editor.text(), "alpha\nbeta");
}

#[test]
fn editor_undo_and_redo_restore_text_and_cursor() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    feed(&mut editor, "ab");
    assert_eq!(editor.text(), "ab");

    feed(&mut editor, "\x1b[45;5u");
    assert_eq!(editor.text(), "");
    assert_eq!(editor.cursor(), 0);

    feed(&mut editor, "\x1b[45;6u");
    assert_eq!(editor.text(), "ab");
    assert_eq!(editor.cursor(), "ab".len());
}

#[test]
fn editor_yank_pop_replaces_previous_yank_in_place() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    feed(&mut editor, "first");
    feed(&mut editor, "\x17");
    feed(&mut editor, "second");
    feed(&mut editor, "\x17");

    feed(&mut editor, "hello world");
    feed(&mut editor, "\x01");
    for _ in 0..6 {
        feed(&mut editor, "\x1b[C");
    }

    feed(&mut editor, "\x19");
    assert_eq!(editor.text(), "hello secondworld");
    feed(&mut editor, "\x1by");
    assert_eq!(editor.text(), "hello firstworld");
}

#[test]
fn editor_word_forward_delete_and_line_start_delete_update_kill_ring() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    feed(&mut editor, "hello world");
    feed(&mut editor, "\x01");
    feed(&mut editor, "\x1bd");
    feed(&mut editor, "\x1bd");
    assert_eq!(editor.text(), "");

    feed(&mut editor, "\x19");
    assert_eq!(editor.text(), "hello world");

    editor.set_text("hello world");
    feed(&mut editor, "\x05");
    feed(&mut editor, "\x15");
    assert_eq!(editor.text(), "");

    feed(&mut editor, "\x01");

    feed(&mut editor, "\x19");
    assert_eq!(editor.text(), "hello world");
}

#[test]
fn editor_multiline_paste_is_inserted_and_undone_atomically() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    feed(&mut editor, "pre ");
    feed(&mut editor, "\x1b[200~one\ntwo\x1b[201~");
    assert_eq!(editor.text(), "pre one\ntwo");

    feed(&mut editor, "\x1b[45;5u");
    assert_eq!(editor.text(), "pre ");
    assert_eq!(editor.cursor(), "pre ".len());
}

#[test]
fn editor_up_and_down_move_between_logical_lines_by_visible_column() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    feed(&mut editor, "abc");
    feed(&mut editor, "\x1b[13;2u");
    feed(&mut editor, "de");

    feed(&mut editor, "\x1b[A");
    assert_eq!(editor.cursor(), "ab".len());

    feed(&mut editor, "\x1b[B");
    assert_eq!(editor.cursor(), "abc\n".len() + "de".len());
}

#[test]
fn editor_up_and_down_move_across_wrapped_visual_lines() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    feed(&mut editor, "abcdefgh");
    editor.render(4);

    feed(&mut editor, "\x1b[A");
    assert_eq!(editor.cursor(), 4);

    feed(&mut editor, "\x1b[B");
    assert_eq!(editor.cursor(), 8);
}

#[test]
fn editor_up_from_wrapped_line_start_moves_to_previous_visual_line() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    feed(&mut editor, "abcdefgh");
    editor.render(4);
    for _ in 0..4 {
        feed(&mut editor, "\x1b[D");
    }
    assert_eq!(editor.cursor(), 4);

    feed(&mut editor, "\x1b[A");
    assert_eq!(editor.cursor(), 0);
}

#[test]
fn editor_set_text_resets_stale_undo_history() {
    let mut editor = Editor::new(KeybindingsManager::new(
        TUI_KEYBINDINGS.clone(),
        Default::default(),
    ));
    feed(&mut editor, "a b");
    editor.set_text("");

    feed(&mut editor, "\x1b[45;5u");
    assert_eq!(editor.text(), "");
    assert_eq!(editor.cursor(), 0);
}

#[test]
fn editor_on_change_fires_for_text_changes_and_disable_submit_blocks_enter() {
    let mut editor = editor();
    let changes = Arc::new(Mutex::new(Vec::new()));
    let changes_for_callback = Arc::clone(&changes);
    editor.set_on_change(Box::new(move |text| {
        changes_for_callback.lock().unwrap().push(text.to_string());
    }));

    feed(&mut editor, "a");
    feed(&mut editor, "\x7f");
    editor.set_text("ready");

    editor.set_disable_submit(true);
    let submitted = Arc::new(Mutex::new(Vec::new()));
    let submitted_for_callback = Arc::clone(&submitted);
    editor.set_on_submit(Box::new(move |text| {
        submitted_for_callback
            .lock()
            .unwrap()
            .push(text.to_string());
    }));
    feed(&mut editor, "\r");

    assert_eq!(editor.text(), "ready");
    assert!(submitted.lock().unwrap().is_empty());

    editor.set_disable_submit(false);
    feed(&mut editor, "\r");

    assert_eq!(submitted.lock().unwrap().as_slice(), &["ready".to_string()]);
    assert_eq!(
        changes.lock().unwrap().as_slice(),
        &[
            "a".to_string(),
            "".to_string(),
            "ready".to_string(),
            "".to_string()
        ]
    );
}

#[test]
fn editor_prompt_history_skips_empty_and_consecutive_duplicates() {
    let mut editor = editor();
    editor.add_to_history("");
    editor.add_to_history("  ");
    editor.add_to_history("first");
    editor.add_to_history("second");
    editor.add_to_history("second");

    feed(&mut editor, "\x1b[A");
    assert_eq!(editor.text(), "second");
    feed(&mut editor, "\x1b[A");
    assert_eq!(editor.text(), "first");
    feed(&mut editor, "\x1b[B");
    assert_eq!(editor.text(), "second");
    feed(&mut editor, "\x1b[B");
    assert_eq!(editor.text(), "");
}

#[test]
fn editor_jump_forward_and_backward_to_requested_character() {
    let mut editor = editor();
    feed(&mut editor, "abcαabc");
    feed(&mut editor, "\x01");

    feed(&mut editor, "\x1d");
    feed(&mut editor, "α");
    assert_eq!(editor.cursor(), "abc".len());

    feed(&mut editor, "\x05");
    feed(&mut editor, "\x1b[93;7u");
    feed(&mut editor, "a");
    assert_eq!(editor.cursor(), "abcα".len());
}

#[test]
fn editor_repeating_jump_hotkey_cancels_jump_mode() {
    let mut editor = editor();
    feed(&mut editor, "abc");
    feed(&mut editor, "\x1d");
    feed(&mut editor, "\x1d");
    feed(&mut editor, "x");

    assert_eq!(editor.text(), "abcx");
    assert_eq!(editor.cursor(), "abcx".len());
}

#[test]
fn editor_large_paste_uses_marker_but_submit_expands_content() {
    let mut editor = editor();
    let pasted = (0..12)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    feed(&mut editor, &format!("\x1b[200~{pasted}\x1b[201~"));

    assert_eq!(editor.text(), "[paste #1 +12 lines]");
    assert_eq!(editor.expanded_text(), pasted);

    let submitted = Arc::new(Mutex::new(None));
    let submitted_for_callback = Arc::clone(&submitted);
    editor.set_on_submit(Box::new(move |text| {
        *submitted_for_callback.lock().unwrap() = Some(text.to_string());
    }));
    feed(&mut editor, "\r");

    assert_eq!(submitted.lock().unwrap().as_deref(), Some(pasted.as_str()));
    assert_eq!(editor.text(), "");
}

#[test]
fn editor_scrolls_long_input_and_shows_more_indicators() {
    let mut editor = editor();
    editor.set_viewport_size(20, 10);
    editor.set_text(
        (0..10)
            .map(|index| format!("line{index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    let lines = editor.render(20);

    assert!(
        lines.len() < 10,
        "expected editor to render a visible window: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("↑")),
        "expected top scroll indicator: {lines:?}"
    );
}

#[test]
fn editor_tab_completion_renders_and_applies_slash_suggestions() {
    let mut editor = editor();
    editor.set_autocomplete_provider(Box::new(CombinedAutocompleteProvider::new(
        vec![SlashCommand::new("model"), SlashCommand::new("session")],
        std::env::temp_dir(),
    )));

    feed(&mut editor, "/mo");
    feed(&mut editor, "\t");
    let rendered = editor.render(40).join("\n");
    assert!(
        rendered.contains("model"),
        "expected autocomplete row in {rendered:?}"
    );

    feed(&mut editor, "\t");
    assert_eq!(editor.text(), "/model ");
}
