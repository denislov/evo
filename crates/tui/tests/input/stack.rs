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
fn parse_key_maps_sequences_to_expected_ids() {
    let cases: &[(&str, &str)] = &[
        ("\r", "enter"),
        ("\x7f", "backspace"),
        ("\x1b[3~", "delete"),
        ("\x1b[97u", "a"),
        ("\x1b[65;5u", "ctrl+a"),
        ("\x1b[65;6u", "ctrl+shift+a"),
        ("\x1bd", "alt+d"),
        ("\x1by", "alt+y"),
        ("\x1b\x7f", "alt+backspace"),
        ("\x1b[57417u", "left"),
        ("\x1b[57419u", "up"),
        ("\x1b[a", "shift+up"),
        ("\x1b[2$", "shift+insert"),
        ("\x1bOa", "ctrl+up"),
        ("\x1b[2^", "ctrl+insert"),
    ];
    for (sequence, expected) in cases {
        let event = parse_key(sequence)
            .unwrap_or_else(|| panic!("parse {sequence:?} failed"));
        assert!(
            matches_key(&InputEvent::Key(event), expected),
            "sequence {sequence:?} should match {expected}"
        );
    }
}

#[test]
fn parse_key_resolves_exact_key_and_modifiers() {
    let release = parse_key("\x1b[97;3:3u").unwrap();
    assert_eq!(release.key, Key::Char("a".to_string()));
    assert_eq!(release.kind, KeyEventKind::Release);
    assert_eq!(release.modifiers, KeyModifiers::ALT);

    let ctrl_c = parse_key("\x1b[27;5;99~").unwrap();
    assert_eq!(ctrl_c.key, Key::Char("c".to_string()));
    assert_eq!(ctrl_c.modifiers, KeyModifiers::CTRL);

    let shift_enter = parse_key("\x1b[27;2;13~").unwrap();
    assert_eq!(shift_enter.key, Key::Enter);
    assert_eq!(shift_enter.modifiers, KeyModifiers::SHIFT);

    assert_eq!(
        parse_key("\x1b[57399u").unwrap().key,
        Key::Char("0".to_string())
    );
    assert_eq!(
        parse_key("\x1b[57400u").unwrap().key,
        Key::Char("1".to_string())
    );

    assert_eq!(parse_key("\x1b[E").unwrap().key, Key::Clear);
    assert_eq!(parse_key("\x1bOE").unwrap().key, Key::Clear);

    let space = parse_key(" ").unwrap();
    assert_eq!(space.key, Key::Space);
    assert!(matches_key(&InputEvent::Key(space), "space"));

    let ctrl_space = parse_key("\x00").unwrap();
    assert_eq!(ctrl_space.key, Key::Space);
    assert_eq!(ctrl_space.modifiers, KeyModifiers::CTRL);
    assert!(matches_key(&InputEvent::Key(ctrl_space), "ctrl+space"));

    // ctrl+- and ctrl+_ share the same control character (byte 31)
    let ctrl_minus = parse_key("\x1f").unwrap();
    assert_eq!(ctrl_minus.key, Key::Char("-".to_string()));
    assert_eq!(ctrl_minus.modifiers, KeyModifiers::CTRL);
    assert!(matches_key(&InputEvent::Key(ctrl_minus.clone()), "ctrl+-"));
    assert!(matches_key(&InputEvent::Key(ctrl_minus), "ctrl+_"));
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
