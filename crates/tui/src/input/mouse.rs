use bitflags::bitflags;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    Down(MouseButton),
    Up(MouseButton),
    Drag(MouseButton),
    Moved,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct MouseModifiers: u8 {
        const SHIFT = 0b001;
        const ALT = 0b010;
        const CTRL = 0b100;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseEventKind,
    pub column: usize,
    pub row: usize,
    pub modifiers: MouseModifiers,
}

pub fn parse_sgr_mouse(data: &str) -> Option<MouseEvent> {
    let body = data.strip_prefix("\x1b[<")?;
    let final_byte = body.chars().next_back()?;
    if !matches!(final_byte, 'M' | 'm') {
        return None;
    }
    let body = &body[..body.len().checked_sub(final_byte.len_utf8())?];
    let mut fields = body.split(';');
    let code = fields.next()?.parse::<u16>().ok()?;
    let column = fields.next()?.parse::<usize>().ok()?.checked_sub(1)?;
    let row = fields.next()?.parse::<usize>().ok()?.checked_sub(1)?;
    if fields.next().is_some() {
        return None;
    }

    let mut modifiers = MouseModifiers::empty();
    if code & 4 != 0 {
        modifiers.insert(MouseModifiers::SHIFT);
    }
    if code & 8 != 0 {
        modifiers.insert(MouseModifiers::ALT);
    }
    if code & 16 != 0 {
        modifiers.insert(MouseModifiers::CTRL);
    }

    let button_code = code & 0b11;
    let kind = if code & 64 != 0 {
        match button_code {
            0 => MouseEventKind::ScrollUp,
            1 => MouseEventKind::ScrollDown,
            2 => MouseEventKind::ScrollLeft,
            3 => MouseEventKind::ScrollRight,
            _ => unreachable!(),
        }
    } else if final_byte == 'm' {
        MouseEventKind::Up(mouse_button(button_code)?)
    } else if code & 32 != 0 {
        match mouse_button(button_code) {
            Some(button) => MouseEventKind::Drag(button),
            None => MouseEventKind::Moved,
        }
    } else {
        MouseEventKind::Down(mouse_button(button_code)?)
    };

    Some(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    })
}

fn mouse_button(code: u16) -> Option<MouseButton> {
    match code {
        0 => Some(MouseButton::Left),
        1 => Some(MouseButton::Middle),
        2 => Some(MouseButton::Right),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sgr_buttons_motion_wheel_and_zero_based_coordinates() {
        assert_eq!(
            parse_sgr_mouse("\x1b[<0;12;4M"),
            Some(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 11,
                row: 3,
                modifiers: MouseModifiers::empty(),
            })
        );
        assert_eq!(
            parse_sgr_mouse("\x1b[<20;2;3m").unwrap().kind,
            MouseEventKind::Up(MouseButton::Left)
        );
        assert_eq!(
            parse_sgr_mouse("\x1b[<34;8;9M").unwrap().kind,
            MouseEventKind::Drag(MouseButton::Right)
        );
        assert_eq!(
            parse_sgr_mouse("\x1b[<35;8;9M").unwrap().kind,
            MouseEventKind::Moved
        );
        assert_eq!(
            parse_sgr_mouse("\x1b[<65;8;9M").unwrap().kind,
            MouseEventKind::ScrollDown
        );
    }

    #[test]
    fn parses_sgr_modifiers_and_rejects_malformed_coordinates() {
        let event = parse_sgr_mouse("\x1b[<28;7;5M").unwrap();
        assert_eq!(event.kind, MouseEventKind::Down(MouseButton::Left));
        assert_eq!(
            event.modifiers,
            MouseModifiers::SHIFT | MouseModifiers::ALT | MouseModifiers::CTRL
        );
        assert!(parse_sgr_mouse("\x1b[<0;0;1M").is_none());
        assert!(parse_sgr_mouse("\x1b[<0;1M").is_none());
        assert!(parse_sgr_mouse("\x1b[A").is_none());
    }
}
