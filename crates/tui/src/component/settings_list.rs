use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::component::Component;
use crate::fuzzy::fuzzy_filter_indices;
use crate::input::{InputEvent, Key, KeyEventKind, KeyModifiers, KeybindingsManager};
use crate::render::{truncate_to_width, visible_width};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingItem {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub current_value: String,
    pub values: Vec<String>,
}

impl SettingItem {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        current_value: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            current_value: current_value.into(),
            values: Vec::new(),
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn values<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.values = values.into_iter().map(Into::into).collect();
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SettingsListOptions {
    pub enable_search: bool,
}

type OnChange = Box<dyn FnMut(&str, &str)>;
type OnCancel = Box<dyn FnMut()>;
pub type SettingsSubmenuDone = Box<dyn FnMut(Option<String>)>;
type SubmenuFactory = Box<dyn FnMut(&str, SettingsSubmenuDone) -> Box<dyn Component>>;

pub struct SettingsList {
    items: Vec<SettingItem>,
    filtered_indices: Vec<usize>,
    selected: usize,
    max_visible: usize,
    keybindings: KeybindingsManager,
    search: String,
    options: SettingsListOptions,
    on_change: Option<OnChange>,
    on_cancel: Option<OnCancel>,
    submenu_factories: HashMap<String, SubmenuFactory>,
    active_submenu: Option<Box<dyn Component>>,
    active_submenu_item_id: Option<String>,
    active_submenu_selected: usize,
    submenu_result: Rc<RefCell<Option<Option<String>>>>,
}

impl SettingsList {
    pub fn new(
        items: Vec<SettingItem>,
        max_visible: usize,
        keybindings: KeybindingsManager,
    ) -> Self {
        Self::with_options(
            items,
            max_visible,
            keybindings,
            SettingsListOptions::default(),
        )
    }

    pub fn with_options(
        items: Vec<SettingItem>,
        max_visible: usize,
        keybindings: KeybindingsManager,
        options: SettingsListOptions,
    ) -> Self {
        let mut list = Self {
            items,
            filtered_indices: Vec::new(),
            selected: 0,
            max_visible,
            keybindings,
            search: String::new(),
            options,
            on_change: None,
            on_cancel: None,
            submenu_factories: HashMap::new(),
            active_submenu: None,
            active_submenu_item_id: None,
            active_submenu_selected: 0,
            submenu_result: Rc::new(RefCell::new(None)),
        };
        list.rebuild_filter();
        list
    }

    pub fn selected_item(&self) -> Option<&SettingItem> {
        self.filtered_indices
            .get(self.selected)
            .and_then(|index| self.items.get(*index))
    }

    pub fn update_value(&mut self, id: &str, new_value: impl Into<String>) {
        let new_value = new_value.into();
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.current_value = new_value;
        }
        self.rebuild_filter();
    }

    pub fn set_on_change(&mut self, callback: OnChange) {
        self.on_change = Some(callback);
    }

    pub fn set_on_cancel(&mut self, callback: OnCancel) {
        self.on_cancel = Some(callback);
    }

    pub fn set_submenu_factory(&mut self, id: impl Into<String>, factory: SubmenuFactory) {
        self.submenu_factories.insert(id.into(), factory);
    }

    fn rebuild_filter(&mut self) {
        self.filtered_indices = if self.options.enable_search {
            fuzzy_filter_indices(&self.items, &self.search, searchable_text)
        } else {
            (0..self.items.len()).collect()
        };
        if self.selected >= self.filtered_indices.len() {
            self.selected = self.filtered_indices.len().saturating_sub(1);
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.filtered_indices.len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = ((self.selected as isize + delta).rem_euclid(len as isize)) as usize;
    }

    fn activate_selected(&mut self) {
        let Some(item_index) = self.filtered_indices.get(self.selected).copied() else {
            return;
        };
        let item_id = self.items[item_index].id.clone();
        let current_value = self.items[item_index].current_value.clone();
        if let Some(factory) = self.submenu_factories.get_mut(&item_id) {
            *self.submenu_result.borrow_mut() = None;
            let result = Rc::clone(&self.submenu_result);
            let done: SettingsSubmenuDone = Box::new(move |selected_value| {
                *result.borrow_mut() = Some(selected_value);
            });
            self.active_submenu = Some(factory(&current_value, done));
            self.active_submenu_item_id = Some(item_id);
            self.active_submenu_selected = self.selected;
            return;
        }

        let item = &mut self.items[item_index];
        if item.values.is_empty() {
            return;
        }

        let next_index = item
            .values
            .iter()
            .position(|value| value == &item.current_value)
            .map_or(0, |index| (index + 1) % item.values.len());
        item.current_value = item.values[next_index].clone();
        let id = item.id.clone();
        let value = item.current_value.clone();
        if let Some(callback) = &mut self.on_change {
            callback(&id, &value);
        }
        self.rebuild_filter();
    }

    fn apply_submenu_result(&mut self) {
        let Some(selected_value) = self.submenu_result.borrow_mut().take() else {
            return;
        };
        let item_id = self.active_submenu_item_id.take();
        self.active_submenu = None;
        self.selected = self.active_submenu_selected;

        let Some(item_id) = item_id else {
            return;
        };
        if let Some(value) = selected_value {
            if let Some(item) = self.items.iter_mut().find(|item| item.id == item_id) {
                item.current_value = value.clone();
            }
            if let Some(callback) = &mut self.on_change {
                callback(&item_id, &value);
            }
            self.rebuild_filter();
        }
    }

    fn handle_search_key(&mut self, key: &Key) {
        match key {
            Key::Char(text) => {
                self.search.push_str(text);
                self.selected = 0;
                self.rebuild_filter();
            }
            Key::Space => {
                self.search.push(' ');
                self.selected = 0;
                self.rebuild_filter();
            }
            Key::Backspace => {
                self.search.pop();
                self.selected = 0;
                self.rebuild_filter();
            }
            _ => {}
        }
    }
}

impl Component for SettingsList {
    fn render(&mut self, width: usize) -> Vec<String> {
        if width == 0 {
            return Vec::new();
        }
        if let Some(submenu) = &mut self.active_submenu {
            return submenu.render(width);
        }

        let mut lines = Vec::new();
        if self.options.enable_search {
            lines.push(fit_line(&format!("Search: {}", self.search), width));
            lines.push(String::new());
        }

        if self.items.is_empty() {
            lines.push(fit_line("  No settings available", width));
            add_hint(&mut lines, width, self.options.enable_search);
            return lines;
        }

        if self.filtered_indices.is_empty() {
            lines.push(fit_line("  No matching settings", width));
            add_hint(&mut lines, width, self.options.enable_search);
            return lines;
        }

        let visible_count = self.max_visible.max(1);
        let start = self
            .selected
            .saturating_sub(visible_count / 2)
            .min(self.filtered_indices.len().saturating_sub(visible_count));
        let end = (start + visible_count).min(self.filtered_indices.len());
        let max_label_width = self
            .items
            .iter()
            .map(|item| visible_width(&item.label))
            .max()
            .unwrap_or(0)
            .min(30);

        for visible_index in start..end {
            let item = &self.items[self.filtered_indices[visible_index]];
            let marker = if visible_index == self.selected {
                "> "
            } else {
                "  "
            };
            let label = pad_visible(&item.label, max_label_width);
            lines.push(fit_line(
                &format!("{marker}{label}  {}", item.current_value),
                width,
            ));
        }

        if start > 0 || end < self.filtered_indices.len() {
            lines.push(fit_line(
                &format!("  ({}/{})", self.selected + 1, self.filtered_indices.len()),
                width,
            ));
        }

        if let Some(description) = self
            .selected_item()
            .and_then(|item| item.description.as_deref())
        {
            lines.push(String::new());
            for line in wrap_plain(description, width.saturating_sub(2).max(1)) {
                lines.push(fit_line(&format!("  {line}"), width));
            }
        }

        add_hint(&mut lines, width, self.options.enable_search);
        lines
    }

    fn handle_input(&mut self, event: &InputEvent) {
        if let Some(submenu) = &mut self.active_submenu {
            submenu.handle_input(event);
            self.apply_submenu_result();
            return;
        }

        match event {
            InputEvent::Key(key_event) if key_event.kind != KeyEventKind::Release => {
                if self.keybindings.matches(event, "tui.select.up") {
                    self.move_selection(-1);
                    return;
                }
                if self.keybindings.matches(event, "tui.select.down") {
                    self.move_selection(1);
                    return;
                }
                if self.keybindings.matches(event, "tui.select.pageUp") {
                    self.move_selection(-(self.max_visible.max(1) as isize));
                    return;
                }
                if self.keybindings.matches(event, "tui.select.pageDown") {
                    self.move_selection(self.max_visible.max(1) as isize);
                    return;
                }
                if self.keybindings.matches(event, "tui.select.confirm")
                    || key_event.key == Key::Space
                {
                    self.activate_selected();
                    return;
                }
                if self.keybindings.matches(event, "tui.select.cancel") {
                    if let Some(callback) = &mut self.on_cancel {
                        callback();
                    }
                    return;
                }

                if self.options.enable_search
                    && !key_event
                        .modifiers
                        .intersects(KeyModifiers::CTRL | KeyModifiers::ALT | KeyModifiers::SUPER)
                {
                    self.handle_search_key(&key_event.key);
                }
            }
            _ => {}
        }
    }
    fn invalidate(&mut self) {
        if let Some(submenu) = &mut self.active_submenu {
            submenu.invalidate();
        }
    }
}

fn searchable_text(item: &SettingItem) -> String {
    let mut text = format!("{} {} {}", item.id, item.label, item.current_value);
    if let Some(description) = &item.description {
        text.push(' ');
        text.push_str(description);
    }
    text
}

fn add_hint(lines: &mut Vec<String>, width: usize, search_enabled: bool) {
    lines.push(String::new());
    let hint = if search_enabled {
        "  Type to search · Enter/Space to change · Esc to cancel"
    } else {
        "  Enter/Space to change · Esc to cancel"
    };
    lines.push(fit_line(hint, width));
}

fn fit_line(line: &str, width: usize) -> String {
    let mut line = truncate_to_width(line, width);
    let line_width = visible_width(&line);
    if line_width < width {
        line.push_str(&" ".repeat(width - line_width));
    }
    line
}

fn pad_visible(text: &str, width: usize) -> String {
    let mut padded = truncate_to_width(text, width);
    let text_width = visible_width(&padded);
    if text_width < width {
        padded.push_str(&" ".repeat(width - text_width));
    }
    padded
}

fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
            continue;
        }

        let candidate = format!("{current} {word}");
        if visible_width(&candidate) <= width {
            current = candidate;
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}
