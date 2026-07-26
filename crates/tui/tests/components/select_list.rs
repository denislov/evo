//! Select-list filtering and navigation behavior.

use tui::api::component::{Component, SelectItem, SelectList};
use tui::api::input::{KeybindingsManager, StdinBuffer, TUI_KEYBINDINGS};

fn feed(list: &mut SelectList, data: &str) {
    let mut buffer = StdinBuffer::new();
    for event in buffer.process(data) {
        list.handle_input(&event);
    }
}

#[test]
fn select_list_wraps_selection_and_filters_items() {
    let keybindings = KeybindingsManager::new(TUI_KEYBINDINGS.clone(), Default::default());
    let mut list = SelectList::new(
        vec![
            SelectItem::new("read", "read").description("Read a file"),
            SelectItem::new("write", "write").description("Write a file"),
        ],
        5,
        keybindings,
    );

    feed(&mut list, "\x1b[A");
    assert_eq!(list.selected_item().unwrap().value, "write");
    list.set_filter("r");
    assert_eq!(list.selected_item().unwrap().value, "read");
}

#[test]
fn select_list_renders_bounded_lines() {
    let keybindings = KeybindingsManager::new(TUI_KEYBINDINGS.clone(), Default::default());
    let mut list = SelectList::new(
        vec![
            SelectItem::new("very-long-command-name", "very-long-command-name")
                .description("long description"),
        ],
        5,
        keybindings,
    );

    for line in list.render(12) {
        assert!(tui::api::render::visible_width(&line) <= 12);
    }
}

#[test]
fn select_list_uses_fuzzy_filtering_for_non_contiguous_input() {
    let keybindings = KeybindingsManager::new(TUI_KEYBINDINGS.clone(), Default::default());
    let mut list = SelectList::new(
        vec![
            SelectItem::new("model-selector", "Model Selector"),
            SelectItem::new("session", "Session"),
        ],
        5,
        keybindings,
    );

    list.set_filter("mdl");
    assert_eq!(list.selected_item().unwrap().value, "model-selector");
}

#[test]
fn select_list_orders_fuzzy_matches_by_score() {
    let keybindings = KeybindingsManager::new(TUI_KEYBINDINGS.clone(), Default::default());
    let mut list = SelectList::new(
        vec![
            SelectItem::new("my-model", "My Model"),
            SelectItem::new("model", "Model"),
        ],
        5,
        keybindings,
    );

    list.set_filter("model");
    assert_eq!(list.selected_item().unwrap().value, "model");
}
