//! Product-independence boundary coverage.

use std::fs;
use std::path::{Path, PathBuf};

use tui::api::input::GENERIC_TUI_KEYBINDINGS;

#[test]
fn generic_tui_keybindings_remain_product_free() {
    let mut violations = Vec::new();

    for id in GENERIC_TUI_KEYBINDINGS.keys() {
        if !id.starts_with("tui.")
            || id.starts_with("app.")
            || id.contains("plugin")
            || id.contains("agent")
            || id.contains("session")
            || id.contains("model")
        {
            violations.push(id.clone());
        }
    }

    assert!(
        violations.is_empty(),
        "generic tui keybindings must stay product-free and tui.* namespaced:\n{}",
        violations.join("\n")
    );
}

#[test]
fn generic_tui_source_does_not_define_product_or_plugin_ui_state() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("crate should live under crates/tui")
        .to_path_buf();
    let mut violations = Vec::new();

    collect_source_violations(&repo_root, &crate_root.join("src"), &mut violations);

    assert!(
        violations.is_empty(),
        "tui must remain a generic terminal UI crate; product/session/plugin UI state belongs in coding-agent adapters:\n{}",
        violations.join("\n")
    );
}

fn collect_source_violations(repo_root: &Path, path: &Path, violations: &mut Vec<String>) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };

    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .expect("read source directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read source entries");
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            collect_source_violations(repo_root, &entry.path(), violations);
        }
        return;
    }

    if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
        return;
    }

    let relative = path
        .strip_prefix(repo_root)
        .expect("scanned file should be under repo root")
        .to_string_lossy()
        .replace('\\', "/");
    let content = fs::read_to_string(path).expect("read source file");
    for (line_index, line) in content.lines().enumerate() {
        for forbidden in PRODUCT_UI_FORBIDDEN_TERMS {
            if line.contains(forbidden) {
                violations.push(format!(
                    "{}:{}: forbidden `{}` in {}",
                    relative,
                    line_index + 1,
                    forbidden,
                    line.trim()
                ));
            }
        }
    }
}

const PRODUCT_UI_FORBIDDEN_TERMS: &[&str] = &[
    "coding_agent",
    "CodingAgent",
    "CodingAgentSession",
    "CodingAgentEvent",
    "AgentProfile",
    "TeamProfile",
    "SessionService",
    "RuntimeService",
    "FlowService",
    "PluginUi",
    "PluginUI",
    "PluginSlashCommand",
    "PluginCommand",
    "PendingPlugin",
    "active_plugin_ui_dialog",
    "pending_plugin_command_request",
    "pending_plugin_ui_action",
    "pending_plugin_ui_dialog",
    "plugin_ui_actions",
    "plugin_ui_dialogs",
    "plugin_keybindings",
];
