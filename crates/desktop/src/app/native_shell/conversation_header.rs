use desktop::shell::{PanelVisibility, SemanticStatus, SemanticTheme, ShellLayout};
use gpui::{
    EventEmitter, FocusHandle, IntoElement, ParentElement as _, Render, Styled as _, Window, div,
    prelude::*, rgb,
};
use gpui_component::{
    Disableable as _,
    button::{Button, ButtonVariants as _},
    menu::{DropdownMenu as _, PopupMenuItem},
};
use std::sync::Arc;

use super::{
    conversation_focus_accent,
    desktop_style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
    semantic_status_color,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConversationHeaderEvent {
    ToggleSessions,
    ToggleInspector,
    SelectNextModel,
    SelectNextSessionProfile,
    CycleThinking,
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
    pub(super) thinking: Arc<str>,
    pub(super) project_name: Arc<str>,
    pub(super) keyboard_focus_visible: bool,
    pub(super) panel_visibility: PanelVisibility,
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
        let workspace_width = ShellLayout::resolve_with_panel_widths(
            u32::from(viewport.width),
            u32::from(viewport.height),
            view_model.panel_visibility,
            view_model.sessions_panel_width,
            view_model.context_panel_width,
        )
        .workspace
        .width;
        let model_profile_label = if workspace_width >= 680 {
            format!("{} / {}", view_model.model, view_model.profile)
        } else {
            "Model / profile".into()
        };
        let focused = self.focus.is_focused(window) && view_model.keyboard_focus_visible;
        let focus_accent = conversation_focus_accent(focused, theme);

        let model_target = cx.entity().downgrade();
        let profile_target = cx.entity().downgrade();
        let thinking_target = cx.entity().downgrade();
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
                        Button::new("toggle-sessions")
                            .debug_selector(|| "desktop-hit-toggle-sessions".into())
                            .compact()
                            .label("Sessions")
                            .tooltip("Show or hide Sessions")
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
                        Button::new("header-model-profile")
                            .debug_selector(|| "desktop-header-model-profile".into())
                            .compact()
                            .label(model_profile_label)
                            .tooltip("Model, profile, and thinking settings")
                            .dropdown_menu(move |menu, _, _| {
                                let model_target = model_target.clone();
                                let profile_target = profile_target.clone();
                                let thinking_target = thinking_target.clone();
                                menu.item(
                                    PopupMenuItem::new(format!(
                                        "Next model · {}",
                                        view_model.model
                                    ))
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
                                .item(
                                    PopupMenuItem::new(format!(
                                        "Thinking · {}",
                                        view_model.thinking
                                    ))
                                        .on_click(move |_, _, cx| {
                                            if let Some(target) = thinking_target.upgrade() {
                                                target.update(cx, |_, cx| {
                                                    cx.emit(ConversationHeaderEvent::CycleThinking);
                                                });
                                            }
                                        }),
                                )
                            }),
                    )
                    .child(
                        Button::new("toggle-inspector")
                            .debug_selector(|| "desktop-hit-toggle-inspector".into())
                            .compact()
                            .label("Inspector")
                            .tooltip("Show or hide Inspector · Ctrl/Cmd+\\")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(ConversationHeaderEvent::ToggleInspector);
                            })),
                    )
                    .when(view_model.composer_running, |actions| {
                        actions.child(
                            Button::new("abort-operation")
                                .compact()
                                .danger()
                                .label(if view_model.abort_pending {
                                    "Aborting…"
                                } else {
                                    "Abort"
                                })
                                .tooltip("Abort the active operation · Ctrl/Cmd+Esc")
                                .disabled(view_model.abort_pending)
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(ConversationHeaderEvent::Abort);
                                })),
                        )
                    })
                    .child(
                        Button::new("header-overflow")
                            .debug_selector(|| "desktop-header-overflow".into())
                            .compact()
                            .label("...")
                            .tooltip("More conversation actions")
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
