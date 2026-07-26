//! Terminal background detection — ported from `detectTerminalBackground`,
//! `parseOsc11BackgroundColor`, and `getThemeForRgbColor` in `theme.ts`.
//!
//! All functions here are pure (no terminal I/O). `detect_terminal_background`
//! inspects `COLORFGBG` only, matching the TS implementation; an actual OSC 11
//! query is performed elsewhere when the TUI can read stdin synchronously.

/// Detected terminal theme: dark or light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalTheme {
    Dark,
    Light,
}

/// How the terminal background was determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionSource {
    /// `COLORFGBG` environment variable.
    ColorFgbg,
    /// No hint found; defaulted to dark.
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionConfidence {
    High,
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalBackgroundDetection {
    pub theme: TerminalTheme,
    pub source: DetectionSource,
    pub detail: String,
    pub confidence: DetectionConfidence,
}

/// An RGB color as `(red, green, blue)` channels.
pub type Rgb = (u8, u8, u8);

/// Approximate ANSI 0-15 colors (terminal-dependent); mirrors `basicColors`
/// in `ansi256ToHex`.
const BASIC_COLORS: [Rgb; 16] = [
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

/// Convert a 256-color index to RGB, mirroring `ansi256ToHex` + `hexToRgb`.
fn ansi256_to_rgb(index: u8) -> Rgb {
    if index < 16 {
        return BASIC_COLORS[index as usize];
    }
    if index < 232 {
        // 6x6x6 cube: 16 + 36*R + 6*G + B, channel = 0 or 55 + n*40.
        let cube = index - 16;
        let r = cube / 36;
        let g = (cube % 36) / 6;
        let b = cube % 6;
        let channel = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
        return (channel(r), channel(g), channel(b));
    }
    // Grayscale ramp 232-255: 8 + (index - 232) * 10.
    let gray = 8 + (index - 232) * 10;
    (gray, gray, gray)
}

/// Relative luminance of an RGB color (sRGB → linear), mirroring
/// `getRgbColorLuminance`.
fn rgb_luminance((r, g, b): Rgb) -> f64 {
    let to_linear = |channel: u8| {
        let v = channel as f64 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powi(2)
        }
    };
    0.2126 * to_linear(r) + 0.7152 * to_linear(g) + 0.0722 * to_linear(b)
}

fn ansi256_luminance(index: u8) -> f64 {
    rgb_luminance(ansi256_to_rgb(index))
}

/// Classify an RGB color as dark or light by luminance (`>= 0.5` = light).
/// Mirrors `getThemeForRgbColor`.
pub fn get_theme_for_rgb_color(rgb: Rgb) -> TerminalTheme {
    if rgb_luminance(rgb) >= 0.5 {
        TerminalTheme::Light
    } else {
        TerminalTheme::Dark
    }
}

/// Parse an OSC 11 background-color response into RGB.
///
/// Mirrors `parseOsc11BackgroundColor`. Accepts `#RRGGBB`, `#RRRRGGGGBBBB`,
/// and `rgb:RRRR/GGGG/BBBB` (or `rgba:`) forms. Returns `None` for malformed
/// input.
pub fn parse_osc11_background_color(data: &str) -> Option<Rgb> {
    // Strip the `\x1b]11;` prefix and the `\x07` / `\x1b\\` terminator.
    let payload = data.strip_prefix("\x1b]11;")?;
    let payload = payload
        .strip_suffix("\x07")
        .or_else(|| payload.strip_suffix("\x1b\\"))?;
    let value = payload.trim();

    if let Some(hex) = value.strip_prefix('#') {
        if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some((r, g, b));
        }
        if hex.len() == 12 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            let r = parse_osc_hex_channel(&hex[0..4])?;
            let g = parse_osc_hex_channel(&hex[4..8])?;
            let b = parse_osc_hex_channel(&hex[8..12])?;
            return Some((r, g, b));
        }
        return None;
    }

    let rgb_value = value
        .strip_prefix("rgb:")
        .or_else(|| value.strip_prefix("rgba:"))?;
    let mut parts = rgb_value.split('/');
    let r = parse_osc_hex_channel(parts.next()?)?;
    let g = parse_osc_hex_channel(parts.next()?)?;
    let b = parse_osc_hex_channel(parts.next()?)?;
    Some((r, g, b))
}

/// Scale a variable-length hex channel to 0-255, mirroring
/// `parseOscHexChannel`.
fn parse_osc_hex_channel(channel: &str) -> Option<u8> {
    if channel.is_empty() || !channel.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(channel, 16).ok()?;
    let max = 16_u32.checked_pow(channel.len() as u32)?.saturating_sub(1);
    if max == 0 {
        return None;
    }
    Some(((value as f64 / max as f64) * 255.0).round() as u8)
}

/// Extract the background color index from a `COLORFGBG` value. The last
/// valid 0-255 integer field is the background (TS scans right-to-left).
fn colorfgbg_background_index(colorfgbg: &str) -> Option<u8> {
    colorfgbg
        .split(';')
        .rev()
        .find_map(|part| part.trim().parse::<u32>().ok().filter(|&n| n <= 255))
        .map(|n| n as u8)
}

/// Detect the terminal background theme from the environment.
///
/// Checks `COLORFGBG` first (high confidence), then falls back to dark (low
/// confidence). Mirrors `detectTerminalBackground`.
pub fn detect_terminal_background<K, V, I>(env: I) -> TerminalBackgroundDetection
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let colorfgbg = env
        .into_iter()
        .find(|(k, _)| k.as_ref().eq_ignore_ascii_case("COLORFGBG"))
        .map(|(_, v)| v.as_ref().to_string())
        .unwrap_or_default();

    if let Some(bg) = colorfgbg_background_index(&colorfgbg) {
        let theme = if ansi256_luminance(bg) >= 0.5 {
            TerminalTheme::Light
        } else {
            TerminalTheme::Dark
        };
        return TerminalBackgroundDetection {
            theme,
            source: DetectionSource::ColorFgbg,
            detail: format!("background color index {bg}"),
            confidence: DetectionConfidence::High,
        };
    }

    TerminalBackgroundDetection {
        theme: TerminalTheme::Dark,
        source: DetectionSource::Fallback,
        detail: "no terminal background hint found".to_string(),
        confidence: DetectionConfidence::Low,
    }
}
