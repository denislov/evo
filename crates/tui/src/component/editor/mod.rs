use std::collections::HashMap;

use unicode_segmentation::UnicodeSegmentation;

pub(super) mod autocomplete;

use self::autocomplete::{AutocompleteItem, AutocompleteOptions, AutocompleteProvider};
use crate::component::Component;
use crate::editing::CURSOR_MARKER;
use crate::editing::KillRing;
use crate::editing::UndoStack;
use crate::editing::{find_word_backward, find_word_forward};
use crate::input::{InputEvent, Key, KeyEventKind, KeyModifiers, KeybindingsManager};
use crate::render::ansi_sequence_len;
use crate::render::{Style, color_enabled, paint_with};
use crate::render::{truncate_to_width, visible_width};
use crate::theme::EditorTheme;

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorSnapshot {
    text: String,
    cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastAction {
    Kill,
    Yank,
    TypeWord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisualLine {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JumpDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutocompleteState {
    Regular,
    Force,
}

type OnChange = Box<dyn FnMut(&str)>;
type OnNoArgFnMut = Box<dyn FnMut()>;

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

    fn insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let should_push_undo = if is_single_plain_word_grapheme(text) {
            self.last_action != Some(LastAction::TypeWord)
        } else {
            true
        };
        if should_push_undo {
            self.push_undo_snapshot();
        }
        self.insert_without_undo(text);
        self.last_action = if is_single_plain_word_grapheme(text) {
            Some(LastAction::TypeWord)
        } else {
            None
        };
        self.last_yank = None;
        self.history_index = None;
    }

    fn insert_without_undo(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    fn submit(&mut self) {
        if self.disable_submit {
            return;
        }
        let submitted = self.expanded_text().trim().to_string();
        if let Some(callback) = &mut self.on_submit {
            callback(&submitted);
        }
        self.text.clear();
        self.cursor = 0;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_action = None;
        self.last_yank = None;
        self.history_index = None;
        self.scroll_offset = 0;
        self.pastes.clear();
        self.paste_counter = 0;
        self.cancel_autocomplete();
    }

    fn delete_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.push_undo_snapshot();
        let start = previous_grapheme_boundary(&self.text, self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.last_action = None;
        self.last_yank = None;
        self.history_index = None;
    }

    fn delete_forward(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        self.push_undo_snapshot();
        let end = next_grapheme_boundary(&self.text, self.cursor);
        self.text.replace_range(self.cursor..end, "");
        self.last_action = None;
        self.last_yank = None;
        self.history_index = None;
    }

    fn delete_word_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = find_word_backward(&self.text, self.cursor);
        self.kill_range(start, self.cursor, true);
        self.cursor = start;
    }

    fn delete_word_forward(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let end = find_word_forward(&self.text, self.cursor);
        self.kill_range(self.cursor, end, false);
    }

    fn delete_to_line_start(&mut self) {
        let start = current_line_start(&self.text, self.cursor);
        if start == self.cursor && self.cursor > 0 {
            let newline_start = previous_grapheme_boundary(&self.text, self.cursor);
            self.kill_range(newline_start, self.cursor, true);
            self.cursor = newline_start;
            return;
        }
        self.kill_range(start, self.cursor, true);
        self.cursor = start;
    }

    fn delete_to_line_end(&mut self) {
        let end = current_line_end(&self.text, self.cursor);
        if end == self.cursor && self.cursor < self.text.len() {
            let newline_end = next_grapheme_boundary(&self.text, self.cursor);
            self.kill_range(self.cursor, newline_end, false);
            return;
        }
        self.kill_range(self.cursor, end, false);
    }

    fn kill_range(&mut self, start: usize, end: usize, prepend: bool) {
        if start >= end {
            return;
        }
        self.push_undo_snapshot();
        let deleted = self.text[start..end].to_string();
        let accumulate = self.last_action == Some(LastAction::Kill);
        self.kill_ring.push(deleted, prepend, accumulate);
        self.text.replace_range(start..end, "");
        if self.cursor > end {
            self.cursor -= end - start;
        } else if self.cursor > start {
            self.cursor = start;
        }
        self.last_action = Some(LastAction::Kill);
        self.last_yank = None;
        self.history_index = None;
    }

    fn yank(&mut self) {
        let Some(text) = self.kill_ring.yank().map(str::to_string) else {
            return;
        };
        self.push_undo_snapshot();
        let start = self.cursor;
        self.insert_without_undo(&text);
        self.last_yank = Some((start, self.cursor));
        self.last_action = Some(LastAction::Yank);
        self.history_index = None;
    }

    fn yank_pop(&mut self) {
        if self.last_action != Some(LastAction::Yank) {
            return;
        }
        let Some((start, end)) = self.last_yank else {
            return;
        };
        let Some(replacement) = self.kill_ring.yank_pop().map(str::to_string) else {
            return;
        };
        self.push_undo_snapshot();
        self.text.replace_range(start..end, "");
        self.cursor = start;
        self.insert_without_undo(&replacement);
        self.last_yank = Some((start, self.cursor));
        self.last_action = Some(LastAction::Yank);
        self.history_index = None;
    }

    fn undo(&mut self) {
        let Some(snapshot) = self.undo_stack.pop() else {
            return;
        };
        self.redo_stack.push(self.snapshot());
        self.restore(snapshot);
    }

    fn redo(&mut self) {
        let Some(snapshot) = self.redo_stack.pop() else {
            return;
        };
        self.undo_stack.push(self.snapshot());
        self.restore(snapshot);
    }

    fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(self.snapshot());
        self.redo_stack.clear();
    }

    fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            text: self.text.clone(),
            cursor: self.cursor,
        }
    }

    fn restore(&mut self, snapshot: EditorSnapshot) {
        self.text = snapshot.text;
        self.cursor = snapshot.cursor.min(self.text.len());
        if !self.text.is_char_boundary(self.cursor) {
            self.cursor = previous_grapheme_boundary(&self.text, self.cursor);
        }
        self.last_action = None;
        self.last_yank = None;
        self.history_index = None;
    }

    fn move_left(&mut self) {
        self.cursor = previous_grapheme_boundary(&self.text, self.cursor);
        self.last_action = None;
        self.last_yank = None;
    }

    fn move_right(&mut self) {
        self.cursor = next_grapheme_boundary(&self.text, self.cursor);
        self.last_action = None;
        self.last_yank = None;
    }

    fn move_line_start(&mut self) {
        self.cursor = current_line_start(&self.text, self.cursor);
        self.last_action = None;
        self.last_yank = None;
    }

    fn move_line_end(&mut self) {
        self.cursor = current_line_end(&self.text, self.cursor);
        self.last_action = None;
        self.last_yank = None;
    }

    fn move_word_left(&mut self) {
        self.cursor = find_word_backward(&self.text, self.cursor);
        self.last_action = None;
        self.last_yank = None;
    }

    fn move_word_right(&mut self) {
        self.cursor = find_word_forward(&self.text, self.cursor);
        self.last_action = None;
        self.last_yank = None;
    }

    fn move_up(&mut self) {
        self.move_vertical(-1);
    }

    fn move_down(&mut self) {
        self.move_vertical(1);
    }

    fn move_vertical(&mut self, delta: isize) {
        let lines = visual_lines(&self.text, self.last_render_width.max(1));
        let Some(current_index) = visual_line_at_cursor(&lines, self.cursor, delta) else {
            return;
        };
        let target_index = current_index as isize + delta;
        if target_index < 0 || target_index >= lines.len() as isize {
            return;
        }
        let current_line = lines[current_index];
        let target_line = lines[target_index as usize];
        let desired_col = visible_width(&self.text[current_line.start..self.cursor]);
        self.cursor = cursor_at_visible_col(&self.text, target_line, desired_col);
        self.last_action = None;
        self.last_yank = None;
    }

    fn emit_change(&mut self) {
        if let Some(callback) = &mut self.on_change {
            callback(&self.text);
        }
    }

    fn handle_paste(&mut self, pasted_text: &str) {
        self.cancel_autocomplete();
        self.history_index = None;
        self.last_action = None;
        self.last_yank = None;

        let filtered = clean_paste_text(pasted_text);
        if filtered.is_empty() {
            return;
        }

        let mut filtered = filtered;
        if starts_like_path(&filtered)
            && let Some(before) = self.text[..self.cursor].chars().next_back()
            && (before == '_' || before.is_alphanumeric())
        {
            filtered.insert(0, ' ');
        }

        let line_count = filtered.split('\n').count();
        let char_count = filtered.chars().count();
        let inserted = if line_count > 10 || char_count > 1000 {
            self.paste_counter += 1;
            let paste_id = self.paste_counter;
            let marker = paste_marker(paste_id, &filtered);
            self.pastes.insert(paste_id, filtered);
            marker
        } else {
            filtered
        };

        self.push_undo_snapshot();
        self.insert_without_undo(&inserted);
        self.last_action = None;
    }

    fn expand_paste_markers(&self, text: &str) -> String {
        let mut expanded = text.to_string();
        for (paste_id, paste_text) in &self.pastes {
            expanded = expanded.replace(&paste_marker(*paste_id, paste_text), paste_text);
        }
        expanded
    }

    fn handle_pending_jump(&mut self, event: &InputEvent) -> bool {
        let Some(direction) = self.jump_mode else {
            return false;
        };

        if self.keybindings.matches(event, "tui.editor.jumpForward")
            || self.keybindings.matches(event, "tui.editor.jumpBackward")
        {
            self.jump_mode = None;
            return true;
        }

        let InputEvent::Key(key_event) = event else {
            self.jump_mode = None;
            return false;
        };
        if key_event.kind == KeyEventKind::Release {
            return true;
        }
        if let Key::Char(text) = &key_event.key
            && !key_event
                .modifiers
                .intersects(KeyModifiers::CTRL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            let target = text.as_str();
            self.jump_mode = None;
            self.jump_to_char(target, direction);
            return true;
        }
        if key_event.key == Key::Space
            && !key_event
                .modifiers
                .intersects(KeyModifiers::CTRL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            self.jump_mode = None;
            self.jump_to_char(" ", direction);
            return true;
        }

        self.jump_mode = None;
        false
    }

    fn jump_to_char(&mut self, target: &str, direction: JumpDirection) {
        if target.is_empty() {
            return;
        }
        let found = match direction {
            JumpDirection::Forward => {
                let start = next_grapheme_boundary(&self.text, self.cursor);
                self.text[start..].find(target).map(|index| start + index)
            }
            JumpDirection::Backward => self.text[..self.cursor].rfind(target),
        };

        if let Some(index) = found.filter(|index| self.text.is_char_boundary(*index)) {
            self.cursor = index;
            self.last_action = None;
            self.last_yank = None;
        }
    }

    fn is_editor_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn is_on_first_visual_line(&self) -> bool {
        current_visual_line_index(&self.text, self.cursor, self.last_render_width.max(1)) == 0
    }

    fn is_on_last_visual_line(&self) -> bool {
        let lines = visual_lines(&self.text, self.last_render_width.max(1));
        current_visual_line_index_from_lines(&lines, self.cursor) + 1 >= lines.len()
    }

    fn navigate_history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next_index = match self.history_index {
            None => 0,
            Some(index) if index + 1 < self.history.len() => index + 1,
            Some(_) => return,
        };
        if self.history_index.is_none() {
            self.push_undo_snapshot();
        }
        self.history_index = Some(next_index);
        let text = self.history[next_index].clone();
        self.replace_text_for_history(text);
    }

    fn navigate_history_down(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index == 0 {
            self.history_index = None;
            self.replace_text_for_history(String::new());
        } else {
            let next_index = index - 1;
            self.history_index = Some(next_index);
            let text = self.history[next_index].clone();
            self.replace_text_for_history(text);
        }
    }

    fn replace_text_for_history(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
        self.scroll_offset = 0;
        self.cancel_autocomplete();
        self.last_action = None;
        self.last_yank = None;
    }

    fn handle_tab_completion(&mut self) {
        if self.autocomplete_state.is_some() {
            self.apply_selected_autocomplete();
            return;
        }

        let (lines, cursor_line, cursor_col) = self.lines_and_cursor();
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let before_cursor = &current_line[..cursor_col.min(current_line.len())];
        let force = !before_cursor.trim_start().starts_with('/')
            || before_cursor.trim_start().contains(char::is_whitespace);
        self.request_autocomplete(force, true);
    }

    fn request_autocomplete(&mut self, force: bool, explicit_tab: bool) {
        let Some(provider) = self.autocomplete_provider.as_ref() else {
            return;
        };
        let (lines, cursor_line, cursor_col) = self.lines_and_cursor();
        if force && !provider.should_trigger_file_completion(&lines, cursor_line, cursor_col) {
            return;
        }

        let Some(suggestions) = provider.get_suggestions(
            &lines,
            cursor_line,
            cursor_col,
            AutocompleteOptions { force },
        ) else {
            self.cancel_autocomplete();
            return;
        };

        if suggestions.items.is_empty() {
            self.cancel_autocomplete();
            return;
        }

        if force && explicit_tab && suggestions.items.len() == 1 {
            self.apply_autocomplete_item(&suggestions.items[0], &suggestions.prefix);
            return;
        }

        self.autocomplete_prefix = suggestions.prefix;
        self.autocomplete_items = suggestions.items;
        self.autocomplete_selected =
            best_autocomplete_match(&self.autocomplete_items, &self.autocomplete_prefix)
                .unwrap_or(0);
        self.autocomplete_state = Some(if force {
            AutocompleteState::Force
        } else {
            AutocompleteState::Regular
        });
    }

    fn refresh_regular_autocomplete(&mut self) {
        if self.autocomplete_provider.is_none() {
            return;
        }
        self.request_autocomplete(false, false);
    }

    fn apply_selected_autocomplete(&mut self) {
        let Some(item) = self
            .autocomplete_items
            .get(self.autocomplete_selected)
            .cloned()
        else {
            self.cancel_autocomplete();
            return;
        };
        let prefix = self.autocomplete_prefix.clone();
        self.apply_autocomplete_item(&item, &prefix);
    }

    fn apply_autocomplete_item(&mut self, item: &AutocompleteItem, prefix: &str) {
        let Some(provider) = self.autocomplete_provider.as_ref() else {
            return;
        };
        let (lines, cursor_line, cursor_col) = self.lines_and_cursor();
        let edit = provider.apply_completion(&lines, cursor_line, cursor_col, item, prefix);
        self.push_undo_snapshot();
        self.apply_completion_edit(edit);
        self.cancel_autocomplete();
        self.history_index = None;
        self.last_action = None;
        self.last_yank = None;
    }

    fn apply_completion_edit(
        &mut self,
        edit: crate::component::editor::autocomplete::CompletionEdit,
    ) {
        self.text = edit.lines.join("\n");
        self.cursor = cursor_from_line_col(&edit.lines, edit.cursor_line, edit.cursor_col);
    }

    fn lines_and_cursor(&self) -> (Vec<String>, usize, usize) {
        let mut lines = Vec::new();
        let mut cursor_line = 0usize;
        let mut cursor_col = 0usize;
        let mut offset = 0usize;
        for (line_index, line) in self.text.split('\n').enumerate() {
            if self.cursor >= offset && self.cursor <= offset + line.len() {
                cursor_line = line_index;
                cursor_col = self.cursor - offset;
            }
            lines.push(line.to_string());
            offset += line.len() + 1;
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        (lines, cursor_line, cursor_col)
    }

    fn cancel_autocomplete(&mut self) {
        self.autocomplete_state = None;
        self.autocomplete_items.clear();
        self.autocomplete_prefix.clear();
        self.autocomplete_selected = 0;
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

impl Editor {
    fn handle_input_inner(&mut self, event: &InputEvent) {
        if self.handle_pending_jump(event) {
            return;
        }

        match event {
            InputEvent::Paste(text) => self.handle_paste(text),
            InputEvent::Key(key_event) if key_event.kind != KeyEventKind::Release => {
                if self.autocomplete_state.is_some() {
                    if self.keybindings.matches(event, "tui.select.cancel") {
                        self.cancel_autocomplete();
                        return;
                    }
                    if self.keybindings.matches(event, "tui.select.up") {
                        if !self.autocomplete_items.is_empty() {
                            self.autocomplete_selected =
                                (self.autocomplete_selected + self.autocomplete_items.len() - 1)
                                    % self.autocomplete_items.len();
                        }
                        return;
                    }
                    if self.keybindings.matches(event, "tui.select.down") {
                        if !self.autocomplete_items.is_empty() {
                            self.autocomplete_selected =
                                (self.autocomplete_selected + 1) % self.autocomplete_items.len();
                        }
                        return;
                    }
                    if self.keybindings.matches(event, "tui.input.tab")
                        || self.keybindings.matches(event, "tui.select.confirm")
                    {
                        self.apply_selected_autocomplete();
                        return;
                    }
                }
                if self.keybindings.matches(event, "tui.editor.undo") {
                    self.undo();
                    return;
                }
                if self.keybindings.matches(event, "tui.editor.redo") {
                    self.redo();
                    return;
                }
                if self.keybindings.matches(event, "tui.input.newLine") {
                    self.insert("\n");
                    self.cancel_autocomplete();
                    return;
                }
                if self.keybindings.matches(event, "tui.input.submit") {
                    self.submit();
                    return;
                }
                if self.keybindings.matches(event, "tui.input.tab") {
                    self.handle_tab_completion();
                    return;
                }
                if self.keybindings.matches(event, "tui.editor.pageUp") {
                    if let Some(callback) = &mut self.on_scroll_page_up {
                        callback();
                    }
                    return;
                }
                if self.keybindings.matches(event, "tui.editor.pageDown") {
                    if let Some(callback) = &mut self.on_scroll_page_down {
                        callback();
                    }
                    return;
                }
                if self
                    .keybindings
                    .matches(event, "tui.editor.deleteToLineEnd")
                {
                    self.delete_to_line_end();
                    self.refresh_regular_autocomplete();
                    return;
                }
                if self
                    .keybindings
                    .matches(event, "tui.editor.deleteToLineStart")
                {
                    self.delete_to_line_start();
                    self.refresh_regular_autocomplete();
                    return;
                }
                if self
                    .keybindings
                    .matches(event, "tui.editor.deleteWordBackward")
                {
                    self.delete_word_backward();
                    self.refresh_regular_autocomplete();
                    return;
                }
                if self
                    .keybindings
                    .matches(event, "tui.editor.deleteWordForward")
                {
                    self.delete_word_forward();
                    self.refresh_regular_autocomplete();
                    return;
                }
                if self
                    .keybindings
                    .matches(event, "tui.editor.deleteCharBackward")
                {
                    self.delete_backward();
                    self.refresh_regular_autocomplete();
                    return;
                }
                if self
                    .keybindings
                    .matches(event, "tui.editor.deleteCharForward")
                {
                    self.delete_forward();
                    self.refresh_regular_autocomplete();
                    return;
                }
                if self.keybindings.matches(event, "tui.editor.yank") {
                    self.yank();
                    return;
                }
                if self.keybindings.matches(event, "tui.editor.yankPop") {
                    self.yank_pop();
                    return;
                }
                if self
                    .keybindings
                    .matches(event, "tui.editor.cursorLineStart")
                {
                    self.move_line_start();
                    return;
                }
                if self.keybindings.matches(event, "tui.editor.cursorLineEnd") {
                    self.move_line_end();
                    return;
                }
                if self.keybindings.matches(event, "tui.editor.cursorWordLeft") {
                    self.move_word_left();
                    return;
                }
                if self
                    .keybindings
                    .matches(event, "tui.editor.cursorWordRight")
                {
                    self.move_word_right();
                    return;
                }
                if self.keybindings.matches(event, "tui.editor.cursorUp") {
                    if self.is_editor_empty()
                        || (self.history_index.is_some() && self.is_on_first_visual_line())
                    {
                        self.navigate_history_up();
                    } else if self.is_on_first_visual_line() {
                        self.move_line_start();
                    } else {
                        self.move_up();
                    }
                    return;
                }
                if self.keybindings.matches(event, "tui.editor.cursorDown") {
                    if self.history_index.is_some() && self.is_on_last_visual_line() {
                        self.navigate_history_down();
                    } else if self.is_on_last_visual_line() {
                        self.move_line_end();
                    } else {
                        self.move_down();
                    }
                    return;
                }
                if self.keybindings.matches(event, "tui.editor.cursorLeft") {
                    self.move_left();
                    return;
                }
                if self.keybindings.matches(event, "tui.editor.cursorRight") {
                    self.move_right();
                    return;
                }
                if self.keybindings.matches(event, "tui.editor.jumpForward") {
                    self.jump_mode = Some(JumpDirection::Forward);
                    self.cancel_autocomplete();
                    return;
                }
                if self.keybindings.matches(event, "tui.editor.jumpBackward") {
                    self.jump_mode = Some(JumpDirection::Backward);
                    self.cancel_autocomplete();
                    return;
                }

                if let Key::Char(text) = &key_event.key {
                    if key_event
                        .modifiers
                        .intersects(KeyModifiers::CTRL | KeyModifiers::ALT | KeyModifiers::SUPER)
                    {
                        return;
                    }
                    self.insert(text);
                    self.refresh_regular_autocomplete();
                } else if key_event.key == Key::Space
                    && !key_event
                        .modifiers
                        .intersects(KeyModifiers::CTRL | KeyModifiers::ALT | KeyModifiers::SUPER)
                {
                    self.insert(" ");
                    self.refresh_regular_autocomplete();
                }
            }
            _ => {}
        }
    }
}

fn wrap_multiline(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for source_line in text.split('\n') {
        wrap_line(source_line, width, &mut lines);
    }
    if text.ends_with('\n') {
        lines.push(String::new());
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn wrap_line(source: &str, width: usize, lines: &mut Vec<String>) {
    if source.is_empty() {
        lines.push(String::new());
        return;
    }

    let mut current = String::new();
    let mut current_width = 0;
    let mut pos = 0;
    while pos < source.len() {
        if let Some(len) = ansi_sequence_len(source, pos) {
            current.push_str(&source[pos..pos + len]);
            pos += len;
            continue;
        }

        let grapheme = source[pos..]
            .graphemes(true)
            .next()
            .expect("pos is inside source");
        let grapheme_width = visible_width(grapheme);
        if current_width + grapheme_width > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
        pos += grapheme.len();
    }
    if !current.is_empty() {
        lines.push(current);
    }
}

fn fit_render_line(line: &str, width: usize) -> String {
    let mut fitted = truncate_to_width(line, width);
    let fitted_width = visible_width(&fitted);
    if fitted_width < width {
        fitted.push_str(&" ".repeat(width - fitted_width));
    }
    fitted
}

fn clean_paste_text(text: &str) -> String {
    let decoded = decode_csi_u_control_bytes(text);
    decoded
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "    ")
        .chars()
        .filter(|ch| *ch == '\n' || *ch >= ' ')
        .collect()
}

fn decode_csi_u_control_bytes(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("\x1b[") {
        out.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find(";5u") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let code = &after_start[..end];
        if let Ok(codepoint) = code.parse::<u8>() {
            if codepoint.is_ascii_lowercase() {
                out.push(char::from(codepoint - b'a' + 1));
                rest = &after_start[end + 3..];
                continue;
            }
            if codepoint.is_ascii_uppercase() {
                out.push(char::from(codepoint - b'A' + 1));
                rest = &after_start[end + 3..];
                continue;
            }
        }
        out.push_str(&rest[start..start + 2 + end + 3]);
        rest = &after_start[end + 3..];
    }
    out.push_str(rest);
    out
}

fn starts_like_path(text: &str) -> bool {
    matches!(text.chars().next(), Some('/' | '~' | '.'))
}

fn paste_marker(paste_id: usize, text: &str) -> String {
    let line_count = text.split('\n').count();
    if line_count > 10 {
        format!("[paste #{paste_id} +{line_count} lines]")
    } else {
        format!("[paste #{paste_id} {} chars]", text.chars().count())
    }
}

fn current_visual_line_index(text: &str, cursor: usize, width: usize) -> usize {
    let lines = visual_lines(text, width);
    current_visual_line_index_from_lines(&lines, cursor)
}

fn current_visual_line_index_from_lines(lines: &[VisualLine], cursor: usize) -> usize {
    lines
        .iter()
        .enumerate()
        .position(|(index, line)| {
            cursor >= line.start
                && (cursor < line.end
                    || (cursor == line.end
                        && (index + 1 == lines.len() || lines[index + 1].start != cursor)))
        })
        .unwrap_or(0)
}

fn cursor_from_line_col(lines: &[String], cursor_line: usize, cursor_col: usize) -> usize {
    let mut cursor = 0usize;
    for (index, line) in lines.iter().enumerate() {
        if index == cursor_line {
            return cursor + cursor_col.min(line.len());
        }
        cursor += line.len() + 1;
    }
    lines.join("\n").len()
}

fn best_autocomplete_match(items: &[AutocompleteItem], prefix: &str) -> Option<usize> {
    if prefix.is_empty() {
        return None;
    }
    let mut first_prefix = None;
    for (index, item) in items.iter().enumerate() {
        if item.value == prefix {
            return Some(index);
        }
        if first_prefix.is_none() && item.value.starts_with(prefix) {
            first_prefix = Some(index);
        }
    }
    first_prefix
}

fn previous_grapheme_boundary(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_grapheme_boundary(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .grapheme_indices(true)
        .nth(1)
        .map(|(index, _)| cursor + index)
        .unwrap_or(text.len())
}

fn current_line_start(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .rfind('\n')
        .map(|index| index + '\n'.len_utf8())
        .unwrap_or(0)
}

fn current_line_end(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .find('\n')
        .map(|index| cursor + index)
        .unwrap_or(text.len())
}

fn is_single_plain_word_grapheme(text: &str) -> bool {
    let mut graphemes = text.graphemes(true);
    let Some(grapheme) = graphemes.next() else {
        return false;
    };
    graphemes.next().is_none()
        && grapheme
            .chars()
            .all(|ch| !ch.is_whitespace() && !ch.is_ascii_punctuation())
}

fn visual_lines(text: &str, width: usize) -> Vec<VisualLine> {
    let mut lines = Vec::new();
    let mut source_start = 0;

    for source_line in text.split_inclusive('\n') {
        let content = source_line.strip_suffix('\n').unwrap_or(source_line);
        push_visual_line_ranges(text, source_start, content.len(), width, &mut lines);
        source_start += source_line.len();
    }

    if text.is_empty() || text.ends_with('\n') {
        lines.push(VisualLine {
            start: text.len(),
            end: text.len(),
        });
    }

    lines
}

fn visual_line_at_cursor(lines: &[VisualLine], cursor: usize, delta: isize) -> Option<usize> {
    lines.iter().enumerate().position(|(index, line)| {
        cursor >= line.start
            && (cursor < line.end
                || (cursor == line.end
                    && (delta > 0 || index + 1 == lines.len() || lines[index + 1].start != cursor)))
    })
}

fn push_visual_line_ranges(
    text: &str,
    source_start: usize,
    source_len: usize,
    width: usize,
    lines: &mut Vec<VisualLine>,
) {
    if source_len == 0 {
        lines.push(VisualLine {
            start: source_start,
            end: source_start,
        });
        return;
    }

    let source_end = source_start + source_len;
    let mut line_start = source_start;
    let mut current_width = 0;
    let mut pos = source_start;
    while pos < source_end {
        if let Some(len) = ansi_sequence_len(text, pos) {
            pos += len;
            continue;
        }

        let grapheme = text[pos..source_end]
            .graphemes(true)
            .next()
            .expect("pos is inside source");
        let grapheme_width = visible_width(grapheme);
        if current_width + grapheme_width > width && line_start < pos {
            lines.push(VisualLine {
                start: line_start,
                end: pos,
            });
            line_start = pos;
            current_width = 0;
        }
        current_width += grapheme_width;
        pos += grapheme.len();
    }

    lines.push(VisualLine {
        start: line_start,
        end: source_end,
    });
}

fn cursor_at_visible_col(text: &str, line: VisualLine, desired_col: usize) -> usize {
    let mut current_col = 0;
    let mut pos = line.start;
    while pos < line.end {
        if let Some(len) = ansi_sequence_len(text, pos) {
            pos += len;
            continue;
        }

        let grapheme = text[pos..line.end]
            .graphemes(true)
            .next()
            .expect("pos is inside visual line");
        let next_col = current_col + visible_width(grapheme);
        if next_col > desired_col {
            return pos;
        }
        current_col = next_col;
        pos += grapheme.len();
    }
    line.end
}
