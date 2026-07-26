//! Rectangular layout, clipping, frame composition, and focus traversal.

use tui::api::render::{
    Axis, Constraint, FocusRing, Frame, HitMap, HitRegion, Layout, Point, Rect, visible_width,
};

#[test]
fn layout_splits_fixed_percentage_and_fill_constraints_deterministically() {
    let bounds = Rect::new(2, 3, 20, 8);
    assert_eq!(
        Layout::horizontal(
            bounds,
            &[
                Constraint::Length(3),
                Constraint::Percentage(25),
                Constraint::Fill(1),
                Constraint::Fill(2),
            ],
        ),
        [
            Rect::new(2, 3, 3, 8),
            Rect::new(5, 3, 5, 8),
            Rect::new(10, 3, 4, 8),
            Rect::new(14, 3, 8, 8),
        ]
    );
}

#[test]
fn layout_clips_constraints_that_exceed_available_space() {
    assert_eq!(
        Layout::split(
            Rect::new(0, 0, 4, 3),
            Axis::Vertical,
            &[
                Constraint::Length(2),
                Constraint::Length(4),
                Constraint::Fill(1)
            ],
        ),
        [
            Rect::new(0, 0, 4, 2),
            Rect::new(0, 2, 4, 1),
            Rect::new(0, 3, 4, 0),
        ]
    );
}

#[test]
fn frame_clips_content_to_rectangles_without_changing_dimensions() {
    let mut frame = Frame::new(10, 4);
    frame.draw(
        Rect::new(1, 1, 4, 2),
        &[
            "abcdef".to_string(),
            "好xy".to_string(),
            "hidden".to_string(),
        ],
    );
    let lines = frame.into_lines();

    assert_eq!(lines.len(), 4);
    assert!(lines[1].contains("abcd"), "{:?}", lines[1]);
    assert!(!lines[1].contains("ef"), "{:?}", lines[1]);
    assert!(lines[2].contains("好xy"), "{:?}", lines[2]);
    assert!(lines.iter().all(|line| visible_width(line) <= 10));
}

#[test]
fn frame_overlap_preserves_ansi_sequence_boundaries() {
    let mut frame = Frame::new(12, 1);
    frame.draw(
        Rect::new(0, 0, 12, 1),
        &["\x1b[31mabcdefghijkl\x1b[0m".to_string()],
    );
    frame.draw(Rect::new(5, 0, 3, 1), &["XYZ".to_string()]);
    let line = frame.into_lines().remove(0);

    assert_eq!(visible_width(&line), 12, "{line:?}");
    assert!(line.contains("abcde"), "{line:?}");
    assert!(line.contains("XYZ"), "{line:?}");
    assert!(line.contains("ijkl"), "{line:?}");
}

#[test]
fn focus_ring_preserves_visible_focus_and_wraps() {
    let mut ring = FocusRing::new(["conversation", "context", "composer"]);
    assert_eq!(ring.current(), Some("conversation"));
    assert_eq!(ring.focus_next(), Some("context"));
    assert_eq!(ring.focus_next(), Some("composer"));
    assert_eq!(ring.focus_next(), Some("conversation"));
    assert_eq!(ring.focus_previous(), Some("composer"));

    ring.set_items(["conversation", "composer"]);
    assert_eq!(ring.current(), Some("composer"));
    assert!(!ring.focus("context"));
}

#[test]
fn hit_map_uses_frame_coordinates_and_last_region_wins_overlap() {
    let mut hits = HitMap::new();
    hits.push(HitRegion::new(Rect::new(2, 3, 8, 4), "panel"));
    hits.push(HitRegion::new(Rect::new(4, 4, 2, 1), "control"));

    assert_eq!(hits.hit(Point::new(2, 3)), Some(&"panel"));
    assert_eq!(hits.hit(Point::new(4, 4)), Some(&"control"));
    assert_eq!(hits.hit(Point::new(10, 4)), None);
    assert_eq!(hits.regions().len(), 2);

    hits.clear();
    assert!(hits.hit(Point::new(4, 4)).is_none());
}
