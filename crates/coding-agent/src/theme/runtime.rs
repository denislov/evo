//! `ResolvedTheme` — a theme with all 51 tokens resolved to concrete colors,
//! ready for runtime ANSI generation.

use std::collections::HashMap;

use super::{ResolvedColor, ThemeBg, ThemeColor};

/// A fully-resolved theme: every token maps to a concrete [`ResolvedColor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTheme {
    pub fg_colors: HashMap<ThemeColor, ResolvedColor>,
    pub bg_colors: HashMap<ThemeBg, ResolvedColor>,
}

impl ResolvedTheme {
    /// Foreground color for a token. Falls back to [`ResolvedColor::Default`]
    /// if the token is absent (themes validated upstream should never hit this).
    pub fn fg(&self, color: ThemeColor) -> ResolvedColor {
        self.fg_colors
            .get(&color)
            .copied()
            .unwrap_or(ResolvedColor::Default)
    }

    /// Background color for a token.
    pub fn bg(&self, color: ThemeBg) -> ResolvedColor {
        self.bg_colors
            .get(&color)
            .copied()
            .unwrap_or(ResolvedColor::Default)
    }
}
