//! Product theme behavior, mirroring TypeScript
//! `packages/coding-agent/test/test-theme-colors.ts` and `theme/theme.ts`.

use crate::internal_tests::product_fixture::theme::{
    ColorValue, ResolveError, ResolvedColor, builtin_dark, resolve,
};
use serde_json::json;
use std::collections::HashMap;

#[test]
fn parses_color_value_formats() {
    use ColorValue::*;
    assert_eq!(
        ColorValue::parse(&json!("#ff0000")),
        Some(Hex(0xff, 0x00, 0x00))
    );
    assert_eq!(
        ColorValue::parse(&json!("#FFAA00")),
        Some(Hex(0xff, 0xaa, 0x00))
    );
    assert_eq!(ColorValue::parse(&json!(0)), Some(Ansi256(0)));
    assert_eq!(ColorValue::parse(&json!(255)), Some(Ansi256(255)));
    assert_eq!(ColorValue::parse(&json!("")), Some(Default));
    assert_eq!(
        ColorValue::parse(&json!("primary")),
        Some(Var("primary".into()))
    );
    assert_eq!(ColorValue::parse(&json!(256)), None);
    assert_eq!(ColorValue::parse(&json!("#fff")), None);
    assert_eq!(ColorValue::parse(&json!(true)), None);
}

#[test]
fn resolves_variable_references_recursively() {
    let vars = HashMap::from([
        ("primary".to_string(), ColorValue::Hex(0x00, 0xaa, 0xff)),
        ("muted".to_string(), ColorValue::Var("primary".to_string())),
        ("gray".to_string(), ColorValue::Ansi256(242)),
    ]);

    assert_eq!(
        resolve(&ColorValue::Var("primary".into()), &vars),
        Ok(ResolvedColor::Hex(0x00, 0xaa, 0xff))
    );
    assert_eq!(
        resolve(&ColorValue::Var("muted".into()), &vars),
        Ok(ResolvedColor::Hex(0x00, 0xaa, 0xff))
    );
    assert_eq!(
        resolve(&ColorValue::Hex(1, 2, 3), &vars),
        Ok(ResolvedColor::Hex(1, 2, 3))
    );
    assert_eq!(
        resolve(&ColorValue::Ansi256(39), &vars),
        Ok(ResolvedColor::Ansi256(39))
    );
    assert_eq!(
        resolve(&ColorValue::Default, &vars),
        Ok(ResolvedColor::Default)
    );
    assert_eq!(
        resolve(&ColorValue::Var("missing".into()), &vars),
        Err(ResolveError::UnknownVar("missing".into()))
    );

    let circular = HashMap::from([
        ("a".to_string(), ColorValue::Var("b".to_string())),
        ("b".to_string(), ColorValue::Var("a".to_string())),
    ]);
    assert_eq!(
        resolve(&ColorValue::Var("a".into()), &circular),
        Err(ResolveError::Circular("a".into()))
    );
}

#[test]
fn parses_builtin_dark_theme_structure() {
    let theme = builtin_dark();
    assert_eq!(theme.name, "dark");
    assert!(theme.vars.contains_key("accent"));
    assert!(theme.vars.contains_key("cyan"));
    assert_eq!(theme.colors.len(), 51);
    assert!(theme.colors.contains_key("accent"));
    assert!(theme.colors.contains_key("bashMode"));
    assert!(theme.colors.contains_key("thinkingXhigh"));
    assert!(theme.colors.contains_key("userMessageBg"));
    let export = theme.export.expect("dark theme has export section");
    assert_eq!(export.page_bg, Some(ColorValue::Hex(0x18, 0x18, 0x1e)));
    assert_eq!(export.card_bg, Some(ColorValue::Hex(0x1e, 0x1e, 0x24)));
    assert_eq!(export.info_bg, Some(ColorValue::Hex(0x3c, 0x37, 0x28)));
}

#[test]
fn resolves_builtin_dark_colors_via_vars() {
    use crate::internal_tests::product_fixture::theme::{ThemeBg, ThemeColor};
    let theme = builtin_dark();
    let resolved = theme.resolve_colors().expect("dark theme resolves");

    // accent -> "accent" var -> "#8abeb7"
    assert_eq!(
        resolved.fg(ThemeColor::Accent),
        ResolvedColor::Hex(0x8a, 0xbe, 0xb7)
    );
    // border -> "blue" var -> "#5f87ff"
    assert_eq!(
        resolved.fg(ThemeColor::Border),
        ResolvedColor::Hex(0x5f, 0x87, 0xff)
    );
    // thinkingMedium is a literal hex (no var) -> "#81a2be"
    assert_eq!(
        resolved.fg(ThemeColor::ThinkingMedium),
        ResolvedColor::Hex(0x81, 0xa2, 0xbe)
    );
    // bashMode -> "green" var -> "#b5bd68"
    assert_eq!(
        resolved.fg(ThemeColor::BashMode),
        ResolvedColor::Hex(0xb5, 0xbd, 0x68)
    );
    // bg token: selectedBg -> "selectedBg" var -> "#3a3a4a"
    assert_eq!(
        resolved.bg(ThemeBg::SelectedBg),
        ResolvedColor::Hex(0x3a, 0x3a, 0x4a)
    );
    // userMessageText -> "text" var -> "#d4d4d4"
    assert_eq!(
        resolved.fg(ThemeColor::UserMessageText),
        ResolvedColor::Hex(0xd4, 0xd4, 0xd4)
    );
}

#[test]
fn reports_missing_required_tokens() {
    let json = r#"{ "name": "broken", "colors": { "accent": 39, "border": 0 } }"#;
    let theme: crate::internal_tests::product_fixture::theme::ThemeJson =
        serde_json::from_str(json).unwrap();
    let missing = theme.missing_tokens();
    // 51 required - 2 present = 49 missing
    assert_eq!(missing.len(), 49);
    assert!(missing.contains(&"borderAccent"));
    assert!(missing.contains(&"bashMode"));
    assert!(missing.contains(&"thinkingXhigh"));
    // a complete built-in theme has no missing tokens
    assert!(builtin_dark().missing_tokens().is_empty());
}

#[test]
fn resource_loader_parses_ts_theme_format_and_reports_missing_tokens() {
    use crate::internal_tests::product_fixture::resources::load_themes;
    use crate::internal_tests::product_fixture::theme::DARK_JSON;
    use std::fs;
    let temp = tempfile::tempdir().unwrap();
    let agent_dir = temp.path().join("agent");
    let themes = agent_dir.join("themes");
    fs::create_dir_all(&themes).unwrap();
    // complete theme (built-in dark, renamed) -> no missing-token diagnostic
    let full_json = DARK_JSON.replace(r#""name": "dark""#, r#""name": "full""#);
    fs::write(themes.join("full.json"), full_json).unwrap();
    // incomplete theme -> missing-token diagnostic
    fs::write(
        themes.join("partial.json"),
        r##"{"name":"partial","colors":{"accent":"#00aaff"}}"##,
    )
    .unwrap();

    let (loaded, diagnostics) = load_themes(std::slice::from_ref(&themes));

    let full = loaded
        .iter()
        .find(|t| t.name == "full")
        .expect("full theme loaded");
    assert_eq!(full.theme.colors.len(), 51);
    let partial = loaded
        .iter()
        .find(|t| t.name == "partial")
        .expect("partial theme loaded");
    assert_eq!(partial.theme.colors.len(), 1);
    assert_eq!(partial.theme.missing_tokens().len(), 50);

    // a diagnostic flags the missing tokens for the partial theme
    let diag = diagnostics
        .iter()
        .find(|d| d.path == partial.path)
        .expect("diagnostic for partial theme");
    assert!(
        diag.message.to_lowercase().contains("missing"),
        "diagnostic message was: {}",
        diag.message
    );
}

#[test]
fn parses_builtin_light_theme_and_exposes_schema() {
    let theme = crate::internal_tests::product_fixture::theme::builtin_light();
    assert_eq!(theme.name, "light");
    assert_eq!(theme.colors.len(), 51);
    // light resolves its vars too (accent -> "teal" var -> "#5a8080")
    let resolved = theme.resolve_colors().expect("light theme resolves");
    assert_eq!(
        resolved.fg(crate::internal_tests::product_fixture::theme::ThemeColor::Accent),
        ResolvedColor::Hex(0x5a, 0x80, 0x80)
    );
    // the schema is embedded and parses as valid JSON
    let schema: serde_json::Value =
        serde_json::from_str(crate::internal_tests::product_fixture::theme::SCHEMA_JSON).unwrap();
    assert_eq!(schema["title"], "Evo Coding Agent Theme");
}

// --- Terminal background detection (mirrors theme-detection.test.ts) ---

#[test]
fn detects_light_background_from_colorfgbg() {
    use crate::internal_tests::product_fixture::theme::{
        DetectionConfidence, DetectionSource, TerminalTheme, detect_terminal_background,
    };
    let env = vec![("COLORFGBG".to_string(), "0;15".to_string())];
    let detection = detect_terminal_background(env);
    assert_eq!(detection.theme, TerminalTheme::Light);
    assert_eq!(detection.source, DetectionSource::ColorFgbg);
    assert_eq!(detection.confidence, DetectionConfidence::High);
}

#[test]
fn detects_dark_background_from_colorfgbg() {
    use crate::internal_tests::product_fixture::theme::{
        DetectionConfidence, DetectionSource, TerminalTheme, detect_terminal_background,
    };
    let env = vec![("COLORFGBG".to_string(), "15;0".to_string())];
    let detection = detect_terminal_background(env);
    assert_eq!(detection.theme, TerminalTheme::Dark);
    assert_eq!(detection.source, DetectionSource::ColorFgbg);
    assert_eq!(detection.confidence, DetectionConfidence::High);
}

#[test]
fn uses_last_colorfgbg_field_as_background() {
    use crate::internal_tests::product_fixture::theme::{
        TerminalTheme, detect_terminal_background,
    };
    // "0;7;15" -> last field 15 (bright white) -> light
    let env = vec![("COLORFGBG".to_string(), "0;7;15".to_string())];
    let detection = detect_terminal_background(env);
    assert_eq!(detection.theme, TerminalTheme::Light);
}

#[test]
fn defaults_to_dark_without_background_hints() {
    use crate::internal_tests::product_fixture::theme::{
        DetectionConfidence, DetectionSource, TerminalTheme, detect_terminal_background,
    };
    let detection = detect_terminal_background(std::iter::empty::<(String, String)>());
    assert_eq!(detection.theme, TerminalTheme::Dark);
    assert_eq!(detection.source, DetectionSource::Fallback);
    assert_eq!(detection.confidence, DetectionConfidence::Low);
}

#[test]
fn parses_osc11_16bit_rgb_response() {
    use crate::internal_tests::product_fixture::theme::parse_osc11_background_color;
    // rgb:0000/8000/ffff -> (0, 128, 255)
    assert_eq!(
        parse_osc11_background_color("\x1b]11;rgb:0000/8000/ffff\x07"),
        Some((0, 128, 255))
    );
}

#[test]
fn parses_osc11_hex_responses() {
    use crate::internal_tests::product_fixture::theme::parse_osc11_background_color;
    assert_eq!(
        parse_osc11_background_color("\x1b]11;#ffffff\x1b\\"),
        Some((255, 255, 255))
    );
    assert_eq!(
        parse_osc11_background_color("\x1b]11;#000000\x07"),
        Some((0, 0, 0))
    );
}

#[test]
fn classifies_rgb_colors_by_luminance() {
    use crate::internal_tests::product_fixture::theme::{TerminalTheme, get_theme_for_rgb_color};
    assert_eq!(get_theme_for_rgb_color((8, 8, 8)), TerminalTheme::Dark);
    assert_eq!(
        get_theme_for_rgb_color((250, 250, 250)),
        TerminalTheme::Light
    );
}

// --- Theme hot reload (mirrors startThemeWatcher) ---

use crate::internal_tests::product_fixture::theme::ThemeWatcher;
use std::path::PathBuf;
use std::time::Duration;

const THEME_RELOAD_SIGNAL_TIMEOUT: Duration = Duration::from_secs(2);
const THEME_FILE_CHANGE_DEBOUNCE: Duration = Duration::from_millis(50);
const THEME_RAPID_EDIT_DEBOUNCE: Duration = Duration::from_millis(80);

async fn recv_theme_reload_signal<T>(
    signal: &mut tokio::sync::mpsc::Receiver<T>,
    context: &str,
) -> T {
    tokio::time::timeout(THEME_RELOAD_SIGNAL_TIMEOUT, signal.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {context}"))
        .unwrap_or_else(|| panic!("theme reload channel closed before {context}"))
}

#[test]
fn watcher_skips_builtin_themes() {
    use crate::internal_tests::product_fixture::theme::should_watch_target;

    // Built-in dark/light are never watched (TS startThemeWatcher returns early).
    assert!(should_watch_target("dark").is_none());
    assert!(should_watch_target("light").is_none());
    // A custom theme name resolves to a `<name>.json` watch target.
    assert_eq!(
        should_watch_target("ocean"),
        Some(PathBuf::from("ocean.json"))
    );
}

#[tokio::test]
async fn watcher_reloads_custom_theme_on_file_change() {
    let dir = tempfile::tempdir().unwrap();
    let themes_dir = dir.path().join("themes");
    std::fs::create_dir_all(&themes_dir).unwrap();
    let theme_file = themes_dir.join("hot.json");
    // Start with a valid complete theme (built-in dark, renamed).
    std::fs::write(
        &theme_file,
        crate::internal_tests::product_fixture::theme::DARK_JSON.replace("\"dark\"", "\"hot\""),
    )
    .unwrap();

    let (watcher, mut signal) = ThemeWatcher::start(
        themes_dir.clone(),
        "hot".to_string(),
        THEME_FILE_CHANGE_DEBOUNCE,
    )
    .expect("watcher starts");

    // Edit the theme file; expect a reload signal within a generous window.
    std::fs::write(
        &theme_file,
        crate::internal_tests::product_fixture::theme::LIGHT_JSON.replace("\"light\"", "\"hot\""),
    )
    .unwrap();

    let reloaded = recv_theme_reload_signal(&mut signal, "reload signal after theme edit").await;

    // The watcher returns the reparsed theme name + resolved tokens.
    assert_eq!(reloaded.name, "hot");
    // light theme accent -> "teal" var -> "#5a8080"
    let resolved = reloaded.theme.resolve_colors().unwrap();
    assert_eq!(
        resolved.fg(crate::internal_tests::product_fixture::theme::ThemeColor::Accent),
        crate::internal_tests::product_fixture::theme::ResolvedColor::Hex(0x5a, 0x80, 0x80)
    );

    drop(watcher);
}

#[tokio::test]
async fn watcher_reloads_after_rapid_edits() {
    let dir = tempfile::tempdir().unwrap();
    let themes_dir = dir.path().join("themes");
    std::fs::create_dir_all(&themes_dir).unwrap();
    let theme_file = themes_dir.join("debounce.json");
    std::fs::write(
        &theme_file,
        crate::internal_tests::product_fixture::theme::DARK_JSON
            .replace("\"dark\"", "\"debounce\""),
    )
    .unwrap();

    let (watcher, mut signal) = ThemeWatcher::start(
        themes_dir,
        "debounce".to_string(),
        THEME_RAPID_EDIT_DEBOUNCE,
    )
    .unwrap();

    for _ in 0..3 {
        std::fs::write(
            &theme_file,
            crate::internal_tests::product_fixture::theme::DARK_JSON
                .replace("\"dark\"", "\"debounce\""),
        )
        .unwrap();
    }

    let first = recv_theme_reload_signal(&mut signal, "first signal after rapid edits").await;
    assert_eq!(first.name, "debounce");

    drop(watcher);
}

// --- HTML export colors (mirrors getThemeExportColors / isLightTheme) ---

#[test]
fn is_light_theme_checks_name() {
    use crate::internal_tests::product_fixture::theme::is_light_theme;
    assert!(is_light_theme(Some("light")));
    assert!(!is_light_theme(Some("dark")));
    assert!(!is_light_theme(Some("custom")));
    assert!(!is_light_theme(None));
}

#[test]
fn export_colors_resolve_vars_and_convert_256_to_hex() {
    use crate::internal_tests::product_fixture::theme::{ThemeJson, get_theme_export_colors};
    // Build a theme with export vars (recursive + 256-color + hex).
    let json: ThemeJson = serde_json::from_str(
        r##"{
        "name": "export-test",
        "vars": {
            "pageBgVar": "#112233",
            "pageBgAlias": "pageBgVar",
            "infoBgVar": "#445566",
            "cardBgVar": "#223344",
            "gray256": 242
        },
        "colors": {
            "accent": "#000000", "border": "#000000", "borderAccent": "#000000",
            "borderMuted": "#000000", "success": "#000000", "error": "#000000",
            "warning": "#000000", "muted": "#000000", "dim": "#000000", "text": "#000000",
            "thinkingText": "#000000", "selectedBg": "#000000", "userMessageBg": "#000000",
            "userMessageText": "#000000", "customMessageBg": "#000000",
            "customMessageText": "#000000", "customMessageLabel": "#000000",
            "toolPendingBg": "#000000", "toolSuccessBg": "#000000", "toolErrorBg": "#000000",
            "toolTitle": "#000000", "toolOutput": "#000000", "mdHeading": "#000000",
            "mdLink": "#000000", "mdLinkUrl": "#000000", "mdCode": "#000000",
            "mdCodeBlock": "#000000", "mdCodeBlockBorder": "#000000", "mdQuote": "#000000",
            "mdQuoteBorder": "#000000", "mdHr": "#000000", "mdListBullet": "#000000",
            "toolDiffAdded": "#000000", "toolDiffRemoved": "#000000",
            "toolDiffContext": "#000000", "syntaxComment": "#000000",
            "syntaxKeyword": "#000000", "syntaxFunction": "#000000",
            "syntaxVariable": "#000000", "syntaxString": "#000000",
            "syntaxNumber": "#000000", "syntaxType": "#000000",
            "syntaxOperator": "#000000", "syntaxPunctuation": "#000000",
            "thinkingOff": "#000000", "thinkingMinimal": "#000000",
            "thinkingLow": "#000000", "thinkingMedium": "#000000",
            "thinkingHigh": "#000000", "thinkingXhigh": "#000000", "bashMode": "#000000"
        },
        "export": {
            "pageBg": "pageBgAlias",
            "cardBg": "cardBgVar",
            "infoBg": "infoBgVar"
        }
    }"##,
    )
    .unwrap();
    let export = get_theme_export_colors(&json);
    assert_eq!(export.page_bg.as_deref(), Some("#112233"));
    assert_eq!(export.card_bg.as_deref(), Some("#223344"));
    assert_eq!(export.info_bg.as_deref(), Some("#445566"));
}

#[test]
fn export_colors_omit_unset_and_convert_256() {
    use crate::internal_tests::product_fixture::theme::{ThemeJson, get_theme_export_colors};
    // No export section -> all None.
    let json: ThemeJson = serde_json::from_str(
        r##"{"name":"noexport","colors":{"accent":"#000000","border":"#000000","borderAccent":"#000000","borderMuted":"#000000","success":"#000000","error":"#000000","warning":"#000000","muted":"#000000","dim":"#000000","text":"#000000","thinkingText":"#000000","selectedBg":"#000000","userMessageBg":"#000000","userMessageText":"#000000","customMessageBg":"#000000","customMessageText":"#000000","customMessageLabel":"#000000","toolPendingBg":"#000000","toolSuccessBg":"#000000","toolErrorBg":"#000000","toolTitle":"#000000","toolOutput":"#000000","mdHeading":"#000000","mdLink":"#000000","mdLinkUrl":"#000000","mdCode":"#000000","mdCodeBlock":"#000000","mdCodeBlockBorder":"#000000","mdQuote":"#000000","mdQuoteBorder":"#000000","mdHr":"#000000","mdListBullet":"#000000","toolDiffAdded":"#000000","toolDiffRemoved":"#000000","toolDiffContext":"#000000","syntaxComment":"#000000","syntaxKeyword":"#000000","syntaxFunction":"#000000","syntaxVariable":"#000000","syntaxString":"#000000","syntaxNumber":"#000000","syntaxType":"#000000","syntaxOperator":"#000000","syntaxPunctuation":"#000000","thinkingOff":"#000000","thinkingMinimal":"#000000","thinkingLow":"#000000","thinkingMedium":"#000000","thinkingHigh":"#000000","thinkingXhigh":"#000000","bashMode":"#000000"}}"##,
    ).unwrap();
    let export = get_theme_export_colors(&json);
    assert_eq!(export.page_bg, None);
    assert_eq!(export.card_bg, None);
    assert_eq!(export.info_bg, None);

    // 256-color export value -> nearest hex (242 -> #6c6c6c).
    let with_256: ThemeJson = serde_json::from_str(
        r##"{"name":"export256","colors":{"accent":"#000000","border":"#000000","borderAccent":"#000000","borderMuted":"#000000","success":"#000000","error":"#000000","warning":"#000000","muted":"#000000","dim":"#000000","text":"#000000","thinkingText":"#000000","selectedBg":"#000000","userMessageBg":"#000000","userMessageText":"#000000","customMessageBg":"#000000","customMessageText":"#000000","customMessageLabel":"#000000","toolPendingBg":"#000000","toolSuccessBg":"#000000","toolErrorBg":"#000000","toolTitle":"#000000","toolOutput":"#000000","mdHeading":"#000000","mdLink":"#000000","mdLinkUrl":"#000000","mdCode":"#000000","mdCodeBlock":"#000000","mdCodeBlockBorder":"#000000","mdQuote":"#000000","mdQuoteBorder":"#000000","mdHr":"#000000","mdListBullet":"#000000","toolDiffAdded":"#000000","toolDiffRemoved":"#000000","toolDiffContext":"#000000","syntaxComment":"#000000","syntaxKeyword":"#000000","syntaxFunction":"#000000","syntaxVariable":"#000000","syntaxString":"#000000","syntaxNumber":"#000000","syntaxType":"#000000","syntaxOperator":"#000000","syntaxPunctuation":"#000000","thinkingOff":"#000000","thinkingMinimal":"#000000","thinkingLow":"#000000","thinkingMedium":"#000000","thinkingHigh":"#000000","thinkingXhigh":"#000000","bashMode":"#000000"},"export":{"infoBg":242}}"##,
    ).unwrap();
    let export = get_theme_export_colors(&with_256);
    assert_eq!(export.info_bg.as_deref(), Some("#6c6c6c"));
}
