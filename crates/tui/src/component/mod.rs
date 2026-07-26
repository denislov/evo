use std::any::Any;

mod box_component;
mod editor;
mod image;
mod input;
mod loader;
mod markdown;
mod select_list;
mod selector_dialog;
mod settings_list;
mod spacer;
mod text;
mod truncated_text;

pub use box_component::{BackgroundFn, Box};
pub use editor::Editor;
pub use editor::autocomplete::{
    AutocompleteItem, AutocompleteOptions, AutocompleteProvider, AutocompleteSuggestions,
    CombinedAutocompleteProvider, CompletionEdit, SlashCommand,
};
pub use image::Image;
pub use input::Input;
pub use loader::{CancellableLoader, Loader, LoaderIndicatorOptions};
pub use markdown::{DefaultTextStyle, Markdown};
pub use select_list::{SelectItem, SelectList};
pub use selector_dialog::{SelectorDialog, SelectorDialogOptions};
pub use settings_list::{SettingItem, SettingsList, SettingsListOptions, SettingsSubmenuDone};
pub use spacer::Spacer;
pub use text::Text;
pub use truncated_text::TruncatedText;

pub type ComponentId = usize;

pub trait Component: Any {
    fn render(&mut self, width: usize) -> Vec<String>;

    fn render_bounded(&mut self, viewport: crate::render::Rect) -> Vec<String> {
        self.set_viewport_size(viewport.width, viewport.height);
        let mut lines = self.render(viewport.width);
        lines.truncate(viewport.height);
        for line in &mut lines {
            *line = crate::render::truncate_to_width(line, viewport.width);
        }
        lines
    }

    fn set_viewport_size(&mut self, _width: usize, _height: usize) {}

    fn handle_input(&mut self, _event: &crate::input::InputEvent) {}

    fn wants_key_release(&self) -> bool {
        false
    }

    fn set_focused(&mut self, _focused: bool) {}

    fn focused(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {}
}

impl dyn Component {
    pub fn as_any(&self) -> &dyn Any {
        self
    }

    pub fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct Container {
    children: Vec<std::boxed::Box<dyn Component>>,
}

impl Container {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    pub fn add_child(&mut self, child: std::boxed::Box<dyn Component>) {
        self.children.push(child);
    }
}

impl Default for Container {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for Container {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for child in &mut self.children {
            lines.extend(child.render(width));
        }
        lines
    }

    fn set_viewport_size(&mut self, width: usize, height: usize) {
        for child in &mut self.children {
            child.set_viewport_size(width, height);
        }
    }

    fn invalidate(&mut self) {
        for child in &mut self.children {
            child.invalidate();
        }
    }
}
