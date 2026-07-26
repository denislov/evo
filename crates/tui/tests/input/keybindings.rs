//! Generic keybinding configuration behavior.

use tui::api::input::{
    InputEvent, KeybindingConflict, KeybindingsManager, TUI_KEYBINDINGS, parse_key,
};

fn key(input: &str) -> InputEvent {
    InputEvent::Key(parse_key(input).unwrap())
}

#[test]
fn default_keybindings_match_editor_actions() {
    assert!(
        !TUI_KEYBINDINGS.keys().any(|id| id.starts_with("app.")),
        "tui default keybindings must stay product-free"
    );
    let manager = KeybindingsManager::new(TUI_KEYBINDINGS.clone(), Default::default());
    assert!(manager.matches(&key("\x1b[A"), "tui.editor.cursorUp"));
    assert!(manager.matches(&key("\x1b[B"), "tui.editor.cursorDown"));
    assert!(manager.matches(&key("\r"), "tui.input.submit"));
}

#[test]
fn user_bindings_override_defaults() {
    let mut user = std::collections::BTreeMap::new();
    user.insert("tui.input.submit".to_string(), vec!["ctrl+j".to_string()]);
    let manager = KeybindingsManager::new(TUI_KEYBINDINGS.clone(), user);
    assert!(!manager.matches(&key("\r"), "tui.input.submit"));
    assert!(manager.matches(&key("\n"), "tui.input.submit"));
}

#[test]
fn conflicts_are_reported_for_user_bindings() {
    let mut user = std::collections::BTreeMap::new();
    user.insert("tui.input.submit".to_string(), vec!["ctrl+x".to_string()]);
    user.insert("tui.select.cancel".to_string(), vec!["ctrl+x".to_string()]);
    let manager = KeybindingsManager::new(TUI_KEYBINDINGS.clone(), user);
    assert_eq!(
        manager.conflicts(),
        vec![KeybindingConflict {
            key: "ctrl+x".to_string(),
            keybindings: vec![
                "tui.input.submit".to_string(),
                "tui.select.cancel".to_string(),
            ],
        }]
    );
}
