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

pub use builtin::{builtin_dark, builtin_light};
pub use color_value::ColorValue;
pub use json::ThemeJson;
pub use reload::{ThemeReloadSignal, ThemeWatcher};
pub use resolve::{ResolveError, ResolvedColor, resolve};
pub use runtime::ResolvedTheme;
pub use tokens::{REQUIRED_TOKEN_KEYS, ThemeBg, ThemeColor};
