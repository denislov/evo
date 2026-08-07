use tui::api::input::KeybindingsManager;

use crate::interactive::TranscriptItem;
use crate::interactive::key_hints::{app_key_hint, key_hint};
use crate::interactive::keybindings;
use crate::interactive::render::{abbreviate_cwd, format_tokens};
use crate::interactive::root::InteractiveRoot;
use crate::interactive::slash::parse_model_selector_arg;
use coding_agent::api::embedding::CodingAgentAuthCommand;

pub(super) fn handle_settings_command(root: &mut InteractiveRoot) {
    root.local.selecting_settings = true;
    root.local.selecting_model = false;
    root.local.selecting_session = false;
    root.local.editor.set_text("");
}

pub(super) fn handle_model_command(root: &mut InteractiveRoot, args: &str) {
    if args.is_empty() {
        root.local.selecting_model = true;
        root.local.selecting_settings = false;
        root.local.selecting_session = false;
        root.local.model_selection_selected = 0;
        root.local.editor.set_text("");
        return;
    }

    let (model_id, thinking_level) = match parse_model_selector_arg(args) {
        Ok(parsed) => parsed,
        Err(error) => {
            root.transcript.push(TranscriptItem::system(error));
            return;
        }
    };

    match coding_agent::api::embedding::model_catalog_entry_by_id(&model_id) {
        Some(model) => root.set_selected_model_with_thinking(model, thinking_level),
        None => {
            root.transcript
                .push(TranscriptItem::system(format!("Unknown model: {model_id}")));
        }
    }
}

pub(super) fn handle_permission_command(root: &mut InteractiveRoot, args: &str) {
    let args = args.trim();
    if args.is_empty() {
        root.transcript.push(TranscriptItem::system(format!(
            "Permission mode: {} (plan = read-only, ask = prompt first, yolo = auto)",
            root.permission_mode
        )));
        return;
    }
    match args.parse::<coding_agent::api::authorization::ToolAuthorizationMode>() {
        Ok(mode) => {
            root.set_permission_mode(mode);
            root.transcript.push(TranscriptItem::system(format!(
                "Permission mode set: {}",
                root.permission_mode
            )));
        }
        Err(error) => {
            root.transcript.push(TranscriptItem::system(error));
        }
    }
}

pub(super) fn handle_resume_command(root: &mut InteractiveRoot, args: &str) {
    if root.session_choices.is_empty() {
        root.transcript.push(TranscriptItem::system(
            "No sessions found for the current workspace.".to_string(),
        ));
        return;
    }

    if !args.is_empty() {
        if let Some(choice) = root
            .session_choices
            .iter()
            .find(|choice| choice.matches_target(args))
            .cloned()
        {
            root.set_selected_session(choice);
        } else {
            root.transcript
                .push(TranscriptItem::system(format!("Unknown session: {args}")));
        }
        return;
    }

    root.local.selecting_session = true;
    root.local.selecting_model = false;
    root.local.selecting_settings = false;
    root.local.session_selection_selected = 0;
    root.local.editor.set_text("");
}

pub(super) fn handle_name_command(root: &mut InteractiveRoot, args: &str) {
    if args.is_empty() {
        root.transcript.push(TranscriptItem::system(format!(
            "Session name: {}",
            root.session_label
        )));
        return;
    }

    root.session_label = args.to_string();
    root.transcript.push(TranscriptItem::system(format!(
        "Session name set: {}",
        root.session_label
    )));
}

pub(super) fn handle_session_command(root: &mut InteractiveRoot) {
    let cwd = abbreviate_cwd(&root.cwd);
    let mut details = format!(
        "Session Info\n\nName: {}\nModel: {}\nCwd: {}\nTokens\nInput: {}\nOutput: {}",
        root.session_label,
        root.model_id,
        cwd,
        format_tokens(root.stats.input),
        format_tokens(root.stats.output)
    );
    if let Some(choice) = &root.active_session {
        details.push_str(&format!(
            "\nStorage: persistent\nSession ID: {}\nEntries: {}",
            choice.id, choice.entry_count
        ));
        if let Some(leaf_id) = root.active_leaf_id.as_deref() {
            details.push_str(&format!("\nActive leaf: {leaf_id}"));
        }
    }
    root.transcript.push(TranscriptItem::system(details));
}

pub(super) fn handle_hotkeys_command(root: &mut InteractiveRoot) {
    let keybindings =
        KeybindingsManager::new(keybindings::default_keybindings(), Default::default());
    let submit = key_hint(&keybindings, "tui.input.submit", "submit");
    let newline = key_hint(&keybindings, "tui.input.newLine", "newline");
    let interrupt = app_key_hint(&keybindings, "app.interrupt", "interrupt/exit");
    let expand = app_key_hint(&keybindings, "app.tools.expand", "expand tools");
    let page_up = key_hint(&keybindings, "tui.editor.pageUp", "scroll up");
    let page_down = key_hint(&keybindings, "tui.editor.pageDown", "scroll down");
    root.transcript.push(TranscriptItem::system(format!(
        "Hotkeys\n\nNavigation\n- {page_up}\n- {page_down}\n\nEditing\n- {submit}\n- {newline}\n\nApp\n- {interrupt}\n- {expand}"
    )));
}

pub(super) fn handle_changelog_command(root: &mut InteractiveRoot) {
    root.transcript.push(TranscriptItem::system(
        "Changelog display is not implemented in the Rust interactive UI yet.".to_string(),
    ));
}

pub(super) fn handle_login_command(root: &mut InteractiveRoot, args: &str) {
    let mut parts = args.split_whitespace();
    let Some(provider) = parts.next() else {
        root.transcript
            .push(TranscriptItem::system("Usage: /login <provider> <api-key>"));
        return;
    };
    let Some(key) = parts.next() else {
        root.transcript
            .push(TranscriptItem::system("Usage: /login <provider> <api-key>"));
        return;
    };
    if parts.next().is_some() {
        root.transcript.push(TranscriptItem::system(
            "Usage: /login <provider> <api-key> (API keys cannot contain whitespace)",
        ));
        return;
    }

    root.queue_auth_command(CodingAgentAuthCommand::store_api_key(provider, key));
}

pub(super) fn handle_logout_command(root: &mut InteractiveRoot, args: &str) {
    let mut parts = args.split_whitespace();
    let Some(provider) = parts.next() else {
        root.transcript
            .push(TranscriptItem::system("Usage: /logout <provider>"));
        return;
    };
    if parts.next().is_some() {
        root.transcript
            .push(TranscriptItem::system("Usage: /logout <provider>"));
        return;
    }

    root.queue_auth_command(CodingAgentAuthCommand::remove_provider(provider));
}
