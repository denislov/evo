use tui::api::render::Color;
use tui::api::theme::{ThemePalette, TuiTheme};

use coding_agent::api::settings::{
    CodingAgentResolvedColor, CodingAgentThemeForeground, CodingAgentThemeSnapshot,
};

/// Build the terminal palette for an already-resolved product theme.
pub(crate) fn tui_theme_from_snapshot(theme: &CodingAgentThemeSnapshot) -> TuiTheme {
    TuiTheme::custom(theme.name().to_string(), palette_from_snapshot(theme))
}

fn palette_from_snapshot(theme: &CodingAgentThemeSnapshot) -> ThemePalette {
    ThemePalette {
        accent: to_color(theme.foreground(CodingAgentThemeForeground::Accent)),
        muted: to_color(theme.foreground(CodingAgentThemeForeground::Muted)),
        text: to_color(theme.foreground(CodingAgentThemeForeground::Text)),
        background: Color::Default,
        error: to_color(theme.foreground(CodingAgentThemeForeground::Error)),
        success: to_color(theme.foreground(CodingAgentThemeForeground::Success)),
        warning: to_color(theme.foreground(CodingAgentThemeForeground::Warning)),
        path: to_color(theme.foreground(CodingAgentThemeForeground::Accent)),
        input_border: to_color(theme.foreground(CodingAgentThemeForeground::BorderMuted)),
        menu_border: to_color(theme.foreground(CodingAgentThemeForeground::Border)),
    }
}

pub(crate) fn to_color(color: CodingAgentResolvedColor) -> Color {
    match color {
        CodingAgentResolvedColor::Default => Color::Default,
        CodingAgentResolvedColor::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
        CodingAgentResolvedColor::Ansi256(value) => Color::Ansi256(value),
    }
}
