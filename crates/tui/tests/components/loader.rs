//! Loader and cancellable-loader component behavior.

use std::cell::Cell;
use std::rc::Rc;

use tui::api::component::{CancellableLoader, Component, Loader, LoaderIndicatorOptions};
use tui::api::input::{KeybindingsManager, StdinBuffer, TUI_KEYBINDINGS};
use tui::api::render::visible_width;

fn feed(loader: &mut CancellableLoader, data: &str) {
    let mut buffer = StdinBuffer::new();
    let mut events = buffer.process(data);
    events.extend(buffer.flush());
    for event in events {
        loader.handle_input(&event);
    }
}

#[test]
fn loader_renders_message_with_default_spinner_and_padding() {
    let mut loader = Loader::new("Loading...");
    assert_eq!(loader.render(20), vec!["⠋ Loading...        "]);
}

#[test]
fn loader_tick_advances_indicator_frame() {
    let mut loader = Loader::new("Working");
    loader.tick();
    assert_eq!(loader.render(20), vec!["⠙ Working           "]);
}

#[test]
fn loader_supports_custom_indicator_and_message_updates() {
    let mut loader = Loader::new("Starting");
    loader.set_indicator(LoaderIndicatorOptions {
        frames: vec![".".to_string(), "o".to_string()],
    });
    loader.set_message("Running");
    assert_eq!(loader.render(12), vec![". Running   "]);
    loader.tick();
    assert_eq!(loader.render(12), vec!["o Running   "]);
}

#[test]
fn loader_can_hide_indicator() {
    let mut loader = Loader::new("Quiet");
    loader.set_indicator(LoaderIndicatorOptions { frames: Vec::new() });
    assert_eq!(loader.render(10), vec!["Quiet     "]);
}

#[test]
fn loader_truncates_to_render_width() {
    let mut loader = Loader::new("A very long loading message");
    let lines = loader.render(10);
    assert_eq!(lines.len(), 1);
    assert!(visible_width(&lines[0]) <= 10);
}

#[test]
fn cancellable_loader_sets_aborted_and_invokes_callback_on_escape() {
    let called = Rc::new(Cell::new(false));
    let called_for_callback = Rc::clone(&called);
    let mut loader = CancellableLoader::new(
        Loader::new("Working"),
        KeybindingsManager::new(TUI_KEYBINDINGS.clone(), Default::default()),
    );
    loader.set_on_abort(Box::new(move || called_for_callback.set(true)));

    feed(&mut loader, "\x1b");

    assert!(loader.aborted());
    assert!(called.get());
}

#[test]
fn cancellable_loader_invokes_callback_only_once() {
    let count = Rc::new(Cell::new(0));
    let count_for_callback = Rc::clone(&count);
    let mut loader = CancellableLoader::new(
        Loader::new("Working"),
        KeybindingsManager::new(TUI_KEYBINDINGS.clone(), Default::default()),
    );
    loader.set_on_abort(Box::new(move || {
        count_for_callback.set(count_for_callback.get() + 1)
    }));

    feed(&mut loader, "\x1b");
    feed(&mut loader, "\x1b");

    assert!(loader.aborted());
    assert_eq!(count.get(), 1);
}
