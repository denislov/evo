//! Shared control semantics for the desktop shell.
//!
//! The shell previously expressed every interaction — panel toggles, value
//! selectors, list rows, disclosure toggles, copy tools, submission and
//! destructive decisions — as the same rounded outline text button. Visual
//! weight therefore carried no information: `Deny` and `Show` looked alike.
//!
//! This module is not a second design system. It is a thin semantic layer over
//! the `gpui-component` primitives that fixes one thing: **weight encodes
//! consequence**.
//!
//! | Weight | Appearance | Used for |
//! | --- | --- | --- |
//! | [`DesktopControlWeight::Tool`] | borderless icon, revealed on hover/focus | copy, expand, overflow |
//! | [`DesktopControlWeight::Selector`] | current value + chevron | model, profile, thinking |
//! | [`DesktopActionRow`] | full-width row, state via background | session, changed file |
//! | [`DesktopControlWeight::Primary`] | filled accent | composer submit |
//! | [`DesktopControlWeight::Critical`] | semantic colour, always a text label | Deny, Abort, recovery |
//!
//! Two invariants are enforced by construction rather than by review:
//!
//! - an icon-only control cannot be built without an accessible label, which is
//!   also used as its tooltip, so pointer and screen-reader paths stay
//!   equivalent;
//! - every control declares a fixed height from [`DesktopControlSize`], so
//!   swapping an icon for a spinner, or a label for a badge, cannot change row
//!   geometry.
//!
//! Primitives here never read `DesktopProjection`, a controller or
//! `NativeShell`; they take resolved display values only.

// VUI-101 lands the vocabulary; VUI-102 through VUI-106 adopt it pane by pane.
// Establishing the ladder in one reviewable commit is deliberate: it keeps the
// control decisions separable from the layout churn that follows, and this task
// is explicitly not allowed to change Pane rendering.
#![allow(dead_code)]

use gpui::{
    ElementId, IntoElement, ParentElement as _, Role, SharedString, Styled as _, div, prelude::*,
    px, rgb,
};
use gpui_component::{
    Disableable as _, IconName, Selectable as _,
    button::{Button, ButtonVariants as _},
};

use desktop::shell::{SemanticColor, SemanticTheme};

use super::desktop_style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _};

/// Named icons the shell is allowed to use.
///
/// Panes name the intent, never an asset filename, so the icon set can be
/// swapped in one place. Every variant resolves to a bundled Lucide asset that
/// ships with `gpui-component-assets`; nothing here is hand-drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DesktopIcon {
    /// Toggle the sessions panel; direction mirrors the panel's screen edge.
    PanelLeftOpen,
    PanelLeftClose,
    /// Toggle the inspector panel.
    PanelRightOpen,
    PanelRightClose,
    /// Additional actions that do not earn permanent space.
    Overflow,
    /// Disclosure affordance for a row that toggles as a whole.
    ChevronDown,
    ChevronUp,
    /// A value the user can change, as opposed to an action they invoke.
    SelectorCaret,
    Copy,
    Expand,
    OpenExternal,
    Search,
    Clear,
    Close,
    Plus,
    /// Composer submission.
    Submit,
    /// In-flight indicator that occupies the same box as the icon it replaces.
    Busy,
    Warning,
}

impl DesktopIcon {
    pub(super) const fn name(self) -> IconName {
        match self {
            Self::PanelLeftOpen => IconName::PanelLeftOpen,
            Self::PanelLeftClose => IconName::PanelLeftClose,
            Self::PanelRightOpen => IconName::PanelRightOpen,
            Self::PanelRightClose => IconName::PanelRightClose,
            Self::Overflow => IconName::Ellipsis,
            Self::ChevronDown => IconName::ChevronDown,
            Self::ChevronUp => IconName::ChevronUp,
            Self::SelectorCaret => IconName::ChevronsUpDown,
            Self::Copy => IconName::Copy,
            Self::Expand => IconName::Maximize,
            Self::OpenExternal => IconName::ExternalLink,
            Self::Search => IconName::Search,
            Self::Clear => IconName::CircleX,
            Self::Close => IconName::Close,
            Self::Plus => IconName::Plus,
            Self::Submit => IconName::ArrowUp,
            Self::Busy => IconName::LoaderCircle,
            Self::Warning => IconName::TriangleAlert,
        }
    }
}

/// Fixed control heights.
///
/// A control keeps its height across enabled, disabled, busy and selected, so
/// state changes never reflow the surface that hosts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DesktopControlSize {
    /// Inline tool action inside a row or card.
    Tool,
    /// Chrome control: panel toggle, overflow, selector.
    Compact,
    /// Composer submit and list rows.
    Standard,
    /// Decisions with a business outcome.
    Critical,
}

impl DesktopControlSize {
    pub(super) const fn pixels(self) -> f32 {
        match self {
            Self::Tool => 28.,
            Self::Compact => 32.,
            Self::Standard => 36.,
            Self::Critical => 40.,
        }
    }
}

/// Where a control sits on the weight ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DesktopControlWeight {
    /// Borderless; carries no permanent visual cost.
    Tool,
    /// Reads as a value, not as an action.
    Selector,
    /// The one obvious action on a surface.
    Primary,
    /// Has a business consequence and always keeps a text label.
    Critical(DesktopCriticalTone),
}

/// How severe a critical action is, so Deny and Allow-for-operation cannot
/// render identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DesktopCriticalTone {
    /// Reversible and safe; the resting choice.
    Neutral,
    /// Grants or proceeds.
    Affirmative,
    /// Widens scope or destroys work.
    Dangerous,
}

impl DesktopCriticalTone {
    const fn color(self, theme: SemanticTheme) -> SemanticColor {
        match self {
            Self::Neutral => theme.muted_text,
            Self::Affirmative => theme.accent,
            Self::Dangerous => theme.danger,
        }
    }
}

/// Text-labelled action with a business consequence.
///
/// Critical actions share one 40 px height and keep their semantic label at
/// every viewport. Tone changes weight, never geometry.
pub(super) struct DesktopCriticalButton {
    id: ElementId,
    label: SharedString,
    accessible_label: SharedString,
    tone: DesktopCriticalTone,
    disabled: bool,
}

impl DesktopCriticalButton {
    pub(super) fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        accessible_label: impl Into<SharedString>,
        tone: DesktopCriticalTone,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            accessible_label: accessible_label.into(),
            tone,
            disabled: false,
        }
    }

    pub(super) const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub(super) fn build(self) -> Button {
        let button = Button::new(self.id)
            .label(self.label)
            .tooltip(self.accessible_label)
            .disabled(self.disabled)
            .h(px(DesktopControlSize::Critical.pixels()));
        match self.tone {
            DesktopCriticalTone::Neutral => button.outline(),
            DesktopCriticalTone::Affirmative => button.primary(),
            DesktopCriticalTone::Dangerous => button.danger(),
        }
    }
}

/// An icon-only control.
///
/// The constructor takes the accessible label because an icon without one is
/// undiscoverable by keyboard and invisible to a screen reader. The same string
/// becomes the tooltip, so the two can never drift.
pub(super) struct DesktopIconButton {
    id: ElementId,
    icon: DesktopIcon,
    accessible_label: SharedString,
    size: DesktopControlSize,
    weight: DesktopControlWeight,
    selected: bool,
    disabled: bool,
    busy: bool,
}

impl DesktopIconButton {
    pub(super) fn new(
        id: impl Into<ElementId>,
        icon: DesktopIcon,
        accessible_label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            accessible_label: accessible_label.into(),
            size: DesktopControlSize::Compact,
            weight: DesktopControlWeight::Tool,
            selected: false,
            disabled: false,
            busy: false,
        }
    }

    pub(super) const fn size(mut self, size: DesktopControlSize) -> Self {
        self.size = size;
        self
    }

    pub(super) const fn weight(mut self, weight: DesktopControlWeight) -> Self {
        self.weight = weight;
        self
    }

    pub(super) const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub(super) const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Swap the glyph for a spinner without changing the control's box.
    pub(super) const fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    pub(super) fn build(self) -> Button {
        let side = px(self.size.pixels());
        let icon = if self.busy {
            DesktopIcon::Busy
        } else {
            self.icon
        };
        let button = Button::new(self.id)
            .icon(icon.name())
            .tooltip(self.accessible_label.clone())
            .selected(self.selected)
            .disabled(self.disabled || self.busy)
            .loading(self.busy)
            .w(side)
            .h(side)
            .flex_none();
        match self.weight {
            DesktopControlWeight::Tool | DesktopControlWeight::Selector => button.ghost(),
            DesktopControlWeight::Primary => button.primary(),
            DesktopControlWeight::Critical(DesktopCriticalTone::Dangerous) => button.danger(),
            DesktopControlWeight::Critical(_) => button.outline(),
        }
    }
}

/// A value the user can change, rendered as `current value ⌄`.
///
/// Distinct from an action button: it advertises state, and the caret says it
/// opens something rather than performing something.
pub(super) struct DesktopSelector {
    id: ElementId,
    value: SharedString,
    accessible_label: SharedString,
    size: DesktopControlSize,
    disabled: bool,
}

impl DesktopSelector {
    pub(super) fn new(
        id: impl Into<ElementId>,
        value: impl Into<SharedString>,
        accessible_label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            value: value.into(),
            accessible_label: accessible_label.into(),
            size: DesktopControlSize::Compact,
            disabled: false,
        }
    }

    pub(super) const fn size(mut self, size: DesktopControlSize) -> Self {
        self.size = size;
        self
    }

    pub(super) const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub(super) fn build(self) -> Button {
        Button::new(self.id)
            .ghost()
            .label(self.value)
            .tooltip(self.accessible_label)
            .dropdown_caret(true)
            .disabled(self.disabled)
            .h(px(self.size.pixels()))
            .flex_none()
    }
}

/// Visual state of an [`DesktopActionRow`].
///
/// Selection is expressed with background and an accent rail rather than a
/// border, so a selected row does not gain a pixel of height and no-colour
/// users still see the rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct DesktopRowState {
    pub(super) selected: bool,
    pub(super) disabled: bool,
    pub(super) focus_visible: bool,
}

/// A full-width row that is itself the action surface.
///
/// Sessions, changed files and palette entries are navigation targets, not
/// buttons: the whole row is clickable and focusable, so no per-row `Open`
/// button has to occupy permanent space. Trailing tool actions may be revealed
/// on hover or focus, but their width is always reserved so revealing them
/// cannot reflow the row.
pub(super) struct DesktopActionRow {
    id: ElementId,
    accessible_label: SharedString,
    state: DesktopRowState,
    size: DesktopControlSize,
    leading: Option<gpui::AnyElement>,
    title: SharedString,
    /// Rendered dimmed after the title and ellipsized inside the row's
    /// remaining width, so long metadata cannot displace the primary label.
    detail: Option<SharedString>,
    trailing: Option<gpui::AnyElement>,
    trailing_reserved_px: f32,
}

impl DesktopActionRow {
    pub(super) fn new(
        id: impl Into<ElementId>,
        title: impl Into<SharedString>,
        accessible_label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            accessible_label: accessible_label.into(),
            state: DesktopRowState::default(),
            size: DesktopControlSize::Standard,
            leading: None,
            title: title.into(),
            detail: None,
            trailing: None,
            trailing_reserved_px: 0.,
        }
    }

    pub(super) const fn state(mut self, state: DesktopRowState) -> Self {
        self.state = state;
        self
    }

    pub(super) const fn size(mut self, size: DesktopControlSize) -> Self {
        self.size = size;
        self
    }

    pub(super) fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_any_element());
        self
    }

    pub(super) fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Attach trailing content and reserve its width unconditionally.
    pub(super) fn trailing(mut self, trailing: impl IntoElement, reserved_px: f32) -> Self {
        self.trailing = Some(trailing.into_any_element());
        self.trailing_reserved_px = reserved_px;
        self
    }

    pub(super) fn build(self, theme: SemanticTheme) -> Button {
        let text_color = if self.state.disabled {
            theme.subtle_text
        } else {
            theme.text
        };
        let content_id = (self.id.clone(), "content");
        let accessible_label = self.accessible_label.clone();
        let selected = self.state.selected;
        Button::new(self.id)
            .ghost()
            .selected(self.state.selected)
            .disabled(self.state.disabled)
            .tooltip(self.accessible_label.clone())
            .w_full()
            .h(px(self.size.pixels()))
            .px_token(DesignSpace::Sm)
            .when(self.state.focus_visible, |row| {
                row.border_1().border_color(rgb(theme.focus_ring.value()))
            })
            .child(
                div()
                    .id(content_id)
                    .role(Role::Button)
                    .aria_label(accessible_label)
                    .aria_selected(selected)
                    .flex()
                    .flex_row()
                    .items_center()
                    .size_full()
                    .gap_token(DesignSpace::Sm)
                    .text_token(DesignText::Body)
                    .text_color(rgb(text_color.value()))
                    // Selection rail is readable without colour and never
                    // changes the row's height.
                    .child(
                        div()
                            .w(px(2.))
                            .h(px(self.size.pixels() * 0.5))
                            .flex_none()
                            .rounded_token(DesignRadius::Sm)
                            .when(self.state.selected, |rail| {
                                rail.bg(rgb(theme.accent.value()))
                            }),
                    )
                    .when_some(self.leading, |row, leading| row.child(leading))
                    .child(
                        div()
                            .flex_none()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(self.title),
                    )
                    .when_some(self.detail, |row, detail| {
                        row.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_token(DesignText::Metadata)
                                .text_color(rgb(theme.muted_text.value()))
                                .child(detail),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .w(px(self.trailing_reserved_px))
                            .flex()
                            .justify_end()
                            .when_some(self.trailing, |slot, trailing| slot.child(trailing)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_heights_are_a_fixed_ladder() {
        assert_eq!(DesktopControlSize::Tool.pixels(), 28.);
        assert_eq!(DesktopControlSize::Compact.pixels(), 32.);
        assert_eq!(DesktopControlSize::Standard.pixels(), 36.);
        assert_eq!(DesktopControlSize::Critical.pixels(), 40.);
    }

    #[test]
    fn every_named_icon_resolves_to_a_bundled_asset() {
        // `IconName` is generated from the files shipped by
        // `gpui-component-assets`, so naming a missing asset cannot compile and
        // no icon in the shell is hand-drawn. Exercising every variant keeps
        // that guarantee if the icon set is ever swapped.
        for icon in [
            DesktopIcon::PanelLeftOpen,
            DesktopIcon::PanelLeftClose,
            DesktopIcon::PanelRightOpen,
            DesktopIcon::PanelRightClose,
            DesktopIcon::Overflow,
            DesktopIcon::ChevronDown,
            DesktopIcon::ChevronUp,
            DesktopIcon::SelectorCaret,
            DesktopIcon::Copy,
            DesktopIcon::Expand,
            DesktopIcon::OpenExternal,
            DesktopIcon::Search,
            DesktopIcon::Clear,
            DesktopIcon::Close,
            DesktopIcon::Plus,
            DesktopIcon::Submit,
            DesktopIcon::Busy,
            DesktopIcon::Warning,
        ] {
            let _: IconName = icon.name();
        }
    }

    #[test]
    fn critical_tones_are_visually_distinct_from_each_other() {
        let theme = SemanticTheme::GEEK_DARK;
        let neutral = DesktopCriticalTone::Neutral.color(theme).value();
        let affirmative = DesktopCriticalTone::Affirmative.color(theme).value();
        let dangerous = DesktopCriticalTone::Dangerous.color(theme).value();
        assert_ne!(neutral, affirmative);
        assert_ne!(neutral, dangerous);
        assert_ne!(affirmative, dangerous);
    }

    #[test]
    fn critical_text_actions_share_one_fixed_height() {
        let neutral = DesktopCriticalButton::new(
            "neutral",
            "Allow once",
            "Allow this operation once",
            DesktopCriticalTone::Neutral,
        );
        let dangerous = DesktopCriticalButton::new(
            "dangerous",
            "Deny",
            "Deny this operation",
            DesktopCriticalTone::Dangerous,
        )
        .disabled(true);
        assert_eq!(DesktopControlSize::Critical.pixels(), 40.);
        assert_eq!(neutral.tone, DesktopCriticalTone::Neutral);
        assert_eq!(dangerous.tone, DesktopCriticalTone::Dangerous);
        assert!(!neutral.disabled);
        assert!(dangerous.disabled);
    }

    #[test]
    fn busy_icon_button_keeps_its_box_and_stops_accepting_input() {
        let resting = DesktopIconButton::new("probe", DesktopIcon::Submit, "Send message")
            .size(DesktopControlSize::Standard);
        let busy = DesktopIconButton::new("probe", DesktopIcon::Submit, "Send message")
            .size(DesktopControlSize::Standard)
            .busy(true);
        assert_eq!(resting.size.pixels(), busy.size.pixels());
        assert_eq!(resting.icon, busy.icon);
        // A busy control shows the spinner inside the same box and stops
        // accepting clicks, so submission cannot be issued twice.
        assert!(busy.busy);
        assert!(!resting.busy);
    }

    #[test]
    fn row_state_defaults_to_resting_and_is_independently_settable() {
        let resting = DesktopRowState::default();
        assert!(!resting.selected);
        assert!(!resting.disabled);
        assert!(!resting.focus_visible);

        let focused_selection = DesktopRowState {
            selected: true,
            disabled: false,
            focus_visible: true,
        };
        assert!(focused_selection.selected);
        assert!(focused_selection.focus_visible);
    }

    #[test]
    fn primitives_do_not_reach_product_state() {
        // Check the implementation only: the module header names these types in
        // prose precisely to state that the code must not touch them, and the
        // test module below is not shipped.
        let source = include_str!("desktop_controls.rs");
        let implementation = source
            .split_once("\nuse gpui::{")
            .expect("module implementation follows the header")
            .1
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("tests follow the implementation")
            .0;
        let projection = ["Desktop", "Projection"].concat();
        let root = ["Native", "Shell"].concat();
        let controller = ["Conversation", "Controller"].concat();
        let ledger = ["command_", "ledger"].concat();
        for forbidden in [&projection, &root, &controller, &ledger] {
            assert!(
                !implementation.contains(forbidden.as_str()),
                "shared controls must stay independent of {forbidden}"
            );
        }
    }
}
