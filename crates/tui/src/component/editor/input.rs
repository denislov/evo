//! Key-input dispatch for [`Editor`]: the keybinding match tree and the
//! autocomplete request/apply flow.

use super::autocomplete::{AutocompleteItem, AutocompleteOptions};
use super::visual::{best_autocomplete_match, cursor_from_line_col};
use super::{Editor, JumpDirection};
use crate::component::editor::autocomplete::CompletionEdit;
use crate::input::{InputEvent, Key, KeyEventKind, KeyModifiers};

impl Editor {
    pub(super) fn handle_input_inner(&mut self, event: &InputEvent) {
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

    pub(super) fn handle_tab_completion(&mut self) {
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

    pub(super) fn request_autocomplete(&mut self, force: bool, explicit_tab: bool) {
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
            super::AutocompleteState::Force
        } else {
            super::AutocompleteState::Regular
        });
    }

    pub(super) fn refresh_regular_autocomplete(&mut self) {
        if self.autocomplete_provider.is_none() {
            return;
        }
        self.request_autocomplete(false, false);
    }

    pub(super) fn apply_selected_autocomplete(&mut self) {
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

    pub(super) fn apply_autocomplete_item(&mut self, item: &AutocompleteItem, prefix: &str) {
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

    pub(super) fn apply_completion_edit(&mut self, edit: CompletionEdit) {
        self.text = edit.lines.join("\n");
        self.cursor = cursor_from_line_col(&edit.lines, edit.cursor_line, edit.cursor_col);
    }

    pub(super) fn lines_and_cursor(&self) -> (Vec<String>, usize, usize) {
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

    pub(super) fn cancel_autocomplete(&mut self) {
        self.autocomplete_state = None;
        self.autocomplete_items.clear();
        self.autocomplete_prefix.clear();
        self.autocomplete_selected = 0;
    }
}
