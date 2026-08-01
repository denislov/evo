//! Pure presentation state for the native desktop shell.
//!
//! GPUI rendering consumes these types, but geometry, focus, theme, and label
//! behavior stay deterministic and directly testable.

use gpui::{App, Hsla};
use gpui_component::{Colorize as _, Theme};
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

pub const SESSION_PANEL_WIDTH: u32 = 240;
pub const CONTEXT_PANEL_WIDTH: u32 = 320;
pub const SESSION_PANEL_MIN_WIDTH: u32 = 200;
pub const SESSION_PANEL_MAX_WIDTH: u32 = 420;
pub const CONTEXT_PANEL_MIN_WIDTH: u32 = 280;
pub const CONTEXT_PANEL_MAX_WIDTH: u32 = 520;
pub const MIN_CONVERSATION_WIDTH: u32 = 520;
pub const CENTER_HEADER_HEIGHT: u32 = 48;
pub const COMPOSER_MIN_HEIGHT: u32 = 88;
pub const COMPOSER_MAX_HEIGHT: u32 = 236;
pub const USER_MESSAGE_MAX_WIDTH: u32 = 920;
pub const ASSISTANT_MESSAGE_MAX_WIDTH: u32 = 960;
/// Maximum centered transcript track, including one large spacing token on
/// either side of the widest message.
pub const CONVERSATION_CONTENT_MAX_WIDTH: u32 =
    ASSISTANT_MESSAGE_MAX_WIDTH + DESKTOP_DESIGN_TOKENS.spacing.lg * 2;
pub const UI_FONT_FAMILY: &str = ".SystemUIFont";
pub const MONOSPACE_FONT_FAMILY: &str = "monospace";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopSpacingScale {
    pub xs: u32,
    pub sm: u32,
    pub md: u32,
    pub lg: u32,
    pub xl: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopRadiusScale {
    pub sm: u32,
    pub md: u32,
    pub lg: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopTypographyScale {
    pub metadata_size: u32,
    pub body_size: u32,
    pub title_size: u32,
    pub metadata_line_height: u32,
    pub body_line_height: u32,
    pub title_line_height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopDesignTokens {
    pub spacing: DesktopSpacingScale,
    pub radius: DesktopRadiusScale,
    pub typography: DesktopTypographyScale,
}

pub const DESKTOP_DESIGN_TOKENS: DesktopDesignTokens = DesktopDesignTokens {
    spacing: DesktopSpacingScale {
        xs: 4,
        sm: 8,
        md: 12,
        lg: 16,
        xl: 24,
    },
    radius: DesktopRadiusScale {
        sm: 4,
        md: 6,
        lg: 8,
    },
    typography: DesktopTypographyScale {
        metadata_size: 12,
        body_size: 14,
        title_size: 16,
        metadata_line_height: 16,
        body_line_height: 21,
        title_line_height: 24,
    },
};

/// Vertical space owned by the virtual transcript row outside the measured card.
#[cfg(test)]
pub const CONVERSATION_ROW_VERTICAL_PADDING_PX: u32 = DESKTOP_DESIGN_TOKENS.spacing.xs * 2;
pub const DESKTOP_OVERLAY_SCRIM_RGBA: u32 = 0x0b0e_14dd;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelVisibility {
    pub sessions: bool,
    pub context: bool,
}

impl Default for PanelVisibility {
    fn default() -> Self {
        Self {
            sessions: true,
            context: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellLayout {
    pub viewport: Rect,
    pub sidebar: Option<Rect>,
    /// The complete center column, including its header and body.
    pub center: Rect,
    pub center_header: Rect,
    /// Everything below the center header. GPUI owns the dynamic
    /// Home/Conversation and composer split inside this rectangle.
    pub center_body: Rect,
    pub inspector: Option<Rect>,
}

impl ShellLayout {
    #[cfg(test)]
    pub fn resolve(width: u32, height: u32, requested: PanelVisibility) -> Self {
        Self::resolve_with_panel_widths(
            width,
            height,
            requested,
            SESSION_PANEL_WIDTH,
            CONTEXT_PANEL_WIDTH,
        )
    }

    pub fn resolve_with_panel_widths(
        width: u32,
        height: u32,
        requested: PanelVisibility,
        sessions_width: u32,
        context_width: u32,
    ) -> Self {
        let sessions_width = sessions_width.clamp(SESSION_PANEL_MIN_WIDTH, SESSION_PANEL_MAX_WIDTH);
        let context_width = context_width.clamp(CONTEXT_PANEL_MIN_WIDTH, CONTEXT_PANEL_MAX_WIDTH);
        let body_height = height;

        let sessions_visible =
            requested.sessions && width >= sessions_width + MIN_CONVERSATION_WIDTH;
        let width_after_sessions =
            width.saturating_sub(if sessions_visible { sessions_width } else { 0 });
        let context_visible =
            requested.context && width_after_sessions >= context_width + MIN_CONVERSATION_WIDTH;

        let sessions_width = if sessions_visible { sessions_width } else { 0 };
        let context_width = if context_visible { context_width } else { 0 };
        let center_width = width
            .saturating_sub(sessions_width)
            .saturating_sub(context_width);
        let center = Rect::new(sessions_width, 0, center_width, body_height);
        let center_header_height = body_height.min(CENTER_HEADER_HEIGHT);

        Self {
            viewport: Rect::new(0, 0, width, height),
            sidebar: sessions_visible.then(|| Rect::new(0, 0, sessions_width, body_height)),
            center,
            center_header: Rect::new(center.x, center.y, center.width, center_header_height),
            center_body: Rect::new(
                center.x,
                center.y.saturating_add(center_header_height),
                center.width,
                center.height.saturating_sub(center_header_height),
            ),
            inspector: context_visible
                .then(|| Rect::new(sessions_width + center_width, 0, context_width, body_height)),
        }
    }

    pub fn is_visible(self, target: FocusTarget) -> bool {
        match target {
            FocusTarget::CenterHeader | FocusTarget::CenterBody | FocusTarget::Composer => true,
            FocusTarget::Sidebar => self.sidebar.is_some(),
            FocusTarget::Inspector => self.inspector.is_some(),
            FocusTarget::Modal => false,
        }
    }

    pub fn focus_order(self) -> Vec<FocusTarget> {
        let mut order = Vec::with_capacity(5);
        order.push(FocusTarget::CenterHeader);
        if self.sidebar.is_some() {
            order.push(FocusTarget::Sidebar);
        }
        order.push(FocusTarget::CenterBody);
        order.push(FocusTarget::Composer);
        if self.inspector.is_some() {
            order.push(FocusTarget::Inspector);
        }
        order
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    CenterHeader,
    Sidebar,
    CenterBody,
    Composer,
    Inspector,
    Modal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusState {
    active: FocusTarget,
    restore_after_modal: Option<FocusTarget>,
}

impl Default for FocusState {
    fn default() -> Self {
        Self {
            active: FocusTarget::Composer,
            restore_after_modal: None,
        }
    }
}

impl FocusState {
    pub fn active(self) -> FocusTarget {
        self.active
    }

    pub fn request(&mut self, target: FocusTarget, layout: ShellLayout) -> bool {
        if self.restore_after_modal.is_some() || !layout.is_visible(target) {
            return false;
        }
        self.active = target;
        true
    }

    pub fn cycle(&mut self, layout: ShellLayout, reverse: bool) {
        if self.restore_after_modal.is_some() {
            return;
        }
        let order = layout.focus_order();
        let current = order
            .iter()
            .position(|target| *target == self.active)
            .unwrap_or(0);
        let next = if reverse {
            current.checked_sub(1).unwrap_or(order.len() - 1)
        } else {
            (current + 1) % order.len()
        };
        self.active = order[next];
    }

    pub fn open_modal(&mut self) {
        if self.restore_after_modal.is_none() {
            self.restore_after_modal = Some(self.active);
            self.active = FocusTarget::Modal;
        }
    }

    pub fn close_modal(&mut self, layout: ShellLayout) {
        let Some(previous) = self.restore_after_modal.take() else {
            return;
        };
        self.active = if layout.is_visible(previous) {
            previous
        } else {
            FocusTarget::Composer
        };
    }

    pub fn reconcile_layout(&mut self, layout: ShellLayout) {
        if self.restore_after_modal.is_none() && !layout.is_visible(self.active) {
            self.active = FocusTarget::Composer;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticColor(u32);

impl SemanticColor {
    pub const fn rgb(value: u32) -> Self {
        Self(value & 0x00ff_ffff)
    }

    pub const fn value(self) -> u32 {
        self.0
    }

    /// Converts an HSLA color from the gpui-component palette to the shell's
    /// opaque RGB representation, dropping alpha the same way `rgb()` does.
    fn from_hsla(hsla: Hsla) -> Self {
        let rgba = hsla.to_rgb();
        let channel = |component: f32| (component.clamp(0.0, 1.0) * 255.0).round() as u32;
        Self::rgb((channel(rgba.r) << 16) | (channel(rgba.g) << 8) | channel(rgba.b))
    }

    #[cfg(test)]
    fn channel_luminance(channel: u32) -> f64 {
        let normalized = f64::from(channel) / 255.0;
        if normalized <= 0.04045 {
            normalized / 12.92
        } else {
            ((normalized + 0.055) / 1.055).powf(2.4)
        }
    }

    #[cfg(test)]
    fn luminance(self) -> f64 {
        let red = (self.0 >> 16) & 0xff;
        let green = (self.0 >> 8) & 0xff;
        let blue = self.0 & 0xff;
        0.2126 * Self::channel_luminance(red)
            + 0.7152 * Self::channel_luminance(green)
            + 0.0722 * Self::channel_luminance(blue)
    }

    #[cfg(test)]
    pub fn contrast_ratio(self, other: Self) -> f64 {
        let left = self.luminance();
        let right = other.luminance();
        (left.max(right) + 0.05) / (left.min(right) + 0.05)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticTheme {
    pub canvas: SemanticColor,
    pub surface: SemanticColor,
    pub elevated: SemanticColor,
    pub hover: SemanticColor,
    pub selection: SemanticColor,
    pub user_surface: SemanticColor,
    pub assistant_surface: SemanticColor,
    pub thinking_surface: SemanticColor,
    pub tool_surface: SemanticColor,
    pub diagnostic_surface: SemanticColor,
    pub summary_surface: SemanticColor,
    pub border: SemanticColor,
    pub divider: SemanticColor,
    pub text: SemanticColor,
    pub muted_text: SemanticColor,
    pub subtle_text: SemanticColor,
    pub accent: SemanticColor,
    pub success: SemanticColor,
    pub warning: SemanticColor,
    pub danger: SemanticColor,
    pub focus_ring: SemanticColor,
    pub reasoning: SemanticColor,
}

impl SemanticTheme {
    pub const GEEK_DARK: Self = Self {
        canvas: SemanticColor::rgb(0x0b0e14),
        surface: SemanticColor::rgb(0x11161f),
        elevated: SemanticColor::rgb(0x18202c),
        hover: SemanticColor::rgb(0x151c26),
        selection: SemanticColor::rgb(0x19324d),
        user_surface: SemanticColor::rgb(0x10243a),
        assistant_surface: SemanticColor::rgb(0x121923),
        thinking_surface: SemanticColor::rgb(0x1d1930),
        tool_surface: SemanticColor::rgb(0x151a22),
        diagnostic_surface: SemanticColor::rgb(0x26151a),
        summary_surface: SemanticColor::rgb(0x171c26),
        border: SemanticColor::rgb(0x2a3442),
        divider: SemanticColor::rgb(0x202a36),
        text: SemanticColor::rgb(0xe8edf4),
        muted_text: SemanticColor::rgb(0x9aa8ba),
        subtle_text: SemanticColor::rgb(0x8795a8),
        accent: SemanticColor::rgb(0x60a5fa),
        success: SemanticColor::rgb(0x56d364),
        warning: SemanticColor::rgb(0xe3b341),
        danger: SemanticColor::rgb(0xff7b72),
        focus_ring: SemanticColor::rgb(0x60a5fa),
        reasoning: SemanticColor::rgb(0xb49aef),
    };

    /// Derives the semantic palette from the gpui-component theme, the single
    /// source of truth for all shell colors. Neutral message surfaces share
    /// the theme's `tiles` fill; the tinted surfaces (user, thinking,
    /// diagnostic) derive from the closest base color so the shell's
    /// surface and status distinctions stay intact.
    pub fn from_theme(theme: &Theme) -> Self {
        let c = &theme.colors;
        Self {
            canvas: SemanticColor::from_hsla(c.background),
            surface: SemanticColor::from_hsla(c.tiles),
            elevated: SemanticColor::from_hsla(c.secondary),
            hover: SemanticColor::from_hsla(c.secondary_hover),
            selection: SemanticColor::from_hsla(c.selection),
            user_surface: SemanticColor::from_hsla(c.blue.darken(0.78)),
            assistant_surface: SemanticColor::from_hsla(c.tiles),
            thinking_surface: SemanticColor::from_hsla(c.magenta.darken(0.78)),
            tool_surface: SemanticColor::from_hsla(c.tiles),
            diagnostic_surface: SemanticColor::from_hsla(c.danger.darken(0.80)),
            summary_surface: SemanticColor::from_hsla(c.tiles),
            border: SemanticColor::from_hsla(c.border),
            divider: SemanticColor::from_hsla(c.border.darken(0.1)),
            text: SemanticColor::from_hsla(c.foreground),
            muted_text: SemanticColor::from_hsla(c.muted_foreground),
            subtle_text: SemanticColor::from_hsla(c.muted_foreground.darken(0.25)),
            accent: SemanticColor::from_hsla(c.blue),
            success: SemanticColor::from_hsla(c.success),
            warning: SemanticColor::from_hsla(c.warning),
            danger: SemanticColor::from_hsla(c.danger),
            focus_ring: SemanticColor::from_hsla(c.ring),
            reasoning: SemanticColor::from_hsla(c.magenta),
        }
    }

    /// The theme for the current window: derived from the global
    /// gpui-component theme when it is dark, otherwise the baked-in
    /// `GEEK_DARK` baseline. Rendering never hardcodes a palette; it reads
    /// this single access point. A future light palette only needs to extend
    /// the dark guard here (and adjust the contrast floor) to light up
    /// everywhere at once.
    pub fn current(cx: &App) -> Self {
        match cx.try_global::<Theme>() {
            Some(theme) if theme.is_dark() => Self::from_theme(theme),
            _ => Self::GEEK_DARK,
        }
    }

    #[cfg(test)]
    pub fn has_readable_contrast(self) -> bool {
        [
            self.text,
            self.muted_text,
            self.subtle_text,
            self.accent,
            self.success,
            self.warning,
            self.danger,
            self.focus_ring,
            self.reasoning,
        ]
        .into_iter()
        .all(|color| color.contrast_ratio(self.canvas) >= 4.5)
            && self.text.contrast_ratio(self.surface) >= 4.5
            && self.text.contrast_ratio(self.elevated) >= 4.5
            && [
                self.hover,
                self.selection,
                self.user_surface,
                self.assistant_surface,
                self.thinking_surface,
                self.tool_surface,
                self.diagnostic_surface,
                self.summary_surface,
            ]
            .into_iter()
            .all(|surface| self.text.contrast_ratio(surface) >= 4.5)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticStatus {
    Idle,
    Running,
    Warning,
    Error,
    Authorization,
}

impl SemanticStatus {
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Idle => "○",
            Self::Running => "◌",
            Self::Warning => "!",
            Self::Error => "×",
            Self::Authorization => "?",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Running => "Running",
            Self::Warning => "Warning",
            Self::Error => "Error",
            Self::Authorization => "Authorization required",
        }
    }
}

/// Truncate a label by terminal display columns without splitting a scalar.
///
/// The UI still measures glyphs through GPUI; this helper provides a stable
/// bound for lists, status text, and tests involving wide Unicode characters.
pub fn truncate_label(label: &str, max_columns: usize) -> String {
    if label.width() <= max_columns {
        return label.to_owned();
    }
    if max_columns == 0 {
        return String::new();
    }

    let ellipsis = '…';
    let content_limit = max_columns.saturating_sub(ellipsis.width().unwrap_or(1));
    let mut output = String::new();
    let mut width = 0;
    for character in label.chars() {
        let character_width = character.width().unwrap_or(0);
        if width + character_width > content_limit {
            break;
        }
        output.push(character);
        width += character_width;
    }
    output.push(ellipsis);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::ThemeMode;

    #[test]
    fn responsive_layout_hides_context_before_sessions() {
        let wide = ShellLayout::resolve(1_300, 900, PanelVisibility::default());
        assert!(wide.sidebar.is_some());
        assert!(wide.inspector.is_some());
        assert_eq!(wide.center.width, 740);

        let medium = ShellLayout::resolve(1_000, 900, PanelVisibility::default());
        assert!(medium.sidebar.is_some());
        assert!(medium.inspector.is_none());
        assert_eq!(medium.center.width, 760);

        let narrow = ShellLayout::resolve(700, 900, PanelVisibility::default());
        assert!(narrow.sidebar.is_none());
        assert!(narrow.inspector.is_none());
        assert_eq!(narrow.center.width, 700);
    }

    #[test]
    fn persisted_panel_widths_drive_layout_without_consuming_handle_space() {
        let layout = ShellLayout::resolve_with_panel_widths(
            1_400,
            900,
            PanelVisibility::default(),
            300,
            380,
        );
        assert_eq!(layout.sidebar.unwrap().width, 300);
        assert_eq!(layout.inspector.unwrap().width, 380);
        assert_eq!(layout.center.width, 720);

        let clamped = ShellLayout::resolve_with_panel_widths(
            1_600,
            900,
            PanelVisibility::default(),
            1,
            u32::MAX,
        );
        assert_eq!(clamped.sidebar.unwrap().width, SESSION_PANEL_MIN_WIDTH);
        assert_eq!(clamped.inspector.unwrap().width, CONTEXT_PANEL_MAX_WIDTH);
    }

    #[test]
    fn center_header_and_body_partition_the_full_center_column() {
        let layout = ShellLayout::resolve(320, 100, PanelVisibility::default());
        assert_eq!(layout.center, Rect::new(0, 0, 320, 100));
        assert_eq!(layout.center_header, Rect::new(0, 0, 320, 48));
        assert_eq!(layout.center_body, Rect::new(0, 48, 320, 52));

        let short = ShellLayout::resolve(320, 32, PanelVisibility::default());
        assert_eq!(short.center_header, Rect::new(0, 0, 320, 32));
        assert_eq!(short.center_body, Rect::new(0, 32, 320, 0));
    }

    #[test]
    fn responsive_breakpoints_change_once_at_the_exact_width() {
        let before_sessions = ShellLayout::resolve(759, 900, PanelVisibility::default());
        let at_sessions = ShellLayout::resolve(760, 900, PanelVisibility::default());
        assert!(before_sessions.sidebar.is_none());
        assert_eq!(before_sessions.center.width, 759);
        assert_eq!(at_sessions.sidebar.unwrap().width, SESSION_PANEL_WIDTH);
        assert_eq!(at_sessions.center.width, MIN_CONVERSATION_WIDTH);

        let before_context = ShellLayout::resolve(1_079, 900, PanelVisibility::default());
        let at_context = ShellLayout::resolve(1_080, 900, PanelVisibility::default());
        assert!(before_context.inspector.is_none());
        assert_eq!(before_context.center.width, 839);
        assert_eq!(at_context.inspector.unwrap().width, CONTEXT_PANEL_WIDTH);
        assert_eq!(at_context.center.width, MIN_CONVERSATION_WIDTH);
    }

    #[test]
    fn requested_panel_toggles_never_violate_conversation_minimum() {
        let hidden = ShellLayout::resolve(
            1_400,
            800,
            PanelVisibility {
                sessions: false,
                context: true,
            },
        );
        assert!(hidden.sidebar.is_none());
        assert!(hidden.inspector.is_some());

        let constrained = ShellLayout::resolve(800, 800, PanelVisibility::default());
        assert!(constrained.sidebar.is_some());
        assert!(constrained.inspector.is_none());
        assert!(constrained.center.width >= MIN_CONVERSATION_WIDTH);
    }

    #[test]
    fn modal_traps_focus_and_restores_visible_owner() {
        let wide = ShellLayout::resolve(1_300, 900, PanelVisibility::default());
        let narrow = ShellLayout::resolve(700, 900, PanelVisibility::default());
        let mut focus = FocusState::default();

        assert!(focus.request(FocusTarget::Inspector, wide));
        focus.open_modal();
        assert_eq!(focus.active(), FocusTarget::Modal);
        focus.cycle(wide, false);
        assert_eq!(focus.active(), FocusTarget::Modal);

        focus.close_modal(narrow);
        assert_eq!(focus.active(), FocusTarget::Composer);
    }

    #[test]
    fn resize_moves_focus_only_when_its_owner_disappears() {
        let wide = ShellLayout::resolve(1_300, 900, PanelVisibility::default());
        let medium = ShellLayout::resolve(1_000, 900, PanelVisibility::default());
        let mut focus = FocusState::default();

        assert!(focus.request(FocusTarget::CenterBody, wide));
        focus.reconcile_layout(medium);
        assert_eq!(focus.active(), FocusTarget::CenterBody);

        assert!(focus.request(FocusTarget::Sidebar, medium));
        focus.reconcile_layout(ShellLayout::resolve(700, 900, PanelVisibility::default()));
        assert_eq!(focus.active(), FocusTarget::Composer);
    }

    #[test]
    fn focus_cycle_contains_only_visible_regions() {
        let layout = ShellLayout::resolve(700, 900, PanelVisibility::default());
        assert_eq!(
            layout.focus_order(),
            vec![
                FocusTarget::CenterHeader,
                FocusTarget::CenterBody,
                FocusTarget::Composer
            ]
        );
    }

    #[test]
    fn home_uses_the_same_columns_with_sidebar_open_and_inspector_closed() {
        let home_visibility = PanelVisibility {
            sessions: true,
            context: false,
        };
        let wide = ShellLayout::resolve(1_300, 900, home_visibility);
        assert!(wide.sidebar.is_some());
        assert!(wide.inspector.is_none());
        assert_eq!(wide.center, Rect::new(240, 0, 1_060, 900));

        let medium = ShellLayout::resolve(900, 800, home_visibility);
        assert!(medium.sidebar.is_some());
        assert!(medium.inspector.is_none());
        assert_eq!(medium.center, Rect::new(240, 0, 660, 800));

        let narrow = ShellLayout::resolve(700, 800, home_visibility);
        assert!(narrow.sidebar.is_none());
        assert!(narrow.inspector.is_none());
        assert_eq!(narrow.center, Rect::new(0, 0, 700, 800));
    }

    #[test]
    fn focus_cycle_has_a_stable_wide_region_order() {
        let layout = ShellLayout::resolve(1_300, 900, PanelVisibility::default());
        assert_eq!(
            layout.focus_order(),
            vec![
                FocusTarget::CenterHeader,
                FocusTarget::Sidebar,
                FocusTarget::CenterBody,
                FocusTarget::Composer,
                FocusTarget::Inspector,
            ]
        );
    }

    #[test]
    fn semantic_theme_meets_text_contrast_floor() {
        assert!(SemanticTheme::GEEK_DARK.has_readable_contrast());
    }

    /// Production rendering derives its palette from the gpui-component dark
    /// theme, so the same contrast floor must hold for the derived palette.
    #[gpui::test]
    fn derived_theme_meets_text_contrast_floor(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
        });
        let theme = cx.update(|cx| SemanticTheme::current(cx));
        assert!(theme.has_readable_contrast());
    }

    /// Derived values must come from the component palette, not from a copy of
    /// the GEEK_DARK constants.
    #[gpui::test]
    fn derived_theme_reads_the_component_palette(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
        });
        let theme = cx.update(|cx| SemanticTheme::current(cx));
        let colors = cx.update(|cx| Theme::global(cx).colors);
        assert_eq!(theme.canvas, SemanticColor::from_hsla(colors.background));
        assert_eq!(theme.text, SemanticColor::from_hsla(colors.foreground));
        assert_eq!(theme.accent, SemanticColor::from_hsla(colors.blue));
        assert_eq!(theme.border, SemanticColor::from_hsla(colors.border));
    }

    /// With no global theme installed, `current` falls back to the baseline
    /// palette instead of panicking on the missing global.
    #[gpui::test]
    fn current_falls_back_to_geek_dark_without_a_global_theme(cx: &mut gpui::TestAppContext) {
        let theme = cx.update(|cx| SemanticTheme::current(cx));
        assert_eq!(theme, SemanticTheme::GEEK_DARK);
    }

    /// A light global theme is not derived (the palette is dark-only for now);
    /// rendering stays on the baseline until a light theme is designed.
    #[gpui::test]
    fn current_falls_back_when_the_global_theme_is_light(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let theme = cx.update(|cx| SemanticTheme::current(cx));
        assert_eq!(theme, SemanticTheme::GEEK_DARK);
    }

    #[test]
    fn semantic_theme_keeps_focus_reasoning_warning_and_failure_distinct() {
        let theme = SemanticTheme::GEEK_DARK;
        assert_eq!(theme.focus_ring, theme.accent);
        assert_ne!(theme.reasoning, theme.focus_ring);
        assert_ne!(theme.reasoning, theme.warning);
        assert_ne!(theme.reasoning, theme.danger);
        assert_ne!(theme.warning, theme.danger);
        assert_ne!(theme.canvas, theme.surface);
        assert_ne!(theme.surface, theme.elevated);
        assert_ne!(theme.hover, theme.selection);
        assert_ne!(theme.divider, theme.focus_ring);
        assert_ne!(theme.divider, theme.border);
        assert_ne!(theme.subtle_text, theme.muted_text);
        assert_eq!(UI_FONT_FAMILY, ".SystemUIFont");
        assert_eq!(MONOSPACE_FONT_FAMILY, "monospace");
    }

    /// The derived theme separates the focus ring from the brand accent (the
    /// component palette's `ring` is neutral, not blue), so focus and
    /// selection stay distinguishable from emphasis.
    #[gpui::test]
    fn derived_theme_keeps_focus_reasoning_warning_and_failure_distinct(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
        });
        let theme = cx.update(|cx| SemanticTheme::current(cx));
        assert_ne!(theme.focus_ring, theme.accent);
        assert_ne!(theme.reasoning, theme.focus_ring);
        assert_ne!(theme.reasoning, theme.warning);
        assert_ne!(theme.reasoning, theme.danger);
        assert_ne!(theme.warning, theme.danger);
        assert_ne!(theme.canvas, theme.surface);
        assert_ne!(theme.surface, theme.elevated);
        assert_ne!(theme.hover, theme.selection);
        assert_ne!(theme.divider, theme.focus_ring);
        assert_ne!(theme.divider, theme.border);
        assert_ne!(theme.subtle_text, theme.muted_text);
    }

    #[test]
    fn desktop_design_tokens_use_the_documented_spacing_radius_and_type_scales() {
        let tokens = DESKTOP_DESIGN_TOKENS;
        assert_eq!(
            [
                tokens.spacing.xs,
                tokens.spacing.sm,
                tokens.spacing.md,
                tokens.spacing.lg,
                tokens.spacing.xl,
            ],
            [4, 8, 12, 16, 24]
        );
        assert_eq!(
            [tokens.radius.sm, tokens.radius.md, tokens.radius.lg],
            [4, 6, 8]
        );
        assert!(tokens.radius.lg <= 8);
        assert_eq!(
            [
                tokens.typography.metadata_size,
                tokens.typography.body_size,
                tokens.typography.title_size,
            ],
            [12, 14, 16]
        );
        assert!(tokens.typography.metadata_line_height > tokens.typography.metadata_size);
        assert!(tokens.typography.body_line_height > tokens.typography.body_size);
        assert!(tokens.typography.title_line_height > tokens.typography.title_size);
        assert_eq!(CONVERSATION_ROW_VERTICAL_PADDING_PX, 8);
        assert_eq!(DESKTOP_OVERLAY_SCRIM_RGBA, 0x0b0e_14dd);
    }

    #[test]
    fn statuses_are_not_color_only() {
        let statuses = [
            SemanticStatus::Idle,
            SemanticStatus::Running,
            SemanticStatus::Warning,
            SemanticStatus::Error,
            SemanticStatus::Authorization,
        ];
        for status in statuses {
            assert!(!status.glyph().is_empty());
            assert!(!status.label().is_empty());
        }
    }

    #[test]
    fn labels_truncate_by_unicode_display_width() {
        assert_eq!(truncate_label("abcdef", 5), "abcd…");
        assert_eq!(truncate_label("项目会话alpha", 8), "项目会…");
        assert_eq!(truncate_label("anything", 0), "");
        assert_eq!(truncate_label("项目", 4), "项目");
    }
}
