//! Overlay lifecycle and compositing for [`Tui`].

use crate::component::{Component, ComponentId};
use crate::render::surface::Tui;
use crate::render::surface::helpers::{
    fit_to_width, overlay_position, resolve_overlay_width, resolve_size, splice_by_columns,
};
use crate::render::{OverlayEntry, OverlayHandle, OverlayOptions, Rect};
use crate::terminal::Terminal;

impl<T: Terminal> Tui<T> {
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

    // ── Overlay compositing ─────────────────────────────────────────────

    pub(super) fn composite_overlays(
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
}
