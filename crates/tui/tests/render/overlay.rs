//! Overlay composition behavior.

use std::sync::{Arc, Mutex};

use tui::api::component::{Component, OverlayAnchor, OverlayMargin, OverlayOptions, SizeValue};
use tui::api::input::{InputEvent, MouseButton, MouseEvent, MouseEventKind, MouseModifiers};
use tui::api::render::Tui;
use tui::api::testing::VirtualTerminal;

struct Lines(Vec<String>);

impl Component for Lines {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.0.clone()
    }
}

#[test]
fn centered_overlay_is_composited_over_base_lines() {
    let mut tui = Tui::new(VirtualTerminal::new(10, 5));
    tui.add_child(Box::new(Lines(vec![
        "..........".to_string(),
        "..........".to_string(),
        "..........".to_string(),
    ])));
    tui.show_overlay(
        Box::new(Lines(vec!["XX".to_string()])),
        OverlayOptions {
            anchor: OverlayAnchor::Center,
            width: Some(2.into()),
            ..Default::default()
        },
    );
    tui.render_once().unwrap();
    let output = tui.terminal().written_output();
    // SEGMENT_RESET is inserted between composite segments to prevent colour bleed.
    // The overlay content "XX" is separated from the base "...." by reset sequences.
    assert!(
        output.contains("XX"),
        "expected overlay content 'XX' in output, got: {output:?}"
    );
    // The last line should contain 4 dots, then SEGMENT_RESET, then XX, then SEGMENT_RESET, then 4 dots.
    // Split by newlines and check the last non-empty line.
    let last_line = output.split('\n').rfind(|l| !l.is_empty()).unwrap_or("");
    assert!(
        last_line.starts_with("...."),
        "expected last line to start with 4 dots, got: {last_line:?}"
    );
    assert!(
        last_line.contains("XX"),
        "expected last line to contain XX, got: {last_line:?}"
    );
}

#[test]
fn overlay_composition_preserves_width_over_ansi_base_lines() {
    let mut tui = Tui::new(VirtualTerminal::new(10, 3));
    tui.add_child(Box::new(Lines(vec![
        "\x1b[31m..........\x1b[0m".to_string(),
        "\x1b]8;;https://example.com\x07link......\x1b]8;;\x07".to_string(),
    ])));
    tui.show_overlay(
        Box::new(Lines(vec!["XXXXXX".to_string()])),
        OverlayOptions {
            anchor: OverlayAnchor::Center,
            width: Some(6.into()),
            ..Default::default()
        },
    );

    tui.render_once().unwrap();

    assert!(
        tui.rendered_lines()
            .iter()
            .all(|line| tui::api::render::visible_width(line) <= 10),
        "{:?}",
        tui.rendered_lines()
    );
}

#[test]
fn hiding_overlay_restores_base_render() {
    let mut tui = Tui::new(VirtualTerminal::new(8, 4));
    tui.add_child(Box::new(Lines(vec!["base".to_string()])));
    let handle = tui.show_overlay(
        Box::new(Lines(vec!["menu".to_string()])),
        Default::default(),
    );
    handle.hide(&mut tui);
    tui.render_once().unwrap();
    assert!(tui.terminal().written_output().contains("base"));
    assert!(!tui.terminal().written_output().contains("menu"));
}

struct ViewportProbe {
    viewport: Arc<Mutex<Option<(usize, usize)>>>,
}

impl Component for ViewportProbe {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["one".into(), "two".into(), "three".into()]
    }

    fn set_viewport_size(&mut self, width: usize, height: usize) {
        *self.viewport.lock().unwrap() = Some((width, height));
    }
}

#[test]
fn overlay_host_uses_bounded_component_rendering() {
    let viewport = Arc::new(Mutex::new(None));
    let mut tui = Tui::new(VirtualTerminal::new(12, 5));
    tui.add_child(Box::new(Lines(vec!["base".to_string()])));
    tui.show_overlay(
        Box::new(ViewportProbe {
            viewport: Arc::clone(&viewport),
        }),
        OverlayOptions {
            width: Some(6.into()),
            max_height: Some(SizeValue::Columns(2)),
            ..Default::default()
        },
    );

    tui.render_once().unwrap();

    assert_eq!(*viewport.lock().unwrap(), Some((6, 2)));
    let output = tui.terminal().written_output();
    assert!(output.contains("one"), "{output:?}");
    assert!(output.contains("two"), "{output:?}");
    assert!(!output.contains("three"), "{output:?}");
}

#[test]
fn overlay_host_subtracts_vertical_margins_from_component_viewport() {
    let viewport = Arc::new(Mutex::new(None));
    let mut tui = Tui::new(VirtualTerminal::new(12, 8));
    tui.add_child(Box::new(Lines(vec!["base".to_string(); 8])));
    tui.show_overlay(
        Box::new(ViewportProbe {
            viewport: Arc::clone(&viewport),
        }),
        OverlayOptions {
            width: Some(6.into()),
            margin: OverlayMargin {
                top: 1,
                bottom: 4,
                ..Default::default()
            },
            ..Default::default()
        },
    );

    tui.render_once().unwrap();

    assert_eq!(*viewport.lock().unwrap(), Some((6, 3)));
}

#[test]
fn overlay_host_does_not_invade_margins_when_no_vertical_space_remains() {
    let viewport = Arc::new(Mutex::new(None));
    let mut tui = Tui::new(VirtualTerminal::new(12, 4));
    tui.add_child(Box::new(Lines(vec!["base".to_string(); 4])));
    tui.show_overlay(
        Box::new(ViewportProbe {
            viewport: Arc::clone(&viewport),
        }),
        OverlayOptions {
            width: Some(6.into()),
            margin: OverlayMargin {
                bottom: 4,
                ..Default::default()
            },
            ..Default::default()
        },
    );

    tui.render_once().unwrap();

    assert_eq!(*viewport.lock().unwrap(), None);
    assert!(!tui.terminal().written_output().contains("one"));
}

#[test]
fn overlay_options_can_be_updated_after_mount() {
    let viewport = Arc::new(Mutex::new(None));
    let mut tui = Tui::new(VirtualTerminal::new(12, 8));
    tui.add_child(Box::new(Lines(vec!["base".to_string(); 8])));
    let handle = tui.show_overlay(
        Box::new(ViewportProbe {
            viewport: Arc::clone(&viewport),
        }),
        OverlayOptions {
            width: Some(4.into()),
            ..Default::default()
        },
    );
    tui.set_overlay_options(
        handle,
        OverlayOptions {
            width: Some(7.into()),
            max_height: Some(2.into()),
            ..Default::default()
        },
    );

    tui.render_once().unwrap();

    assert_eq!(*viewport.lock().unwrap(), Some((7, 2)));
}

struct FocusProbe {
    focused: Arc<Mutex<bool>>,
}

impl Component for FocusProbe {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["focus".into()]
    }

    fn set_focused(&mut self, focused: bool) {
        *self.focused.lock().unwrap() = focused;
    }
}

#[test]
fn capturing_overlay_traps_and_restores_focus() {
    let base_focused = Arc::new(Mutex::new(false));
    let overlay_focused = Arc::new(Mutex::new(false));
    let mut tui = Tui::new(VirtualTerminal::new(12, 5));
    let base_id = tui.add_child_with_id(Box::new(FocusProbe {
        focused: Arc::clone(&base_focused),
    }));
    tui.set_focus(Some(base_id));
    let handle = tui.show_overlay(
        Box::new(FocusProbe {
            focused: Arc::clone(&overlay_focused),
        }),
        Default::default(),
    );

    handle.focus(&mut tui);
    assert!(!*base_focused.lock().unwrap());
    assert!(*overlay_focused.lock().unwrap());

    handle.hide(&mut tui);
    assert!(*base_focused.lock().unwrap());
    assert!(!*overlay_focused.lock().unwrap());
}

struct InputProbe {
    events: Arc<Mutex<Vec<InputEvent>>>,
}

impl Component for InputProbe {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["input".into()]
    }

    fn handle_input(&mut self, event: &InputEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

#[test]
fn capturing_overlay_receives_mouse_without_leaking_to_the_base_component() {
    let base_events = Arc::new(Mutex::new(Vec::new()));
    let overlay_events = Arc::new(Mutex::new(Vec::new()));
    let mut tui = Tui::new(VirtualTerminal::new(12, 5));
    let base_id = tui.add_child_with_id(Box::new(InputProbe {
        events: Arc::clone(&base_events),
    }));
    tui.set_focus(Some(base_id));
    let handle = tui.show_overlay(
        Box::new(InputProbe {
            events: Arc::clone(&overlay_events),
        }),
        Default::default(),
    );
    handle.focus(&mut tui);
    let event = InputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 4,
        row: 2,
        modifiers: MouseModifiers::empty(),
    });

    tui.dispatch_input(&event);

    assert!(base_events.lock().unwrap().is_empty());
    assert_eq!(overlay_events.lock().unwrap().as_slice(), &[event]);
}
