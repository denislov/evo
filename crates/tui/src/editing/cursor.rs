use crate::render::visible_width;

pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPosition {
    pub row: usize,
    pub col: usize,
}

pub fn extract_cursor_marker(
    lines: &mut [String],
    terminal_height: usize,
) -> Option<CursorPosition> {
    let start = lines.len().saturating_sub(terminal_height);
    for row in (start..lines.len()).rev() {
        let Some(byte_index) = lines[row].find(CURSOR_MARKER) else {
            continue;
        };
        let col = visible_width(&lines[row][..byte_index]);
        lines[row].replace_range(byte_index..byte_index + CURSOR_MARKER.len(), "");
        return Some(CursorPosition { row, col });
    }
    None
}
