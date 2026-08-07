//! Text-editing operations for [`Editor`]: insert/delete, kill/yank, undo/
//! redo, cursor movement, history navigation, and paste handling.

use super::{Editor, EditorSnapshot, JumpDirection, LastAction};
use crate::editing::{find_word_backward, find_word_forward};
use crate::input::{InputEvent, Key, KeyEventKind, KeyModifiers};
use crate::render::visible_width;

use super::visual::{
    clean_paste_text, current_line_end, current_line_start, current_visual_line_index,
    current_visual_line_index_from_lines, cursor_at_visible_col, is_single_plain_word_grapheme,
    next_grapheme_boundary, paste_marker, previous_grapheme_boundary, starts_like_path,
    visual_line_at_cursor, visual_lines,
};

impl Editor {
    pub(super) fn insert(&mut self, text: &str) {
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

    pub(super) fn insert_without_undo(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub(super) fn submit(&mut self) {
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

    pub(super) fn delete_backward(&mut self) {
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

    pub(super) fn delete_forward(&mut self) {
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

    pub(super) fn delete_word_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = find_word_backward(&self.text, self.cursor);
        self.kill_range(start, self.cursor, true);
        self.cursor = start;
    }

    pub(super) fn delete_word_forward(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let end = find_word_forward(&self.text, self.cursor);
        self.kill_range(self.cursor, end, false);
    }

    pub(super) fn delete_to_line_start(&mut self) {
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

    pub(super) fn delete_to_line_end(&mut self) {
        let end = current_line_end(&self.text, self.cursor);
        if end == self.cursor && self.cursor < self.text.len() {
            let newline_end = next_grapheme_boundary(&self.text, self.cursor);
            self.kill_range(self.cursor, newline_end, false);
            return;
        }
        self.kill_range(self.cursor, end, false);
    }

    pub(super) fn kill_range(&mut self, start: usize, end: usize, prepend: bool) {
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

    pub(super) fn yank(&mut self) {
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

    pub(super) fn yank_pop(&mut self) {
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

    pub(super) fn undo(&mut self) {
        let Some(snapshot) = self.undo_stack.pop() else {
            return;
        };
        self.redo_stack.push(self.snapshot());
        self.restore(snapshot);
    }

    pub(super) fn redo(&mut self) {
        let Some(snapshot) = self.redo_stack.pop() else {
            return;
        };
        self.undo_stack.push(self.snapshot());
        self.restore(snapshot);
    }

    pub(super) fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(self.snapshot());
        self.redo_stack.clear();
    }

    pub(super) fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            text: self.text.clone(),
            cursor: self.cursor,
        }
    }

    pub(super) fn restore(&mut self, snapshot: EditorSnapshot) {
        self.text = snapshot.text;
        self.cursor = snapshot.cursor.min(self.text.len());
        if !self.text.is_char_boundary(self.cursor) {
            self.cursor = previous_grapheme_boundary(&self.text, self.cursor);
        }
        self.last_action = None;
        self.last_yank = None;
        self.history_index = None;
    }

    pub(super) fn move_left(&mut self) {
        self.cursor = previous_grapheme_boundary(&self.text, self.cursor);
        self.last_action = None;
        self.last_yank = None;
    }

    pub(super) fn move_right(&mut self) {
        self.cursor = next_grapheme_boundary(&self.text, self.cursor);
        self.last_action = None;
        self.last_yank = None;
    }

    pub(super) fn move_line_start(&mut self) {
        self.cursor = current_line_start(&self.text, self.cursor);
        self.last_action = None;
        self.last_yank = None;
    }

    pub(super) fn move_line_end(&mut self) {
        self.cursor = current_line_end(&self.text, self.cursor);
        self.last_action = None;
        self.last_yank = None;
    }

    pub(super) fn move_word_left(&mut self) {
        self.cursor = find_word_backward(&self.text, self.cursor);
        self.last_action = None;
        self.last_yank = None;
    }

    pub(super) fn move_word_right(&mut self) {
        self.cursor = find_word_forward(&self.text, self.cursor);
        self.last_action = None;
        self.last_yank = None;
    }

    pub(super) fn move_up(&mut self) {
        self.move_vertical(-1);
    }

    pub(super) fn move_down(&mut self) {
        self.move_vertical(1);
    }

    pub(super) fn move_vertical(&mut self, delta: isize) {
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

    pub(super) fn emit_change(&mut self) {
        if let Some(callback) = &mut self.on_change {
            callback(&self.text);
        }
    }

    pub(super) fn handle_paste(&mut self, pasted_text: &str) {
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

    pub(super) fn expand_paste_markers(&self, text: &str) -> String {
        let mut expanded = text.to_string();
        for (paste_id, paste_text) in &self.pastes {
            expanded = expanded.replace(&paste_marker(*paste_id, paste_text), paste_text);
        }
        expanded
    }

    pub(super) fn handle_pending_jump(&mut self, event: &InputEvent) -> bool {
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

    pub(super) fn jump_to_char(&mut self, target: &str, direction: JumpDirection) {
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

    pub(super) fn is_editor_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub(super) fn is_on_first_visual_line(&self) -> bool {
        current_visual_line_index(&self.text, self.cursor, self.last_render_width.max(1)) == 0
    }

    pub(super) fn is_on_last_visual_line(&self) -> bool {
        let lines = visual_lines(&self.text, self.last_render_width.max(1));
        current_visual_line_index_from_lines(&lines, self.cursor) + 1 >= lines.len()
    }

    pub(super) fn navigate_history_up(&mut self) {
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

    pub(super) fn navigate_history_down(&mut self) {
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

    pub(super) fn replace_text_for_history(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
        self.scroll_offset = 0;
        self.cancel_autocomplete();
        self.last_action = None;
        self.last_yank = None;
    }
}
