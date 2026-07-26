//! Selector-dialog behavior.

use std::cell::RefCell;
use std::rc::Rc;

use tui::api::component::{Component, SelectItem, SelectorDialog, SelectorDialogOptions};
use tui::api::input::{KeybindingsManager, StdinBuffer, TUI_KEYBINDINGS};
use tui::api::render::visible_width;

fn keybindings() -> KeybindingsManager {
    KeybindingsManager::new(TUI_KEYBINDINGS.clone(), Default::default())
}

fn feed(dialog: &mut SelectorDialog, data: &str) {
    let mut buffer = StdinBuffer::new();
    let mut events = buffer.process(data);
    events.extend(buffer.flush());
    for event in events {
        dialog.handle_input(&event);
    }
}

#[test]
fn selector_dialog_renders_title_help_and_bounded_select_list() {
    let mut dialog = SelectorDialog::new(
        "Model",
        vec![SelectItem::new("claude-haiku-4-5", "claude-haiku-4-5").description("Anthropic")],
        keybindings(),
        SelectorDialogOptions {
            max_visible: 5,
            help: Some("Enter confirm, Esc cancel".to_string()),
            ..SelectorDialogOptions::default()
        },
    );

    let lines = dialog.render(32);

    assert_eq!(lines[0], "Model");
    assert!(lines.iter().any(|line| line.contains("claude-haiku")));
    assert!(lines.iter().any(|line| line.contains("Esc cancel")));
    assert!(lines.iter().all(|line| visible_width(line) <= 32));
}

#[test]
fn selector_dialog_invokes_confirm_and_cancel_callbacks() {
    let confirmed = Rc::new(RefCell::new(Vec::new()));
    let confirmed_for_callback = Rc::clone(&confirmed);
    let canceled = Rc::new(RefCell::new(0));
    let canceled_for_callback = Rc::clone(&canceled);
    let mut dialog = SelectorDialog::new(
        "Theme",
        vec![
            SelectItem::new("dark", "Dark"),
            SelectItem::new("light", "Light"),
        ],
        keybindings(),
        SelectorDialogOptions::default(),
    );
    dialog.set_on_confirm(Box::new(move |item| {
        confirmed_for_callback
            .borrow_mut()
            .push(item.value.to_string());
    }));
    dialog.set_on_cancel(Box::new(move || {
        *canceled_for_callback.borrow_mut() += 1;
    }));

    feed(&mut dialog, "\x1b[B\r\x1b");

    assert_eq!(confirmed.borrow().as_slice(), &["light".to_string()]);
    assert_eq!(*canceled.borrow(), 1);
}
