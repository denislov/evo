use crate::component::Component;
use crate::render::{truncate_to_width, visible_width};

pub type BackgroundFn = std::boxed::Box<dyn Fn(&str) -> String>;

pub struct Box {
    children: Vec<std::boxed::Box<dyn Component>>,
    padding_x: usize,
    padding_y: usize,
    background_fn: Option<BackgroundFn>,
}

impl Box {
    pub fn new() -> Self {
        Self::with_padding(1, 1)
    }

    pub fn with_padding(padding_x: usize, padding_y: usize) -> Self {
        Self {
            children: Vec::new(),
            padding_x,
            padding_y,
            background_fn: None,
        }
    }

    pub fn add_child(&mut self, child: std::boxed::Box<dyn Component>) {
        self.children.push(child);
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }

    pub fn set_background_fn(&mut self, background_fn: Option<BackgroundFn>) {
        self.background_fn = background_fn;
    }

    fn apply_background(&self, line: String) -> String {
        if let Some(background_fn) = &self.background_fn {
            background_fn(&line)
        } else {
            line
        }
    }

    fn fit_line(&self, line: &str, width: usize) -> String {
        let mut line = truncate_to_width(line, width);
        let line_width = visible_width(&line);
        if line_width < width {
            line.push_str(&" ".repeat(width - line_width));
        }
        line
    }
}

impl Default for Box {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for Box {
    fn render(&mut self, width: usize) -> Vec<String> {
        if width == 0 || self.children.is_empty() {
            return Vec::new();
        }

        let padding_x = self.padding_x.min(width.saturating_sub(1) / 2);
        let content_width = width.saturating_sub(padding_x * 2).max(1);
        let left_padding = " ".repeat(padding_x);

        let mut child_lines = Vec::new();
        for child in &mut self.children {
            for line in child.render(content_width) {
                child_lines.push(format!("{left_padding}{line}"));
            }
        }

        if child_lines.is_empty() {
            return Vec::new();
        }

        let mut lines = Vec::new();
        let empty_line = self.fit_line("", width);
        for _ in 0..self.padding_y {
            lines.push(self.apply_background(empty_line.clone()));
        }

        for line in child_lines {
            let line = self.fit_line(&line, width);
            lines.push(self.apply_background(line));
        }

        for _ in 0..self.padding_y {
            lines.push(self.apply_background(empty_line.clone()));
        }

        lines
    }

    fn invalidate(&mut self) {
        for child in &mut self.children {
            child.invalidate();
        }
    }
}
