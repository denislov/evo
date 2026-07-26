use crate::component::Component;
use crate::component::{SelectItem, SelectList};
use crate::input::{InputEvent, KeybindingsManager};
use crate::render::truncate_to_width;
use crate::theme::SelectListTheme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorDialogOptions {
    pub max_visible: usize,
    pub help: Option<String>,
    pub theme: SelectListTheme,
}

impl Default for SelectorDialogOptions {
    fn default() -> Self {
        Self {
            max_visible: 10,
            help: None,
            theme: SelectListTheme::default(),
        }
    }
}

pub struct SelectorDialog {
    title: String,
    help: Option<String>,
    list: SelectList,
}

impl SelectorDialog {
    pub fn new(
        title: impl Into<String>,
        items: Vec<SelectItem>,
        keybindings: KeybindingsManager,
        options: SelectorDialogOptions,
    ) -> Self {
        let list =
            SelectList::new(items, options.max_visible, keybindings).with_theme(options.theme);
        Self {
            title: title.into(),
            help: options.help,
            list,
        }
    }

    pub fn selected_item(&self) -> Option<&SelectItem> {
        self.list.selected_item()
    }

    pub fn set_filter(&mut self, filter: impl Into<String>) {
        self.list.set_filter(filter);
    }

    pub fn set_on_confirm(&mut self, callback: Box<dyn FnMut(&SelectItem)>) {
        self.list.set_on_confirm(callback);
    }

    pub fn set_on_cancel(&mut self, callback: Box<dyn FnMut()>) {
        self.list.set_on_cancel(callback);
    }

    pub fn list_mut(&mut self) -> &mut SelectList {
        &mut self.list
    }
}

impl Component for SelectorDialog {
    fn render(&mut self, width: usize) -> Vec<String> {
        if width == 0 {
            return Vec::new();
        }

        let mut lines = vec![fit_line(&self.title, width)];
        lines.extend(self.list.render(width));
        if let Some(help) = &self.help {
            lines.push(fit_line(help, width));
        }
        lines
    }

    fn handle_input(&mut self, event: &InputEvent) {
        self.list.handle_input(event);
    }
}

fn fit_line(line: &str, width: usize) -> String {
    truncate_to_width(line, width)
}
