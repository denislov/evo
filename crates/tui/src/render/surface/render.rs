//! Frame composition and terminal painting for [`Tui`].

use crate::component::{Component, ComponentId};
use crate::editing::CursorPosition;
use crate::render::surface::kitty::collect_kitty_image_ids_in_range;
use crate::render::surface::{Tui, TuiError};
use crate::terminal::Terminal;
use crate::terminal::delete_kitty_image;

const SYNC_START: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";
const LINE_RESET: &str = "\x1b[0m\x1b]8;;\x07";

impl<T: Terminal> Tui<T> {
    pub(super) fn render_lines(&mut self, width: usize, height: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for (_, child) in &mut self.children {
            child.set_viewport_size(width, height);
            lines.extend(child.render(width));
        }
        self.composite_overlays(&mut lines, width, height);
        lines
    }

    pub(super) fn child_mut(&mut self, id: ComponentId) -> Option<&mut Box<dyn Component>> {
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

    pub(super) fn overlay_index(&self, id: usize) -> Option<usize> {
        self.overlays.iter().position(|overlay| overlay.id == id)
    }

    pub(super) fn synchronized_render<R>(
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

    pub(super) fn end_synchronized_output(&mut self) -> Result<(), TuiError> {
        if !self.synchronized_output_active {
            return Ok(());
        }
        self.terminal.write(SYNC_END)?;
        self.synchronized_output_active = false;
        Ok(())
    }

    pub(super) fn write_lines(&mut self, lines: &[String]) -> Result<(), TuiError> {
        for (index, line) in lines.iter().enumerate() {
            self.terminal.write(line)?;
            self.terminal.write(LINE_RESET)?;
            if index + 1 < lines.len() {
                self.terminal.write("\r\n")?;
            }
        }
        Ok(())
    }

    pub(super) fn position_hardware_cursor(
        &mut self,
        cursor: Option<CursorPosition>,
    ) -> Result<(), TuiError> {
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

    // ── Kitty image tracking (mirrors TS) ────────────────────────────────

    /// Delete all Kitty images from the *previous* render pass.
    pub(super) fn delete_previous_kitty_images(&mut self) -> Result<(), TuiError> {
        for id in &self.previous_kitty_image_ids {
            self.terminal.write(&delete_kitty_image(*id))?;
        }
        Ok(())
    }

    /// Delete Kitty images that appear in the changed line range of the
    /// *previous* render.
    pub(super) fn delete_changed_kitty_images(
        &mut self,
        first: usize,
        last: usize,
    ) -> Result<(), TuiError> {
        let ids = collect_kitty_image_ids_in_range(&self.previous_lines, first, last);
        for id in ids {
            self.terminal.write(&delete_kitty_image(id))?;
        }
        Ok(())
    }
}
