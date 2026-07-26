use tui::api::input::KeybindingsManager;

/// Format a set of keybinding alternatives into display text.
///
/// `"ctrl+c"` -> `"Ctrl+C"`, `"shift+enter"` -> `"Shift+Enter"`.
/// Alternates are joined with `/`.
pub fn format_key_text(keys: &[String]) -> String {
    keys.iter()
        .map(|key| {
            key.split('+')
                .map(capitalize_part)
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn capitalize_part(part: &str) -> String {
    let mut chars = part.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Format a hint for a keybinding id known to the keybinding manager.
///
/// Falls back to the description alone if the action has no registered keys.
pub fn key_hint(kb: &KeybindingsManager, action: &str, description: &str) -> String {
    let keys = kb.get_keys(action);
    if keys.is_empty() {
        description.to_string()
    } else {
        format!("{} {}", format_key_text(&keys), description)
    }
}

/// Format a hint for an app-level action that may not be registered in
/// the active app keybinding catalog. Registered app bindings, including user
/// overrides, win before the small legacy fallback table.
pub fn app_key_hint(kb: &KeybindingsManager, action: &str, description: &str) -> String {
    let keys = kb.get_keys(action);
    if !keys.is_empty() {
        return format!("{} {}", format_key_text(&keys), description);
    }
    if let Some(key) = app_fallback_key(action) {
        format!("{} {}", format_key_text(&[key.to_string()]), description)
    } else {
        description.to_string()
    }
}

fn app_fallback_key(action: &str) -> Option<&'static str> {
    match action {
        "app.interrupt" => Some("ctrl+c"),
        "app.exit" => Some("ctrl+c"),
        "app.tools.expand" => Some("ctrl+o"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interactive::keybindings;
    use std::collections::BTreeMap;

    fn kb() -> KeybindingsManager {
        KeybindingsManager::new(keybindings::default_keybindings(), BTreeMap::new())
    }

    #[test]
    fn format_key_text_capitalizes_modifiers_and_keys() {
        assert_eq!(format_key_text(&["ctrl+c".to_string()]), "Ctrl+C");
        assert_eq!(format_key_text(&["enter".to_string()]), "Enter");
        assert_eq!(format_key_text(&["shift+enter".to_string()]), "Shift+Enter");
    }

    #[test]
    fn format_key_text_joins_alternates_with_slash() {
        assert_eq!(
            format_key_text(&["ctrl+b".to_string(), "left".to_string()]),
            "Ctrl+B/Left"
        );
    }

    #[test]
    fn key_hint_uses_registered_binding() {
        let kb = kb();
        assert_eq!(key_hint(&kb, "tui.input.submit", "submit"), "Enter submit");
    }

    #[test]
    fn app_key_hint_uses_fallback_for_unknown_action() {
        let kb = kb();
        assert!(kb.definition("app.interrupt").is_some());
        assert!(kb.definition("app.tools.expand").is_some());
        assert!(kb.definition("app.model.next").is_some());
        assert!(kb.definition("app.tree.foldOrUp").is_some());
        assert_eq!(
            app_key_hint(&kb, "app.interrupt", "interrupt"),
            "Ctrl+C interrupt"
        );
        assert_eq!(
            app_key_hint(&kb, "app.tools.expand", "expand tools"),
            "Ctrl+O expand tools"
        );
    }

    #[test]
    fn app_key_hint_falls_back_to_registered_when_present() {
        // tui.input.copy is registered to ctrl+c; app_key_hint should prefer the
        // registered binding over the static table when the action is known.
        let kb = kb();
        assert_eq!(app_key_hint(&kb, "tui.input.copy", "copy"), "Ctrl+C copy");
    }

    #[test]
    fn app_key_hint_prefers_user_registered_app_binding() {
        let mut user_bindings = BTreeMap::new();
        user_bindings.insert(
            "app.tools.expand".to_string(),
            vec!["ctrl+shift+x".to_string()],
        );
        let kb = KeybindingsManager::new(keybindings::default_keybindings(), user_bindings);

        assert_eq!(
            app_key_hint(&kb, "app.tools.expand", "expand tools"),
            "Ctrl+Shift+X expand tools"
        );
    }
}
