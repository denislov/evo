use std::collections::BTreeMap;
use std::sync::LazyLock;

use super::InputEvent;
use super::matches_key;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingDefinition {
    pub default_keys: Vec<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingConflict {
    pub key: String,
    pub keybindings: Vec<String>,
}

pub type KeybindingsConfig = BTreeMap<String, Vec<String>>;

#[derive(Debug, Clone)]
pub struct KeybindingsManager {
    definitions: BTreeMap<String, KeybindingDefinition>,
    keys_by_id: BTreeMap<String, Vec<String>>,
    conflicts: Vec<KeybindingConflict>,
}

pub static TUI_KEYBINDINGS: LazyLock<BTreeMap<String, KeybindingDefinition>> =
    LazyLock::new(default_keybindings);

pub static GENERIC_TUI_KEYBINDINGS: LazyLock<BTreeMap<String, KeybindingDefinition>> =
    LazyLock::new(|| {
        TUI_KEYBINDINGS
            .iter()
            .filter(|(id, _definition)| !id.starts_with("app."))
            .map(|(id, definition)| (id.clone(), definition.clone()))
            .collect()
    });

impl KeybindingsManager {
    pub fn new(
        definitions: BTreeMap<String, KeybindingDefinition>,
        user_bindings: KeybindingsConfig,
    ) -> Self {
        let (keys_by_id, conflicts) = resolve_bindings(&definitions, &user_bindings);
        Self {
            definitions,
            keys_by_id,
            conflicts,
        }
    }

    pub fn matches(&self, event: &InputEvent, keybinding: &str) -> bool {
        self.keys_by_id
            .get(keybinding)
            .into_iter()
            .flatten()
            .any(|key| matches_key(event, key))
    }

    pub fn get_keys(&self, keybinding: &str) -> Vec<String> {
        self.keys_by_id.get(keybinding).cloned().unwrap_or_default()
    }

    pub fn definition(&self, keybinding: &str) -> Option<&KeybindingDefinition> {
        self.definitions.get(keybinding)
    }

    pub fn conflicts(&self) -> Vec<KeybindingConflict> {
        self.conflicts.clone()
    }
}

fn resolve_bindings(
    definitions: &BTreeMap<String, KeybindingDefinition>,
    user_bindings: &KeybindingsConfig,
) -> (BTreeMap<String, Vec<String>>, Vec<KeybindingConflict>) {
    let mut user_claims: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (keybinding, keys) in user_bindings {
        if !definitions.contains_key(keybinding) {
            continue;
        }
        for key in normalize_keys(keys) {
            user_claims.entry(key).or_default().push(keybinding.clone());
        }
    }

    let conflicts = user_claims
        .into_iter()
        .filter_map(|(key, keybindings)| {
            if keybindings.len() > 1 {
                Some(KeybindingConflict { key, keybindings })
            } else {
                None
            }
        })
        .collect();

    let mut keys_by_id = BTreeMap::new();
    for (id, definition) in definitions {
        let keys = user_bindings
            .get(id)
            .map(|keys| normalize_keys(keys))
            .unwrap_or_else(|| normalize_keys(&definition.default_keys));
        keys_by_id.insert(id.clone(), keys);
    }

    (keys_by_id, conflicts)
}

fn normalize_keys(keys: &[String]) -> Vec<String> {
    let mut seen = BTreeMap::new();
    let mut normalized = Vec::new();
    for key in keys {
        let key = key.trim();
        if key.is_empty() || seen.contains_key(key) {
            continue;
        }
        seen.insert(key.to_string(), ());
        normalized.push(key.to_string());
    }
    normalized
}

fn default_keybindings() -> BTreeMap<String, KeybindingDefinition> {
    let mut definitions = BTreeMap::new();

    insert(
        &mut definitions,
        "tui.editor.cursorUp",
        &["up"],
        "Move cursor up",
    );
    insert(
        &mut definitions,
        "tui.editor.cursorDown",
        &["down"],
        "Move cursor down",
    );
    insert(
        &mut definitions,
        "tui.editor.cursorLeft",
        &["left", "ctrl+b"],
        "Move cursor left",
    );
    insert(
        &mut definitions,
        "tui.editor.cursorRight",
        &["right", "ctrl+f"],
        "Move cursor right",
    );
    insert(
        &mut definitions,
        "tui.editor.cursorWordLeft",
        &["alt+left", "ctrl+left", "alt+b"],
        "Move cursor word left",
    );
    insert(
        &mut definitions,
        "tui.editor.cursorWordRight",
        &["alt+right", "ctrl+right", "alt+f"],
        "Move cursor word right",
    );
    insert(
        &mut definitions,
        "tui.editor.cursorLineStart",
        &["home", "ctrl+a"],
        "Move to line start",
    );
    insert(
        &mut definitions,
        "tui.editor.cursorLineEnd",
        &["end", "ctrl+e"],
        "Move to line end",
    );
    insert(
        &mut definitions,
        "tui.editor.jumpForward",
        &["ctrl+]"],
        "Jump forward to character",
    );
    insert(
        &mut definitions,
        "tui.editor.jumpBackward",
        &["ctrl+alt+]"],
        "Jump backward to character",
    );
    insert(
        &mut definitions,
        "tui.editor.pageUp",
        &["pageUp"],
        "Page up",
    );
    insert(
        &mut definitions,
        "tui.editor.pageDown",
        &["pageDown"],
        "Page down",
    );
    insert(
        &mut definitions,
        "tui.editor.deleteCharBackward",
        &["backspace"],
        "Delete character backward",
    );
    insert(
        &mut definitions,
        "tui.editor.deleteCharForward",
        &["delete", "ctrl+d"],
        "Delete character forward",
    );
    insert(
        &mut definitions,
        "tui.editor.deleteWordBackward",
        &["ctrl+w", "alt+backspace"],
        "Delete word backward",
    );
    insert(
        &mut definitions,
        "tui.editor.deleteWordForward",
        &["alt+d", "alt+delete"],
        "Delete word forward",
    );
    insert(
        &mut definitions,
        "tui.editor.deleteToLineStart",
        &["ctrl+u"],
        "Delete to line start",
    );
    insert(
        &mut definitions,
        "tui.editor.deleteToLineEnd",
        &["ctrl+k"],
        "Delete to line end",
    );
    insert(&mut definitions, "tui.editor.yank", &["ctrl+y"], "Yank");
    insert(
        &mut definitions,
        "tui.editor.yankPop",
        &["alt+y"],
        "Yank pop",
    );
    insert(&mut definitions, "tui.editor.undo", &["ctrl+-"], "Undo");
    insert(
        &mut definitions,
        "tui.editor.redo",
        &["ctrl+shift+-"],
        "Redo",
    );
    insert(
        &mut definitions,
        "tui.input.newLine",
        &["shift+enter"],
        "Insert newline",
    );
    insert(
        &mut definitions,
        "tui.input.submit",
        &["enter"],
        "Submit input",
    );
    insert(
        &mut definitions,
        "tui.input.tab",
        &["tab"],
        "Tab / autocomplete",
    );
    insert(
        &mut definitions,
        "tui.input.copy",
        &["ctrl+c"],
        "Copy selection",
    );
    insert(
        &mut definitions,
        "tui.select.up",
        &["up"],
        "Move selection up",
    );
    insert(
        &mut definitions,
        "tui.select.down",
        &["down"],
        "Move selection down",
    );
    insert(
        &mut definitions,
        "tui.select.pageUp",
        &["pageUp"],
        "Selection page up",
    );
    insert(
        &mut definitions,
        "tui.select.pageDown",
        &["pageDown"],
        "Selection page down",
    );
    insert(
        &mut definitions,
        "tui.select.confirm",
        &["enter"],
        "Confirm selection",
    );
    insert(
        &mut definitions,
        "tui.select.cancel",
        &["escape", "ctrl+c"],
        "Cancel selection",
    );
    definitions
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
