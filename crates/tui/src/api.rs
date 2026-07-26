//! Stable generic terminal UI facade.
//!
//! Product-specific coding-agent actions, sessions, model state, tree state,
//! tools, and plugin dispatch belong in `coding-agent` adapters.

/// Terminal lifecycle, capability negotiation, colors, and image protocol
/// support. Product interaction policy does not belong here.
pub mod terminal {
    pub use crate::terminal::{
        calculate_image_cell_size, delete_all_kitty_images, delete_kitty_image,
        detect_terminal_capabilities_from_env, encode_iterm2, encode_kitty, hyperlink,
        image_dimensions_from_base64, image_dimensions_from_bytes, is_apple_terminal_session,
        is_color_scheme_report, is_image_line, is_osc11_background_color_response,
        normalize_apple_terminal_input, parse_color_scheme_report, parse_osc11_background_color,
        query_background_color, query_color_scheme, render_image, set_color_scheme_notifications,
        CellDimensions, ImageCellSize, ImageDimensions, ImageProtocol, ImageRenderOptions,
        NegotiationResult, ProcessTerminal, RenderedImage, RgbColor, Terminal,
        TerminalCapabilities, TerminalColorScheme, TerminalMode, TerminalSize,
    };
}

/// Normalized input, keybindings, completion, editing history, and word
/// navigation primitives.
pub mod input {
    pub use crate::component::{
        AutocompleteItem, AutocompleteOptions, AutocompleteProvider, AutocompleteSuggestions,
        CombinedAutocompleteProvider, CompletionEdit, SlashCommand,
    };
    pub use crate::editing::{find_word_backward, find_word_forward, KillRing, UndoStack};
    pub use crate::fuzzy::{fuzzy_filter_indices, fuzzy_match, FuzzyMatch};
    pub use crate::input::{
        is_key_release, is_kitty_protocol_active, matches_key, parse_key, parse_sgr_mouse,
        set_kitty_protocol_active, InputEvent, Key, KeyEvent, KeyEventKind, KeyModifiers,
        KeybindingConflict, KeybindingDefinition, KeybindingsConfig, KeybindingsManager,
        MouseButton, MouseEvent, MouseEventKind, MouseModifiers, StdinBuffer,
        GENERIC_TUI_KEYBINDINGS, TUI_KEYBINDINGS,
    };
}

/// Generic components, cursor projection, and overlay composition.
pub mod component {
    pub use crate::component::{
        BackgroundFn, Box, CancellableLoader, Component, ComponentId, Container, DefaultTextStyle,
        Editor, Image, Input, Loader, LoaderIndicatorOptions, Markdown, SelectItem, SelectList,
        SelectorDialog, SelectorDialogOptions, SettingItem, SettingsList, SettingsListOptions,
        SettingsSubmenuDone, Spacer, Text, TruncatedText,
    };
    pub use crate::editing::{extract_cursor_marker, CursorPosition, CURSOR_MARKER};
    pub use crate::render::{
        OverlayAnchor, OverlayHandle, OverlayMargin, OverlayOptions, OverlayVisibleFn, SizeValue,
    };
}

/// Render scheduling, surfaces, styles, painting, and display-width helpers.
pub mod render {
    pub use crate::render::{
        color_enabled, color_level, detect_color_level_from_env, paint, paint_with,
        paint_with_level, truncate_to_width, truncate_to_width_with_ellipsis, visible_width,
        wrap_text_with_ansi, Axis, Color, ColorLevel, Constraint, FocusRing, Frame, HitMap,
        HitRegion, InputListenerResult, Layout, Point, Rect, RenderOutcome, RenderScheduler,
        RenderStrategy, Style, Tui, TuiError, ERROR, PATH, STATUS_IDLE, STATUS_RUNNING, SYSTEM,
        TOOL_ERROR, TOOL_NAME, USER,
    };
}

/// Generic widget themes and palettes.
pub mod theme {
    pub use crate::theme::{
        dark_theme, light_theme, EditorTheme, ImageTheme, MarkdownTheme, SelectListTheme,
        SettingsListTheme, ThemeMode, ThemePalette, TuiTheme,
    };
}

/// Deterministic terminal inspection support. Production code must not import
/// this category.
#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    pub use crate::testing::{TerminalOp, VirtualTerminal};
}
