use tui::api::component::{Component, Editor};
use tui::api::input::{InputEvent, KeybindingsManager, fuzzy_filter_indices};
use tui::api::render::{SYSTEM, USER, color_enabled, paint_with};

use crate::interactive::render::fit_line;
use coding_agent::api::embedding::CodingAgentModelCatalogEntry;

const MAX_MODEL_CHOICES: usize = 12;

pub(super) enum SelectorInput {
    Handled,
    Cancel,
    Confirm(Option<usize>),
}

pub(super) fn selection_indices(
    models: &[CodingAgentModelCatalogEntry],
    query: &str,
) -> Vec<usize> {
    fuzzy_filter_indices(models, query, |model| {
        format!("{} {} {}", model.id, model.name, model.provider)
    })
}

pub(super) fn render(
    models: &[CodingAgentModelCatalogEntry],
    query: &str,
    selected: &mut usize,
    width: usize,
) -> Vec<String> {
    let indices = selection_indices(models, query);
    *selected = (*selected).min(indices.len().saturating_sub(1));

    let color = color_enabled();
    let mut lines = vec![fit_line("Select model", width)];
    if indices.is_empty() {
        lines.push(fit_line(
            &paint_with(
                "  No models for configured providers. Add keys in auth.toml or env.",
                &SYSTEM,
                color,
            ),
            width,
        ));
        lines.push(fit_line(&paint_with("  Esc close", &SYSTEM, color), width));
        return lines;
    }

    let window_start = selected.saturating_add(1).saturating_sub(MAX_MODEL_CHOICES);
    let mut previous_provider: Option<&str> = None;
    for (visible_offset, model_index) in indices
        .iter()
        .copied()
        .skip(window_start)
        .take(MAX_MODEL_CHOICES)
        .enumerate()
    {
        let absolute_index = window_start + visible_offset;
        let model = &models[model_index];
        if previous_provider != Some(model.provider.as_str()) {
            lines.push(fit_line(
                &paint_with(&format!("  {}", model.provider), &SYSTEM, color),
                width,
            ));
            previous_provider = Some(model.provider.as_str());
        }
        let marker = if absolute_index == *selected {
            "->"
        } else {
            "  "
        };
        let line = format!(
            "{marker} {:<24} {} · {}",
            model.id,
            paint_with(&model.provider, &SYSTEM, color),
            paint_with(&model.name, &SYSTEM, color)
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
                "({}/{}) Enter select · Esc close",
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
    models: &[CodingAgentModelCatalogEntry],
) -> SelectorInput {
    let indices = selection_indices(models, editor.text());
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
        *selected = selected.saturating_sub(MAX_MODEL_CHOICES);
        return SelectorInput::Handled;
    }
    if keybindings.matches(event, "tui.select.pageDown") {
        *selected = (*selected + MAX_MODEL_CHOICES).min(indices.len().saturating_sub(1));
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
