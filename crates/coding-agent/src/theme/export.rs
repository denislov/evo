//! HTML export color helpers — ported from `getThemeExportColors`,
//! `getResolvedThemeColors`, `isLightTheme`, and `ansi256ToHex` in `theme.ts`.
//!
//! These convert resolved theme colors into CSS-compatible hex strings for
//! `/export` HTML output.

use super::{ColorValue, ResolvedColor, ThemeJson, resolve};

/// Whether a theme is a "light" theme. Mirrors TS `isLightTheme` (currently a
/// name check).
pub fn is_light_theme(name: Option<&str>) -> bool {
    name == Some("light")
}

/// Resolved export colors. Each field is `None` when the theme doesn't specify
/// it (or specifies `""`); 256-color values are converted to hex.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThemeExportColors {
    pub page_bg: Option<String>,
    pub card_bg: Option<String>,
    pub info_bg: Option<String>,
}

/// Resolve a theme's `export` section to hex strings, mirroring
/// `getThemeExportColors`. Variable references are resolved (recursively),
/// 256-color indices convert to nearest hex, and empty/default values become
/// `None`. Returns all-`None` when the theme has no `export` section.
pub fn get_theme_export_colors(theme: &ThemeJson) -> ThemeExportColors {
    let Some(export) = &theme.export else {
        return ThemeExportColors::default();
    };
    ThemeExportColors {
        page_bg: resolve_export_field(export.page_bg.as_ref(), theme),
        card_bg: resolve_export_field(export.card_bg.as_ref(), theme),
        info_bg: resolve_export_field(export.info_bg.as_ref(), theme),
    }
}

fn resolve_export_field(value: Option<&ColorValue>, theme: &ThemeJson) -> Option<String> {
    let value = value?;
    match resolve(value, &theme.vars) {
        Ok(ResolvedColor::Default) => None,
        Ok(ResolvedColor::Hex(r, g, b)) => Some(rgb_to_hex(r, g, b)),
        Ok(ResolvedColor::Ansi256(n)) => Some(ansi256_to_hex(n)),
        Err(_) => None,
    }
}

fn rgb_to_hex(r: u8, g: u8, b: u8) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Convert a 256-color index to a `#rrggbb` hex string, mirroring `ansi256ToHex`.
fn ansi256_to_hex(index: u8) -> String {
    let (r, g, b) = ansi256_to_rgb(index);
    rgb_to_hex(r, g, b)
}

/// 256-color index -> RGB. Mirrors `ansi256ToHex` + `hexToRgb` and the private
/// `ansi256_to_rgb` in `detection.rs` (kept private there; duplicated here to
/// avoid widening the detection API for a single HTML-export caller).
fn ansi256_to_rgb(index: u8) -> (u8, u8, u8) {
    const BASIC: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0x80, 0x00, 0x00),
        (0x00, 0x80, 0x00),
        (0x80, 0x80, 0x00),
        (0x00, 0x00, 0x80),
        (0x80, 0x00, 0x80),
        (0x00, 0x80, 0x80),
        (0xc0, 0xc0, 0xc0),
        (0x80, 0x80, 0x80),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x00, 0x00, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    if index < 16 {
        return BASIC[index as usize];
    }
    if index < 232 {
        let cube = index - 16;
        let r = cube / 36;
        let g = (cube % 36) / 6;
        let b = cube % 6;
        let channel = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
        return (channel(r), channel(g), channel(b));
    }
    let gray = 8 + (index - 232) * 10;
    (gray, gray, gray)
}
