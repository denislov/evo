use crate::component::Component;
use crate::render::{truncate_to_width, visible_width};

pub struct TruncatedText {
    text: String,
    padding_x: usize,
    padding_y: usize,
}

impl TruncatedText {
    pub fn new(text: impl Into<String>) -> Self {
        Self::with_padding(text, 0, 0)
    }

    pub fn with_padding(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }
}

impl Component for TruncatedText {
    fn render(&mut self, width: usize) -> Vec<String> {
        if width == 0 {
            return Vec::new();
        }

        let mut lines = Vec::new();
        let empty_line = " ".repeat(width);
        for _ in 0..self.padding_y {
            lines.push(empty_line.clone());
        }

        let padding_x = self.padding_x.min(width.saturating_sub(1) / 2);
        let available_width = width.saturating_sub(padding_x * 2);
        let first_line = self.text.split('\n').next().unwrap_or("");
        let display = truncate_to_width(first_line, available_width);
        let mut line = format!(
            "{}{}{}",
            " ".repeat(padding_x),
            display,
            " ".repeat(padding_x)
        );
        let line_width = visible_width(&line);
        if line_width < width {
            line.push_str(&" ".repeat(width - line_width));
        }
        lines.push(truncate_to_width(&line, width));

        for _ in 0..self.padding_y {
            lines.push(empty_line.clone());
        }

        lines
    }
}
