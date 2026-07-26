use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use tui::api::component::Component;
use tui::api::input::InputEvent;

#[derive(Default)]
struct TransientOverlayState {
    lines: Vec<String>,
    pending_input: VecDeque<InputEvent>,
    focused: bool,
}

#[derive(Clone, Default)]
pub(super) struct TransientOverlayBridge {
    state: Rc<RefCell<TransientOverlayState>>,
}

impl TransientOverlayBridge {
    pub(super) fn component(&self) -> TransientOverlay {
        TransientOverlay {
            bridge: self.clone(),
        }
    }

    pub(super) fn set_lines(&self, lines: Vec<String>) {
        self.state.borrow_mut().lines = lines;
    }

    pub(super) fn take_pending_input(&self) -> Vec<InputEvent> {
        self.state.borrow_mut().pending_input.drain(..).collect()
    }

    #[cfg(test)]
    pub(super) fn focused(&self) -> bool {
        self.state.borrow().focused
    }
}

pub(super) struct TransientOverlay {
    bridge: TransientOverlayBridge,
}

impl Component for TransientOverlay {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.bridge.state.borrow().lines.clone()
    }

    fn handle_input(&mut self, event: &InputEvent) {
        self.bridge
            .state
            .borrow_mut()
            .pending_input
            .push_back(event.clone());
    }

    fn set_focused(&mut self, focused: bool) {
        self.bridge.state.borrow_mut().focused = focused;
    }

    fn focused(&self) -> bool {
        self.bridge.state.borrow().focused
    }
}

#[cfg(test)]
mod tests {
    use tui::api::component::Component;
    use tui::api::input::{InputEvent, parse_key};

    use super::TransientOverlayBridge;

    #[test]
    fn component_projects_lines_and_queues_focused_input() {
        let bridge = TransientOverlayBridge::default();
        bridge.set_lines(vec!["authorization".into()]);
        let mut component = bridge.component();

        component.set_focused(true);
        component.handle_input(&InputEvent::Key(parse_key("enter").unwrap()));

        assert!(bridge.focused());
        assert_eq!(component.render(40), vec!["authorization"]);
        assert_eq!(bridge.take_pending_input().len(), 1);
        assert!(bridge.take_pending_input().is_empty());
    }
}
