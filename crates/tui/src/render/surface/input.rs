//! Input dispatch for [`Tui`]: global listeners, focus routing, and
//! terminal colour-scheme response handling.

use crate::component::ComponentId;
use crate::input::{InputEvent, Key, is_key_release};
use crate::render::surface::{InputListenerResult, Tui};
use crate::terminal::Terminal;
use crate::terminal::{
    TerminalColorScheme, is_color_scheme_report, is_osc11_background_color_response,
    parse_color_scheme_report, parse_osc11_background_color,
};

impl<T: Terminal> Tui<T> {
    // ── Input listener API ──────────────────────────────────────────────

    /// Register a global input listener that runs *before* input is dispatched
    /// to the focused component.
    ///
    /// Returns a token that, when dropped, removes the listener.
    /// Mirrors TS `tui.addInputListener()`.
    pub fn add_input_listener<F>(&mut self, listener: F)
    where
        F: FnMut(&str) -> InputListenerResult + 'static,
    {
        self.input_listeners.push(Box::new(listener));
    }

    /// Remove all input listeners.
    pub fn clear_input_listeners(&mut self) {
        self.input_listeners.clear();
    }

    // ── Terminal colour scheme listeners ────────────────────────────────

    /// Register a listener for terminal colour scheme changes (OSC 997).
    /// Mirrors TS `tui.onTerminalColorSchemeChange()`.
    pub fn on_color_scheme_change<F>(&mut self, listener: F)
    where
        F: FnMut(TerminalColorScheme) + 'static,
    {
        self.color_scheme_listeners.push(Box::new(listener));
    }

    pub fn set_focus(&mut self, id: Option<ComponentId>) {
        if self.focused_component == id {
            return;
        }

        if let Some(previous) = self.focused_component
            && let Some(component) = self.child_mut(previous)
        {
            component.set_focused(false);
        }

        self.focused_component = id;
        if let Some(next) = id {
            if let Some(component) = self.child_mut(next) {
                component.set_focused(true);
            } else {
                self.focused_component = None;
            }
        }
    }

    /// Dispatch an input event.  Runs global listeners first, then forwards
    /// to the focused component.
    ///
    /// Also intercepts OSC 11 / OSC 997 / Apple Terminal sequences here so
    /// that downstream code does not need to.
    pub fn dispatch_input(&mut self, event: &InputEvent) {
        // ── Consume terminal colour responses ────────────────────────
        if let InputEvent::Raw(data) = event
            && self.try_consume_color_scheme_response(data)
        {
            return;
        }

        // ── Dispatch through input listeners ─────────────────────────
        let data = match event {
            InputEvent::Key(ke) => {
                // Convert KeyEvent back to a string for listener dispatch
                // (listeners expect raw strings, matching TS behaviour).
                // For text events, just forward the character.
                // For paste events, we forward as-is.
                // This is a simplified pass-through; the TS listeners intercept
                // at the raw-string level before parsing.
                match &ke.key {
                    Key::Char(ch) => ch.as_str(),
                    _ => return self.dispatch_to_focused(event),
                }
            }
            InputEvent::Mouse(_) | InputEvent::Paste(_) => return self.dispatch_to_focused(event),
            InputEvent::Raw(data) => data.as_str(),
            InputEvent::Resize(_) => {
                // Resize events are always forwarded directly.
                return self.dispatch_to_focused(event);
            }
        };

        // Run input listeners (raw string interception)
        let mut current = data.to_string();
        for listener in &mut self.input_listeners {
            match listener(&current) {
                InputListenerResult::Consumed => return,
                InputListenerResult::Replace(new_data) => current = new_data,
                InputListenerResult::Continue => {}
            }
        }
        if current.is_empty() {
            return;
        }

        // Re-wrap into InputEvent for dispatch
        let modified_event = if current != data {
            InputEvent::Raw(current)
        } else {
            event.clone()
        };
        self.dispatch_to_focused(&modified_event);
    }

    /// Forward an event to the focused component, with Apple Terminal
    /// Shift+Enter correction.
    pub(super) fn dispatch_to_focused(&mut self, event: &InputEvent) {
        // Check Apple Terminal *before* borrowing child_mut.
        let _apple_shift_enter = self.is_apple_terminal_session()
            && matches!(event, InputEvent::Key(ke) if ke.key == Key::Enter && ke.modifiers.is_empty());

        let Some(id) = self.focused_component else {
            return;
        };
        let Some(component) = self.child_mut(id) else {
            self.focused_component = None;
            return;
        };
        if is_key_release(event) && !component.wants_key_release() {
            return;
        }

        component.handle_input(event);
    }

    /// Try to consume an OSC 11 / OSC 997 colour response.
    /// Returns `true` if the data was consumed.
    pub(super) fn try_consume_color_scheme_response(&mut self, data: &str) -> bool {
        // OSC 997 colour scheme report
        if is_color_scheme_report(data)
            && let Some(scheme) = parse_color_scheme_report(data)
        {
            for listener in &mut self.color_scheme_listeners {
                listener(scheme);
            }
            return true;
        }

        // OSC 11 background colour response
        if self.pending_osc11_replies > 0 && is_osc11_background_color_response(data) {
            self.pending_osc11_replies = self.pending_osc11_replies.saturating_sub(1);
            // Parse and store — currently we just consume it.
            // Downstream can use `on_color_scheme_change` for the scheme.
            let _rgb = parse_osc11_background_color(data);
            return true;
        }

        false
    }

    /// Query the terminal background colour (OSC 11).
    /// Call this when you need the background colour.
    pub fn query_background_color(&mut self) {
        self.pending_osc11_replies += 1;
        let _ = self
            .terminal
            .write(&crate::terminal::query_background_color());
    }

    // ── Apple Terminal detection ────────────────────────────────────────

    pub(super) fn is_apple_terminal_session(&mut self) -> bool {
        *self
            .is_apple_terminal
            .get_or_insert_with(|| std::env::var("TERM_PROGRAM").as_deref() == Ok("Apple_Terminal"))
    }
}
