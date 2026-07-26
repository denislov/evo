use crate::component::Component;
use crate::render::{truncate_to_width, visible_width};

pub struct Text {
    text: String,
}

impl Text {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }
}

impl Component for Text {
    fn render(&mut self, width: usize) -> Vec<String> {
        if width == 0 {
            return Vec::new();
        }

        let mut lines = Vec::new();
        for source_line in self.text.lines() {
            wrap_source_line(source_line, width, &mut lines);
        }
        if self.text.ends_with('\n') {
            lines.push(String::new());
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        lines
    }
}

fn wrap_source_line(source: &str, width: usize, lines: &mut Vec<String>) {
    let mut current = String::new();

    for word in source.split_whitespace() {
        if visible_width(word) > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            split_long_word(word, width, lines);
            continue;
        }

        if current.is_empty() {
            current.push_str(word);
            continue;
        }

        let candidate = format!("{} {}", current, word);
        if visible_width(&candidate) <= width {
            current = candidate;
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }

    if !current.is_empty() {
        lines.push(current);
    } else if source.is_empty() {
        lines.push(String::new());
    }
}

fn split_long_word(word: &str, width: usize, lines: &mut Vec<String>) {
    let mut rest = word.to_string();
    while !rest.is_empty() {
        let chunk = truncate_to_width(&rest, width);
        if chunk.is_empty() {
            break;
        }
        let consumed = chunk.len();
        lines.push(chunk);
        rest = rest[consumed..].to_string();
    }
}
