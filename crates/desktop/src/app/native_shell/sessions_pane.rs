use desktop::shell::{MONOSPACE_FONT_FAMILY, SESSION_PANEL_WIDTH, SemanticTheme, truncate_label};
use gpui::{
    EventEmitter, IntoElement, ParentElement as _, Render, Styled as _, WeakEntity, Window, div,
    prelude::*, px, rgb,
};
use gpui_component::input::Input;
use gpui_component::{Disableable as _, button::Button};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

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

fn relative_session_time(updated_at: &str, now: OffsetDateTime) -> String {
    let Ok(updated) = OffsetDateTime::parse(updated_at, &Rfc3339) else {
        return truncate_label(updated_at, 16);
    };
    let seconds = (now - updated).whole_seconds().max(0);
    match seconds {
        0..=59 => "now".into(),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        86_400..=604_799 => format!("{}d ago", seconds / 86_400),
        _ => updated_at.get(0..10).unwrap_or(updated_at).to_owned(),
    }
}

impl Render for SessionsPane {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(owner) = self.owner.upgrade() else {
            return div()
                .w(px(SESSION_PANEL_WIDTH as f32))
                .h_full()
                .into_any_element();
        };
        let owner = owner.read(cx);
        let panel_width = owner.preferences.sessions_panel_width;
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
        let search_input = owner.sessions_search_input.clone();
        let search = search_input.read(cx).value().trim().to_lowercase();
        let omitted_sessions = owner.omitted_sessions;
        let focused = owner.sessions_focus.is_focused(window);
        let now = OffsetDateTime::now_utc();
        let session_rows = owner
            .session_catalog
            .iter()
            .filter(|session| {
                search.is_empty()
                    || session.session_id.to_lowercase().contains(&search)
                    || session.updated_at.to_lowercase().contains(&search)
            })
            .enumerate()
            .map(|(index, session)| {
                let target = session.session_id.clone();
                let active = target == active_session_id;
                let semantic_name = if active {
                    "Current task".to_owned()
                } else {
                    format!("Recent task {}", index + 1)
                };
                let relative_time = relative_session_time(&session.updated_at, now);
                let status = if active && composer_running {
                    "running"
                } else if active {
                    "current"
                } else {
                    "idle"
                };
                div()
                    .id(("session-row", index))
                    .rounded_md()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .bg(rgb(if active {
                        theme.elevated.value()
                    } else {
                        theme.canvas.value()
                    }))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(rgb(if active {
                                theme.accent.value()
                            } else {
                                theme.text.value()
                            }))
                            .child(semantic_name)
                            .child(relative_time),
                    )
                    .child(
                        div()
                            .font_family(MONOSPACE_FONT_FAMILY)
                            .text_xs()
                            .text_color(rgb(theme.muted_text.value()))
                            .child(format!("{} · {status}", truncate_label(&target, 22))),
                    )
                    .when(!active, |row| {
                        row.child(
                            Button::new(("open-session", index))
                                .compact()
                                .label("Open")
                                .tooltip("Open this recent coding task")
                                .disabled(
                                    composer_running || awaiting_prompt_start || session_pending,
                                )
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(SessionsPaneEvent::Open(target.clone()));
                                })),
                        )
                    })
            })
            .collect::<Vec<_>>();

        div()
            .id("sessions-panel")
            .track_focus(&owner.sessions_focus)
            .w(px(panel_width as f32))
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
                    .id("sessions-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().child(Input::new(&search_input).appearance(false)))
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
                    .when(
                        !search.is_empty()
                            && !owner.session_catalog.iter().any(|session| {
                                session.session_id.to_lowercase().contains(&search)
                                    || session.updated_at.to_lowercase().contains(&search)
                            }),
                        |panel| {
                            panel.child(
                                div()
                                    .p_2()
                                    .text_color(rgb(theme.muted_text.value()))
                                    .child("No matching sessions."),
                            )
                        },
                    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_session_time_is_stable_and_bounded() {
        let now = OffsetDateTime::parse("2026-07-27T12:00:00Z", &Rfc3339).unwrap();
        assert_eq!(relative_session_time("2026-07-27T11:59:45Z", now), "now");
        assert_eq!(
            relative_session_time("2026-07-27T11:35:00Z", now),
            "25m ago"
        );
        assert_eq!(relative_session_time("2026-07-27T06:00:00Z", now), "6h ago");
        assert_eq!(relative_session_time("2026-07-24T12:00:00Z", now), "3d ago");
        assert_eq!(
            relative_session_time("2026-06-01T00:00:00Z", now),
            "2026-06-01"
        );
        assert_eq!(relative_session_time("malformed", now), "malformed");
    }
}
