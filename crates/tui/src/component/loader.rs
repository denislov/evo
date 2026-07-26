use crate::component::Component;
use crate::input::{InputEvent, KeyEventKind, KeybindingsManager};
use crate::render::{truncate_to_width, visible_width};

const DEFAULT_LOADER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoaderIndicatorOptions {
    pub frames: Vec<String>,
}

pub struct Loader {
    message: String,
    frames: Vec<String>,
    current_frame: usize,
}

impl Loader {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            frames: DEFAULT_LOADER_FRAMES
                .iter()
                .map(|frame| (*frame).to_string())
                .collect(),
            current_frame: 0,
        }
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = message.into();
    }

    pub fn set_indicator(&mut self, indicator: LoaderIndicatorOptions) {
        self.frames = indicator.frames;
        self.current_frame = 0;
    }

    pub fn tick(&mut self) {
        if !self.frames.is_empty() {
            self.current_frame = (self.current_frame + 1) % self.frames.len();
        }
    }

    pub fn render_text(&self) -> String {
        let frame = self
            .frames
            .get(self.current_frame)
            .map_or("", String::as_str);
        if frame.is_empty() {
            self.message.clone()
        } else {
            format!("{frame} {}", self.message)
        }
    }
}

impl Component for Loader {
    fn render(&mut self, width: usize) -> Vec<String> {
        if width == 0 {
            return Vec::new();
        }
        vec![fit_line(&self.render_text(), width)]
    }
}

pub struct CancellableLoader {
    loader: Loader,
    keybindings: KeybindingsManager,
    aborted: bool,
    on_abort: Option<Box<dyn FnMut()>>,
}

impl CancellableLoader {
    pub fn new(loader: Loader, keybindings: KeybindingsManager) -> Self {
        Self {
            loader,
            keybindings,
            aborted: false,
            on_abort: None,
        }
    }

    pub fn aborted(&self) -> bool {
        self.aborted
    }

    pub fn set_on_abort(&mut self, callback: Box<dyn FnMut()>) {
        self.on_abort = Some(callback);
    }

    pub fn tick(&mut self) {
        self.loader.tick();
    }

    pub fn loader_mut(&mut self) -> &mut Loader {
        &mut self.loader
    }

    fn abort_once(&mut self) {
        if self.aborted {
            return;
        }
        self.aborted = true;
        if let Some(callback) = &mut self.on_abort {
            callback();
        }
    }
}

impl Component for CancellableLoader {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.loader.render(width)
    }

    fn handle_input(&mut self, event: &InputEvent) {
        match event {
            InputEvent::Key(key_event)
                if key_event.kind != KeyEventKind::Release
                    && self.keybindings.matches(event, "tui.select.cancel") =>
            {
                self.abort_once();
            }
            _ => {}
        }
    }
}

fn fit_line(line: &str, width: usize) -> String {
    let mut line = truncate_to_width(line, width);
    let line_width = visible_width(&line);
    if line_width < width {
        line.push_str(&" ".repeat(width - line_width));
    }
    line
}
