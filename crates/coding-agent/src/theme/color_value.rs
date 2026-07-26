//! `ColorValue` — the four color formats supported by evo themes.
//!
//! Ported from the `ColorValueSchema` union in
//! `packages/coding-agent/src/modes/interactive/theme/theme.ts`:
//! hex (`"#ff0000"`), 256-color index (`0..=255`), variable reference
//! (`"primary"`), or terminal default (`""`).

use serde::de::{self, Deserialize};
use serde_json::Value;

/// A raw, unresolved color value as it appears in theme JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColorValue {
    /// Empty string `""` — the terminal's default color.
    Default,
    /// Six-digit hex RGB, e.g. `"#ff0000"`.
    Hex(u8, u8, u8),
    /// xterm 256-color palette index (0-255).
    Ansi256(u8),
    /// A reference to a `vars` entry, resolved later by [`super::resolve`].
    Var(String),
}

impl ColorValue {
    /// Classify a JSON value into a [`ColorValue`].
    ///
    /// Returns `None` for malformed input (out-of-range integers, short hex,
    /// non-string/non-integer types) so callers can report diagnostics.
    pub fn parse(value: &Value) -> Option<Self> {
        if let Some(n) = value.as_u64() {
            return u8::try_from(n).ok().map(ColorValue::Ansi256);
        }
        let s = value.as_str()?;
        if s.is_empty() {
            Some(ColorValue::Default)
        } else if let Some(hex) = s.strip_prefix('#') {
            parse_hex(hex).map(|(r, g, b)| ColorValue::Hex(r, g, b))
        } else {
            // Any other string is treated as a variable reference; invalid
            // references are caught during resolution.
            Some(ColorValue::Var(s.to_string()))
        }
    }
}

fn parse_hex(hex: &str) -> Option<(u8, u8, u8)> {
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}

/// Deserialize a [`ColorValue`] directly from theme JSON. Reuses [`ColorValue::parse`]
/// so serde-driven and manual parsing share one source of truth.
impl<'de> Deserialize<'de> for ColorValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        ColorValue::parse(&value)
            .ok_or_else(|| de::Error::custom(format!("invalid color value: {value}")))
    }
}
