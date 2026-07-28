use desktop::shell::{PanelVisibility, SemanticStatus, SemanticTheme, ShellLayout};
use gpui::{
    EventEmitter, FocusHandle, IntoElement, ParentElement as _, Render, Styled as _, Window, div,
    prelude::*, rgb,
};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use std::sync::Arc;

use super::{
    conversation_focus_accent,
    desktop_controls::{
        DesktopCriticalButton, DesktopCriticalTone, DesktopIcon, DesktopIconButton, DesktopSelector,
    },
    desktop_style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
    semantic_status_color,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConversationHeaderEvent {
    ToggleSessions,
    ToggleInspector,
    SelectNextModel,
    SelectNextSessionProfile,
    Reload,
    Abort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConversationHeaderViewModel {
    pub(super) status: SemanticStatus,
    pub(super) composer_running: bool,
    pub(super) abort_pending: bool,
    pub(super) reload_pending: bool,
    pub(super) selector_disabled: bool,
    pub(super) model_cycle_available: bool,
    pub(super) profile_cycle_available: bool,
    pub(super) model: Arc<str>,
    pub(super) profile: Arc<str>,
    pub(super) project_name: Arc<str>,
    pub(super) keyboard_focus_visible: bool,
    pub(super) panel_visibility: PanelVisibility,
    pub(super) narrow_sessions_open: bool,
    pub(super) narrow_context_open: bool,
    pub(super) sessions_panel_width: u32,
    pub(super) context_panel_width: u32,
}

pub(super) struct ConversationHeader {
    focus: FocusHandle,
    view_model: Option<ConversationHeaderViewModel>,
}

impl ConversationHeader {
    pub(super) fn new(focus: FocusHandle) -> Self {
        Self {
            focus,
            view_model: None,
        }
    }

    pub(super) fn set_view_model(&mut self, view_model: ConversationHeaderViewModel) {
        self.view_model = Some(view_model);
    }
}

impl EventEmitter<ConversationHeaderEvent> for ConversationHeader {}

impl Render for ConversationHeader {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(view_model) = self.view_model.clone() else {
            return div().h_12().into_any_element();
        };
        let theme = SemanticTheme::GEEK_DARK;
        let status = view_model.status;
        let viewport = window.viewport_size();
        let layout = ShellLayout::resolve_with_panel_widths(
            u32::from(viewport.width),
            u32::from(viewport.height),
            view_model.panel_visibility,
            view_model.sessions_panel_width,
            view_model.context_panel_width,
        );
        let forced_layout = ShellLayout::resolve_with_panel_widths(
            u32::from(viewport.width),
            u32::from(viewport.height),
            PanelVisibility::default(),
            view_model.sessions_panel_width,
            view_model.context_panel_width,
        );
        let sessions_open = if forced_layout.sessions.is_some() {
            view_model.panel_visibility.sessions
        } else {
            view_model.narrow_sessions_open
        };
        let inspector_open = if forced_layout.context.is_some() {
            view_model.panel_visibility.context
        } else {
            view_model.narrow_context_open
        };
        let model_profile_label = if layout.workspace.width >= 680 {
            format!("{} / {}", view_model.model, view_model.profile)
        } else {
            view_model.model.to_string()
        };
        let model_profile_accessible_label =
            format!("Model {}, profile {}", view_model.model, view_model.profile);
        let focused = self.focus.is_focused(window) && view_model.keyboard_focus_visible;
        let focus_accent = conversation_focus_accent(focused, theme);

        let model_target = cx.entity().downgrade();
        let profile_target = cx.entity().downgrade();
        let reload_target = cx.entity().downgrade();

        div()
            .id("conversation-header")
            .debug_selector(|| "desktop-conversation-header".into())
            .h_12()
            .px_token(DesignSpace::Lg)
            .flex()
            .items_center()
            .gap_token(DesignSpace::Md)
            .border_b_1()
            .border_color(rgb(focus_accent.value()))
            .child(
                div()
                    .debug_selector(|| "desktop-header-identity".into())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_token(DesignSpace::Sm)
                    .child(
                        DesktopIconButton::new(
                            "toggle-sessions",
                            if sessions_open {
                                DesktopIcon::PanelLeftClose
                            } else {
                                DesktopIcon::PanelLeftOpen
                            },
                            if sessions_open {
                                "Hide Sessions panel"
                            } else {
                                "Show Sessions panel"
                            },
                        )
                        .selected(sessions_open)
                        .build()
                        .debug_selector(|| "desktop-hit-toggle-sessions".into())
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.emit(ConversationHeaderEvent::ToggleSessions);
                        })),
                    )
                    .child(
                        div()
                            .debug_selector(|| "desktop-header-session-title".into())
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .text_color(rgb(if focused {
                                theme.accent.value()
                            } else {
                                theme.text.value()
                            }))
                            .child(
                                div()
                                    .text_token(DesignText::Title)
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child("Current task"),
                            )
                            .child(
                                div()
                                    .text_token(DesignText::Metadata)
                                    .text_color(rgb(theme.subtle_text.value()))
                                    .child(view_model.project_name.to_string()),
                            ),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "desktop-header-actions".into())
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_token(DesignSpace::Sm)
                    .child(
                        div()
                            .debug_selector(|| "desktop-header-runtime-status".into())
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_token(DesignSpace::Xs)
                            .rounded_token(DesignRadius::Md)
                            .px_token(DesignSpace::Sm)
                            .py_token(DesignSpace::Xs)
                            .bg(rgb(theme.surface.value()))
                            .text_token(DesignText::Metadata)
                            .text_color(semantic_status_color(status))
                            .child(status.glyph())
                            .child(status.label()),
                    )
                    .child(
                        DesktopSelector::new(
                            "header-model-profile",
                            model_profile_label,
                            model_profile_accessible_label,
                        )
                        .disabled(view_model.selector_disabled)
                        .build()
                        .debug_selector(|| "desktop-header-model-profile".into())
                        .dropdown_menu(move |menu, _, _| {
                            let model_target = model_target.clone();
                            let profile_target = profile_target.clone();
                            menu.item(
                                PopupMenuItem::new(format!("Next model · {}", view_model.model))
                                    .disabled(
                                        view_model.selector_disabled
                                            || !view_model.model_cycle_available,
                                    )
                                    .on_click(move |_, _, cx| {
                                        if let Some(target) = model_target.upgrade() {
                                            target.update(cx, |_, cx| {
                                                cx.emit(ConversationHeaderEvent::SelectNextModel);
                                            });
                                        }
                                    }),
                            )
                            .item(
                                PopupMenuItem::new(format!(
                                    "Next profile · {}",
                                    view_model.profile
                                ))
                                .disabled(
                                    view_model.selector_disabled
                                        || !view_model.profile_cycle_available,
                                )
                                .on_click(move |_, _, cx| {
                                    if let Some(target) = profile_target.upgrade() {
                                        target.update(cx, |_, cx| {
                                            cx.emit(
                                                ConversationHeaderEvent::SelectNextSessionProfile,
                                            );
                                        });
                                    }
                                }),
                            )
                        }),
                    )
                    .child(
                        DesktopIconButton::new(
                            "toggle-inspector",
                            if inspector_open {
                                DesktopIcon::PanelRightClose
                            } else {
                                DesktopIcon::PanelRightOpen
                            },
                            if inspector_open {
                                "Hide Inspector panel · Ctrl/Cmd+\\"
                            } else {
                                "Show Inspector panel · Ctrl/Cmd+\\"
                            },
                        )
                        .selected(inspector_open)
                        .build()
                        .debug_selector(|| "desktop-hit-toggle-inspector".into())
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.emit(ConversationHeaderEvent::ToggleInspector);
                        })),
                    )
                    .when(view_model.composer_running, |actions| {
                        actions.child(
                            DesktopCriticalButton::new(
                                "abort-operation",
                                if view_model.abort_pending {
                                    "Aborting…"
                                } else {
                                    "Abort"
                                },
                                "Abort the active operation · Ctrl/Cmd+Esc",
                                DesktopCriticalTone::Dangerous,
                            )
                            .disabled(view_model.abort_pending)
                            .build()
                            .debug_selector(|| "desktop-hit-abort-operation".into())
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(ConversationHeaderEvent::Abort);
                            })),
                        )
                    })
                    .child(
                        DesktopIconButton::new(
                            "header-overflow",
                            DesktopIcon::Overflow,
                            "More conversation actions",
                        )
                        .build()
                        .debug_selector(|| "desktop-header-overflow".into())
                        .dropdown_menu(move |menu, _, _| {
                            let reload_target = reload_target.clone();
                            menu.item(
                                PopupMenuItem::new(if view_model.reload_pending {
                                    "Reloading local resources…"
                                } else {
                                    "Reload local resources"
                                })
                                .disabled(view_model.selector_disabled)
                                .on_click(move |_, _, cx| {
                                    if let Some(target) = reload_target.upgrade() {
                                        target.update(cx, |_, cx| {
                                            cx.emit(ConversationHeaderEvent::Reload);
                                        });
                                    }
                                }),
                            )
                        }),
                    ),
            )
            .into_any_element()
    }
}
