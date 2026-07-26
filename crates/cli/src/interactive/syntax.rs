//! Syntax highlighting — ported from `highlightCode`, `getLanguageFromPath`,
//! and `buildCliHighlightTheme` in `theme.ts`.
//!
//! Uses `syntect` (Sublime syntaxes) in place of TS's `cli-highlight`
//! (highlight.js). We build a `syntect::highlighting::Theme` whose scope
//! selectors map to the 9 theme syntax tokens, mirroring the hljs-scope ->
//! token mapping in `buildCliHighlightTheme`. `HighlightLines` then yields
//! per-region foreground colors already chosen from the active evo theme.

use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SyntectColor, ScopeSelectors, StyleModifier, Theme as SyntectTheme, ThemeItem,
};
use syntect::parsing::SyntaxSet;

use coding_agent::api::settings::{
    CodingAgentResolvedColor, CodingAgentThemeForeground, CodingAgentThemeSnapshot,
};

/// A loaded `SyntaxSet`. Cached for the process lifetime (loading parses
/// embedded syntax definitions, ~ms on first call).
fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Resolve a language name (e.g. "rust", or a file extension like "rs") to a
/// syntect syntax reference.
fn syntax_for_language(lang: &str) -> Option<&'static syntect::parsing::SyntaxReference> {
    let set = syntax_set();
    set.find_syntax_by_extension(lang)
        .or_else(|| set.find_syntax_by_token(lang))
        .or_else(|| {
            set.syntaxes()
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(lang))
        })
}

/// Build a `syntect::Theme` whose scope selectors map to the 9 syntax tokens
/// of the active evo theme, mirroring TS `buildCliHighlightTheme`. Each
/// selector's foreground is the resolved theme color for the corresponding
/// token; the `text`/default token is the code-block fallback color.
fn build_syntect_theme(theme: &CodingAgentThemeSnapshot) -> SyntectTheme {
    let mut syntect_theme = SyntectTheme::default();

    let push = |t: &mut SyntectTheme, selector: &str, color: CodingAgentResolvedColor| {
        if let Some(c) = syntect_color(color) {
            t.scopes.push(ThemeItem {
                scope: selector
                    .parse::<ScopeSelectors>()
                    .expect("valid scope selector"),
                style: StyleModifier {
                    foreground: Some(c),
                    background: None,
                    font_style: Default::default(),
                },
            });
        }
    };

    // (scope selector, token) pairs mirroring buildCliHighlightTheme.
    push(
        &mut syntect_theme,
        "comment, doctag",
        theme.foreground(CodingAgentThemeForeground::SyntaxComment),
    );
    push(
        &mut syntect_theme,
        "string, regexp",
        theme.foreground(CodingAgentThemeForeground::SyntaxString),
    );
    push(
        &mut syntect_theme,
        "constant.numeric, constant.language",
        theme.foreground(CodingAgentThemeForeground::SyntaxNumber),
    );
    // `keyword` and all `storage` (including storage.type.function like
    // Rust's `fn`) map to the keyword token, matching hljs classification.
    push(
        &mut syntect_theme,
        "keyword, storage",
        theme.foreground(CodingAgentThemeForeground::SyntaxKeyword),
    );
    push(
        &mut syntect_theme,
        "entity.name.function, support.function, variable.function",
        theme.foreground(CodingAgentThemeForeground::SyntaxFunction),
    );
    push(
        &mut syntect_theme,
        "entity.name.class, entity.name.type, support.type",
        theme.foreground(CodingAgentThemeForeground::SyntaxType),
    );
    push(
        &mut syntect_theme,
        "variable, entity.name.attribute, meta.parameter",
        theme.foreground(CodingAgentThemeForeground::SyntaxVariable),
    );
    push(
        &mut syntect_theme,
        "keyword.operator",
        theme.foreground(CodingAgentThemeForeground::SyntaxOperator),
    );
    push(
        &mut syntect_theme,
        "punctuation",
        theme.foreground(CodingAgentThemeForeground::SyntaxPunctuation),
    );

    syntect_theme
}

fn syntect_color(color: CodingAgentResolvedColor) -> Option<SyntectColor> {
    match color {
        CodingAgentResolvedColor::Default => None,
        CodingAgentResolvedColor::Rgb(r, g, b) => Some(SyntectColor { r, g, b, a: 0xff }),
        CodingAgentResolvedColor::Ansi256(_) => None, // syntect is RGB-only; 256 left default
    }
}

/// Highlight `code` for `lang`, returning one painted string per line using
/// the theme's syntax tokens. Mirrors `highlightCode`:
/// - unknown/empty language -> each line painted with `mdCodeBlock` (single color)
/// - parse error -> fall back to single-color lines
pub fn highlight_code(
    code: &str,
    lang: Option<&str>,
    theme: &CodingAgentThemeSnapshot,
) -> Vec<String> {
    let Some(lang) = lang.filter(|l| !l.is_empty()) else {
        return single_color_lines(code, theme);
    };
    let Some(syntax) = syntax_for_language(lang) else {
        return single_color_lines(code, theme);
    };

    let set = syntax_set();
    let syntect_theme = build_syntect_theme(theme);
    let mut highlighter = HighlightLines::new(syntax, &syntect_theme);
    let fallback = theme.foreground(CodingAgentThemeForeground::MdCodeBlock);

    let mut out = Vec::new();
    for line in code.trim_end_matches('\n').split('\n') {
        match highlighter.highlight_line(line, set) {
            Ok(ranges) => {
                let mut painted = String::new();
                for (style, text) in ranges {
                    let color = syntect_color_to_resolved(style.foreground, fallback);
                    painted.push_str(&paint(text, color));
                }
                out.push(format!("   {painted}"));
            }
            Err(_) => {
                out.push(format!("   {}", paint(line, fallback)));
            }
        }
    }
    out
}

/// Convert a syntect foreground color back to a resolved product color. syntect uses
/// `{r:0,g:0,b:0,a:0}` for "no color" (transparent/default), which we treat as
/// the code-block fallback.
fn syntect_color_to_resolved(
    color: SyntectColor,
    fallback: CodingAgentResolvedColor,
) -> CodingAgentResolvedColor {
    if color.a == 0 {
        return fallback;
    }
    CodingAgentResolvedColor::Rgb(color.r, color.g, color.b)
}

fn single_color_lines(code: &str, theme: &CodingAgentThemeSnapshot) -> Vec<String> {
    let color = theme.foreground(CodingAgentThemeForeground::MdCodeBlock);
    code.trim_end_matches('\n')
        .split('\n')
        .map(|line| format!("   {}", paint(line, color)))
        .collect()
}

/// Render `text` with the given resolved color as ANSI, using `tui`'s
/// paint layer. `Default` leaves text uncolored.
fn paint(text: &str, color: CodingAgentResolvedColor) -> String {
    use tui::api::render::{Color, ColorLevel, Style, paint_with_level};
    let style = Style::fg(match color {
        CodingAgentResolvedColor::Default => Color::Default,
        CodingAgentResolvedColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
        CodingAgentResolvedColor::Ansi256(n) => Color::Ansi256(n),
    });
    paint_with_level(text, &style, ColorLevel::TrueColor)
}
