use std::path::Path;

use tui::api::component::{Component, Editor};
use tui::api::input::{InputEvent, KeybindingsManager, fuzzy_filter_indices};
use tui::api::render::{SYSTEM, USER, color_enabled, paint_with};

use crate::interactive::render::{abbreviate_cwd, fit_line};
use crate::interactive::session_actions::SessionChoice;

const MAX_SESSION_CHOICES: usize = 12;

pub(super) enum SelectorInput {
    Handled,
    Cancel,
    Confirm(Option<usize>),
}

pub(super) fn selection_indices(choices: &[SessionChoice], query: &str) -> Vec<usize> {
    fuzzy_filter_indices(choices, query, |choice| choice.searchable_text())
}

pub(super) fn render(
    choices: &[SessionChoice],
    query: &str,
    selected: &mut usize,
    width: usize,
) -> Vec<String> {
    let indices = selection_indices(choices, query);
    *selected = (*selected).min(indices.len().saturating_sub(1));

    let color = color_enabled();
    let mut lines = vec![fit_line("Select session", width)];
    if indices.is_empty() {
        lines.push(fit_line(
            &paint_with("  No matching sessions", &SYSTEM, color),
            width,
        ));
        lines.push(fit_line(&paint_with("  Esc close", &SYSTEM, color), width));
        return lines;
    }

    let window_start = selected
        .saturating_add(1)
        .saturating_sub(MAX_SESSION_CHOICES);
    for (visible_offset, session_index) in indices
        .iter()
        .copied()
        .skip(window_start)
        .take(MAX_SESSION_CHOICES)
        .enumerate()
    {
        let absolute_index = window_start + visible_offset;
        let choice = &choices[session_index];
        let marker = if absolute_index == *selected {
            "->"
        } else {
            "  "
        };
        let cwd = abbreviate_cwd(Path::new(&choice.cwd));
        let line = format!(
            "{marker} {:<24} {} · {} · {} entries",
            choice.display_name(),
            paint_with(&choice.id, &SYSTEM, color),
            paint_with(&cwd, &SYSTEM, color),
            choice.entry_count
        );
        if absolute_index == *selected {
            lines.push(fit_line(&paint_with(&line, &USER, color), width));
        } else {
            lines.push(fit_line(&line, width));
        }
    }
    lines.push(fit_line(
        &paint_with(
            &format!(
                "({}/{}) Enter resume · Esc close",
                *selected + 1,
                indices.len()
            ),
            &SYSTEM,
            color,
        ),
        width,
    ));
    lines
}

pub(super) fn handle_input(
    keybindings: &KeybindingsManager,
    event: &InputEvent,
    editor: &mut Editor,
    selected: &mut usize,
    choices: &[SessionChoice],
) -> SelectorInput {
    let indices = selection_indices(choices, editor.text());
    if keybindings.matches(event, "tui.select.up") {
        if !indices.is_empty() {
            *selected = (*selected + indices.len() - 1) % indices.len();
        }
        return SelectorInput::Handled;
    }
    if keybindings.matches(event, "tui.select.down") {
        if !indices.is_empty() {
            *selected = (*selected + 1) % indices.len();
        }
        return SelectorInput::Handled;
    }
    if keybindings.matches(event, "tui.select.pageUp") {
        *selected = selected.saturating_sub(MAX_SESSION_CHOICES);
        return SelectorInput::Handled;
    }
    if keybindings.matches(event, "tui.select.pageDown") {
        *selected = (*selected + MAX_SESSION_CHOICES).min(indices.len().saturating_sub(1));
        return SelectorInput::Handled;
    }
    if keybindings.matches(event, "tui.select.cancel") {
        return SelectorInput::Cancel;
    }
    if keybindings.matches(event, "tui.select.confirm") {
        return SelectorInput::Confirm(indices.get(*selected).copied());
    }

    let before_text = editor.text().to_string();
    editor.handle_input(event);
    if editor.text() != before_text {
        *selected = 0;
    }
    SelectorInput::Handled
}
