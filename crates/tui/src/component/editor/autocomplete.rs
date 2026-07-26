use std::fs;
use std::path::{Path, PathBuf};

use crate::fuzzy::fuzzy_filter_indices;

const PATH_DELIMITERS: &[char] = &[' ', '\t', '"', '\'', '='];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

impl AutocompleteItem {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description: None,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
}

impl SlashCommand {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            argument_hint: None,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn argument_hint(mut self, argument_hint: impl Into<String>) -> Self {
        self.argument_hint = Some(argument_hint.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteSuggestions {
    pub items: Vec<AutocompleteItem>,
    pub prefix: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AutocompleteOptions {
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionEdit {
    pub lines: Vec<String>,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

pub trait AutocompleteProvider {
    fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        options: AutocompleteOptions,
    ) -> Option<AutocompleteSuggestions>;

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> CompletionEdit;

    fn should_trigger_file_completion(
        &self,
        _lines: &[String],
        _cursor_line: usize,
        _cursor_col: usize,
    ) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct CombinedAutocompleteProvider {
    commands: Vec<SlashCommand>,
    base_path: PathBuf,
    env: Vec<(String, String)>,
}

impl CombinedAutocompleteProvider {
    pub fn new(commands: Vec<SlashCommand>, base_path: impl AsRef<Path>) -> Self {
        Self::with_env(commands, base_path, std::env::vars().collect::<Vec<_>>())
    }

    pub fn with_env(
        commands: Vec<SlashCommand>,
        base_path: impl AsRef<Path>,
        env: Vec<(String, String)>,
    ) -> Self {
        Self {
            commands,
            base_path: base_path.as_ref().to_path_buf(),
            env,
        }
    }

    pub fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        options: AutocompleteOptions,
    ) -> Option<AutocompleteSuggestions> {
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let cursor_col = cursor_col.min(current_line.len());
        let text_before_cursor = &current_line[..cursor_col];

        if let Some(prefix) = extract_env_prefix(text_before_cursor) {
            let items = self.env_suggestions(&prefix);
            return (!items.is_empty()).then_some(AutocompleteSuggestions { items, prefix });
        }

        if let Some(prefix) = extract_at_prefix(text_before_cursor) {
            let items = self.file_suggestions(&prefix);
            return (!items.is_empty()).then_some(AutocompleteSuggestions { items, prefix });
        }

        if !options.force && text_before_cursor.starts_with('/') {
            let space_index = text_before_cursor.find(' ');
            if space_index.is_none() {
                let prefix = text_before_cursor.to_string();
                let query = prefix.trim_start_matches('/');
                let indices =
                    fuzzy_filter_indices(&self.commands, query, |command| command.name.clone());
                let items = indices
                    .into_iter()
                    .filter_map(|index| self.commands.get(index))
                    .map(command_item)
                    .collect::<Vec<_>>();
                return (!items.is_empty()).then_some(AutocompleteSuggestions { items, prefix });
            }
            return None;
        }

        let prefix = extract_path_prefix(text_before_cursor, options.force)?;
        let items = self.file_suggestions(&prefix);
        (!items.is_empty()).then_some(AutocompleteSuggestions { items, prefix })
    }

    pub fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> CompletionEdit {
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let cursor_col = cursor_col.min(current_line.len());
        let prefix_start = cursor_col.saturating_sub(prefix.len());
        let before_prefix = &current_line[..prefix_start];
        let after_cursor = &current_line[cursor_col..];
        let adjusted_after_cursor = if is_quoted_prefix(prefix)
            && item.value.ends_with('"')
            && after_cursor.starts_with('"')
        {
            &after_cursor[1..]
        } else {
            after_cursor
        };

        let mut new_lines = lines.to_vec();
        if cursor_line >= new_lines.len() {
            new_lines.resize(cursor_line + 1, String::new());
        }

        if is_slash_command_completion(prefix, before_prefix) {
            new_lines[cursor_line] =
                format!("{before_prefix}/{} {adjusted_after_cursor}", item.value);
            return CompletionEdit {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix.len() + item.value.len() + 2,
            };
        }

        if prefix.starts_with('@') {
            let is_directory = item.label.ends_with('/');
            let suffix = if is_directory { "" } else { " " };
            new_lines[cursor_line] = format!(
                "{before_prefix}{}{suffix}{adjusted_after_cursor}",
                item.value
            );
            let cursor_offset = directory_quote_cursor_offset(item, is_directory);
            return CompletionEdit {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix.len() + cursor_offset + suffix.len(),
            };
        }

        new_lines[cursor_line] = format!("{before_prefix}{}{adjusted_after_cursor}", item.value);
        let is_directory = item.label.ends_with('/');
        CompletionEdit {
            lines: new_lines,
            cursor_line,
            cursor_col: before_prefix.len() + directory_quote_cursor_offset(item, is_directory),
        }
    }

    pub fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let cursor_col = cursor_col.min(current_line.len());
        let text_before_cursor = current_line[..cursor_col].trim();
        !text_before_cursor.starts_with('/') || text_before_cursor.contains(' ')
    }

    fn env_suggestions(&self, prefix: &str) -> Vec<AutocompleteItem> {
        let query = prefix.trim_start_matches('$');
        let indices = fuzzy_filter_indices(&self.env, query, |(name, _)| name.clone());
        indices
            .into_iter()
            .filter_map(|index| self.env.get(index))
            .map(|(name, value)| {
                AutocompleteItem::new(format!("${name}"), name).description(value.clone())
            })
            .collect()
    }

    fn file_suggestions(&self, prefix: &str) -> Vec<AutocompleteItem> {
        let parsed = ParsedPathPrefix::parse(prefix);
        let (search_dir, search_prefix) = match self.search_scope(&parsed.raw_prefix) {
            Some(scope) => scope,
            None => return Vec::new(),
        };

        let Ok(entries) = fs::read_dir(&search_dir) else {
            return Vec::new();
        };

        let mut suggestions = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name
                .to_lowercase()
                .starts_with(&search_prefix.to_lowercase())
            {
                continue;
            }

            let is_directory = entry.file_type().map(|ty| ty.is_dir()).unwrap_or(false);
            let display_path = display_path_for_entry(&parsed.raw_prefix, &name, is_directory);
            let value = build_completion_value(
                &display_path,
                is_directory,
                parsed.is_at_prefix,
                parsed.is_quoted_prefix,
            );
            suggestions.push(AutocompleteItem::new(
                value,
                format!("{name}{}", if is_directory { "/" } else { "" }),
            ));
        }

        suggestions.sort_by(|a, b| {
            let a_dir = a.label.ends_with('/');
            let b_dir = b.label.ends_with('/');
            b_dir.cmp(&a_dir).then_with(|| a.label.cmp(&b.label))
        });
        suggestions
    }

    fn search_scope(&self, raw_prefix: &str) -> Option<(PathBuf, String)> {
        let expanded = expand_home(raw_prefix);
        let is_root_prefix = matches!(raw_prefix, "" | "./" | "../" | "~" | "~/" | "/");
        if is_root_prefix || raw_prefix.ends_with('/') {
            let dir = if expanded.starts_with('/') {
                PathBuf::from(expanded)
            } else {
                self.base_path.join(expanded)
            };
            return Some((dir, String::new()));
        }

        let path = Path::new(&expanded);
        let file = path.file_name()?.to_string_lossy().to_string();
        let dir = path.parent().unwrap_or_else(|| Path::new(""));
        let search_dir = if Path::new(raw_prefix).is_absolute() || expanded.starts_with('/') {
            dir.to_path_buf()
        } else {
            self.base_path.join(dir)
        };
        Some((search_dir, file))
    }
}

impl AutocompleteProvider for CombinedAutocompleteProvider {
    fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        options: AutocompleteOptions,
    ) -> Option<AutocompleteSuggestions> {
        CombinedAutocompleteProvider::get_suggestions(self, lines, cursor_line, cursor_col, options)
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> CompletionEdit {
        CombinedAutocompleteProvider::apply_completion(
            self,
            lines,
            cursor_line,
            cursor_col,
            item,
            prefix,
        )
    }

    fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        CombinedAutocompleteProvider::should_trigger_file_completion(
            self,
            lines,
            cursor_line,
            cursor_col,
        )
    }
}

fn command_item(command: &SlashCommand) -> AutocompleteItem {
    let description = match (&command.argument_hint, &command.description) {
        (Some(hint), Some(description)) => Some(format!("{hint} - {description}")),
        (Some(hint), None) => Some(hint.clone()),
        (None, Some(description)) => Some(description.clone()),
        (None, None) => None,
    };
    let mut item = AutocompleteItem::new(&command.name, &command.name);
    item.description = description;
    item
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedPathPrefix {
    raw_prefix: String,
    is_at_prefix: bool,
    is_quoted_prefix: bool,
}

impl ParsedPathPrefix {
    fn parse(prefix: &str) -> Self {
        if let Some(raw) = prefix.strip_prefix("@\"") {
            return Self {
                raw_prefix: raw.to_string(),
                is_at_prefix: true,
                is_quoted_prefix: true,
            };
        }
        if let Some(raw) = prefix.strip_prefix('"') {
            return Self {
                raw_prefix: raw.to_string(),
                is_at_prefix: false,
                is_quoted_prefix: true,
            };
        }
        if let Some(raw) = prefix.strip_prefix('@') {
            return Self {
                raw_prefix: raw.to_string(),
                is_at_prefix: true,
                is_quoted_prefix: false,
            };
        }
        Self {
            raw_prefix: prefix.to_string(),
            is_at_prefix: false,
            is_quoted_prefix: false,
        }
    }
}

fn extract_env_prefix(text: &str) -> Option<String> {
    let token = current_token(text);
    token.starts_with('$').then_some(token.to_string())
}

fn extract_at_prefix(text: &str) -> Option<String> {
    let token = current_token(text);
    token.starts_with('@').then_some(token.to_string())
}

fn extract_path_prefix(text: &str, force: bool) -> Option<String> {
    let token = current_token(text);
    if force {
        return Some(token.to_string());
    }
    if token.contains('/') || token.starts_with('.') || token.starts_with("~/") {
        return Some(token.to_string());
    }
    (token.is_empty() && text.ends_with(' ')).then_some(String::new())
}

fn current_token(text: &str) -> &str {
    let start = text
        .char_indices()
        .rev()
        .find_map(|(index, ch)| {
            PATH_DELIMITERS
                .contains(&ch)
                .then_some(index + ch.len_utf8())
        })
        .unwrap_or(0);
    &text[start..]
}

fn display_path_for_entry(raw_prefix: &str, name: &str, is_directory: bool) -> String {
    let mut path = if raw_prefix.ends_with('/') {
        format!("{raw_prefix}{name}")
    } else if raw_prefix.contains('/') {
        let parent = Path::new(raw_prefix)
            .parent()
            .unwrap_or_else(|| Path::new(""));
        let joined = if parent.as_os_str().is_empty() {
            PathBuf::from(name)
        } else {
            parent.join(name)
        };
        let mut path = joined.to_string_lossy().replace('\\', "/");
        if raw_prefix.starts_with("./") && !path.starts_with("./") {
            path = format!("./{path}");
        }
        path
    } else {
        name.to_string()
    };

    if is_directory {
        path.push('/');
    }
    path
}

fn build_completion_value(
    path: &str,
    _is_directory: bool,
    is_at_prefix: bool,
    is_quoted_prefix: bool,
) -> String {
    let prefix = if is_at_prefix { "@" } else { "" };
    if is_quoted_prefix || path.contains(' ') {
        format!("{prefix}\"{path}\"")
    } else {
        format!("{prefix}{path}")
    }
}

fn expand_home(path: &str) -> String {
    if path == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return Path::new(&home).join(rest).to_string_lossy().to_string();
    }
    path.to_string()
}

fn is_quoted_prefix(prefix: &str) -> bool {
    prefix.starts_with('"') || prefix.starts_with("@\"")
}

fn is_slash_command_completion(prefix: &str, before_prefix: &str) -> bool {
    prefix.starts_with('/') && before_prefix.trim().is_empty() && !prefix[1..].contains('/')
}

fn directory_quote_cursor_offset(item: &AutocompleteItem, is_directory: bool) -> usize {
    if is_directory && item.value.ends_with('"') {
        item.value.len() - 1
    } else {
        item.value.len()
    }
}
