//! Render scheduler and runtime behavior.

use std::time::{Duration, Instant};

use tui::api::component::Component;
use tui::api::input::{InputEvent, Key, KeyEvent, KeyEventKind, KeyModifiers};
use tui::api::render::{RenderScheduler, Tui};
use tui::api::testing::VirtualTerminal;

const RENDER_SCHEDULER_MIN_INTERVAL: Duration = Duration::from_millis(16);
const RENDER_SCHEDULER_BEFORE_INTERVAL: Duration = Duration::from_millis(8);
const RENDER_SCHEDULER_PENDING_OFFSET: Duration = Duration::from_millis(5);

fn render_scheduler_clock_anchor() -> Instant {
    Instant::now()
}

#[derive(Default)]
struct RecordingComponent {
    focused: bool,
    inputs: Vec<InputEvent>,
}

impl Component for RecordingComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec![if self.focused { "focused" } else { "idle" }.to_string()]
    }

    fn handle_input(&mut self, event: &InputEvent) {
        self.inputs.push(event.clone());
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn focused(&self) -> bool {
        self.focused
    }
}

#[test]
fn focused_component_receives_input() {
    let terminal = VirtualTerminal::new(20, 5);
    let mut tui = Tui::new(terminal);
    let id = tui.add_child_with_id(Box::new(RecordingComponent::default()));
    tui.set_focus(Some(id));
    tui.dispatch_input(&InputEvent::Key(KeyEvent {
        key: Key::Char("x".to_string()),
        modifiers: KeyModifiers::empty(),
        kind: KeyEventKind::Press,
    }));
    let component = tui.component_as::<RecordingComponent>(id).unwrap();
    assert_eq!(component.inputs.len(), 1);
    assert!(component.focused);
}

#[test]
fn render_scheduler_coalesces_requests_until_interval_elapses() {
    let mut scheduler = RenderScheduler::new(RENDER_SCHEDULER_MIN_INTERVAL);
    let start = render_scheduler_clock_anchor();
    scheduler.request(false);
    assert!(scheduler.should_render_now(start));
    assert!(scheduler.mark_rendered(start));

    scheduler.request(false);
    assert!(!scheduler.should_render_now(start + RENDER_SCHEDULER_BEFORE_INTERVAL));
    assert!(scheduler.should_render_now(start + RENDER_SCHEDULER_MIN_INTERVAL));
}

#[test]
fn render_scheduler_reports_next_pending_deadline() {
    let mut scheduler = RenderScheduler::new(RENDER_SCHEDULER_MIN_INTERVAL);
    let start = render_scheduler_clock_anchor();

    assert!(!scheduler.has_pending());
    assert_eq!(scheduler.next_render_at(start), None);

    scheduler.request(false);
    assert!(scheduler.has_pending());
    assert_eq!(scheduler.next_render_at(start), Some(start));
    assert!(scheduler.mark_rendered(start));

    scheduler.request(false);
    assert_eq!(
        scheduler.next_render_at(start + RENDER_SCHEDULER_PENDING_OFFSET),
        Some(start + RENDER_SCHEDULER_MIN_INTERVAL)
    );

    scheduler.request(true);
    assert_eq!(
        scheduler.next_render_at(start + RENDER_SCHEDULER_PENDING_OFFSET),
        Some(start + RENDER_SCHEDULER_PENDING_OFFSET)
    );
}
