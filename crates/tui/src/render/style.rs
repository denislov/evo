use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Red,
    Green,
    Yellow,
    Blue,
    Cyan,
    Magenta,
    White,
    Ansi256(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    fn ansi16_code(self) -> Option<u8> {
        match self {
            Color::Default => None,
            Color::Red => Some(1),
            Color::Green => Some(2),
            Color::Yellow => Some(3),
            Color::Blue => Some(4),
            Color::Magenta => Some(5),
            Color::Cyan => Some(6),
            Color::White => Some(7),
            Color::Ansi256(_) | Color::Rgb(_, _, _) => None,
        }
    }

    fn sgr_params(self, foreground: bool) -> Vec<String> {
        match self {
            Color::Default => Vec::new(),
            Color::Ansi256(index) => {
                vec![
                    if foreground { "38" } else { "48" }.to_string(),
                    "5".to_string(),
                    index.to_string(),
                ]
            }
            Color::Rgb(red, green, blue) => {
                vec![
                    if foreground { "38" } else { "48" }.to_string(),
                    "2".to_string(),
                    red.to_string(),
                    green.to_string(),
                    blue.to_string(),
                ]
            }
            color => color
                .ansi16_code()
                .map(|code| vec![format!("{}{code}", if foreground { "3" } else { "4" })])
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ColorLevel {
    #[default]
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub reverse: bool,
}

impl Style {
    pub const fn fg(color: Color) -> Self {
        Self {
            fg: color,
            bg: Color::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            strikethrough: false,
            reverse: false,
        }
    }

    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub const fn dim(mut self) -> Self {
        self.dim = true;
        self
    }

    pub const fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    pub const fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    pub const fn strikethrough(mut self) -> Self {
        self.strikethrough = true;
        self
    }

    pub const fn reverse(mut self) -> Self {
        self.reverse = true;
        self
    }

    pub fn has_any(&self) -> bool {
        self.fg != Color::Default
            || self.bg != Color::Default
            || self.bold
            || self.dim
            || self.italic
            || self.underline
            || self.strikethrough
            || self.reverse
    }
}

pub fn paint(text: &str, style: &Style) -> String {
    paint_with(text, style, color_enabled())
}

pub fn paint_with(text: &str, style: &Style, enabled: bool) -> String {
    paint_with_level(
        text,
        style,
        if enabled {
            ColorLevel::TrueColor
        } else {
            ColorLevel::None
        },
    )
}

pub fn paint_with_level(text: &str, style: &Style, level: ColorLevel) -> String {
    if level == ColorLevel::None || !style.has_any() {
        return text.to_string();
    }

    // Downgrade RGB to the nearest 256-color index on Ansi256/Ansi16 terminals,
    // mirroring TS `rgbTo256` (6x6x6 cube + grayscale, weighted luminance).
    let fg = downsample_color(style.fg, level);
    let bg = downsample_color(style.bg, level);

    let mut params: Vec<String> = Vec::new();
    if style.bold {
        params.push("1".to_string());
    }
    if style.dim {
        params.push("2".to_string());
    }
    if style.italic {
        params.push("3".to_string());
    }
    if style.underline {
        params.push("4".to_string());
    }
    if style.strikethrough {
        params.push("9".to_string());
    }
    if style.reverse {
        params.push("7".to_string());
    }
    params.extend(fg.sgr_params(true));
    params.extend(bg.sgr_params(false));

    format!("\x1b[{}m{}\x1b[0m", params.join(";"), text)
}

/// Downsample a [`Color`] for the terminal's color level. RGB values are
/// quantized to the 256-color palette on `Ansi256`/`Ansi16` terminals
/// (mirrors TS `rgbTo256`); `TrueColor` leaves them unchanged. Non-RGB colors
/// pass through at any level.
fn downsample_color(color: Color, level: ColorLevel) -> Color {
    if level >= ColorLevel::TrueColor {
        return color;
    }
    match color {
        Color::Rgb(r, g, b) => Color::Ansi256(rgb_to_256(r, g, b)),
        other => other,
    }
}

/// Quantize an RGB color to the nearest 256-color palette index, mirroring TS
/// `rgbTo256`. Picks the closer of the 6x6x6 cube and the grayscale ramp
/// (weighted-luminance distance); saturated colors prefer the cube to keep
/// their tint.
fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
    // Grayscale ramp 232-255: 24 shades from 8 to 238.
    let gray_values: [u8; 24] = std::array::from_fn(|i| (8 + i * 10) as u8);

    let r_idx = nearest_cube_index(r);
    let g_idx = nearest_cube_index(g);
    let b_idx = nearest_cube_index(b);
    let cube_index: u8 = (16 + 36 * r_idx + 6 * g_idx + b_idx) as u8;
    let cube_dist = color_distance(r, g, b, CUBE[r_idx], CUBE[g_idx], CUBE[b_idx]);

    let gray = ((0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64).round()) as u8;
    let (gray_idx, gray_value) = nearest_gray(gray, &gray_values);
    let gray_index = 232 + gray_idx as u8;
    let gray_dist = color_distance(r, g, b, gray_value, gray_value, gray_value);

    // Prefer the cube for noticeably saturated colors; only use grayscale when
    // the color is nearly neutral AND closer to gray.
    let spread = r.max(g).max(b).saturating_sub(r.min(g).min(b));
    if spread < 10 && gray_dist < cube_dist {
        gray_index
    } else {
        cube_index
    }
}

fn nearest_cube_index(value: u8) -> usize {
    const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let mut best = 0;
    let mut best_dist = u32::MAX;
    for (i, c) in CUBE.iter().enumerate() {
        let dist = value.abs_diff(*c) as u32;
        if dist < best_dist {
            best_dist = dist;
            best = i;
        }
    }
    best
}

fn nearest_gray(value: u8, gray_values: &[u8]) -> (usize, u8) {
    let mut best = 0;
    let mut best_dist = u32::MAX;
    for (i, g) in gray_values.iter().enumerate() {
        let dist = value.abs_diff(*g) as u32;
        if dist < best_dist {
            best_dist = dist;
            best = i;
        }
    }
    (best, gray_values[best])
}

/// Weighted squared distance (human eye is more sensitive to green), mirroring
/// TS `colorDistance`.
fn color_distance(r1: u8, g1: u8, b1: u8, r2: u8, g2: u8, b2: u8) -> u32 {
    let dr = r1 as i32 - r2 as i32;
    let dg = g1 as i32 - g2 as i32;
    let db = b1 as i32 - b2 as i32;
    (dr * dr * 299 + dg * dg * 587 + db * db * 114) as u32 / 1000
}

static CACHED_COLOR_LEVEL: OnceLock<ColorLevel> = OnceLock::new();

pub fn color_enabled() -> bool {
    color_level() != ColorLevel::None
}

pub fn color_level() -> ColorLevel {
    *CACHED_COLOR_LEVEL.get_or_init(|| detect_color_level_from_env(std::env::vars()))
}

pub fn detect_color_level_from_env<I, K, V>(env: I) -> ColorLevel
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let mut no_color = false;
    let mut term = String::new();
    let mut color_term = String::new();
    let mut term_program = String::new();
    let mut terminal_emulator = String::new();
    let mut kitty = false;
    let mut ghostty = false;
    let mut wezterm = false;
    let mut iterm = false;
    let mut windows_terminal = false;

    for (key, value) in env {
        let key = key.as_ref();
        let value = value.as_ref();
        match key {
            "NO_COLOR" => no_color = true,
            "TERM" => term = value.to_lowercase(),
            "COLORTERM" => color_term = value.to_lowercase(),
            "TERM_PROGRAM" => term_program = value.to_lowercase(),
            "TERMINAL_EMULATOR" => terminal_emulator = value.to_lowercase(),
            "KITTY_WINDOW_ID" => kitty = true,
            "GHOSTTY_RESOURCES_DIR" => ghostty = true,
            "WEZTERM_PANE" => wezterm = true,
            "ITERM_SESSION_ID" => iterm = true,
            "WT_SESSION" => windows_terminal = true,
            _ => {}
        }
    }

    if no_color || term == "dumb" {
        return ColorLevel::None;
    }

    if matches!(color_term.as_str(), "truecolor" | "24bit")
        || kitty
        || ghostty
        || wezterm
        || iterm
        || windows_terminal
        || matches!(
            term_program.as_str(),
            "kitty" | "ghostty" | "wezterm" | "iterm.app" | "vscode" | "alacritty"
        )
        || terminal_emulator == "jetbrains-jediterm"
        || term.contains("ghostty")
    {
        return ColorLevel::TrueColor;
    }

    if term.contains("256color") {
        return ColorLevel::Ansi256;
    }

    if term.is_empty() {
        ColorLevel::None
    } else {
        ColorLevel::Ansi16
    }
}

pub const USER: Style = Style::fg(Color::Cyan);
pub const TOOL_NAME: Style = Style::fg(Color::Yellow);
pub const ERROR: Style = Style::fg(Color::Red).bold();
pub const TOOL_ERROR: Style = Style::fg(Color::Red);
pub const SYSTEM: Style = Style::fg(Color::Default).dim();
pub const STATUS_IDLE: Style = Style::fg(Color::Default).dim();
pub const STATUS_RUNNING: Style = Style::fg(Color::Yellow);
pub const PATH: Style = Style::fg(Color::Cyan);
