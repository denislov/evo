use desktop::shell::{MONOSPACE_FONT_FAMILY, STATUS_HEIGHT, SemanticStatus, SemanticTheme};
use gpui::{
    FocusHandle, IntoElement, ParentElement as _, Render, Role, Styled as _, Window, div,
    prelude::*, px, rgb,
};
use gpui_component::{
    button::Button,
    menu::{DropdownMenu as _, PopupMenuItem},
};
use std::sync::Arc;

use super::{
    desktop_style::{DesignSpace, DesignText, DesktopStyledExt as _},
    semantic_status_color,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StatusBarViewModel {
    pub(super) status: SemanticStatus,
    pub(super) changed_file_count: usize,
    pub(super) notice: Option<Arc<str>>,
    pub(super) keyboard_focus_visible: bool,
}

pub(super) struct StatusBar {
    focus: FocusHandle,
    view_model: Option<StatusBarViewModel>,
}

impl StatusBar {
    pub(super) fn new(focus: FocusHandle) -> Self {
        Self {
            focus,
            view_model: None,
        }
    }

    pub(super) fn set_view_model(&mut self, view_model: StatusBarViewModel) {
        self.view_model = Some(view_model);
    }
}

impl Render for StatusBar {
    fn render(&mut self, window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(view_model) = self.view_model.clone() else {
            return div().h(px(STATUS_HEIGHT as f32)).into_any_element();
        };
        let theme = SemanticTheme::GEEK_DARK;
        let status = view_model.status;
        let notice = view_model.notice.clone();
        let notice_for_menu = notice.clone();
        let focused = self.focus.is_focused(window) && view_model.keyboard_focus_visible;

        div()
            .id("status-panel")
            .role(Role::Status)
            .aria_label(format!("Desktop status: {}", status.label()))
            .when_some(notice.clone(), |bar, notice| bar.aria_description(notice))
            .debug_selector(|| "desktop-status-panel".into())
            .track_focus(&self.focus)
            .h(px(STATUS_HEIGHT as f32))
            .px_token(DesignSpace::Md)
            .flex()
            .items_center()
            .justify_between()
            .gap_token(DesignSpace::Md)
            .border_t_1()
            .border_color(rgb(if focused {
                theme.focus_ring.value()
            } else {
                theme.divider.value()
            }))
            .bg(rgb(theme.elevated.value()))
            .child(
                div()
                    .debug_selector(|| "desktop-status-primary".into())
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_token(DesignSpace::Sm)
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .gap_token(DesignSpace::Xs)
                            .text_color(semantic_status_color(status))
                            .child(status.glyph())
                            .child(status.label()),
                    )
                    .child(
                        div()
                            .debug_selector(|| "desktop-status-changes".into())
                            .flex_none()
                            .text_token(DesignText::Metadata)
                            .text_color(rgb(theme.subtle_text.value()))
                            .child(if view_model.changed_file_count == 1 {
                                "1 changed file".to_owned()
                            } else {
                                format!("{} changed files", view_model.changed_file_count)
                            }),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "desktop-status-secondary".into())
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_token(DesignSpace::Sm)
                    .font_family(MONOSPACE_FONT_FAMILY)
                    .text_color(rgb(theme.subtle_text.value()))
                    .when_some(notice_for_menu, |bar, notice| {
                        bar.child(
                            Button::new("status-details")
                                .debug_selector(|| "desktop-status-details".into())
                                .compact()
                                .label("Notice")
                                .tooltip("Open status details")
                                .dropdown_menu(move |menu, _, _| {
                                    menu.item(PopupMenuItem::label(notice.clone()))
                                }),
                        )
                    })
                    .child(
                        div()
                            .debug_selector(|| "desktop-command-palette-hint".into())
                            .flex_none()
                            .text_token(DesignText::Metadata)
                            .child("Commands Ctrl/Cmd+K"),
                    ),
            )
            .into_any_element()
    }
}
