//! Pure presentation state for the native desktop shell.
//!
//! GPUI rendering consumes these types, but geometry, focus, theme, and label
//! behavior stay deterministic and directly testable.

use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

pub const SESSION_PANEL_WIDTH: u32 = 240;
pub const CONTEXT_PANEL_WIDTH: u32 = 320;
pub const SESSION_PANEL_MIN_WIDTH: u32 = 200;
pub const SESSION_PANEL_MAX_WIDTH: u32 = 420;
pub const CONTEXT_PANEL_MIN_WIDTH: u32 = 280;
pub const CONTEXT_PANEL_MAX_WIDTH: u32 = 520;
pub const MIN_CONVERSATION_WIDTH: u32 = 520;
pub const COMPOSER_MIN_HEIGHT: u32 = 88;
pub const COMPOSER_MAX_HEIGHT: u32 = 236;
pub const STATUS_HEIGHT: u32 = 30;
pub const USER_MESSAGE_MAX_WIDTH: u32 = 920;
pub const ASSISTANT_MESSAGE_MAX_WIDTH: u32 = 960;
pub const USER_MESSAGE_WIDTH_PERCENT: u32 = 70;
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
    pub sessions: Option<Rect>,
    /// The complete center column. GPUI owns the dynamic transcript/composer
    /// split inside this rectangle because the composer auto-grows.
    pub workspace: Rect,
    pub context: Option<Rect>,
    pub status: Rect,
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
        let body_height = height.saturating_sub(STATUS_HEIGHT);

        let sessions_visible =
            requested.sessions && width >= sessions_width + MIN_CONVERSATION_WIDTH;
        let width_after_sessions =
            width.saturating_sub(if sessions_visible { sessions_width } else { 0 });
        let context_visible =
            requested.context && width_after_sessions >= context_width + MIN_CONVERSATION_WIDTH;

        let sessions_width = if sessions_visible { sessions_width } else { 0 };
        let context_width = if context_visible { context_width } else { 0 };
        let conversation_width = width
            .saturating_sub(sessions_width)
            .saturating_sub(context_width);

        Self {
            viewport: Rect::new(0, 0, width, height),
            sessions: sessions_visible.then(|| Rect::new(0, 0, sessions_width, body_height)),
            workspace: Rect::new(sessions_width, 0, conversation_width, body_height),
            context: context_visible.then(|| {
                Rect::new(
                    sessions_width + conversation_width,
                    0,
                    context_width,
                    body_height,
                )
            }),
            status: Rect::new(0, body_height, width, height.saturating_sub(body_height)),
        }
    }

    pub fn is_visible(self, target: FocusTarget) -> bool {
        match target {
            FocusTarget::Sessions => self.sessions.is_some(),
            FocusTarget::Context => self.context.is_some(),
            FocusTarget::Conversation | FocusTarget::Composer | FocusTarget::Status => true,
            FocusTarget::Overlay => false,
        }
    }

    pub fn focus_order(self) -> Vec<FocusTarget> {
        let mut order = Vec::with_capacity(5);
        if self.sessions.is_some() {
            order.push(FocusTarget::Sessions);
        }
        order.push(FocusTarget::Conversation);
        order.push(FocusTarget::Composer);
        if self.context.is_some() {
            order.push(FocusTarget::Context);
        }
        order.push(FocusTarget::Status);
        order
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    Sessions,
    Conversation,
    Composer,
    Context,
    Status,
    Overlay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusState {
    active: FocusTarget,
    restore_after_overlay: Option<FocusTarget>,
}

impl Default for FocusState {
    fn default() -> Self {
        Self {
            active: FocusTarget::Composer,
            restore_after_overlay: None,
        }
    }
}

impl FocusState {
    pub fn active(self) -> FocusTarget {
        self.active
    }

    pub fn request(&mut self, target: FocusTarget, layout: ShellLayout) -> bool {
        if self.restore_after_overlay.is_some() || !layout.is_visible(target) {
            return false;
        }
        self.active = target;
        true
    }

    pub fn cycle(&mut self, layout: ShellLayout, reverse: bool) {
        if self.restore_after_overlay.is_some() {
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

    pub fn open_overlay(&mut self) {
        if self.restore_after_overlay.is_none() {
            self.restore_after_overlay = Some(self.active);
            self.active = FocusTarget::Overlay;
        }
    }

    pub fn close_overlay(&mut self, layout: ShellLayout) {
        let Some(previous) = self.restore_after_overlay.take() else {
            return;
        };
        self.active = if layout.is_visible(previous) {
            previous
        } else {
            FocusTarget::Composer
        };
    }

    pub fn reconcile_layout(&mut self, layout: ShellLayout) {
        if self.restore_after_overlay.is_none() && !layout.is_visible(self.active) {
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

    #[test]
    fn responsive_layout_hides_context_before_sessions() {
        let wide = ShellLayout::resolve(1_300, 900, PanelVisibility::default());
        assert!(wide.sessions.is_some());
        assert!(wide.context.is_some());
        assert_eq!(wide.workspace.width, 740);

        let medium = ShellLayout::resolve(1_000, 900, PanelVisibility::default());
        assert!(medium.sessions.is_some());
        assert!(medium.context.is_none());
        assert_eq!(medium.workspace.width, 760);

        let narrow = ShellLayout::resolve(700, 900, PanelVisibility::default());
        assert!(narrow.sessions.is_none());
        assert!(narrow.context.is_none());
        assert_eq!(narrow.workspace.width, 700);
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
        assert_eq!(layout.sessions.unwrap().width, 300);
        assert_eq!(layout.context.unwrap().width, 380);
        assert_eq!(layout.workspace.width, 720);

        let clamped = ShellLayout::resolve_with_panel_widths(
            1_600,
            900,
            PanelVisibility::default(),
            1,
            u32::MAX,
        );
        assert_eq!(clamped.sessions.unwrap().width, SESSION_PANEL_MIN_WIDTH);
        assert_eq!(clamped.context.unwrap().width, CONTEXT_PANEL_MAX_WIDTH);
    }

    #[test]
    fn workspace_and_status_geometry_are_disjoint_for_tiny_windows() {
        let layout = ShellLayout::resolve(320, 100, PanelVisibility::default());
        assert_eq!(layout.workspace, Rect::new(0, 0, 320, 70));
        assert_eq!(layout.status.height, 30);
        assert_eq!(layout.status.y, 70);
    }

    #[test]
    fn responsive_breakpoints_change_once_at_the_exact_width() {
        let before_sessions = ShellLayout::resolve(759, 900, PanelVisibility::default());
        let at_sessions = ShellLayout::resolve(760, 900, PanelVisibility::default());
        assert!(before_sessions.sessions.is_none());
        assert_eq!(before_sessions.workspace.width, 759);
        assert_eq!(at_sessions.sessions.unwrap().width, SESSION_PANEL_WIDTH);
        assert_eq!(at_sessions.workspace.width, MIN_CONVERSATION_WIDTH);

        let before_context = ShellLayout::resolve(1_079, 900, PanelVisibility::default());
        let at_context = ShellLayout::resolve(1_080, 900, PanelVisibility::default());
        assert!(before_context.context.is_none());
        assert_eq!(before_context.workspace.width, 839);
        assert_eq!(at_context.context.unwrap().width, CONTEXT_PANEL_WIDTH);
        assert_eq!(at_context.workspace.width, MIN_CONVERSATION_WIDTH);
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
        assert!(hidden.sessions.is_none());
        assert!(hidden.context.is_some());

        let constrained = ShellLayout::resolve(800, 800, PanelVisibility::default());
        assert!(constrained.sessions.is_some());
        assert!(constrained.context.is_none());
        assert!(constrained.workspace.width >= MIN_CONVERSATION_WIDTH);
    }

    #[test]
    fn overlay_traps_focus_and_restores_visible_owner() {
        let wide = ShellLayout::resolve(1_300, 900, PanelVisibility::default());
        let narrow = ShellLayout::resolve(700, 900, PanelVisibility::default());
        let mut focus = FocusState::default();

        assert!(focus.request(FocusTarget::Context, wide));
        focus.open_overlay();
        assert_eq!(focus.active(), FocusTarget::Overlay);
        focus.cycle(wide, false);
        assert_eq!(focus.active(), FocusTarget::Overlay);

        focus.close_overlay(narrow);
        assert_eq!(focus.active(), FocusTarget::Composer);
    }

    #[test]
    fn resize_moves_focus_only_when_its_owner_disappears() {
        let wide = ShellLayout::resolve(1_300, 900, PanelVisibility::default());
        let medium = ShellLayout::resolve(1_000, 900, PanelVisibility::default());
        let mut focus = FocusState::default();

        assert!(focus.request(FocusTarget::Conversation, wide));
        focus.reconcile_layout(medium);
        assert_eq!(focus.active(), FocusTarget::Conversation);

        assert!(focus.request(FocusTarget::Sessions, medium));
        focus.reconcile_layout(ShellLayout::resolve(700, 900, PanelVisibility::default()));
        assert_eq!(focus.active(), FocusTarget::Composer);
    }

    #[test]
    fn focus_cycle_contains_only_visible_regions() {
        let layout = ShellLayout::resolve(700, 900, PanelVisibility::default());
        assert_eq!(
            layout.focus_order(),
            vec![
                FocusTarget::Conversation,
                FocusTarget::Composer,
                FocusTarget::Status
            ]
        );
    }

    #[test]
    fn semantic_theme_meets_text_contrast_floor() {
        assert!(SemanticTheme::GEEK_DARK.has_readable_contrast());
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
        assert_ne!(theme.subtle_text, theme.muted_text);
        assert_eq!(UI_FONT_FAMILY, ".SystemUIFont");
        assert_eq!(MONOSPACE_FONT_FAMILY, "monospace");
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
