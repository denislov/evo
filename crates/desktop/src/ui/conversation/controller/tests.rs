//! Conversation controller unit tests: row indices, scrolling, and bounded
//! indexed updates.

use super::{adjacent_conversation_index, distance_to_bottom, upsert_indexed_item};

#[test]
fn bottom_distance_matches_negative_gpui_offsets() {
    assert_eq!(distance_to_bottom(0.0, 640.0), 640.0);
    assert_eq!(distance_to_bottom(-400.0, 640.0), 240.0);
    assert_eq!(distance_to_bottom(-640.0, 640.0), 0.0);
    assert_eq!(distance_to_bottom(-641.0, 640.0), 0.0);
    assert_eq!(distance_to_bottom(4.0, 0.0), 0.0);
}

#[test]
fn keyboard_selection_is_bounded_and_predictable() {
    assert_eq!(adjacent_conversation_index(0, None, false), None);
    assert_eq!(adjacent_conversation_index(4, None, false), Some(0));
    assert_eq!(adjacent_conversation_index(4, None, true), Some(3));
    assert_eq!(adjacent_conversation_index(4, Some(2), false), Some(3));
    assert_eq!(adjacent_conversation_index(4, Some(3), false), Some(3));
    assert_eq!(adjacent_conversation_index(4, Some(1), true), Some(0));
    assert_eq!(adjacent_conversation_index(4, Some(0), true), Some(0));
    assert_eq!(adjacent_conversation_index(4, Some(99), false), Some(0));
}

#[test]
fn indexed_update_accepts_non_clone_history_and_changes_one_slot() {
    #[derive(Debug, PartialEq, Eq)]
    struct NonClone(usize);

    let mut rows = (0..=10_000).map(NonClone).collect::<Vec<_>>();
    let capacity = rows.capacity();
    let index = upsert_indexed_item(&mut rows, Some(10_000), 10_000, NonClone(42));
    assert_eq!(index, 10_000);
    assert_eq!(rows.len(), 10_001);
    assert_eq!(rows[0], NonClone(0));
    assert_eq!(rows[9_999], NonClone(9_999));
    assert_eq!(rows[10_000], NonClone(42));
    assert_eq!(rows.capacity(), capacity);

    let append_index = rows.len();
    let appended = upsert_indexed_item(&mut rows, None, append_index, NonClone(43));
    assert_eq!(appended, 10_001);
    assert_eq!(rows[10_001], NonClone(43));
}
