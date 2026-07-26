//! Generic terminal operation behavior.

use tui::api::terminal::{Terminal, TerminalSize};
use tui::api::testing::{TerminalOp, VirtualTerminal};

#[test]
fn virtual_terminal_records_operations() {
    let mut terminal = VirtualTerminal::new(12, 4);

    terminal.hide_cursor().unwrap();
    terminal.write("hello").unwrap();
    terminal.move_by(-2).unwrap();
    terminal.clear_from_cursor().unwrap();
    terminal.flush().unwrap();

    assert_eq!(
        terminal.size(),
        TerminalSize {
            columns: 12,
            rows: 4
        }
    );
    assert_eq!(
        terminal.ops(),
        &[
            TerminalOp::HideCursor,
            TerminalOp::Write("hello".to_string()),
            TerminalOp::MoveBy(-2),
            TerminalOp::ClearFromCursor,
            TerminalOp::Flush,
        ]
    );
    assert!(!terminal.cursor_visible());
    assert_eq!(terminal.cursor_row(), 0);
    assert_eq!(terminal.cursor_col(), 5);
    assert_eq!(terminal.clear_screen_count(), 0);
    assert_eq!(terminal.writes(), &["hello".to_string()]);
}

#[test]
fn virtual_terminal_can_resize_and_clear_ops() {
    let mut terminal = VirtualTerminal::new(12, 4);
    terminal.write("hello").unwrap();
    terminal.resize(20, 8);
    terminal.clear_ops();

    assert_eq!(
        terminal.size(),
        TerminalSize {
            columns: 20,
            rows: 8
        }
    );
    assert!(terminal.ops().is_empty());
}

#[test]
fn virtual_terminal_tracks_cursor_state_and_clear_screen_calls() {
    let mut terminal = VirtualTerminal::new(12, 4);

    terminal.write("abc\r\nde").unwrap();
    terminal.move_by(1).unwrap();
    terminal.move_to_column(2).unwrap();
    terminal.show_cursor().unwrap();
    terminal.clear_screen().unwrap();

    assert!(terminal.cursor_visible());
    assert_eq!(terminal.cursor_row(), 0);
    assert_eq!(terminal.cursor_col(), 0);
    assert_eq!(terminal.clear_screen_count(), 1);
}
