use desktop::runtime::DesktopSessionCatalogEntry;
use desktop::shell::{MONOSPACE_FONT_FAMILY, SESSION_PANEL_WIDTH, SemanticTheme, truncate_label};
use gpui::{
    EventEmitter, FocusHandle, IntoElement, ParentElement as _, Render, Role, Styled as _,
    Subscription, Window, div, prelude::*, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{
    Disableable as _,
    button::Button,
    menu::{DropdownMenu as _, PopupMenuItem},
};
use std::sync::Arc;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{
    desktop_style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
    semantic_status_color,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SessionsPaneEvent {
    Create,
    Refresh,
    Open(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionsPaneViewModel {
    pub(super) panel_width: u32,
    pub(super) catalog: Arc<[DesktopSessionCatalogEntry]>,
    pub(super) omitted_sessions: usize,
    pub(super) active_session_id: Arc<str>,
    pub(super) composer_running: bool,
    pub(super) awaiting_prompt_start: bool,
    pub(super) session_pending: bool,
    pub(super) session_catalog_pending: bool,
    pub(super) active_status: desktop::shell::SemanticStatus,
    pub(super) notice: Option<Arc<str>>,
    pub(super) keyboard_focus_visible: bool,
}

pub(super) struct SessionsPane {
    focus: FocusHandle,
    search_input: gpui::Entity<InputState>,
    view_model: Option<SessionsPaneViewModel>,
    _search_subscription: Subscription,
}

impl SessionsPane {
    pub(super) fn new(
        focus: FocusHandle,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search sessions…"));
        let search_subscription =
            cx.subscribe_in(&search_input, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            });
        Self {
            focus,
            search_input,
            view_model: None,
            _search_subscription: search_subscription,
        }
    }

    pub(super) fn set_view_model(&mut self, view_model: SessionsPaneViewModel) {
        self.view_model = Some(view_model);
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
        let Some(view_model) = self.view_model.clone() else {
            return div()
                .w(px(SESSION_PANEL_WIDTH as f32))
                .h_full()
                .into_any_element();
        };
        let panel_width = view_model.panel_width;
        let theme = SemanticTheme::GEEK_DARK;
        let active_session_id = view_model.active_session_id.as_ref();
        let composer_running = view_model.composer_running;
        let awaiting_prompt_start = view_model.awaiting_prompt_start;
        let session_pending = view_model.session_pending;
        let session_catalog_pending = view_model.session_catalog_pending;
        let search_input = self.search_input.clone();
        let search = search_input.read(cx).value().trim().to_lowercase();
        let omitted_sessions = view_model.omitted_sessions;
        let focused = self.focus.is_focused(window) && view_model.keyboard_focus_visible;
        let active_semantic_status = view_model.active_status;
        let refresh_target = cx.entity().downgrade();
        let now = OffsetDateTime::now_utc();
        let visible_session_count = view_model
            .catalog
            .iter()
            .filter(|session| {
                search.is_empty()
                    || session.session_id.to_lowercase().contains(&search)
                    || session.updated_at.to_lowercase().contains(&search)
            })
            .count();
        let session_rows = view_model
            .catalog
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
                let (status_glyph, status, status_color) = if active {
                    let label = active_semantic_status.label();
                    (
                        active_semantic_status.glyph(),
                        if label == "Idle" {
                            "current".to_owned()
                        } else {
                            label.to_lowercase()
                        },
                        semantic_status_color(active_semantic_status),
                    )
                } else {
                    ("○", "available".to_owned(), rgb(theme.muted_text.value()))
                };
                let accessible_label =
                    format!("{semantic_name}, {status}, updated {relative_time}");
                div()
                    .id(("session-row", index))
                    .role(Role::ListItem)
                    .aria_label(accessible_label)
                    .aria_selected(active)
                    .aria_position_in_set(index + 1)
                    .aria_size_of_set(visible_session_count)
                    .rounded_token(DesignRadius::Md)
                    .p_token(DesignSpace::Sm)
                    .flex()
                    .flex_col()
                    .gap_token(DesignSpace::Xs)
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
                            .flex()
                            .items_center()
                            .gap_token(DesignSpace::Xs)
                            .font_family(MONOSPACE_FONT_FAMILY)
                            .text_token(DesignText::Metadata)
                            .text_color(rgb(theme.muted_text.value()))
                            .child(div().text_color(status_color).child(status_glyph))
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
            .role(Role::Navigation)
            .aria_label("Sessions")
            .when_some(view_model.notice.clone(), |panel, notice| {
                panel.aria_description(notice)
            })
            .debug_selector(|| "desktop-sessions-panel".into())
            .track_focus(&self.focus)
            .w(px(panel_width as f32))
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(rgb(if focused {
                theme.focus_ring.value()
            } else {
                theme.divider.value()
            }))
            .bg(rgb(theme.surface.value()))
            .child(
                div()
                    .h_12()
                    .px_token(DesignSpace::Lg)
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(theme.divider.value()))
                    .child("SESSIONS")
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_token(DesignSpace::Xs)
                            .child(
                                Button::new("create-session")
                                    .debug_selector(|| "desktop-hit-create-session".into())
                                    .compact()
                                    .label("New")
                                    .tooltip("Create a new session · Ctrl/Cmd+N")
                                    .disabled(
                                        composer_running
                                            || awaiting_prompt_start
                                            || session_pending,
                                    )
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(SessionsPaneEvent::Create);
                                    })),
                            )
                            .child(
                                Button::new("sessions-overflow")
                                    .debug_selector(|| "desktop-hit-sessions-overflow".into())
                                    .compact()
                                    .label("...")
                                    .tooltip("More Sessions actions")
                                    .dropdown_menu(move |menu, _, _| {
                                        let refresh_target = refresh_target.clone();
                                        menu.item(
                                            PopupMenuItem::new(if session_catalog_pending {
                                                "Loading sessions…"
                                            } else {
                                                "Refresh sessions"
                                            })
                                            .disabled(session_catalog_pending || composer_running)
                                            .on_click(move |_, _, cx| {
                                                if let Some(target) = refresh_target.upgrade() {
                                                    target.update(cx, |_, cx| {
                                                        cx.emit(SessionsPaneEvent::Refresh);
                                                    });
                                                }
                                            }),
                                        )
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .id("sessions-list")
                    .role(Role::List)
                    .aria_label("Recent coding sessions")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_token(DesignSpace::Md)
                    .flex()
                    .flex_col()
                    .gap_token(DesignSpace::Sm)
                    .child(
                        div()
                            .id("sessions-search")
                            .role(Role::Search)
                            .aria_label("Search sessions")
                            .child(
                                Input::new(&search_input)
                                    .role(Role::SearchInput)
                                    .appearance(false),
                            ),
                    )
                    .children(session_rows)
                    .when(
                        !search.is_empty()
                            && !view_model.catalog.iter().any(|session| {
                                session.session_id.to_lowercase().contains(&search)
                                    || session.updated_at.to_lowercase().contains(&search)
                            }),
                        |panel| {
                            panel.child(
                                div()
                                    .p_token(DesignSpace::Sm)
                                    .text_color(rgb(theme.muted_text.value()))
                                    .child("No matching sessions."),
                            )
                        },
                    )
                    .when(omitted_sessions > 0, |panel| {
                        panel.child(
                            div()
                                .text_token(DesignText::Body)
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
