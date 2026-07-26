use std::sync::atomic::{AtomicBool, Ordering};

use super::InputEvent;

static KITTY_PROTOCOL_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn set_kitty_protocol_active(active: bool) {
    KITTY_PROTOCOL_ACTIVE.store(active, Ordering::Relaxed);
}

pub fn is_kitty_protocol_active() -> bool {
    KITTY_PROTOCOL_ACTIVE.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Char(String),
    Enter,
    Tab,
    Escape,
    Space,
    Backspace,
    Delete,
    Insert,
    Clear,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    Function(u8),
    Unknown(String),
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct KeyModifiers: u8 {
        const SHIFT = 0b0001;
        const ALT = 0b0010;
        const CTRL = 0b0100;
        const SUPER = 0b1000;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventKind {
    Press,
    Repeat,
    Release,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub modifiers: KeyModifiers,
    pub kind: KeyEventKind,
}

pub fn parse_key(data: &str) -> Option<KeyEvent> {
    if data.is_empty() {
        return None;
    }

    if let Some(event) = parse_kitty_csi_u(data) {
        return Some(event);
    }

    if let Some(event) = parse_modify_other_keys(data) {
        return Some(event);
    }

    if let Some(event) = parse_legacy_csi(data) {
        return Some(event);
    }

    if let Some(event) = parse_legacy_ss3(data) {
        return Some(event);
    }

    Some(match data {
        "\r" | "\x1bOM" => key(Key::Enter),
        "\t" => key(Key::Tab),
        "\x1b" => key(Key::Escape),
        "\x7f" => key(Key::Backspace),
        "\x08" => {
            if is_windows_terminal_session() {
                modified(Key::Backspace, KeyModifiers::CTRL, KeyEventKind::Press)
            } else {
                key(Key::Backspace)
            }
        }
        "\x00" => modified(Key::Space, KeyModifiers::CTRL, KeyEventKind::Press),
        " " => key(Key::Space),
        "\x1b[Z" => modified(Key::Tab, KeyModifiers::SHIFT, KeyEventKind::Press),
        "\x1b\r" => {
            if is_kitty_protocol_active() {
                modified(Key::Enter, KeyModifiers::SHIFT, KeyEventKind::Press)
            } else {
                modified(Key::Enter, KeyModifiers::ALT, KeyEventKind::Press)
            }
        }
        "\x1b " => {
            if !is_kitty_protocol_active() {
                modified(Key::Space, KeyModifiers::ALT, KeyEventKind::Press)
            } else {
                return None;
            }
        }
        "\x1b\x7f" | "\x1b\x08" => modified(Key::Backspace, KeyModifiers::ALT, KeyEventKind::Press),
        "\x1bB" => {
            if !is_kitty_protocol_active() {
                modified(Key::Left, KeyModifiers::ALT, KeyEventKind::Press)
            } else {
                return None;
            }
        }
        "\x1bF" => {
            if !is_kitty_protocol_active() {
                modified(Key::Right, KeyModifiers::ALT, KeyEventKind::Press)
            } else {
                return None;
            }
        }
        _ => {
            if let Some(control) = parse_control_char(data) {
                control
            } else if data.starts_with('\x1b') {
                if let Some(alt) = parse_alt_sequence(data) {
                    alt
                } else {
                    KeyEvent {
                        key: Key::Unknown(data.to_string()),
                        modifiers: KeyModifiers::empty(),
                        kind: KeyEventKind::Press,
                    }
                }
            } else {
                key(Key::Char(data.to_string()))
            }
        }
    })
}

pub fn matches_key(event: &InputEvent, key_id: &str) -> bool {
    let InputEvent::Key(event) = event else {
        return false;
    };
    if event.kind == KeyEventKind::Release {
        return false;
    }

    let Some((expected_key, expected_modifiers)) = parse_key_id(key_id) else {
        return false;
    };

    if keys_equal(&event.key, &expected_key) && event.modifiers == expected_modifiers {
        return true;
    }

    // Legacy equivalence: \n (ctrl+j) also matches "enter" when Kitty is not active.
    // This mirrors TS where matchesKey("\n", "enter") returns true in legacy mode.
    // The reverse (\r matching "ctrl+j") is NOT true.
    if !is_kitty_protocol_active()
        && event.key == Key::Char("j".to_string())
        && event.modifiers == KeyModifiers::CTRL
        && expected_key == Key::Enter
        && expected_modifiers.is_empty()
    {
        return true;
    }

    false
}

pub fn is_key_release(event: &InputEvent) -> bool {
    matches!(
        event,
        InputEvent::Key(KeyEvent {
            kind: KeyEventKind::Release,
            ..
        })
    )
}

fn key(key: Key) -> KeyEvent {
    KeyEvent {
        key,
        modifiers: KeyModifiers::empty(),
        kind: KeyEventKind::Press,
    }
}

fn modified(key: Key, modifiers: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
    KeyEvent {
        key,
        modifiers,
        kind,
    }
}

fn parse_control_char(data: &str) -> Option<KeyEvent> {
    if data.len() != 1 {
        return None;
    }
    let byte = data.as_bytes()[0];
    match byte {
        0 => Some(modified(
            Key::Space,
            KeyModifiers::CTRL,
            KeyEventKind::Press,
        )),
        1..=26 => {
            let ch = char::from(b'a' + byte - 1);
            Some(modified(
                Key::Char(ch.to_string()),
                KeyModifiers::CTRL,
                KeyEventKind::Press,
            ))
        }
        28 => Some(modified(
            Key::Char("\\".to_string()),
            KeyModifiers::CTRL,
            KeyEventKind::Press,
        )),
        29 => Some(modified(
            Key::Char("]".to_string()),
            KeyModifiers::CTRL,
            KeyEventKind::Press,
        )),
        30 => Some(modified(
            Key::Char("^".to_string()),
            KeyModifiers::CTRL,
            KeyEventKind::Press,
        )),
        31 => Some(modified(
            Key::Char("-".to_string()),
            KeyModifiers::CTRL,
            KeyEventKind::Press,
        )),
        _ => None,
    }
}

fn is_windows_terminal_session() -> bool {
    cfg!(target_os = "windows")
        && std::env::var("WT_SESSION").is_ok()
        && std::env::var("SSH_CONNECTION").is_err()
        && std::env::var("SSH_CLIENT").is_err()
        && std::env::var("SSH_TTY").is_err()
}

fn parse_legacy_csi(data: &str) -> Option<KeyEvent> {
    let (key, modifiers) = match data {
        "\x1b[A" => (Key::Up, KeyModifiers::empty()),
        "\x1b[B" => (Key::Down, KeyModifiers::empty()),
        "\x1b[C" => (Key::Right, KeyModifiers::empty()),
        "\x1b[D" => (Key::Left, KeyModifiers::empty()),
        "\x1b[H" | "\x1b[1~" | "\x1b[7~" => (Key::Home, KeyModifiers::empty()),
        "\x1b[F" | "\x1b[4~" | "\x1b[8~" => (Key::End, KeyModifiers::empty()),
        "\x1b[2~" => (Key::Insert, KeyModifiers::empty()),
        "\x1b[3~" => (Key::Delete, KeyModifiers::empty()),
        "\x1b[5~" | "\x1b[[5~" => (Key::PageUp, KeyModifiers::empty()),
        "\x1b[6~" | "\x1b[[6~" => (Key::PageDown, KeyModifiers::empty()),
        "\x1b[E" => (Key::Clear, KeyModifiers::empty()),
        "\x1b[Z" => (Key::Tab, KeyModifiers::SHIFT),
        // rxvt shift sequences
        "\x1b[a" => (Key::Up, KeyModifiers::SHIFT),
        "\x1b[b" => (Key::Down, KeyModifiers::SHIFT),
        "\x1b[c" => (Key::Right, KeyModifiers::SHIFT),
        "\x1b[d" => (Key::Left, KeyModifiers::SHIFT),
        "\x1b[e" => (Key::Clear, KeyModifiers::SHIFT),
        "\x1b[2$" => (Key::Insert, KeyModifiers::SHIFT),
        "\x1b[3$" => (Key::Delete, KeyModifiers::SHIFT),
        "\x1b[5$" => (Key::PageUp, KeyModifiers::SHIFT),
        "\x1b[6$" => (Key::PageDown, KeyModifiers::SHIFT),
        "\x1b[7$" => (Key::Home, KeyModifiers::SHIFT),
        "\x1b[8$" => (Key::End, KeyModifiers::SHIFT),
        // rxvt ctrl sequences
        "\x1b[2^" => (Key::Insert, KeyModifiers::CTRL),
        "\x1b[3^" => (Key::Delete, KeyModifiers::CTRL),
        "\x1b[5^" => (Key::PageUp, KeyModifiers::CTRL),
        "\x1b[6^" => (Key::PageDown, KeyModifiers::CTRL),
        "\x1b[7^" => (Key::Home, KeyModifiers::CTRL),
        "\x1b[8^" => (Key::End, KeyModifiers::CTRL),
        "\x1b[11~" | "\x1b[[A" => (Key::Function(1), KeyModifiers::empty()),
        "\x1b[12~" | "\x1b[[B" => (Key::Function(2), KeyModifiers::empty()),
        "\x1b[13~" | "\x1b[[C" => (Key::Function(3), KeyModifiers::empty()),
        "\x1b[14~" | "\x1b[[D" => (Key::Function(4), KeyModifiers::empty()),
        "\x1b[15~" | "\x1b[[E" => (Key::Function(5), KeyModifiers::empty()),
        "\x1b[17~" => (Key::Function(6), KeyModifiers::empty()),
        "\x1b[18~" => (Key::Function(7), KeyModifiers::empty()),
        "\x1b[19~" => (Key::Function(8), KeyModifiers::empty()),
        "\x1b[20~" => (Key::Function(9), KeyModifiers::empty()),
        "\x1b[21~" => (Key::Function(10), KeyModifiers::empty()),
        "\x1b[23~" => (Key::Function(11), KeyModifiers::empty()),
        "\x1b[24~" => (Key::Function(12), KeyModifiers::empty()),
        _ => return parse_modified_legacy_csi(data),
    };

    Some(modified(key, modifiers, KeyEventKind::Press))
}

fn parse_modified_legacy_csi(data: &str) -> Option<KeyEvent> {
    if let Some(rest) = data.strip_prefix("\x1b[1;") {
        let final_char = rest.chars().last()?;
        let params = &rest[..rest.len() - final_char.len_utf8()];
        let (modifier, kind) = parse_modifier_and_kind(params)?;
        let key = match final_char {
            'A' => Key::Up,
            'B' => Key::Down,
            'C' => Key::Right,
            'D' => Key::Left,
            'H' => Key::Home,
            'F' => Key::End,
            _ => return None,
        };
        return Some(modified(key, modifier, kind));
    }

    if let Some(rest) = data.strip_prefix("\x1b[") {
        let number_end = rest.find(';')?;
        let key_number = rest[..number_end].parse::<u16>().ok()?;
        let rest = &rest[number_end + 1..];
        let params = rest.strip_suffix('~')?;
        let (modifier, kind) = parse_modifier_and_kind(params)?;
        let key = match key_number {
            2 => Key::Insert,
            3 => Key::Delete,
            5 => Key::PageUp,
            6 => Key::PageDown,
            7 => Key::Home,
            8 => Key::End,
            _ => return None,
        };
        return Some(modified(key, modifier, kind));
    }

    None
}

fn parse_legacy_ss3(data: &str) -> Option<KeyEvent> {
    // alt-modified arrow keys (legacy only; Kitty sends CSI-u instead)
    if !is_kitty_protocol_active() {
        match data {
            "\x1bb" => return Some(modified(Key::Left, KeyModifiers::ALT, KeyEventKind::Press)),
            "\x1bf" => return Some(modified(Key::Right, KeyModifiers::ALT, KeyEventKind::Press)),
            "\x1bp" => return Some(modified(Key::Up, KeyModifiers::ALT, KeyEventKind::Press)),
            "\x1bn" => return Some(modified(Key::Down, KeyModifiers::ALT, KeyEventKind::Press)),
            _ => {}
        }
    }

    let parsed_key = match data {
        "\x1bOA" => Key::Up,
        "\x1bOB" => Key::Down,
        "\x1bOC" => Key::Right,
        "\x1bOD" => Key::Left,
        "\x1bOH" => Key::Home,
        "\x1bOF" => Key::End,
        "\x1bOE" => Key::Clear,
        "\x1bOP" => Key::Function(1),
        "\x1bOQ" => Key::Function(2),
        "\x1bOR" => Key::Function(3),
        "\x1bOS" => Key::Function(4),
        // SS3 ctrl-modified arrow keys
        "\x1bOa" => return Some(modified(Key::Up, KeyModifiers::CTRL, KeyEventKind::Press)),
        "\x1bOb" => return Some(modified(Key::Down, KeyModifiers::CTRL, KeyEventKind::Press)),
        "\x1bOc" => {
            return Some(modified(
                Key::Right,
                KeyModifiers::CTRL,
                KeyEventKind::Press,
            ));
        }
        "\x1bOd" => return Some(modified(Key::Left, KeyModifiers::CTRL, KeyEventKind::Press)),
        "\x1bOe" => {
            return Some(modified(
                Key::Clear,
                KeyModifiers::CTRL,
                KeyEventKind::Press,
            ));
        }
        _ => return parse_alt_sequence(data),
    };
    Some(key(parsed_key))
}

fn parse_alt_sequence(data: &str) -> Option<KeyEvent> {
    let rest = data.strip_prefix('\x1b')?;
    if rest.is_empty() {
        return None;
    }
    // When Kitty protocol is active, ambiguous legacy sequences are
    // reinterpreted as custom terminal mappings:
    //   \x1b\r = shift+enter (Kitty/Ghostty mapping), not alt+enter
    //   \x1b<space> = not a valid input (Kitty sends CSI-u instead)
    if is_kitty_protocol_active() {
        if rest == "\r" {
            return Some(modified(
                Key::Enter,
                KeyModifiers::SHIFT,
                KeyEventKind::Press,
            ));
        }
        if rest == " " {
            return None;
        }
    }
    let mut event = parse_key(rest)?;
    if event.modifiers.is_empty() {
        event.modifiers.insert(KeyModifiers::ALT);
        Some(event)
    } else if !is_kitty_protocol_active()
        && rest.len() == 1
        && rest.as_bytes()[0] >= 1
        && rest.as_bytes()[0] <= 26
    {
        // Legacy: \x1b followed by a control character (byte 1-26) is
        // ctrl+alt+letter, not alt+ctrl+letter on a non-control char.
        event.modifiers.insert(KeyModifiers::ALT);
        Some(event)
    } else {
        None
    }
}

fn parse_kitty_csi_u(data: &str) -> Option<KeyEvent> {
    let body = data.strip_prefix("\x1b[")?.strip_suffix('u')?;
    let (codepoint_part, modifier_part) = match body.split_once(';') {
        Some(parts) => parts,
        None => (body, "1"),
    };

    // Parse codepoint:shifted:base format (Kitty flag 4 alternate keys)
    let mut cp_parts = codepoint_part.split(':');
    let codepoint: u32 = cp_parts.next()?.parse().ok()?;
    let _shifted_key: Option<u32> = cp_parts
        .next()
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse().ok());
    let _base_layout_key: Option<u32> = cp_parts
        .next()
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse().ok());

    let (modifier_value, event_type) = match modifier_part.split_once(':') {
        Some((modifier, event_type)) => (
            modifier.parse::<u8>().ok()?,
            parse_event_type(Some(event_type)),
        ),
        None => (modifier_part.parse::<u8>().ok()?, KeyEventKind::Press),
    };

    let modifiers = kitty_modifiers(modifier_value);
    let normalized_cp = normalize_kitty_functional_codepoint(codepoint);
    let mut key = key_from_codepoint(normalized_cp)?;

    // Normalize uppercase ASCII letters: when SHIFT is held and the terminal
    // reports the uppercase codepoint, convert to lowercase and keep SHIFT.
    // This matches TS normalizeShiftedLetterIdentityCodepoint.
    if let Key::Char(character) = &mut key
        && modifiers.contains(KeyModifiers::SHIFT)
        && character.chars().all(|ch| ch.is_ascii_uppercase())
    {
        *character = character.to_ascii_lowercase();
    }

    Some(modified(key, modifiers, event_type))
}

fn parse_modifier_and_kind(params: &str) -> Option<(KeyModifiers, KeyEventKind)> {
    let (modifier, event_type) = match params.split_once(':') {
        Some((modifier, event_type)) => (modifier, Some(event_type)),
        None => (params, None),
    };
    Some((
        kitty_modifiers(modifier.parse::<u8>().ok()?),
        parse_event_type(event_type),
    ))
}

fn parse_event_type(value: Option<&str>) -> KeyEventKind {
    match value.and_then(|value| value.parse::<u8>().ok()) {
        Some(2) => KeyEventKind::Repeat,
        Some(3) => KeyEventKind::Release,
        _ => KeyEventKind::Press,
    }
}

fn kitty_modifiers(value: u8) -> KeyModifiers {
    let mask = value.saturating_sub(1);
    let mut modifiers = KeyModifiers::empty();
    if mask & 0b0001 != 0 {
        modifiers.insert(KeyModifiers::SHIFT);
    }
    if mask & 0b0010 != 0 {
        modifiers.insert(KeyModifiers::ALT);
    }
    if mask & 0b0100 != 0 {
        modifiers.insert(KeyModifiers::CTRL);
    }
    if mask & 0b1000 != 0 {
        modifiers.insert(KeyModifiers::SUPER);
    }
    modifiers
}

fn key_from_codepoint(codepoint: u32) -> Option<Key> {
    match codepoint {
        9 => Some(Key::Tab),
        13 | 57414 => Some(Key::Enter),
        27 => Some(Key::Escape),
        32 => Some(Key::Space),
        127 => Some(Key::Backspace),
        // Negative codepoints for navigation / functional keys (from normalize_kitty_functional_codepoint)
        0xFFFF_FFFF => Some(Key::Left),
        0xFFFF_FFFE => Some(Key::Right),
        0xFFFF_FFFD => Some(Key::Up),
        0xFFFF_FFFC => Some(Key::Down),
        0xFFFF_FFF6 => Some(Key::PageUp),
        0xFFFF_FFF5 => Some(Key::PageDown),
        0xFFFF_FFF4 => Some(Key::Home),
        0xFFFF_FFF3 => Some(Key::End),
        0xFFFF_FFF2 => Some(Key::Insert),
        0xFFFF_FFF1 => Some(Key::Delete),
        value => char::from_u32(value).map(|ch| Key::Char(ch.to_string())),
    }
}

fn normalize_kitty_functional_codepoint(codepoint: u32) -> u32 {
    match codepoint {
        57399 => b'0' as u32,
        57400 => b'1' as u32,
        57401 => b'2' as u32,
        57402 => b'3' as u32,
        57403 => b'4' as u32,
        57404 => b'5' as u32,
        57405 => b'6' as u32,
        57406 => b'7' as u32,
        57407 => b'8' as u32,
        57408 => b'9' as u32,
        57409 => b'.' as u32,
        57410 => b'/' as u32,
        57411 => b'*' as u32,
        57412 => b'-' as u32,
        57413 => b'+' as u32,
        57415 => b'=' as u32,
        57416 => b',' as u32,
        57417 => 0xFFFF_FFFF, // left
        57418 => 0xFFFF_FFFE, // right
        57419 => 0xFFFF_FFFD, // up
        57420 => 0xFFFF_FFFC, // down
        57421 => 0xFFFF_FFF6, // pageUp
        57422 => 0xFFFF_FFF5, // pageDown
        57423 => 0xFFFF_FFF4, // home
        57424 => 0xFFFF_FFF3, // end
        57425 => 0xFFFF_FFF2, // insert
        57426 => 0xFFFF_FFF1, // delete
        other => other,
    }
}

fn parse_modify_other_keys(data: &str) -> Option<KeyEvent> {
    // xterm modifyOtherKeys format: CSI 27 ; modifiers ; keycode ~
    let body = data.strip_prefix("\x1b[27;")?.strip_suffix('~')?;
    let (mod_str, codepoint_str) = body.split_once(';')?;
    let mod_value: u8 = mod_str.parse().ok()?;
    let codepoint: u32 = codepoint_str.parse().ok()?;
    let modifiers = kitty_modifiers(mod_value);
    let key = key_from_codepoint(codepoint)?;
    Some(modified(key, modifiers, KeyEventKind::Press))
}

fn parse_key_id(key_id: &str) -> Option<(Key, KeyModifiers)> {
    let mut modifiers = KeyModifiers::empty();
    let mut key = None;

    for part in key_id.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "shift" => modifiers.insert(KeyModifiers::SHIFT),
            "alt" => modifiers.insert(KeyModifiers::ALT),
            "ctrl" => modifiers.insert(KeyModifiers::CTRL),
            "super" => modifiers.insert(KeyModifiers::SUPER),
            value => key = Some(parse_key_name(value)),
        }
    }

    Some((key??, modifiers))
}

fn parse_key_name(name: &str) -> Option<Key> {
    Some(match name {
        "escape" | "esc" => Key::Escape,
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "space" => Key::Space,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "clear" => Key::Clear,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        value if value.len() == 2 && value.starts_with('f') => {
            Key::Function(value[1..].parse().ok()?)
        }
        value if value.len() == 3 && value.starts_with('f') => {
            Key::Function(value[1..].parse().ok()?)
        }
        value => Key::Char(value.to_string()),
    })
}

fn keys_equal(actual: &Key, expected: &Key) -> bool {
    match (actual, expected) {
        (Key::Char(actual), Key::Char(expected)) => {
            if actual.eq_ignore_ascii_case(expected) {
                return true;
            }
            // ctrl+- and ctrl+_ share the same control character (byte 31)
            // because - and _ are on the same physical key on US keyboards.
            if (actual == "-" && expected == "_") || (actual == "_" && expected == "-") {
                return true;
            }
            false
        }
        // Space key can be represented as Key::Space or Key::Char(" ")
        (Key::Space, Key::Char(s)) | (Key::Char(s), Key::Space) => s == " ",
        (actual, expected) => actual == expected,
    }
}
