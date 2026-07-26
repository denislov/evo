//! Cursor extraction and visible-column behavior.

use tui::api::component::{CURSOR_MARKER, extract_cursor_marker};

#[test]
fn cursor_marker_is_stripped_and_column_is_visible_width() {
    let mut lines = vec![
        "before".to_string(),
        format!("a\x1b[31m好\x1b[0m{CURSOR_MARKER}z"),
    ];

    let cursor = extract_cursor_marker(&mut lines, 24).unwrap();
    assert_eq!(cursor.row, 1);
    assert_eq!(cursor.col, 3);
    assert_eq!(lines[1], "a\x1b[31m好\x1b[0mz");
}

#[test]
fn cursor_marker_only_scans_visible_viewport() {
    let mut lines = vec![format!("old{CURSOR_MARKER}"), "visible".to_string()];

    assert_eq!(extract_cursor_marker(&mut lines, 1), None);
    assert_eq!(lines[0], format!("old{CURSOR_MARKER}"));
}

#[test]
fn cursor_marker_prefers_bottom_visible_marker() {
    let mut lines = vec![
        format!("top{CURSOR_MARKER}"),
        format!("bottom{CURSOR_MARKER}"),
    ];

    let cursor = extract_cursor_marker(&mut lines, 2).unwrap();

    assert_eq!(cursor.row, 1);
    assert_eq!(cursor.col, 6);
    assert_eq!(lines[0], format!("top{CURSOR_MARKER}"));
    assert_eq!(lines[1], "bottom");
}
