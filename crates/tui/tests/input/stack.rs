//! Input decoding and terminal-key normalization behavior.

use std::sync::{Mutex, MutexGuard};

use tui::api::input::{
    InputEvent, Key, KeyEventKind, KeyModifiers, StdinBuffer, matches_key, parse_key,
    set_kitty_protocol_active,
};

static KITTY_PROTOCOL_TEST_LOCK: Mutex<()> = Mutex::new(());

struct KittyProtocolGuard {
    _guard: MutexGuard<'static, ()>,
}

impl KittyProtocolGuard {
    fn active() -> Self {
        let guard = KITTY_PROTOCOL_TEST_LOCK.lock().unwrap();
        set_kitty_protocol_active(true);
        Self { _guard: guard }
    }
}

impl Drop for KittyProtocolGuard {
    fn drop(&mut self) {
        set_kitty_protocol_active(false);
    }
}

#[test]
fn stdin_buffer_splits_batched_escape_sequences() {
    let mut buffer = StdinBuffer::new();
    let events = buffer.process("\x1b[A\x1b[Bx");
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], InputEvent::Key(_)));
    assert!(matches!(events[1], InputEvent::Key(_)));
    assert!(matches!(events[2], InputEvent::Key(_)));
    assert!(matches_key(&events[0], "up"));
    assert!(matches_key(&events[1], "down"));
    assert!(matches_key(&events[2], "x"));
}

#[test]
fn stdin_buffer_waits_for_partial_csi_sequence() {
    let mut buffer = StdinBuffer::new();
    assert!(buffer.process("\x1b[").is_empty());
    let events = buffer.process("A");
    assert_eq!(events.len(), 1);
    assert!(matches_key(&events[0], "up"));
}

#[test]
fn bracketed_paste_is_one_paste_event() {
    let mut buffer = StdinBuffer::new();
    let events = buffer.process("\x1b[200~hello\nworld\x1b[201~");
    assert_eq!(events, vec![InputEvent::Paste("hello\nworld".to_string())]);
}

#[test]
fn parse_legacy_and_kitty_keys() {
    assert!(matches_key(
        &InputEvent::Key(parse_key("\r").unwrap()),
        "enter"
    ));
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x7f").unwrap()),
        "backspace"
    ));
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1b[3~").unwrap()),
        "delete"
    ));
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1b[97u").unwrap()),
        "a"
    ));
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1b[65;5u").unwrap()),
        "ctrl+a"
    ));
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1b[65;6u").unwrap()),
        "ctrl+shift+a"
    ));
}

#[test]
fn parse_alt_modified_printable_keys() {
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1bd").unwrap()),
        "alt+d"
    ));
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1by").unwrap()),
        "alt+y"
    ));
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1b\x7f").unwrap()),
        "alt+backspace"
    ));
}

#[test]
fn kitty_release_events_are_detected() {
    let event = parse_key("\x1b[97;3:3u").unwrap();
    assert_eq!(event.key, Key::Char("a".to_string()));
    assert_eq!(event.kind, KeyEventKind::Release);
    assert_eq!(event.modifiers, KeyModifiers::ALT);
}

#[test]
fn parse_modify_other_keys_ctrl_c() {
    let event = parse_key("\x1b[27;5;99~").unwrap();
    assert_eq!(event.key, Key::Char("c".to_string()));
    assert_eq!(event.modifiers, KeyModifiers::CTRL);
}

#[test]
fn parse_modify_other_keys_shift_enter() {
    let event = parse_key("\x1b[27;2;13~").unwrap();
    assert_eq!(event.key, Key::Enter);
    assert_eq!(event.modifiers, KeyModifiers::SHIFT);
}

#[test]
fn parse_kitty_keypad_digits() {
    assert_eq!(
        parse_key("\x1b[57399u").unwrap().key,
        Key::Char("0".to_string())
    );
    assert_eq!(
        parse_key("\x1b[57400u").unwrap().key,
        Key::Char("1".to_string())
    );
}

#[test]
fn parse_kitty_keypad_navigation() {
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1b[57417u").unwrap()),
        "left"
    ));
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1b[57419u").unwrap()),
        "up"
    ));
}

#[test]
fn parse_rxvt_shift_sequences() {
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1b[a").unwrap()),
        "shift+up"
    ));
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1b[2$").unwrap()),
        "shift+insert"
    ));
}

#[test]
fn parse_rxvt_ctrl_sequences() {
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1bOa").unwrap()),
        "ctrl+up"
    ));
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1b[2^").unwrap()),
        "ctrl+insert"
    ));
}

#[test]
fn parse_clear_key() {
    assert_eq!(parse_key("\x1b[E").unwrap().key, Key::Clear);
    assert_eq!(parse_key("\x1bOE").unwrap().key, Key::Clear);
}

#[test]
fn space_key_roundtrip() {
    let event = parse_key(" ").unwrap();
    assert_eq!(event.key, Key::Space);
    assert!(matches_key(&InputEvent::Key(event), "space"));
}

#[test]
fn kitty_active_changes_newline_semantics() {
    let _kitty = KittyProtocolGuard::active();
    assert!(matches_key(
        &InputEvent::Key(parse_key("\n").unwrap()),
        "ctrl+j"
    ));
    // \n = ctrl+j, which in legacy mode also matches "enter"
    assert!(!matches_key(
        &InputEvent::Key(parse_key("\n").unwrap()),
        "enter"
    ));
    set_kitty_protocol_active(false);
    assert!(matches_key(
        &InputEvent::Key(parse_key("\n").unwrap()),
        "enter"
    ));
    assert!(matches_key(
        &InputEvent::Key(parse_key("\n").unwrap()),
        "ctrl+j"
    ));
}

#[test]
fn kitty_active_changes_alt_enter_semantics() {
    let _kitty = KittyProtocolGuard::active();
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1b\r").unwrap()),
        "shift+enter"
    ));
    set_kitty_protocol_active(false);
    assert!(matches_key(
        &InputEvent::Key(parse_key("\x1b\r").unwrap()),
        "alt+enter"
    ));
}

#[test]
fn ctrl_space_is_parsed_correctly() {
    let event = parse_key("\x00").unwrap();
    assert_eq!(event.key, Key::Space);
    assert_eq!(event.modifiers, KeyModifiers::CTRL);
    assert!(matches_key(&InputEvent::Key(event), "ctrl+space"));
}

#[test]
fn ctrl_minus_and_ctrl_underscore_are_equivalent() {
    // ctrl+- and ctrl+_ share the same control character (byte 31)
    let event = parse_key("\x1f").unwrap();
    assert_eq!(event.key, Key::Char("-".to_string()));
    assert_eq!(event.modifiers, KeyModifiers::CTRL);
    // Both key IDs should match the same event
    assert!(matches_key(&InputEvent::Key(event.clone()), "ctrl+-"));
    assert!(matches_key(&InputEvent::Key(event), "ctrl+_"));
}
