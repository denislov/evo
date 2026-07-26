//! `ThemeJson` — the on-disk theme format, ported from `ThemeJsonSchema`
//! in `theme.ts`. Variable references are kept unresolved here; resolution
//! happens later in [`super::resolve`] / the runtime theme.

use std::collections::HashMap;

use serde::Deserialize;

use super::{
    ColorValue, REQUIRED_TOKEN_KEYS, ResolveError, ResolvedColor, ResolvedTheme, ThemeBg,
    ThemeColor, resolve,
};

/// A parsed theme file: name, optional `vars`, the 51 required `colors`
/// tokens, and an optional `export` section for HTML output.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ThemeJson {
    /// `$schema` is ignored at runtime; only present for editor support.
    #[serde(default, rename = "$schema")]
    pub _schema: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub vars: HashMap<String, ColorValue>,
    pub colors: HashMap<String, ColorValue>,
    #[serde(default)]
    pub export: Option<ExportSection>,
}

/// Optional HTML export colors (`export` block). Any field may be omitted;
/// the TS exporter derives defaults from `userMessageBg` when absent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ExportSection {
    #[serde(rename = "pageBg", default)]
    pub page_bg: Option<ColorValue>,
    #[serde(rename = "cardBg", default)]
    pub card_bg: Option<ColorValue>,
    #[serde(rename = "infoBg", default)]
    pub info_bg: Option<ColorValue>,
}

impl ThemeJson {
    /// Resolve every `colors` token against `vars`, producing a
    /// [`ResolvedTheme`]. Unknown color keys are ignored here; required-token
    /// validation is handled by [`Self::missing_tokens`] / the resource loader.
    pub fn resolve_colors(&self) -> Result<ResolvedTheme, ResolveError> {
        let mut fg_colors = HashMap::new();
        let mut bg_colors = HashMap::new();
        for (key, value) in &self.colors {
            let resolved: ResolvedColor = resolve(value, &self.vars)?;
            if let Some(bg) = ThemeBg::from_key(key) {
                bg_colors.insert(bg, resolved);
            } else if let Some(fg) = ThemeColor::from_key(key) {
                fg_colors.insert(fg, resolved);
            }
            // Unknown keys are skipped; reported by the loader if needed.
        }
        Ok(ResolvedTheme {
            fg_colors,
            bg_colors,
        })
    }

    /// Required token keys absent from `colors`, in schema order. Mirrors the
    /// missing-color diagnostics produced by TS `parseThemeJson`.
    pub fn missing_tokens(&self) -> Vec<&'static str> {
        REQUIRED_TOKEN_KEYS
            .iter()
            .copied()
            .filter(|key| !self.colors.contains_key(*key))
            .collect()
    }
}
