use desktop::shell::{SESSION_PANEL_WIDTH, SemanticTheme, truncate_label};
use gpui::{
    EventEmitter, IntoElement, ParentElement as _, Render, Styled as _, WeakEntity, Window, div,
    prelude::*, px, rgb,
};
use gpui_component::{Disableable as _, button::Button};

use super::{DesktopCommandIntent, NativeShell};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SessionsPaneEvent {
    Create,
    Refresh,
    Open(String),
}

pub(super) struct SessionsPane {
    owner: WeakEntity<NativeShell>,
}

impl SessionsPane {
    pub(super) fn new(owner: WeakEntity<NativeShell>) -> Self {
        Self { owner }
    }
}

impl EventEmitter<SessionsPaneEvent> for SessionsPane {}

impl Render for SessionsPane {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(owner) = self.owner.upgrade() else {
            return div()
                .w(px(SESSION_PANEL_WIDTH as f32))
                .h_full()
                .into_any_element();
        };
        let owner = owner.read(cx);
        let theme = SemanticTheme::GEEK_DARK;
        let active_session_id = owner.projection.snapshot().session.session_id.as_str();
        let composer_running = owner.projection.snapshot().active_operation.is_some();
        let awaiting_prompt_start = owner.composer.submitted().is_some() && !composer_running;
        let session_pending = owner.command_ledger.contains_where(|intent| {
            matches!(
                intent,
                DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
            )
        });
        let session_catalog_pending = owner
            .command_ledger
            .contains(&DesktopCommandIntent::ListSessions);
        let current_session_label = truncate_label(active_session_id, 24);
        let omitted_sessions = owner.omitted_sessions;
        let focused = owner.sessions_focus.is_focused(window);
        let session_rows = owner
            .session_catalog
            .iter()
            .enumerate()
            .map(|(index, session)| {
                let target = session.session_id.clone();
                let active = target == active_session_id;
                let label = format!(
                    "{} {}",
                    if active { "●" } else { "○" },
                    truncate_label(&target, 24)
                );
                Button::new(("open-session", index))
                    .compact()
                    .label(label)
                    .tooltip(if active {
                        "Active coding-agent session"
                    } else {
                        "Open this coding-agent session"
                    })
                    .disabled(
                        active || composer_running || awaiting_prompt_start || session_pending,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(SessionsPaneEvent::Open(target.clone()));
                    }))
            })
            .collect::<Vec<_>>();

        div()
            .id("sessions-panel")
            .track_focus(&owner.sessions_focus)
            .w(px(SESSION_PANEL_WIDTH as f32))
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(rgb(if focused {
                theme.focus_ring.value()
            } else {
                theme.border.value()
            }))
            .bg(rgb(theme.surface.value()))
            .child(
                div()
                    .h_12()
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(theme.border.value()))
                    .child("SESSIONS")
                    .child(
                        Button::new("create-session")
                            .compact()
                            .label("New")
                            .tooltip("Create a new session · Ctrl/Cmd+N")
                            .disabled(composer_running || awaiting_prompt_start || session_pending)
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(SessionsPaneEvent::Create);
                            })),
                    ),
            )
            .child(
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .rounded_md()
                            .p_3()
                            .bg(rgb(theme.elevated.value()))
                            .child(current_session_label),
                    )
                    .child(
                        Button::new("refresh-session-catalog")
                            .compact()
                            .label(if session_catalog_pending {
                                "Loading sessions…"
                            } else {
                                "Refresh sessions"
                            })
                            .tooltip("Load the bounded project session catalog")
                            .disabled(session_catalog_pending || composer_running)
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(SessionsPaneEvent::Refresh);
                            })),
                    )
                    .children(session_rows)
                    .when(omitted_sessions > 0, |panel| {
                        panel.child(
                            div()
                                .text_sm()
                                .text_color(rgb(theme.warning.value()))
                                .child(format!("+ {omitted_sessions} older session(s) omitted")),
                        )
                    }),
            )
            .into_any_element()
    }
}
