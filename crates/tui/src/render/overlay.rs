use crate::component::{Component, ComponentId};
use crate::render::Tui;
use crate::terminal::Terminal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAnchor {
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    TopCenter,
    BottomCenter,
    LeftCenter,
    RightCenter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeValue {
    Columns(usize),
    Percent(u8),
}

impl From<usize> for SizeValue {
    fn from(value: usize) -> Self {
        Self::Columns(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OverlayMargin {
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
    pub left: usize,
}

/// Visibility predicate for overlays.
/// Return `true` to show the overlay, `false` to hide it.
/// Called each render cycle with the current terminal dimensions.
pub type OverlayVisibleFn = Box<dyn FnMut(usize, usize) -> bool>;

/// Options for overlay positioning and sizing.
///
/// Mirrors TS `OverlayOptions` in `pi/packages/tui/src/tui.ts`.
pub struct OverlayOptions {
    pub width: Option<SizeValue>,
    pub min_width: Option<usize>,
    pub max_height: Option<SizeValue>,
    pub anchor: OverlayAnchor,
    pub offset_x: isize,
    pub offset_y: isize,
    pub row: Option<SizeValue>,
    pub col: Option<SizeValue>,
    pub margin: OverlayMargin,
    pub non_capturing: bool,
    /// Optional visibility callback.
    /// If provided, the overlay is only rendered when this returns `true`.
    /// Called each render cycle with `(term_width, term_height)`.
    pub visible: Option<OverlayVisibleFn>,
}

impl std::fmt::Debug for OverlayOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayOptions")
            .field("width", &self.width)
            .field("min_width", &self.min_width)
            .field("max_height", &self.max_height)
            .field("anchor", &self.anchor)
            .field("offset_x", &self.offset_x)
            .field("offset_y", &self.offset_y)
            .field("row", &self.row)
            .field("col", &self.col)
            .field("margin", &self.margin)
            .field("non_capturing", &self.non_capturing)
            .field(
                "visible",
                &self
                    .visible
                    .as_ref()
                    .map(|_| &"<fn>" as &dyn std::fmt::Debug),
            )
            .finish()
    }
}

impl Default for OverlayOptions {
    fn default() -> Self {
        Self {
            width: None,
            min_width: None,
            max_height: None,
            anchor: OverlayAnchor::Center,
            offset_x: 0,
            offset_y: 0,
            row: None,
            col: None,
            margin: OverlayMargin::default(),
            non_capturing: false,
            visible: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayHandle {
    pub(crate) id: usize,
}

impl OverlayHandle {
    pub fn hide<T: Terminal>(self, tui: &mut Tui<T>) {
        tui.hide_overlay(self);
    }

    pub fn set_hidden<T: Terminal>(self, tui: &mut Tui<T>, hidden: bool) {
        tui.set_overlay_hidden(self, hidden);
    }

    pub fn focus<T: Terminal>(self, tui: &mut Tui<T>) {
        tui.focus_overlay(self);
    }

    pub fn unfocus<T: Terminal>(self, tui: &mut Tui<T>, target: Option<ComponentId>) {
        tui.unfocus_overlay(self, target);
    }
}

pub(crate) struct OverlayEntry {
    pub id: usize,
    pub component_id: ComponentId,
    pub component: Box<dyn Component>,
    pub options: OverlayOptions,
    pub hidden: bool,
    pub restore_focus: Option<ComponentId>,
}

impl OverlayEntry {
    /// Check whether this overlay is currently visible.
    /// Returns `false` if `hidden` is set, or if the `visible` callback returns `false`.
    pub fn is_visible(&mut self, term_width: usize, term_height: usize) -> bool {
        if self.hidden {
            return false;
        }
        if let Some(ref mut visible_fn) = self.options.visible {
            visible_fn(term_width, term_height)
        } else {
            true
        }
    }
}
