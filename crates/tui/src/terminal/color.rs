/// Terminal color query/response support.
///
/// Mirrors TS `pi/packages/tui/src/terminal-colors.ts`
///
/// - OSC 11 background color query/response
/// - OSC 997 terminal color scheme report (dark/light)
///
/// Parsed RGB color
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Terminal color scheme (dark / light)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColorScheme {
    Dark,
    Light,
}

// ── OSC 11 background color response ─────────────────────────────────────

const OSC11_BG_RESPONSE_PREFIX: &str = "\x1b]11;";
/// Returns true if `data` looks like an OSC 11 background color response.
pub fn is_osc11_background_color_response(data: &str) -> bool {
    data.starts_with(OSC11_BG_RESPONSE_PREFIX)
        && (data.ends_with('\x07') || data.ends_with("\x1b\\"))
}

/// Parse an OSC 11 background color response into an [`RgbColor`].
///
/// Supported formats (from the terminal):
/// - `\x1b]11;#rrggbb\x07`
/// - `\x1b]11;rgb:rrrr/gggg/bbbb\x07`
pub fn parse_osc11_background_color(data: &str) -> Option<RgbColor> {
    let body = data.strip_prefix(OSC11_BG_RESPONSE_PREFIX)?;
    // Trim trailing BEL (\x07) or ST (\x1b\\).
    let value = body
        .strip_suffix('\x07')
        .or_else(|| body.strip_suffix("\x1b\\"))
        .unwrap_or(body);

    // Hex format: #rrggbb or #rrrrggggbbbb
    if let Some(hex) = value.strip_prefix('#') {
        return parse_osc_hex(hex);
    }

    // RGB format: rgb:rrrr/gggg/bbbb or rgba:rrrr/gggg/bbbb/aaaa
    let rgb_value = if let Some(v) = value.strip_prefix("rgba:") {
        v
    } else {
        value.strip_prefix("rgb:")?
    };

    let mut parts = rgb_value.splitn(3, '/');
    let r = parse_osc_hex_channel(parts.next()?)?;
    let g = parse_osc_hex_channel(parts.next()?)?;
    let b = parse_osc_hex_channel(parts.next()?)?;
    Some(RgbColor { r, g, b })
}

fn parse_osc_hex(hex: &str) -> Option<RgbColor> {
    match hex.len() {
        6 => {
            // #rrggbb
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(RgbColor { r, g, b })
        }
        12 => {
            // #rrrrggggbbbb
            let r = parse_osc_hex_channel(&hex[0..4])?;
            let g = parse_osc_hex_channel(&hex[4..8])?;
            let b = parse_osc_hex_channel(&hex[8..12])?;
            Some(RgbColor { r, g, b })
        }
        _ => None,
    }
}

fn parse_osc_hex_channel(channel: &str) -> Option<u8> {
    if !channel.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let len = channel.len();
    if !(1..=4).contains(&len) {
        return None;
    }
    let max = 16usize.pow(len as u32);
    let value = usize::from_str_radix(channel, 16).ok()?;
    Some((value * 255 / (max - 1)) as u8)
}

// ── OSC 997 terminal color scheme report ────────────────────────────────

/// Returns true if `data` matches the OSC 997 color scheme report format.
pub fn is_color_scheme_report(data: &str) -> bool {
    data.starts_with("\x1b[?997;") && data.ends_with('n')
}

/// Parse a terminal color scheme report (OSC 997).
/// Returns `Some(TerminalColorScheme::Light)` for `\x1b[?997;2n`,
/// `Some(TerminalColorScheme::Dark)` for `\x1b[?997;1n`,
/// and `None` for unrecognised responses.
pub fn parse_color_scheme_report(data: &str) -> Option<TerminalColorScheme> {
    let body = data.strip_prefix("\x1b[?997;")?.strip_suffix('n')?;
    match body.trim() {
        "2" => Some(TerminalColorScheme::Light),
        "1" => Some(TerminalColorScheme::Dark),
        _ => None,
    }
}

// ── Query helpers (non-blocking) ────────────────────────────────────────

/// Query the terminal background colour (OSC 11).
/// Returns the escape sequence to send.
pub fn query_background_color() -> String {
    "\x1b]11;?\x07".to_string()
}

/// Enable/disable terminal colour scheme notifications (OSC 2031).
/// Returns the escape sequence to send.
pub fn set_color_scheme_notifications(enabled: bool) -> String {
    if enabled {
        "\x1b[?2031h".to_string()
    } else {
        "\x1b[?2031l".to_string()
    }
}

/// Query the terminal colour scheme (OSC 997).
/// Returns the escape sequence to send.
pub fn query_color_scheme() -> String {
    "\x1b[?997n".to_string()
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_osc11_hex_6() {
        let result = parse_osc11_background_color("\x1b]11;#ff8800\x07");
        assert_eq!(
            result,
            Some(RgbColor {
                r: 255,
                g: 136,
                b: 0
            })
        );
    }

    #[test]
    fn parse_osc11_hex_12() {
        let result = parse_osc11_background_color("\x1b]11;#ffff88880000\x07");
        assert_eq!(
            result,
            Some(RgbColor {
                r: 255,
                g: 136,
                b: 0
            })
        );
    }

    #[test]
    fn parse_osc11_rgb() {
        let result = parse_osc11_background_color("\x1b]11;rgb:ffff/8888/0000\x07");
        assert_eq!(
            result,
            Some(RgbColor {
                r: 255,
                g: 136,
                b: 0
            })
        );
    }

    #[test]
    fn parse_color_scheme_dark() {
        assert_eq!(
            parse_color_scheme_report("\x1b[?997;1n"),
            Some(TerminalColorScheme::Dark)
        );
    }

    #[test]
    fn parse_color_scheme_light() {
        assert_eq!(
            parse_color_scheme_report("\x1b[?997;2n"),
            Some(TerminalColorScheme::Light)
        );
    }

    #[test]
    fn is_osc11_response_true() {
        assert!(is_osc11_background_color_response("\x1b]11;#000000\x07"));
        assert!(is_osc11_background_color_response(
            "\x1b]11;rgb:0000/0000/0000\x1b\\"
        ));
    }

    #[test]
    fn is_osc11_response_false() {
        assert!(!is_osc11_background_color_response("hello"));
        assert!(!is_osc11_background_color_response("\x1b[?997;1n"));
    }
}
