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
