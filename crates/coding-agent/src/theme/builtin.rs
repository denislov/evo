//! Built-in `dark` and `light` themes, embedded from the TypeScript
//! reference so Rust ships byte-identical defaults.

use super::ThemeJson;

/// The embedded `dark` theme JSON.
pub const DARK_JSON: &str = include_str!("dark.json");

/// The embedded `light` theme JSON.
pub const LIGHT_JSON: &str = include_str!("light.json");

/// The embedded `theme-schema.json`, used for `$schema` references and
/// editor validation (mirrors the TS schema URL payload).
#[cfg(test)]
pub const SCHEMA_JSON: &str = include_str!("theme-schema.json");

/// Parse the built-in `dark` theme. Panics if the embedded JSON is invalid
/// (a programming error, not a user input).
pub fn builtin_dark() -> ThemeJson {
    parse_builtin(DARK_JSON, "dark")
}

/// Parse the built-in `light` theme.
pub fn builtin_light() -> ThemeJson {
    parse_builtin(LIGHT_JSON, "light")
}

fn parse_builtin(content: &str, name: &str) -> ThemeJson {
    serde_json::from_str(content)
        .unwrap_or_else(|e| panic!("built-in {name} theme is invalid: {e}"))
}
