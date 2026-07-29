use desktop::preferences::DesktopThinkingLevel;
use desktop::shell::{
    CENTER_HEADER_HEIGHT, PanelVisibility, SemanticStatus, SemanticTheme, ShellLayout,
};
use gpui::{
    EventEmitter, FocusHandle, IntoElement, ParentElement as _, Render, Role, Styled as _, Window,
    div, prelude::*, px, rgb,
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

/// Stable space for the attention-only runtime indicator. Idle leaves this
/// slot empty so status changes never move the selectors or panel actions.
pub(super) const HEADER_RUNTIME_STATUS_SLOT_WIDTH: f32 = 104.;
const HEADER_RUNTIME_STATUS_COMPACT_SLOT_WIDTH: f32 = 80.;

pub(super) const fn header_runtime_status_slot_width(viewport_width: u32) -> f32 {
    if viewport_width < 900 {
        HEADER_RUNTIME_STATUS_COMPACT_SLOT_WIDTH
    } else {
        HEADER_RUNTIME_STATUS_SLOT_WIDTH
    }
}

const fn header_runtime_status_label(status: SemanticStatus, compact: bool) -> &'static str {
    match status {
        // The full accessible name remains `Authorization required`; the
        // compact visual label keeps the attention slot bounded at the
        // narrowest supported Conversation workspace.
        SemanticStatus::Authorization if compact => "Auth",
        SemanticStatus::Authorization => "Approval",
        _ => status.label(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ConversationHeaderEvent {
    ToggleSessions,
    ToggleInspector,
    SelectModel(Arc<str>),
    SelectSessionProfile(Arc<str>),
    SelectThinking(DesktopThinkingLevel),
    Reload,
    Abort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConversationHeaderSelectorOption {
    pub(super) id: Arc<str>,
    pub(super) label: Arc<str>,
    pub(super) selectable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConversationHeaderViewModel {
    pub(super) idle: bool,
    pub(super) status: SemanticStatus,
    pub(super) composer_running: bool,
    pub(super) abort_pending: bool,
    pub(super) reload_pending: bool,
    pub(super) selector_disabled: bool,
    pub(super) model: Arc<str>,
    pub(super) profile: Arc<str>,
    pub(super) thinking: Arc<str>,
    pub(super) thinking_selection: DesktopThinkingLevel,
    pub(super) current_model_id: Arc<str>,
    pub(super) current_profile_id: Arc<str>,
    pub(super) model_options: Arc<[ConversationHeaderSelectorOption]>,
    pub(super) profile_options: Arc<[ConversationHeaderSelectorOption]>,
    pub(super) project_name: Arc<str>,
    pub(super) keyboard_focus_visible: bool,
    pub(super) panel_visibility: PanelVisibility,
    pub(super) sessions_drawer_open: bool,
    pub(super) inspector_drawer_open: bool,
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
            return div().h(px(CENTER_HEADER_HEIGHT as f32)).into_any_element();
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
        let sessions_open = if forced_layout.sidebar.is_some() {
            view_model.panel_visibility.sessions
        } else {
            view_model.sessions_drawer_open
        };
        let inspector_open = if forced_layout.inspector.is_some() {
            view_model.panel_visibility.context
        } else {
            view_model.inspector_drawer_open
        };
        let viewport_width = u32::from(viewport.width);
        let expanded_chrome =
            layout.center.width >= 900 || (view_model.idle && viewport_width >= 1_100);
        let (model_label, profile_label, thinking_label) = if expanded_chrome {
            (
                format!("Model · {}", view_model.model),
                format!("Profile · {}", view_model.profile),
                format!("Thinking · {}", view_model.thinking),
            )
        } else {
            (
                format!("M · {}", view_model.model),
                format!("P · {}", view_model.profile),
                format!("T · {}", view_model.thinking),
            )
        };
        let show_session_title = expanded_chrome || (view_model.idle && viewport_width >= 900);
        let compact_actions = viewport_width < 900;
        let status_slot_width = header_runtime_status_slot_width(viewport_width);
        let model_accessible_label =
            format!("Select model; current {}", view_model.current_model_id);
        let profile_accessible_label = format!(
            "Select session profile; current {}",
            view_model.current_profile_id
        );
        let thinking_accessible_label = format!(
            "Select session thinking level; current {}",
            view_model.thinking
        );
        let focused = self.focus.is_focused(window) && view_model.keyboard_focus_visible;
        let focus_accent = conversation_focus_accent(focused, theme);

        let model_target = cx.entity().downgrade();
        let profile_target = cx.entity().downgrade();
        let thinking_target = cx.entity().downgrade();
        let reload_target = cx.entity().downgrade();
        let model_options = Arc::clone(&view_model.model_options);
        let profile_options = Arc::clone(&view_model.profile_options);
        let current_model_id = Arc::clone(&view_model.current_model_id);
        let current_profile_id = Arc::clone(&view_model.current_profile_id);
        let selector_disabled = view_model.selector_disabled;

        div()
            .id("conversation-header")
            .debug_selector(|| "desktop-conversation-header".into())
            .track_focus(&self.focus)
            .h(px(CENTER_HEADER_HEIGHT as f32))
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
                    .when(show_session_title, |identity| identity.child(
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
                                    .child(if view_model.idle {
                                        "New task"
                                    } else {
                                        "Current task"
                                    }),
                            )
                            .child(
                                div()
                                    .text_token(DesignText::Metadata)
                                    .text_color(rgb(theme.subtle_text.value()))
                                    .child(view_model.project_name.to_string()),
                            ),
                    )),
            )
            .child(
                div()
                    .debug_selector(|| "desktop-header-actions".into())
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_token(if compact_actions {
                        DesignSpace::Xs
                    } else {
                        DesignSpace::Sm
                    })
                    .child(
                        div()
                            .debug_selector(|| "desktop-header-runtime-status-slot".into())
                            .w(px(status_slot_width))
                            .flex_none()
                            .flex()
                            .items_center()
                            .when(status != SemanticStatus::Idle, |slot| {
                                slot.child(
                                    div()
                                        .id("header-runtime-status")
                                        .debug_selector(|| {
                                            "desktop-header-runtime-status".into()
                                        })
                                        .role(Role::Status)
                                        .aria_label(status.label())
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
                                        .child(header_runtime_status_label(
                                            status,
                                            compact_actions,
                                        )),
                                )
                            }),
                    )
                    .child(
                        DesktopSelector::new(
                            "header-model-selector",
                            model_label,
                            model_accessible_label,
                        )
                        .disabled(selector_disabled)
                        .build()
                        .debug_selector(|| "desktop-header-model-selector".into())
                        .dropdown_menu(move |menu, _, _| {
                            model_options.iter().fold(
                                menu.min_w(px(280.))
                                    .max_w(px(480.))
                                    .max_h(px(320.))
                                    .scrollable(model_options.len() > 8),
                                |menu, option| {
                                    let target = model_target.clone();
                                    let id = Arc::clone(&option.id);
                                    menu.item(
                                        PopupMenuItem::new(option.label.to_string())
                                            .checked(option.id == current_model_id)
                                            .disabled(!option.selectable)
                                            .on_click(move |_, _, cx| {
                                                if let Some(target) = target.upgrade() {
                                                    let id = Arc::clone(&id);
                                                    target.update(cx, |_, cx| {
                                                        cx.emit(ConversationHeaderEvent::SelectModel(
                                                            id,
                                                        ));
                                                    });
                                                }
                                            }),
                                    )
                                },
                            )
                        }),
                    )
                    .child(
                        DesktopSelector::new(
                            "header-thinking-selector",
                            thinking_label,
                            thinking_accessible_label,
                        )
                        .build()
                        .debug_selector(|| "desktop-header-thinking-selector".into())
                        .dropdown_menu(move |menu, _, _| {
                            DesktopThinkingLevel::ALL.iter().fold(
                                menu.min_w(px(180.)).max_w(px(280.)),
                                |menu, level| {
                                    let target = thinking_target.clone();
                                    let level = *level;
                                    menu.item(
                                        PopupMenuItem::new(match level {
                                            DesktopThinkingLevel::Default => "Default",
                                            DesktopThinkingLevel::Off => "Off",
                                            DesktopThinkingLevel::Minimal => "Minimal",
                                            DesktopThinkingLevel::Low => "Low",
                                            DesktopThinkingLevel::Medium => "Medium",
                                            DesktopThinkingLevel::High => "High",
                                            DesktopThinkingLevel::XHigh => "XHigh",
                                        })
                                        .checked(level == view_model.thinking_selection)
                                        .on_click(move |_, _, cx| {
                                            if let Some(target) = target.upgrade() {
                                                target.update(cx, |_, cx| {
                                                    cx.emit(
                                                        ConversationHeaderEvent::SelectThinking(
                                                            level,
                                                        ),
                                                    );
                                                });
                                            }
                                        }),
                                    )
                                },
                            )
                        }),
                    )
                    .child(
                        DesktopSelector::new(
                            "header-profile-selector",
                            profile_label,
                            profile_accessible_label,
                        )
                        .disabled(selector_disabled)
                        .build()
                        .debug_selector(|| "desktop-header-profile-selector".into())
                        .dropdown_menu(move |menu, _, _| {
                            profile_options.iter().fold(
                                menu.min_w(px(240.))
                                    .max_w(px(420.))
                                    .max_h(px(320.))
                                    .scrollable(profile_options.len() > 8),
                                |menu, option| {
                                    let target = profile_target.clone();
                                    let id = Arc::clone(&option.id);
                                    menu.item(
                                        PopupMenuItem::new(option.label.to_string())
                                            .checked(option.id == current_profile_id)
                                            .disabled(!option.selectable)
                                            .on_click(move |_, _, cx| {
                                                if let Some(target) = target.upgrade() {
                                                    let id = Arc::clone(&id);
                                                    target.update(cx, |_, cx| {
                                                        cx.emit(
                                                            ConversationHeaderEvent::SelectSessionProfile(
                                                                id,
                                                            ),
                                                        );
                                                    });
                                                }
                                            }),
                                    )
                                },
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
