use std::collections::HashMap;

pub(super) mod autocomplete;
mod edit;
mod input;
mod visual;

use self::autocomplete::{AutocompleteItem, AutocompleteProvider};
use crate::component::Component;
use crate::editing::CURSOR_MARKER;
use crate::editing::KillRing;
use crate::editing::UndoStack;
use crate::input::{InputEvent, KeybindingsManager};
use crate::render::{Style, color_enabled, paint_with};
use crate::theme::EditorTheme;

use self::visual::{current_visual_line_index, fit_render_line, wrap_multiline};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EditorSnapshot {
    text: String,
    cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LastAction {
    Kill,
    Yank,
    TypeWord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VisualLine {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JumpDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AutocompleteState {
    Regular,
    Force,
}

pub(super) type OnChange = Box<dyn FnMut(&str)>;
pub(super) type OnNoArgFnMut = Box<dyn FnMut()>;

pub struct Editor {
    text: String,
    cursor: usize,
    focused: bool,
    last_render_width: usize,
    viewport_height: usize,
    scroll_offset: usize,
    show_border: bool,
    theme: EditorTheme,
    keybindings: KeybindingsManager,
    kill_ring: KillRing,
    undo_stack: UndoStack<EditorSnapshot>,
    redo_stack: UndoStack<EditorSnapshot>,
    last_action: Option<LastAction>,
    last_yank: Option<(usize, usize)>,
    on_submit: Option<OnChange>,
    on_change: Option<OnChange>,
    disable_submit: bool,
    on_scroll_page_up: Option<OnNoArgFnMut>,
    on_scroll_page_down: Option<OnNoArgFnMut>,
    history: Vec<String>,
    history_index: Option<usize>,
    jump_mode: Option<JumpDirection>,
    pastes: HashMap<usize, String>,
    paste_counter: usize,
    autocomplete_provider: Option<Box<dyn AutocompleteProvider>>,
    autocomplete_state: Option<AutocompleteState>,
    autocomplete_items: Vec<AutocompleteItem>,
    autocomplete_selected: usize,
    autocomplete_prefix: String,
    autocomplete_max_visible: usize,
}

impl Editor {
    pub fn new(keybindings: KeybindingsManager) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            focused: false,
            last_render_width: 80,
            viewport_height: 24,
            scroll_offset: 0,
            show_border: false,
            theme: EditorTheme::default(),
            keybindings,
            kill_ring: KillRing::default(),
            undo_stack: UndoStack::default(),
            redo_stack: UndoStack::default(),
            last_action: None,
            last_yank: None,
            on_submit: None,
            on_change: None,
            disable_submit: false,
            on_scroll_page_up: None,
            on_scroll_page_down: None,
            history: Vec::new(),
            history_index: None,
            jump_mode: None,
            pastes: HashMap::new(),
            paste_counter: 0,
            autocomplete_provider: None,
            autocomplete_state: None,
            autocomplete_items: Vec::new(),
            autocomplete_selected: 0,
            autocomplete_prefix: String::new(),
            autocomplete_max_visible: 5,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        let new_text = text.into();
        let changed = self.text != new_text;
        self.text = new_text;
        self.cursor = self.text.len();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_action = None;
        self.last_yank = None;
        self.history_index = None;
        self.scroll_offset = 0;
        self.cancel_autocomplete();
        if changed {
            self.emit_change();
        }
    }

    pub fn set_on_submit(&mut self, callback: OnChange) {
        self.on_submit = Some(callback);
    }

    pub fn set_on_change(&mut self, callback: OnChange) {
        self.on_change = Some(callback);
    }

    pub fn set_disable_submit(&mut self, disabled: bool) {
        self.disable_submit = disabled;
    }

    pub fn disable_submit(&self) -> bool {
        self.disable_submit
    }

    pub fn add_to_history(&mut self, text: impl AsRef<str>) {
        let trimmed = text.as_ref().trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.first().is_some_and(|entry| entry == trimmed) {
            return;
        }
        self.history.insert(0, trimmed.to_string());
        if self.history.len() > 100 {
            self.history.pop();
        }
    }

    pub fn expanded_text(&self) -> String {
        self.expand_paste_markers(&self.text)
    }

    pub fn set_autocomplete_provider(&mut self, provider: Box<dyn AutocompleteProvider>) {
        self.cancel_autocomplete();
        self.autocomplete_provider = Some(provider);
    }

    pub fn set_autocomplete_max_visible(&mut self, max_visible: usize) {
        self.autocomplete_max_visible = max_visible.clamp(3, 20);
    }

    pub fn set_theme(&mut self, theme: EditorTheme) {
        self.theme = theme;
    }

    pub fn set_show_border(&mut self, show_border: bool) {
        self.show_border = show_border;
    }

    pub fn set_on_scroll_page_up(&mut self, callback: OnNoArgFnMut) {
        self.on_scroll_page_up = Some(callback);
    }

    pub fn set_on_scroll_page_down(&mut self, callback: OnNoArgFnMut) {
        self.on_scroll_page_down = Some(callback);
    }

    pub fn render_assistance(&self, width: usize) -> Vec<String> {
        if self.autocomplete_state.is_none() || self.autocomplete_items.is_empty() {
            return Vec::new();
        }
        let color = color_enabled();
        let start = self
            .autocomplete_selected
            .saturating_add(1)
            .saturating_sub(self.autocomplete_max_visible);
        self.autocomplete_items
            .iter()
            .enumerate()
            .skip(start)
            .take(self.autocomplete_max_visible)
            .map(|(index, item)| {
                let selected = index == self.autocomplete_selected;
                let marker = if selected { "› " } else { "  " };
                let marker_style = if selected {
                    self.theme.select_list.selected_prefix
                } else {
                    self.theme.select_list.description
                };
                let text_style = if selected {
                    self.theme.select_list.selected_text
                } else {
                    Style::default()
                };
                let mut line = format!(
                    "{}{}",
                    paint_with(marker, &marker_style, color),
                    paint_with(&item.label, &text_style, color)
                );
                if let Some(description) = &item.description {
                    line.push_str("  ");
                    line.push_str(&paint_with(
                        description,
                        &self.theme.select_list.description,
                        color,
                    ));
                }
                fit_render_line(&line, width)
            })
            .collect()
    }

    /// Render only the editable input viewport. Assistance can be projected
    /// separately with [`Self::render_assistance`] when a host owns overlay
    /// placement; [`Component::render`] preserves the embedded legacy shape.
    pub fn render_input(&mut self, width: usize) -> Vec<String> {
        if width == 0 {
            return Vec::new();
        }
        self.last_render_width = width;

        let mut text = self.text.clone();
        if self.focused {
            text.insert_str(self.cursor, CURSOR_MARKER);
        }
        let layout_lines = wrap_multiline(&text, width);
        let cursor_line = current_visual_line_index(&self.text, self.cursor, width);
        let border_rows = if self.show_border { 2 } else { 0 };
        let max_visible_lines = self
            .viewport_height
            .saturating_sub(border_rows)
            .saturating_mul(3)
            .checked_div(10)
            .unwrap_or(0)
            .max(5)
            .min(layout_lines.len().max(1));

        if cursor_line < self.scroll_offset {
            self.scroll_offset = cursor_line;
        } else if cursor_line >= self.scroll_offset + max_visible_lines {
            self.scroll_offset = cursor_line - max_visible_lines + 1;
        }
        let max_scroll_offset = layout_lines.len().saturating_sub(max_visible_lines);
        self.scroll_offset = self.scroll_offset.min(max_scroll_offset);

        let mut lines = Vec::new();
        let color = color_enabled();
        if self.show_border {
            let style = if self.focused {
                &self.theme.active_border
            } else {
                &self.theme.border
            };
            lines.push(fit_render_line(
                &paint_with(&"─".repeat(width), style, color),
                width,
            ));
        } else if self.scroll_offset > 0 {
            lines.push(fit_render_line(
                &format!("─── ↑ {} more ", self.scroll_offset),
                width,
            ));
        }

        lines.extend(
            layout_lines
                .iter()
                .skip(self.scroll_offset)
                .take(max_visible_lines)
                .cloned(),
        );

        let lines_below = layout_lines
            .len()
            .saturating_sub(self.scroll_offset + max_visible_lines);
        if self.show_border {
            let style = if self.focused {
                &self.theme.active_border
            } else {
                &self.theme.border
            };
            let border = if lines_below > 0 {
                format!("─── ↓ {lines_below} more ")
            } else {
                "─".repeat(width)
            };
            lines.push(fit_render_line(&paint_with(&border, style, color), width));
        } else if lines_below > 0 {
            lines.push(fit_render_line(
                &format!("─── ↓ {lines_below} more "),
                width,
            ));
        }

        lines
    }
}

impl Component for Editor {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = self.render_input(width);
        lines.extend(self.render_assistance(width));
        lines
    }

    fn handle_input(&mut self, event: &InputEvent) {
        let before = self.text.clone();
        self.handle_input_inner(event);
        if self.text != before {
            self.emit_change();
        }
    }

    fn set_viewport_size(&mut self, _width: usize, height: usize) {
        self.viewport_height = height.max(1);
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn focused(&self) -> bool {
        self.focused
    }
}
