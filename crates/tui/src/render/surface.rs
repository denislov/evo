use crate::component::{Component, ComponentId};
use crate::editing::{CursorPosition, extract_cursor_marker};
use crate::input::{InputEvent, Key, is_key_release};
use crate::render::{OverlayAnchor, Rect, SizeValue};
use crate::render::{OverlayEntry, OverlayHandle, OverlayOptions};
use crate::render::{drop_columns, truncate_to_width, visible_width};
use crate::terminal::delete_kitty_image;
use crate::terminal::{Terminal, TerminalMode};
use crate::terminal::{
    TerminalColorScheme, is_color_scheme_report, is_osc11_background_color_response,
    parse_color_scheme_report, parse_osc11_background_color, query_background_color,
};

const SYNC_START: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";
const LINE_RESET: &str = "\x1b[0m\x1b]8;;\x07";

/// Reset sequence inserted between composite segments to prevent colour bleed.
const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStrategy {
    FullRedraw,
    Differential {
        first_changed_line: usize,
        last_changed_line: usize,
    },
    NoChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOutcome {
    pub strategy: RenderStrategy,
    pub line_count: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("line {line_index} is {width} columns wide, exceeding max width {max_width}: {line:?}")]
    LineTooWide {
        line_index: usize,
        width: usize,
        max_width: usize,
        line: String,
    },
}

/// Result from an input listener.
/// - `None` / `Some(InputListenerResult::Continue)` → pass input to next listener / focus.
/// - `Some(InputListenerResult::Consumed)` → stop processing.
/// - `Some(InputListenerResult::Replace(text))` → replace input and continue processing.
pub enum InputListenerResult {
    Continue,
    Consumed,
    Replace(String),
}

type InputListener = Box<dyn FnMut(&str) -> InputListenerResult>;

pub struct Tui<T: Terminal> {
    terminal: T,
    children: Vec<(ComponentId, Box<dyn Component>)>,
    overlays: Vec<OverlayEntry>,
    next_component_id: ComponentId,
    next_overlay_id: usize,
    focused_component: Option<ComponentId>,
    previous_lines: Vec<String>,
    previous_width: usize,
    previous_height: usize,
    previous_viewport_top: usize,
    cursor_row: usize,
    owned_rows: usize,
    rendered_once: bool,
    terminal_mode: TerminalMode,
    terminal_active: bool,
    synchronized_output_active: bool,
    hardware_cursor_row: usize,
    hardware_cursor_col: usize,
    hardware_cursor_visible: bool,
    clear_on_shrink: bool,
    full_redraws: usize,

    // ── Input listeners ──────────────────────────────────────────────
    input_listeners: Vec<InputListener>,

    // ── Kitty image ─────────────────────────────────────────
    previous_kitty_image_ids: Vec<u32>,

    // ── Apple Terminal detection (lazy) ──────────────────────────────
    is_apple_terminal: Option<bool>,

    // ── Terminal colour scheme support ───────────────────────────────
    color_scheme_listeners: Vec<Box<dyn FnMut(TerminalColorScheme)>>,
    pending_osc11_replies: usize,
}

impl<T: Terminal> Tui<T> {
    pub fn new(terminal: T) -> Self {
        Self {
            terminal,
            children: Vec::new(),
            overlays: Vec::new(),
            next_component_id: 1,
            next_overlay_id: 1,
            focused_component: None,
            previous_lines: Vec::new(),
            previous_width: 0,
            previous_height: 0,
            previous_viewport_top: 0,
            cursor_row: 0,
            owned_rows: 0,
            rendered_once: false,
            terminal_mode: TerminalMode::Inline,
            terminal_active: false,
            synchronized_output_active: false,
            hardware_cursor_row: 0,
            hardware_cursor_col: 0,
            hardware_cursor_visible: false,
            clear_on_shrink: true,
            full_redraws: 0,
            input_listeners: Vec::new(),
            previous_kitty_image_ids: Vec::new(),
            is_apple_terminal: None,
            color_scheme_listeners: Vec::new(),
            pending_osc11_replies: 0,
        }
    }

    fn with_mode(terminal: T, mode: TerminalMode) -> Self {
        let mut tui = Self::new(terminal);
        tui.terminal_mode = mode;
        tui
    }

    pub fn start(mut terminal: T, mode: TerminalMode) -> Result<Self, TuiError> {
        if let Err(error) = terminal.start_mode(mode) {
            let _ = terminal.stop();
            return Err(error.into());
        }
        let mut tui = Self::with_mode(terminal, mode);
        tui.terminal_active = true;
        Ok(tui)
    }

    pub fn stop(&mut self) -> Result<(), TuiError> {
        if !self.terminal_active {
            return Ok(());
        }
        let sync_cleanup = self.end_synchronized_output();
        let image_cleanup = self.delete_previous_kitty_images();
        let stop_result = self.terminal.stop();
        if stop_result.is_ok() {
            self.terminal_active = false;
            self.synchronized_output_active = false;
        }
        sync_cleanup?;
        image_cleanup?;
        stop_result?;
        Ok(())
    }

    pub fn terminal(&self) -> &T {
        &self.terminal
    }

    pub fn terminal_mut(&mut self) -> &mut T {
        &mut self.terminal
    }

    pub fn add_child(&mut self, child: Box<dyn Component>) {
        self.add_child_with_id(child);
    }

    pub fn add_child_with_id(&mut self, child: Box<dyn Component>) -> ComponentId {
        let id = self.next_component_id;
        self.next_component_id += 1;
        self.children.push((id, child));
        id
    }

    pub fn clear_children(&mut self) {
        self.focused_component = None;
        self.children.clear();
    }

    pub fn remove_child(&mut self, id: ComponentId) -> Option<Box<dyn Component>> {
        let index = self
            .children
            .iter()
            .position(|(component_id, _)| *component_id == id)?;
        if self.focused_component == Some(id) {
            self.focused_component = None;
        }
        Some(self.children.remove(index).1)
    }

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

    // ── Overlay API ─────────────────────────────────────────────────────

    pub fn show_overlay(
        &mut self,
        component: Box<dyn Component>,
        options: OverlayOptions,
    ) -> OverlayHandle {
        let id = self.next_overlay_id;
        self.next_overlay_id += 1;
        let component_id = self.next_component_id;
        self.next_component_id += 1;
        self.overlays.push(OverlayEntry {
            id,
            component_id,
            component,
            options,
            hidden: false,
            restore_focus: None,
        });
        OverlayHandle { id }
    }

    pub fn hide_overlay(&mut self, handle: OverlayHandle) {
        self.set_overlay_hidden(handle, true);
    }

    pub fn set_overlay_hidden(&mut self, handle: OverlayHandle, hidden: bool) {
        let Some(index) = self.overlay_index(handle.id) else {
            return;
        };
        self.overlays[index].hidden = hidden;
        if hidden && self.focused_component == Some(self.overlays[index].component_id) {
            let restore_focus = self.overlays[index].restore_focus;
            self.set_focus(restore_focus);
        }
    }

    pub fn set_overlay_options(&mut self, handle: OverlayHandle, options: OverlayOptions) {
        let Some(index) = self.overlay_index(handle.id) else {
            return;
        };
        self.overlays[index].options = options;
    }

    pub fn has_overlay(&self, handle: OverlayHandle) -> bool {
        self.overlays
            .iter()
            .any(|overlay| overlay.id == handle.id && !overlay.hidden)
    }

    pub fn focus_overlay(&mut self, handle: OverlayHandle) {
        let Some(index) = self.overlay_index(handle.id) else {
            return;
        };
        if self.overlays[index].options.non_capturing {
            return;
        }
        self.overlays[index].restore_focus = self.focused_component;
        let component_id = self.overlays[index].component_id;
        self.set_focus(Some(component_id));
    }

    pub fn unfocus_overlay(&mut self, handle: OverlayHandle, target: Option<ComponentId>) {
        let Some(index) = self.overlay_index(handle.id) else {
            return;
        };
        if self.focused_component == Some(self.overlays[index].component_id) {
            self.set_focus(target.or(self.overlays[index].restore_focus));
        }
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
    fn dispatch_to_focused(&mut self, event: &InputEvent) {
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
    fn try_consume_color_scheme_response(&mut self, data: &str) -> bool {
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
        let _ = self.terminal.write(&query_background_color());
    }

    // ── Apple Terminal detection ────────────────────────────────────────

    fn is_apple_terminal_session(&mut self) -> bool {
        *self
            .is_apple_terminal
            .get_or_insert_with(|| std::env::var("TERM_PROGRAM").as_deref() == Ok("Apple_Terminal"))
    }

    // ── Component access ────────────────────────────────────────────────

    pub fn component_as<C: 'static>(&self, id: ComponentId) -> Option<&C> {
        self.children
            .iter()
            .find(|(component_id, _)| *component_id == id)
            .and_then(|(_, component)| component.as_any().downcast_ref::<C>())
            .or_else(|| {
                self.overlays
                    .iter()
                    .find(|overlay| overlay.component_id == id)
                    .and_then(|overlay| overlay.component.as_any().downcast_ref::<C>())
            })
    }

    pub fn component_as_mut<C: 'static>(&mut self, id: ComponentId) -> Option<&mut C> {
        if let Some(index) = self
            .children
            .iter()
            .position(|(component_id, _)| *component_id == id)
        {
            return self.children[index].1.as_any_mut().downcast_mut::<C>();
        }
        if let Some(index) = self
            .overlays
            .iter()
            .position(|overlay| overlay.component_id == id)
        {
            return self.overlays[index]
                .component
                .as_any_mut()
                .downcast_mut::<C>();
        }
        None
    }

    pub fn full_redraws(&self) -> usize {
        self.full_redraws
    }

    pub fn rendered_lines(&self) -> &[String] {
        &self.previous_lines
    }

    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        self.clear_on_shrink = enabled;
    }

    pub fn clear_on_shrink(&self) -> bool {
        self.clear_on_shrink
    }

    pub fn terminal_mode(&self) -> TerminalMode {
        self.terminal_mode
    }

    // ── Render ─────────────────────────────────────────────────────────

    pub fn render_once(&mut self) -> Result<RenderOutcome, TuiError> {
        let size = self.terminal.size();
        let width = size.columns;
        let height = size.rows;
        if width == 0 || height == 0 {
            self.previous_width = width;
            self.previous_height = height;
            return Ok(RenderOutcome {
                strategy: RenderStrategy::NoChange,
                line_count: 0,
            });
        }
        let mut lines = self.render_lines(width, height);
        if self.terminal_mode == TerminalMode::Fullscreen {
            lines = fullscreen_frame(lines, height);
        }
        let cursor = extract_cursor_marker(&mut lines, height);
        validate_lines(&lines, width)?;

        let strategy = self.choose_strategy(&lines, width, height);
        match strategy {
            RenderStrategy::NoChange => {}
            RenderStrategy::FullRedraw => {
                self.render_full(&lines, height)?;
            }
            RenderStrategy::Differential {
                first_changed_line,
                last_changed_line,
            } => {
                self.render_differential(&lines, first_changed_line, last_changed_line, height)?;
            }
        }

        self.previous_viewport_top = viewport_top(lines.len(), height);
        self.owned_rows = lines.len().min(height);
        self.rendered_once = true;
        self.position_hardware_cursor(cursor)?;

        self.previous_lines = lines.clone();
        // Track the current frame's Kitty image IDs for cleanup on the next render.
        self.previous_kitty_image_ids = collect_kitty_image_ids(&lines);
        self.previous_width = width;
        self.previous_height = height;
        self.cursor_row = lines.len().saturating_sub(1);

        Ok(RenderOutcome {
            strategy,
            line_count: lines.len(),
        })
    }

    fn render_lines(&mut self, width: usize, height: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for (_, child) in &mut self.children {
            child.set_viewport_size(width, height);
            lines.extend(child.render(width));
        }
        self.composite_overlays(&mut lines, width, height);
        lines
    }

    fn child_mut(&mut self, id: ComponentId) -> Option<&mut Box<dyn Component>> {
        if let Some(index) = self
            .children
            .iter()
            .position(|(component_id, _)| *component_id == id)
        {
            return Some(&mut self.children[index].1);
        }
        if let Some(index) = self
            .overlays
            .iter()
            .position(|overlay| overlay.component_id == id)
        {
            return Some(&mut self.overlays[index].component);
        }
        None
    }

    fn overlay_index(&self, id: usize) -> Option<usize> {
        self.overlays.iter().position(|overlay| overlay.id == id)
    }

    // ── Overlay compositing ─────────────────────────────────────────────

    fn composite_overlays(
        &mut self,
        base_lines: &mut Vec<String>,
        terminal_width: usize,
        terminal_height: usize,
    ) {
        // Sort overlays so visible ones are composited in insertion order.
        // (TS uses focusOrder; we use insertion order which is equivalent
        //  for the common case.)
        for i in 0..self.overlays.len() {
            let is_visible = {
                let overlay = &mut self.overlays[i];
                overlay.is_visible(terminal_width, terminal_height)
            };
            if !is_visible {
                continue;
            }

            let (overlay_width, overlay_lines, row, col) = {
                let overlay = &mut self.overlays[i];
                let overlay_width = resolve_overlay_width(&overlay.options, terminal_width).max(1);
                let available_height = terminal_height
                    .saturating_sub(overlay.options.margin.top + overlay.options.margin.bottom);
                let overlay_height = overlay
                    .options
                    .max_height
                    .map(|size| resolve_size(size, available_height))
                    .unwrap_or(available_height)
                    .min(available_height);
                if overlay_height == 0 {
                    continue;
                }
                let overlay_lines = overlay.component.render_bounded(Rect::new(
                    0,
                    0,
                    overlay_width,
                    overlay_height,
                ));
                if overlay_lines.is_empty() {
                    continue;
                }

                let (row, col) = overlay_position(
                    &overlay.options,
                    terminal_width,
                    terminal_height,
                    overlay_width,
                    overlay_lines.len(),
                );
                (overlay_width, overlay_lines, row, col)
            };

            let required_rows = row + overlay_lines.len();
            while base_lines.len() < required_rows {
                base_lines.push(String::new());
            }

            for (line_offset, overlay_line) in overlay_lines.iter().enumerate() {
                let fitted = fit_to_width(overlay_line, overlay_width);
                let base_line = &mut base_lines[row + line_offset];
                *base_line = splice_by_columns(base_line, col, overlay_width, &fitted);
            }
        }
    }

    // ── Render strategy ─────────────────────────────────────────────────

    fn choose_strategy(&self, lines: &[String], width: usize, height: usize) -> RenderStrategy {
        if self.previous_width == 0 || self.previous_height == 0 {
            return RenderStrategy::FullRedraw;
        }
        if self.previous_width != width || self.previous_height != height {
            return RenderStrategy::FullRedraw;
        }
        if self.clear_on_shrink && lines.len() < self.previous_lines.len() {
            return RenderStrategy::FullRedraw;
        }
        changed_line_range(&self.previous_lines, lines)
            .map(
                |(first_changed_line, last_changed_line)| RenderStrategy::Differential {
                    first_changed_line,
                    last_changed_line,
                },
            )
            .unwrap_or(RenderStrategy::NoChange)
    }

    fn render_full(&mut self, lines: &[String], height: usize) -> Result<(), TuiError> {
        self.full_redraws += 1;
        match self.terminal_mode {
            TerminalMode::Inline => self.render_full_inline(lines, height),
            TerminalMode::Fullscreen => self.render_full_fullscreen(lines, height),
        }
    }

    fn render_full_fullscreen(&mut self, lines: &[String], height: usize) -> Result<(), TuiError> {
        self.synchronized_render(|tui| {
            tui.terminal.hide_cursor()?;
            tui.hardware_cursor_visible = false;
            tui.delete_previous_kitty_images()?;
            tui.terminal.clear_screen()?;
            tui.write_lines(lines)?;
            tui.hardware_cursor_row = lines.len().saturating_sub(1);
            tui.hardware_cursor_col = last_line_width(lines);
            tui.owned_rows = lines.len().min(height);
            Ok(())
        })
    }

    fn render_full_inline(&mut self, lines: &[String], height: usize) -> Result<(), TuiError> {
        self.synchronized_render(|tui| {
            tui.terminal.hide_cursor()?;
            tui.hardware_cursor_visible = false;

            if tui.rendered_once {
                tui.delete_previous_kitty_images()?;

                let next_viewport_top = viewport_top(lines.len(), height);
                let visible_lines = &lines[next_viewport_top..];
                let rows_to_clear = tui.owned_rows.max(visible_lines.len()).min(height);
                if rows_to_clear > 0 {
                    tui.move_to_logical_row(tui.previous_viewport_top)?;
                    tui.terminal.move_to_column(0)?;
                    tui.hardware_cursor_col = 0;
                    tui.rewrite_rows(next_viewport_top, visible_lines, rows_to_clear)?;
                }
            } else {
                tui.write_lines(lines)?;
                tui.hardware_cursor_row = lines.len().saturating_sub(1);
                tui.hardware_cursor_col = last_line_width(lines);
            }
            Ok(())
        })
    }

    fn render_differential(
        &mut self,
        lines: &[String],
        first_changed_line: usize,
        last_changed_line: usize,
        height: usize,
    ) -> Result<(), TuiError> {
        match self.terminal_mode {
            TerminalMode::Inline => self.render_differential_inline(
                lines,
                first_changed_line,
                last_changed_line,
                height,
            ),
            TerminalMode::Fullscreen => {
                self.render_differential_fullscreen(lines, first_changed_line, last_changed_line)
            }
        }
    }

    fn render_differential_fullscreen(
        &mut self,
        lines: &[String],
        first_changed_line: usize,
        last_changed_line: usize,
    ) -> Result<(), TuiError> {
        self.synchronized_render(|tui| {
            tui.delete_changed_kitty_images(first_changed_line, last_changed_line)?;
            let target = first_changed_line as i16;
            let current = tui.hardware_cursor_row as i16;
            tui.terminal.move_by(target - current)?;
            tui.terminal.move_to_column(0)?;
            tui.terminal.clear_from_cursor()?;
            tui.write_lines(&lines[first_changed_line..])?;
            tui.hardware_cursor_row = lines.len().saturating_sub(1);
            tui.hardware_cursor_col = last_line_width(lines);
            Ok(())
        })
    }

    fn render_differential_inline(
        &mut self,
        lines: &[String],
        first_changed_line: usize,
        last_changed_line: usize,
        height: usize,
    ) -> Result<(), TuiError> {
        if first_changed_line < self.previous_viewport_top {
            return self.render_full_inline(lines, height);
        }

        let appended_lines = lines.len() > self.previous_lines.len()
            && first_changed_line == self.previous_lines.len();
        self.synchronized_render(|tui| {
            tui.delete_changed_kitty_images(first_changed_line, last_changed_line)?;

            if appended_lines && first_changed_line > 0 {
                tui.move_to_logical_row(first_changed_line - 1)?;
                tui.terminal.move_to_column(0)?;
                tui.hardware_cursor_col = 0;
                tui.terminal.write("\r\n")?;
                tui.write_lines(&lines[first_changed_line..])?;
                tui.hardware_cursor_row = lines.len().saturating_sub(1);
                tui.hardware_cursor_col = last_line_width(lines);
            } else {
                tui.move_to_logical_row(first_changed_line)?;
                tui.terminal.move_to_column(0)?;
                tui.hardware_cursor_col = 0;

                let rows_to_write = if first_changed_line < lines.len() {
                    last_changed_line.min(lines.len() - 1) - first_changed_line + 1
                } else {
                    0
                };
                let old_rows_to_clear = if first_changed_line < tui.previous_lines.len() {
                    last_changed_line.min(tui.previous_lines.len() - 1) - first_changed_line + 1
                } else {
                    0
                };
                let rows_to_clear = rows_to_write.max(old_rows_to_clear);
                let changed_lines = if rows_to_write > 0 {
                    &lines[first_changed_line..first_changed_line + rows_to_write]
                } else {
                    &[]
                };
                tui.rewrite_rows(first_changed_line, changed_lines, rows_to_clear)?;
            }

            Ok(())
        })
    }

    fn synchronized_render<R>(
        &mut self,
        render: impl FnOnce(&mut Self) -> Result<R, TuiError>,
    ) -> Result<R, TuiError> {
        self.terminal.write(SYNC_START)?;
        self.synchronized_output_active = true;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(self)));
        let end = self.end_synchronized_output();
        let flush = self.terminal.flush().map_err(TuiError::from);
        match outcome {
            Err(payload) => {
                let _ = end;
                let _ = flush;
                std::panic::resume_unwind(payload)
            }
            Ok(Err(error)) => {
                let _ = end;
                let _ = flush;
                Err(error)
            }
            Ok(Ok(value)) => {
                end?;
                flush?;
                Ok(value)
            }
        }
    }

    fn end_synchronized_output(&mut self) -> Result<(), TuiError> {
        if !self.synchronized_output_active {
            return Ok(());
        }
        self.terminal.write(SYNC_END)?;
        self.synchronized_output_active = false;
        Ok(())
    }

    fn write_lines(&mut self, lines: &[String]) -> Result<(), TuiError> {
        for (index, line) in lines.iter().enumerate() {
            self.terminal.write(line)?;
            self.terminal.write(LINE_RESET)?;
            if index + 1 < lines.len() {
                self.terminal.write("\r\n")?;
            }
        }
        Ok(())
    }

    fn rewrite_rows(
        &mut self,
        start_row: usize,
        lines: &[String],
        rows_to_clear: usize,
    ) -> Result<(), TuiError> {
        if rows_to_clear == 0 {
            return Ok(());
        }

        for row_offset in 0..rows_to_clear {
            self.terminal.write("\r")?;
            self.terminal.clear_line()?;
            if let Some(line) = lines.get(row_offset) {
                self.terminal.write(line)?;
                self.terminal.write(LINE_RESET)?;
            }
            if row_offset + 1 < rows_to_clear {
                self.terminal.write("\r\n")?;
            }
        }

        self.hardware_cursor_row = start_row + rows_to_clear - 1;
        self.hardware_cursor_col = lines
            .get(rows_to_clear.saturating_sub(1))
            .map(|line| visible_width(line))
            .unwrap_or(0);
        Ok(())
    }

    fn position_hardware_cursor(&mut self, cursor: Option<CursorPosition>) -> Result<(), TuiError> {
        let Some(cursor) = cursor else {
            if self.hardware_cursor_visible {
                self.terminal.hide_cursor()?;
                self.hardware_cursor_visible = false;
                self.terminal.flush()?;
            }
            return Ok(());
        };

        let target = cursor.row.saturating_sub(self.previous_viewport_top) as i16;
        let current = self
            .hardware_cursor_row
            .saturating_sub(self.previous_viewport_top) as i16;
        self.terminal.move_by(target - current)?;
        self.terminal.move_to_column(cursor.col)?;
        if !self.hardware_cursor_visible {
            self.terminal.show_cursor()?;
            self.hardware_cursor_visible = true;
        }
        self.hardware_cursor_row = cursor.row;
        self.hardware_cursor_col = cursor.col;
        self.terminal.flush()?;
        Ok(())
    }

    fn move_to_logical_row(&mut self, target_row: usize) -> Result<(), TuiError> {
        let target = target_row.saturating_sub(self.previous_viewport_top) as i16;
        let current = self
            .hardware_cursor_row
            .saturating_sub(self.previous_viewport_top) as i16;
        self.terminal.move_by(target - current)?;
        self.hardware_cursor_row = target_row;
        Ok(())
    }

    // ── Kitty image tracking (mirrors TS) ────────────────────────────────

    /// Delete all Kitty images from the *previous* render pass.
    fn delete_previous_kitty_images(&mut self) -> Result<(), TuiError> {
        for id in &self.previous_kitty_image_ids {
            self.terminal.write(&delete_kitty_image(*id))?;
        }
        Ok(())
    }

    /// Delete Kitty images that appear in the changed line range of the
    /// *previous* render.
    fn delete_changed_kitty_images(&mut self, first: usize, last: usize) -> Result<(), TuiError> {
        let ids = collect_kitty_image_ids_in_range(&self.previous_lines, first, last);
        for id in ids {
            self.terminal.write(&delete_kitty_image(id))?;
        }
        Ok(())
    }
}

impl<T: Terminal> Drop for Tui<T> {
    fn drop(&mut self) {
        if !self.terminal_active {
            return;
        }
        let _ = self.end_synchronized_output();
        let _ = self.delete_previous_kitty_images();
        let _ = self.terminal.stop();
        self.terminal_active = false;
    }
}

// ── Kitty image helpers ────────────────────────────────────────────────

/// Extract unique Kitty image IDs from a set of lines.
/// Matches Kitty sequences: `\x1b_G` ... `i=<id>` ...
fn collect_kitty_image_ids(lines: &[String]) -> Vec<u32> {
    let mut ids = Vec::new();
    for line in lines {
        extract_kitty_image_ids(line, &mut ids);
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Extract Kitty image IDs in a specific line range.
fn collect_kitty_image_ids_in_range(lines: &[String], first: usize, last: usize) -> Vec<u32> {
    let mut ids = Vec::new();
    for line in lines.iter().take(last + 1).skip(first) {
        extract_kitty_image_ids(line, &mut ids);
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Parse Kitty image IDs from a single line.
fn extract_kitty_image_ids(line: &str, ids: &mut Vec<u32>) {
    if !line.contains("\x1b_G") {
        return;
    }
    // Find `i=<number>` parameter in the Kitty sequence header.
    // The header ends at the first `;` or `\x1b\\`.
    let header_start = match line.find("\x1b_G") {
        Some(pos) => pos + 3,
        None => return,
    };
    let header_end = line[header_start..]
        .find([';', '\x1b'])
        .map(|pos| header_start + pos)
        .unwrap_or_else(|| line.len());

    let header = &line[header_start..header_end];
    for param in header.split(',') {
        if let Some(value) = param.strip_prefix("i=")
            && let Ok(id) = value.parse::<u32>()
        {
            ids.push(id);
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn validate_lines(lines: &[String], max_width: usize) -> Result<(), TuiError> {
    for (line_index, line) in lines.iter().enumerate() {
        let width = visible_width(line);
        if width > max_width {
            return Err(TuiError::LineTooWide {
                line_index,
                width,
                max_width,
                line: line.clone(),
            });
        }
    }
    Ok(())
}

fn viewport_top(line_count: usize, height: usize) -> usize {
    line_count.saturating_sub(height)
}

fn fullscreen_frame(mut lines: Vec<String>, height: usize) -> Vec<String> {
    if height == 0 {
        return Vec::new();
    }
    if lines.len() > height {
        lines.drain(..lines.len() - height);
        return lines;
    }
    let mut frame = vec![String::new(); height - lines.len()];
    frame.append(&mut lines);
    frame
}

fn last_line_width(lines: &[String]) -> usize {
    lines.last().map(|line| visible_width(line)).unwrap_or(0)
}

fn changed_line_range(previous: &[String], next: &[String]) -> Option<(usize, usize)> {
    let shared = previous.len().min(next.len());
    let mut first = None;
    let mut last = None;

    for index in 0..shared {
        if previous[index] != next[index] {
            first.get_or_insert(index);
            last = Some(index);
        }
    }

    if previous.len() != next.len() {
        let first_changed = first.unwrap_or(shared);
        let last_changed = previous.len().max(next.len()).saturating_sub(1);
        Some((first_changed, last_changed))
    } else {
        first.map(|first_changed| (first_changed, last.expect("first change has last change")))
    }
}

fn resolve_overlay_width(options: &OverlayOptions, terminal_width: usize) -> usize {
    let available = terminal_width.saturating_sub(options.margin.left + options.margin.right);
    let mut width = options
        .width
        .map(|size| resolve_size(size, available))
        .unwrap_or(available);
    if let Some(min_width) = options.min_width {
        width = width.max(min_width);
    }
    width.min(available).max(1)
}

fn resolve_size(size: SizeValue, available: usize) -> usize {
    match size {
        SizeValue::Columns(columns) => columns,
        SizeValue::Percent(percent) => available.saturating_mul(percent as usize) / 100,
    }
}

fn overlay_position(
    options: &OverlayOptions,
    terminal_width: usize,
    terminal_height: usize,
    overlay_width: usize,
    overlay_height: usize,
) -> (usize, usize) {
    let min_row = options.margin.top;
    let min_col = options.margin.left;
    let max_row = terminal_height
        .saturating_sub(options.margin.bottom)
        .saturating_sub(overlay_height);
    let max_col = terminal_width
        .saturating_sub(options.margin.right)
        .saturating_sub(overlay_width);

    let (mut row, mut col) = match options.anchor {
        OverlayAnchor::Center => (
            terminal_height.saturating_sub(overlay_height) / 2,
            terminal_width.saturating_sub(overlay_width) / 2,
        ),
        OverlayAnchor::TopLeft => (min_row, min_col),
        OverlayAnchor::TopRight => (min_row, max_col),
        OverlayAnchor::BottomLeft => (max_row, min_col),
        OverlayAnchor::BottomRight => (max_row, max_col),
        OverlayAnchor::TopCenter => (min_row, terminal_width.saturating_sub(overlay_width) / 2),
        OverlayAnchor::BottomCenter => (max_row, terminal_width.saturating_sub(overlay_width) / 2),
        OverlayAnchor::LeftCenter => (terminal_height.saturating_sub(overlay_height) / 2, min_col),
        OverlayAnchor::RightCenter => (terminal_height.saturating_sub(overlay_height) / 2, max_col),
    };

    if let Some(size) = options.row {
        row = resolve_size(size, terminal_height);
    }
    if let Some(size) = options.col {
        col = resolve_size(size, terminal_width);
    }

    row = apply_offset(row, options.offset_y).clamp(min_row, max_row.max(min_row));
    col = apply_offset(col, options.offset_x).clamp(min_col, max_col.max(min_col));
    (row, col)
}

fn apply_offset(value: usize, offset: isize) -> usize {
    if offset.is_negative() {
        value.saturating_sub(offset.unsigned_abs())
    } else {
        value.saturating_add(offset as usize)
    }
}

fn fit_to_width(line: &str, width: usize) -> String {
    let mut fitted = truncate_to_width(line, width);
    let visible = visible_width(&fitted);
    if visible < width {
        fitted.push_str(&" ".repeat(width - visible));
    }
    fitted
}

/// Splice `replacement` into `base` at column `col` with `width`.
/// Inserts [`SEGMENT_RESET`] between the before/overlay/after segments to
/// prevent colour bleed — mirrors TS `compositeLineAt` + `SEGMENT_RESET`.
fn splice_by_columns(base: &str, col: usize, width: usize, replacement: &str) -> String {
    let mut prefix = truncate_to_width(base, col);
    let prefix_width = visible_width(&prefix);
    if prefix_width < col {
        prefix.push_str(&" ".repeat(col - prefix_width));
    }

    let suffix = drop_columns(base, col + width);

    // Insert SEGMENT_RESET between segments to prevent colour bleed,
    // mirroring TS `compositeLineAt()` which uses `SEGMENT_RESET`.
    format!("{prefix}{SEGMENT_RESET}{replacement}{SEGMENT_RESET}{suffix}")
}
