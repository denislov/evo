//! Theme system ported from TypeScript `packages/coding-agent/src/modes/interactive/theme/theme.ts`.
//!
//! Implements the 51-token color model, variable resolution, JSON loading,
//! runtime color resolution, and terminal background detection. Built-in
//! themes (`dark.json`, `light.json`) and `theme-schema.json` are embedded
//! alongside this module. ANSI escape generation lives in the `tui`
//! `Style`/`paint` layer.

mod builtin;
mod color_value;
#[cfg(test)]
mod detection;
#[cfg(test)]
mod export;
mod json;
mod reload;
mod resolve;
mod runtime;
mod tokens;

#[cfg(test)]
pub use builtin::{DARK_JSON, LIGHT_JSON, SCHEMA_JSON};
pub use builtin::{builtin_dark, builtin_light};
pub use color_value::ColorValue;
#[cfg(test)]
pub use detection::{
    DetectionConfidence, DetectionSource, TerminalTheme, detect_terminal_background,
    get_theme_for_rgb_color, parse_osc11_background_color,
};
#[cfg(test)]
pub use export::{get_theme_export_colors, is_light_theme};
pub use json::ThemeJson;
#[cfg(test)]
pub use reload::should_watch_target;
pub use reload::{ThemeReloadSignal, ThemeWatcher};
pub use resolve::{ResolveError, ResolvedColor, resolve};
pub use runtime::ResolvedTheme;
pub use tokens::{REQUIRED_TOKEN_KEYS, ThemeBg, ThemeColor};
