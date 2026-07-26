//! Settings-list and submenu behavior.

use std::cell::RefCell;
use std::rc::Rc;

use tui::api::component::{
    Component, SettingItem, SettingsList, SettingsListOptions, SettingsSubmenuDone,
};
use tui::api::input::{InputEvent, KeybindingsManager, StdinBuffer, TUI_KEYBINDINGS, matches_key};
use tui::api::render::visible_width;

fn keybindings() -> KeybindingsManager {
    KeybindingsManager::new(TUI_KEYBINDINGS.clone(), Default::default())
}

fn feed(list: &mut SettingsList, data: &str) {
    let mut buffer = StdinBuffer::new();
    let mut events = buffer.process(data);
    events.extend(buffer.flush());
    for event in events {
        list.handle_input(&event);
    }
}

#[test]
fn settings_list_renders_values_and_description_with_bounded_width() {
    let mut list = SettingsList::new(
        vec![
            SettingItem::new("model", "Model", "sonnet").description("Choose the active model"),
            SettingItem::new("theme", "Theme", "dark"),
        ],
        5,
        keybindings(),
    );

    let lines = list.render(24);

    assert_eq!(lines[0], "> Model  sonnet         ");
    assert!(lines.iter().any(|line| line.contains("Choose the active")));
    assert!(lines.iter().all(|line| visible_width(line) <= 24));
}

#[test]
fn settings_list_navigation_wraps_and_selected_item_updates() {
    let mut list = SettingsList::new(
        vec![
            SettingItem::new("model", "Model", "sonnet"),
            SettingItem::new("theme", "Theme", "dark"),
        ],
        5,
        keybindings(),
    );

    feed(&mut list, "\x1b[A");
    assert_eq!(list.selected_item().unwrap().id, "theme");
    feed(&mut list, "\x1b[B");
    assert_eq!(list.selected_item().unwrap().id, "model");
}

#[test]
fn settings_list_cycles_values_and_invokes_on_change() {
    let changes = Rc::new(RefCell::new(Vec::new()));
    let changes_for_callback = Rc::clone(&changes);
    let mut list = SettingsList::new(
        vec![SettingItem::new("theme", "Theme", "dark").values(["dark", "light"])],
        5,
        keybindings(),
    );
    list.set_on_change(Box::new(move |id, value| {
        changes_for_callback
            .borrow_mut()
            .push((id.to_string(), value.to_string()));
    }));

    feed(&mut list, "\r");

    assert_eq!(list.selected_item().unwrap().current_value, "light");
    assert_eq!(
        changes.borrow().as_slice(),
        &[("theme".to_string(), "light".to_string())]
    );
}

#[test]
fn settings_list_search_filters_with_fuzzy_matching() {
    let mut list = SettingsList::with_options(
        vec![
            SettingItem::new("model", "Model Selector", "sonnet"),
            SettingItem::new("theme", "Theme", "dark"),
        ],
        5,
        keybindings(),
        SettingsListOptions {
            enable_search: true,
        },
    );

    feed(&mut list, "mdl");

    assert_eq!(list.selected_item().unwrap().id, "model");
    let lines = list.render(24);
    assert!(lines.iter().any(|line| line.contains("Search: mdl")));
    assert!(lines.iter().all(|line| visible_width(line) <= 24));
}

#[test]
fn settings_list_escape_invokes_cancel_each_time() {
    let count = Rc::new(RefCell::new(0));
    let count_for_callback = Rc::clone(&count);
    let mut list = SettingsList::new(
        vec![SettingItem::new("theme", "Theme", "dark")],
        5,
        keybindings(),
    );
    list.set_on_cancel(Box::new(move || {
        *count_for_callback.borrow_mut() += 1;
    }));

    feed(&mut list, "\x1b");
    feed(&mut list, "\x1b");

    assert_eq!(*count.borrow(), 2);
}

struct DoneSubmenu {
    done: Option<SettingsSubmenuDone>,
}

impl Component for DoneSubmenu {
    fn render(&mut self, width: usize) -> Vec<String> {
        vec![format!("submenu{}", " ".repeat(width.saturating_sub(7)))]
    }

    fn handle_input(&mut self, event: &InputEvent) {
        if matches_key(event, "enter")
            && let Some(mut done) = self.done.take()
        {
            done(Some("light".to_string()));
        }
    }
}

#[test]
fn settings_list_opens_submenu_and_applies_done_value() {
    let changes = Rc::new(RefCell::new(Vec::new()));
    let changes_for_callback = Rc::clone(&changes);
    let mut list = SettingsList::new(
        vec![SettingItem::new("theme", "Theme", "dark")],
        5,
        keybindings(),
    );
    list.set_on_change(Box::new(move |id, value| {
        changes_for_callback
            .borrow_mut()
            .push((id.to_string(), value.to_string()));
    }));
    list.set_submenu_factory(
        "theme",
        Box::new(|current_value, done| {
            assert_eq!(current_value, "dark");
            Box::new(DoneSubmenu { done: Some(done) })
        }),
    );

    feed(&mut list, "\r");
    assert_eq!(list.render(12), vec!["submenu     ".to_string()]);

    feed(&mut list, "\r");

    assert_eq!(list.selected_item().unwrap().current_value, "light");
    assert_eq!(
        changes.borrow().as_slice(),
        &[("theme".to_string(), "light".to_string())]
    );
}
