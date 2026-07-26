use unicode_segmentation::UnicodeSegmentation;

use crate::component::Component;
use crate::editing::CURSOR_MARKER;
use crate::input::{InputEvent, Key, KeyEventKind, KeyModifiers, KeybindingsManager};

type OnSubmit = Box<dyn FnMut(&str)>;
type OnEscape = Box<dyn FnMut()>;

pub struct Input {
    value: String,
    cursor: usize,
    focused: bool,
    keybindings: KeybindingsManager,
    on_submit: Option<OnSubmit>,
    on_escape: Option<OnEscape>,
}

impl Input {
    pub fn new(keybindings: KeybindingsManager) -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            focused: false,
            keybindings,
            on_submit: None,
            on_escape: None,
        }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.value.len();
    }

    pub fn set_on_submit(&mut self, callback: Box<dyn FnMut(&str)>) {
        self.on_submit = Some(callback);
    }

    pub fn set_on_escape(&mut self, callback: Box<dyn FnMut()>) {
        self.on_escape = Some(callback);
    }

    fn insert(&mut self, text: &str) {
        self.value.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    fn delete_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = previous_grapheme_boundary(&self.value, self.cursor);
        self.value.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    fn delete_forward(&mut self) {
        if self.cursor >= self.value.len() {
            return;
        }
        let end = next_grapheme_boundary(&self.value, self.cursor);
        self.value.replace_range(self.cursor..end, "");
    }

    fn move_left(&mut self) {
        self.cursor = previous_grapheme_boundary(&self.value, self.cursor);
    }

    fn move_right(&mut self) {
        self.cursor = next_grapheme_boundary(&self.value, self.cursor);
    }
}

impl Component for Input {
    fn render(&mut self, _width: usize) -> Vec<String> {
        let mut line = self.value.clone();
        if self.focused {
            line.insert_str(self.cursor, CURSOR_MARKER);
        }
        vec![line]
    }

    fn handle_input(&mut self, event: &InputEvent) {
        match event {
            InputEvent::Paste(text) => self.insert(text),
            InputEvent::Key(key_event) if key_event.kind != KeyEventKind::Release => {
                if self.keybindings.matches(event, "tui.input.submit") {
                    if let Some(callback) = &mut self.on_submit {
                        callback(&self.value);
                    }
                    return;
                }
                if matches!(key_event.key, Key::Escape) {
                    if let Some(callback) = &mut self.on_escape {
                        callback();
                    }
                    return;
                }
                if self
                    .keybindings
                    .matches(event, "tui.editor.deleteCharBackward")
                {
                    self.delete_backward();
                    return;
                }
                if self
                    .keybindings
                    .matches(event, "tui.editor.deleteCharForward")
                {
                    self.delete_forward();
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
                if self
                    .keybindings
                    .matches(event, "tui.editor.cursorLineStart")
                {
                    self.cursor = 0;
                    return;
                }
                if self.keybindings.matches(event, "tui.editor.cursorLineEnd") {
                    self.cursor = self.value.len();
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
                } else if key_event.key == Key::Space
                    && !key_event
                        .modifiers
                        .intersects(KeyModifiers::CTRL | KeyModifiers::ALT | KeyModifiers::SUPER)
                {
                    self.insert(" ");
                }
            }
            _ => {}
        }
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn focused(&self) -> bool {
        self.focused
    }
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
