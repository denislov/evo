use tui::api::component::Editor;
use tui::api::input::{InputEvent, KeybindingsManager, fuzzy_filter_indices};
use tui::api::render::{SYSTEM, USER, color_enabled, paint_with};
use tui::api::theme::SelectListTheme;

use crate::interactive::render::fit_line;
use coding_agent::api::embedding::CodingAgentThinkingLevel;

const MAX_SLASH_SUGGESTIONS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BuiltinSlashCommand {
    pub(super) name: String,
    pub(super) description: String,
}

pub(super) fn builtin_slash_commands() -> Vec<BuiltinSlashCommand> {
    vec![
        BuiltinSlashCommand {
            name: "help".into(),
            description: "Show help".into(),
        },
        BuiltinSlashCommand {
            name: "settings".into(),
            description: "Open settings menu".into(),
        },
        BuiltinSlashCommand {
            name: "model".into(),
            description: "Select model".into(),
        },
        BuiltinSlashCommand {
            name: "permission".into(),
            description: "Set permission mode: plan | ask | yolo".into(),
        },
        BuiltinSlashCommand {
            name: "agents".into(),
            description: "List agent profiles".into(),
        },
        BuiltinSlashCommand {
            name: "agent".into(),
            description: "Use or run an agent profile".into(),
        },
        BuiltinSlashCommand {
            name: "teams".into(),
            description: "List team profiles".into(),
        },
        BuiltinSlashCommand {
            name: "team".into(),
            description: "Run a team profile".into(),
        },
        BuiltinSlashCommand {
            name: "delegations".into(),
            description: "Review pending delegation confirmations".into(),
        },
        BuiltinSlashCommand {
            name: "delegation".into(),
            description: "Approve or reject a delegation confirmation".into(),
        },
        BuiltinSlashCommand {
            name: "scoped-models".into(),
            description: "Enable or disable models for cycling".into(),
        },
        BuiltinSlashCommand {
            name: "export".into(),
            description: "Export session".into(),
        },
        BuiltinSlashCommand {
            name: "share".into(),
            description: "Share session as a secret GitHub gist".into(),
        },
        BuiltinSlashCommand {
            name: "copy".into(),
            description: "Copy last assistant message to clipboard".into(),
        },
        BuiltinSlashCommand {
            name: "name".into(),
            description: "Show or set the session display name".into(),
        },
        BuiltinSlashCommand {
            name: "session".into(),
            description: "Show session info and stats".into(),
        },
        BuiltinSlashCommand {
            name: "changelog".into(),
            description: "Show changelog entries".into(),
        },
        BuiltinSlashCommand {
            name: "hotkeys".into(),
            description: "Show keyboard shortcuts".into(),
        },
        BuiltinSlashCommand {
            name: "fork".into(),
            description: "Create a new fork from a previous user message".into(),
        },
        BuiltinSlashCommand {
            name: "clone".into(),
            description: "Duplicate the current session at the current position".into(),
        },
        BuiltinSlashCommand {
            name: "tree".into(),
            description: "Navigate session tree".into(),
        },
        BuiltinSlashCommand {
            name: "login".into(),
            description: "Configure provider authentication".into(),
        },
        BuiltinSlashCommand {
            name: "logout".into(),
            description: "Remove provider authentication".into(),
        },
        BuiltinSlashCommand {
            name: "new".into(),
            description: "Start a new session".into(),
        },
        BuiltinSlashCommand {
            name: "compact".into(),
            description: "Manually compact the session context".into(),
        },
        BuiltinSlashCommand {
            name: "branch-summary".into(),
            description: "Summarize an abandoned session branch".into(),
        },
        BuiltinSlashCommand {
            name: "self-healing-edit".into(),
            description: "Apply a checked edit through the session workflow".into(),
        },
        BuiltinSlashCommand {
            name: "resume".into(),
            description: "Resume a different session".into(),
        },
        BuiltinSlashCommand {
            name: "reload".into(),
            description: "Reload local configuration and resources".into(),
        },
        BuiltinSlashCommand {
            name: "quit".into(),
            description: "Quit evo".into(),
        },
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedSlashCommand {
    pub(super) name: String,
    pub(super) args: String,
    pub(super) original: String,
}

pub(super) fn parse_slash_command(text: &str) -> Option<ParsedSlashCommand> {
    if !text.starts_with('/') {
        return None;
    }
    let without_slash = &text[1..];
    let mut parts = without_slash.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").to_lowercase();
    let args = parts.next().unwrap_or("").trim().to_string();
    Some(ParsedSlashCommand {
        name,
        args,
        original: text.to_string(),
    })
}

pub(super) fn parse_model_selector_arg(
    arg: &str,
) -> Result<(String, Option<CodingAgentThinkingLevel>), String> {
    match arg.rsplit_once(':') {
        Some((model_id, level)) if !model_id.is_empty() && !level.is_empty() => {
            let thinking = level
                .parse::<CodingAgentThinkingLevel>()
                .map_err(|error| error.to_string())?;
            Ok((model_id.to_string(), Some(thinking)))
        }
        _ => Ok((arg.to_string(), None)),
    }
}

pub(super) fn slash_completion_query(text: &str, cursor: usize) -> Option<&str> {
    if cursor != text.len() || !text.starts_with('/') || text.contains('\n') {
        return None;
    }
    let query = &text[1..cursor];
    if query.chars().any(char::is_whitespace) {
        return None;
    }
    Some(query)
}

fn suggestion_indices(
    text: &str,
    cursor: usize,
    dismissed_for: Option<&str>,
    commands: &[BuiltinSlashCommand],
) -> Option<Vec<usize>> {
    if dismissed_for.is_some_and(|dismissed| dismissed == text) {
        return None;
    }
    let query = slash_completion_query(text, cursor)?;
    let indices = fuzzy_filter_indices(commands, query, |command| command.name.to_string());
    (!indices.is_empty()).then_some(indices)
}

pub(super) fn render_suggestions(
    text: &str,
    cursor: usize,
    dismissed_for: Option<&str>,
    selected: &mut usize,
    width: usize,
    commands: &[BuiltinSlashCommand],
    theme: &SelectListTheme,
) -> Vec<String> {
    let Some(indices) = suggestion_indices(text, cursor, dismissed_for, commands) else {
        return Vec::new();
    };
    *selected = (*selected).min(indices.len().saturating_sub(1));

    let color = color_enabled();
    let window_start = (*selected)
        .saturating_add(1)
        .saturating_sub(MAX_SLASH_SUGGESTIONS);
    let mut lines = Vec::new();
    for (visible_offset, command_index) in indices
        .iter()
        .copied()
        .skip(window_start)
        .take(MAX_SLASH_SUGGESTIONS)
        .enumerate()
    {
        let absolute_index = window_start + visible_offset;
        let command = &commands[command_index];
        let label = format!("/{}", command.name);
        // The two-column marker gutter is owned by autocomplete selection.
        // Keeping the command label at column two aligns it with the composer
        // text origin without reusing the composer prompt marker.
        let marker = if absolute_index == *selected {
            "› "
        } else {
            "  "
        };
        let marker_style = if absolute_index == *selected {
            theme.selected_prefix
        } else {
            SYSTEM
        };
        let label_style = if absolute_index == *selected {
            theme.selected_text
        } else {
            USER
        };
        let line = format!(
            "{}{} {}",
            paint_with(marker, &marker_style, color),
            paint_with(&format!("{label:<17}"), &label_style, color),
            paint_with(&command.description, &theme.description, color)
        );
        lines.push(fit_line(&line, width));
    }
    lines.push(fit_line(
        &paint_with(
            &format!("({}/{})", *selected + 1, indices.len()),
            &SYSTEM,
            color,
        ),
        width,
    ));
    lines
}

pub(super) fn handle_suggestion_input(
    keybindings: &KeybindingsManager,
    event: &InputEvent,
    editor: &mut Editor,
    selected: &mut usize,
    dismissed_for: &mut Option<String>,
    commands: &[BuiltinSlashCommand],
) -> bool {
    let Some(indices) = suggestion_indices(
        editor.text(),
        editor.cursor(),
        dismissed_for.as_deref(),
        commands,
    ) else {
        return false;
    };

    if keybindings.matches(event, "tui.select.up") {
        *selected = (*selected + indices.len() - 1) % indices.len();
        return true;
    }
    if keybindings.matches(event, "tui.select.down") {
        *selected = (*selected + 1) % indices.len();
        return true;
    }
    if keybindings.matches(event, "tui.select.pageUp") {
        *selected = selected.saturating_sub(MAX_SLASH_SUGGESTIONS);
        return true;
    }
    if keybindings.matches(event, "tui.select.pageDown") {
        *selected = (*selected + MAX_SLASH_SUGGESTIONS).min(indices.len().saturating_sub(1));
        return true;
    }
    let exact_query_matches_command = slash_completion_query(editor.text(), editor.cursor())
        .is_some_and(|query| indices.iter().any(|index| commands[*index].name == query));
    if keybindings.matches(event, "tui.select.confirm") && exact_query_matches_command {
        return false;
    }
    if keybindings.matches(event, "tui.select.confirm")
        || keybindings.matches(event, "tui.input.tab")
    {
        let command_index = indices[(*selected).min(indices.len() - 1)];
        let command = &commands[command_index];
        editor.set_text(format!("/{} ", command.name));
        *selected = 0;
        *dismissed_for = None;
        return true;
    }
    if keybindings.matches(event, "tui.select.cancel") {
        *dismissed_for = Some(editor.text().to_string());
        return true;
    }

    false
}

pub(super) fn help_text() -> String {
    let commands = builtin_slash_commands();
    let mut lines = vec![
        "commands:".to_string(),
        "  /help, /h, /? - show this help".to_string(),
    ];
    for command in commands {
        if command.name == "help" {
            continue;
        }
        lines.push(format!("  /{:<13} - {}", command.name, command.description));
    }
    lines.push("  /q, /exit      - aliases for /quit".to_string());
    lines.join("\n")
}
