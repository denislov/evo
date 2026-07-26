use std::collections::BTreeMap;
use std::sync::LazyLock;

use tui::api::input::{KeybindingDefinition, TUI_KEYBINDINGS};

pub(crate) static APP_KEYBINDINGS: LazyLock<BTreeMap<String, KeybindingDefinition>> =
    LazyLock::new(|| {
        let mut definitions = TUI_KEYBINDINGS.clone();
        insert(
            &mut definitions,
            "app.interrupt",
            &["ctrl+c"],
            "Interrupt or exit",
        );
        insert(&mut definitions, "app.exit", &["ctrl+c"], "Exit");
        insert(
            &mut definitions,
            "app.focus.next",
            &["tab"],
            "Focus next region",
        );
        insert(
            &mut definitions,
            "app.focus.previous",
            &["shift+tab"],
            "Focus previous region",
        );
        insert(
            &mut definitions,
            "app.context.toggle",
            &["ctrl+g"],
            "Toggle context region",
        );
        insert(
            &mut definitions,
            "app.context.previousTab",
            &["left"],
            "Select previous context tab",
        );
        insert(
            &mut definitions,
            "app.context.nextTab",
            &["right"],
            "Select next context tab",
        );
        insert(
            &mut definitions,
            "app.transcript.arguments",
            &["a"],
            "Toggle selected tool arguments",
        );
        insert(
            &mut definitions,
            "app.tools.expand",
            &["ctrl+o"],
            "Expand tool results",
        );
        insert(
            &mut definitions,
            "app.model.next",
            &["ctrl+p"],
            "Cycle to next model",
        );
        insert(
            &mut definitions,
            "app.model.previous",
            &["ctrl+shift+p"],
            "Cycle to previous model",
        );
        insert(
            &mut definitions,
            "app.tree.foldOrUp",
            &["ctrl+left", "alt+left"],
            "Fold node or go up branch",
        );
        insert(
            &mut definitions,
            "app.tree.unfoldOrDown",
            &["ctrl+right", "alt+right"],
            "Unfold node or go down branch",
        );
        insert(
            &mut definitions,
            "app.tree.editLabel",
            &["shift+l"],
            "Edit label for selected node",
        );
        insert(
            &mut definitions,
            "app.tree.toggleLabelTimestamp",
            &["shift+t"],
            "Toggle label timestamp display",
        );
        insert(
            &mut definitions,
            "app.tree.filter.default",
            &["ctrl+d"],
            "Tree filter: default",
        );
        insert(
            &mut definitions,
            "app.tree.filter.noTools",
            &["ctrl+t"],
            "Tree filter: no-tools",
        );
        insert(
            &mut definitions,
            "app.tree.filter.userOnly",
            &["ctrl+u"],
            "Tree filter: user-only",
        );
        insert(
            &mut definitions,
            "app.tree.filter.labeledOnly",
            &["ctrl+l"],
            "Tree filter: labeled-only",
        );
        insert(
            &mut definitions,
            "app.tree.filter.all",
            &["ctrl+a"],
            "Tree filter: all",
        );
        insert(
            &mut definitions,
            "app.tree.filter.cycleForward",
            &["ctrl+o"],
            "Cycle tree filter forward",
        );
        insert(
            &mut definitions,
            "app.tree.filter.cycleBackward",
            &["ctrl+shift+o"],
            "Cycle tree filter backward",
        );
        definitions
    });

pub(crate) fn default_keybindings() -> BTreeMap<String, KeybindingDefinition> {
    APP_KEYBINDINGS.clone()
}

fn insert(
    definitions: &mut BTreeMap<String, KeybindingDefinition>,
    id: &str,
    keys: &[&str],
    description: &str,
) {
    definitions.insert(
        id.to_string(),
        KeybindingDefinition {
            default_keys: keys.iter().map(|key| key.to_string()).collect(),
            description: Some(description.to_string()),
        },
    );
}
