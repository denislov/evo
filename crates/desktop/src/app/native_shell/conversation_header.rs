use desktop::shell::SemanticTheme;
use gpui::{
    EventEmitter, IntoElement, ParentElement as _, Render, Styled as _, WeakEntity, Window, div,
    prelude::*, rgb,
};
use gpui_component::{Disableable as _, button::Button};

use super::{DesktopCommandIntent, NativeShell, conversation_focus_accent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConversationHeaderEvent {
    ToggleSessions,
    ToggleContext,
    Reload,
    CopySelected,
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
        let composer_running = snapshot.active_operation.is_some();
        let awaiting_prompt_start = owner.composer.submitted().is_some() && !composer_running;
        let abort_pending = owner
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Abort { .. }));
        let reload_pending = owner.command_ledger.contains(&DesktopCommandIntent::Reload);
        let selection_pending = owner
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Selection(_)));
        let reload_disabled =
            composer_running || awaiting_prompt_start || reload_pending || selection_pending;
        let committed_selection = owner
            .conversation_viewport
            .selected_block_id()
            .is_some_and(|id| owner.projection.conversation().block(id).is_some());
        let focused = owner.conversation_focus.is_focused(window) && owner.keyboard_focus_visible();
        let focus_accent = conversation_focus_accent(focused, theme);

        div()
            .id("conversation-header")
            .h_12()
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(focus_accent.value()))
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(if focused {
                        theme.accent.value()
                    } else {
                        theme.text.value()
                    }))
                    .child("EVO · CONVERSATION"),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("toggle-sessions")
                            .compact()
                            .label("Sessions")
                            .tooltip("Show or hide Sessions")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(ConversationHeaderEvent::ToggleSessions);
                            })),
                    )
                    .child(
                        Button::new("toggle-context")
                            .compact()
                            .label("Context")
                            .tooltip("Show or hide Context · Ctrl/Cmd+\\")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(ConversationHeaderEvent::ToggleContext);
                            })),
                    )
                    .child(
                        Button::new("reload-local-resources")
                            .compact()
                            .label(if reload_pending {
                                "Reloading…"
                            } else {
                                "Reload"
                            })
                            .tooltip("Reload product-owned local resources")
                            .disabled(reload_disabled)
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(ConversationHeaderEvent::Reload);
                            })),
                    )
                    .child(
                        Button::new("copy-conversation-block")
                            .compact()
                            .label("Copy")
                            .tooltip("Copy the selected durable conversation block")
                            .disabled(!committed_selection)
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(ConversationHeaderEvent::CopySelected);
                            })),
                    )
                    .when(composer_running, |actions| {
                        actions.child(
                            Button::new("abort-operation")
                                .compact()
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
                    }),
            )
            .into_any_element()
    }
}
