use desktop::shell::{SemanticTheme, truncate_label};
use gpui::{
    EventEmitter, IntoElement, ParentElement as _, Render, Styled as _, WeakEntity, Window, div,
    prelude::*, rgb,
};
use gpui_component::{
    Disableable as _,
    button::{Button, ButtonVariants as _},
    menu::{DropdownMenu as _, PopupMenuItem},
};

use super::{
    DesktopCommandIntent, NativeShell, conversation_focus_accent,
    desktop_style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
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

pub(super) struct ConversationHeader {
    owner: WeakEntity<NativeShell>,
}

impl ConversationHeader {
    pub(super) fn new(owner: WeakEntity<NativeShell>) -> Self {
        Self { owner }
    }
}

impl EventEmitter<ConversationHeaderEvent> for ConversationHeader {}

impl Render for ConversationHeader {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(owner) = self.owner.upgrade() else {
            return div().h_12().into_any_element();
        };
        let owner = owner.read(cx);
        let theme = SemanticTheme::GEEK_DARK;
        let snapshot = owner.projection.snapshot();
        let project = owner.projection.project();
        let status = owner.semantic_status();
        let composer_running = snapshot.active_operation.is_some();
        let awaiting_prompt_start = owner.composer.submitted().is_some() && !composer_running;
        let abort_pending = owner
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Abort { .. }));
        let reload_pending = owner.command_ledger.contains(&DesktopCommandIntent::Reload);
        let selection_pending = owner
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Selection(_)));
        let selector_disabled =
            composer_running || awaiting_prompt_start || reload_pending || selection_pending;
        let model_cycle_available = project
            .models
            .iter()
            .filter(|model| model.supports_text && (model.configured || model.selected))
            .take(2)
            .count()
            > 1;
        let profile_cycle_available = project.profiles.len() > 1;
        let status_model = truncate_label(&project.selected_model_id, 10);
        let status_profile = truncate_label(snapshot.session.default_agent_profile_id.as_str(), 9);
        let thinking = owner
            .thinking_selection
            .label(project.settings.default_thinking_level.as_deref());
        let workspace_width = owner.layout(window).workspace.width;
        let model_profile_label = if workspace_width >= 680 {
            format!("{status_model} / {status_profile}")
        } else {
            "Model / profile".into()
        };
        let project_name = project
            .cwd
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| truncate_label(name, 18))
            .unwrap_or_else(|| "Project".into());
        let focused = owner.conversation_focus.is_focused(window) && owner.keyboard_focus_visible();
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
                                    .child(project_name),
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
                            .text_color(owner.status_color(status))
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
                                    PopupMenuItem::new(format!("Next model · {status_model}"))
                                        .disabled(selector_disabled || !model_cycle_available)
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
                                        "Next profile · {status_profile}"
                                    ))
                                    .disabled(selector_disabled || !profile_cycle_available)
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
                                    PopupMenuItem::new(format!("Thinking · {thinking}"))
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
                    .when(composer_running, |actions| {
                        actions.child(
                            Button::new("abort-operation")
                                .compact()
                                .danger()
                                .label(if abort_pending {
                                    "Aborting…"
                                } else {
                                    "Abort"
                                })
                                .tooltip("Abort the active operation · Ctrl/Cmd+Esc")
                                .disabled(abort_pending)
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
                                    PopupMenuItem::new(if reload_pending {
                                        "Reloading local resources…"
                                    } else {
                                        "Reload local resources"
                                    })
                                    .disabled(selector_disabled)
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
