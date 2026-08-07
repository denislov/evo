//! Terminal surface: full-screen render loop, diffing, and sync painting.
//!
//! The public [`Tui`] type owns the terminal and a set of child components.
//! Rendering produces a frame, diffs it against the previous frame, and
//! repaints only the changed range. Input, overlays, Kitty image tracking
//! and low-level painting live in sibling modules.

use crate::component::{Component, ComponentId};
use crate::editing::extract_cursor_marker;
use crate::render::OverlayEntry;
use crate::render::surface::helpers::{
    changed_line_range, fullscreen_frame, last_line_width, validate_lines, viewport_top,
};
use crate::render::surface::kitty::collect_kitty_image_ids;
use crate::terminal::Terminal;
use crate::terminal::TerminalColorScheme;

mod helpers;
mod input;
mod kitty;
mod overlay;
mod render;

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

    pub fn start(mut terminal: T) -> Result<Self, TuiError> {
        if let Err(error) = terminal.start() {
            let _ = terminal.stop();
            return Err(error.into());
        }
        let mut tui = Self::new(terminal);
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
        lines = fullscreen_frame(lines, height);
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
                self.render_differential(&lines, first_changed_line, last_changed_line)?;
            }
        }

        self.previous_viewport_top = viewport_top(lines.len(), height);
        self.owned_rows = lines.len().min(height);
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

    // ── Render strategy ─────────────────────────────────────────────────

    pub(super) fn choose_strategy(
        &self,
        lines: &[String],
        width: usize,
        height: usize,
    ) -> RenderStrategy {
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

    pub(super) fn render_full(&mut self, lines: &[String], height: usize) -> Result<(), TuiError> {
        self.full_redraws += 1;
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

    pub(super) fn render_differential(
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
