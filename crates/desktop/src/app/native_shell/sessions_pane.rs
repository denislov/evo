use desktop::runtime::DesktopSessionCatalogEntry;
use desktop::shell::{SESSION_PANEL_WIDTH, SemanticTheme, truncate_label};
use gpui::{
    EventEmitter, FocusHandle, IntoElement, ParentElement as _, Render, Role, Styled as _,
    Subscription, Window, div, prelude::*, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{
    Disableable as _, Icon, Sizable as _,
    menu::{DropdownMenu as _, PopupMenuItem},
};
use std::sync::Arc;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{
    desktop_controls::{
        DesktopActionRow, DesktopControlSize, DesktopIcon, DesktopIconButton, DesktopRowState,
    },
    desktop_style::{DesignSpace, DesignText, DesktopStyledExt as _},
    semantic_status_color,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SessionsPaneEvent {
    Create,
    Refresh,
    Open(String),
    CloseSession(String),
    Dismiss,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionRuntimeState {
    pub(super) session_id: Arc<str>,
    pub(super) status: desktop::shell::SemanticStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionsPaneViewModel {
    pub(super) panel_width: u32,
    pub(super) catalog: Arc<[DesktopSessionCatalogEntry]>,
    pub(super) omitted_sessions: usize,
    pub(super) active_session_id: Arc<str>,
    pub(super) runtime_states: Arc<[SessionRuntimeState]>,
    pub(super) workspace_limit_reached: bool,
    pub(super) composer_running: bool,
    pub(super) awaiting_prompt_start: bool,
    pub(super) session_pending: bool,
    pub(super) session_catalog_pending: bool,
    pub(super) active_status: desktop::shell::SemanticStatus,
    pub(super) keyboard_focus_visible: bool,
    pub(super) context_is_overlay: bool,
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
        let context_is_overlay = view_model.context_is_overlay;
        let search_input = self.search_input.clone();
        let clear_search_input = search_input.clone();
        let search = search_input.read(cx).value().trim().to_lowercase();
        let omitted_sessions = view_model.omitted_sessions;
        let focused = self.focus.is_focused(window) && view_model.keyboard_focus_visible;
        let active_semantic_status = view_model.active_status;
        let runtime_states = Arc::clone(&view_model.runtime_states);
        let refresh_target = cx.entity().downgrade();
        let now = OffsetDateTime::now_utc();
        let visible_session_count = view_model
            .catalog
            .iter()
            .filter(|session| {
                search.is_empty()
                    || session.session_id.to_lowercase().contains(&search)
                    || session
                        .name
                        .as_deref()
                        .is_some_and(|name| name.to_lowercase().contains(&search))
                    || session
                        .cwd
                        .as_deref()
                        .is_some_and(|cwd| cwd.to_lowercase().contains(&search))
                    || session.updated_at.to_lowercase().contains(&search)
            })
            .count();
        let session_rows = view_model
            .catalog
            .iter()
            .filter(|session| {
                search.is_empty()
                    || session.session_id.to_lowercase().contains(&search)
                    || session
                        .name
                        .as_deref()
                        .is_some_and(|name| name.to_lowercase().contains(&search))
                    || session
                        .cwd
                        .as_deref()
                        .is_some_and(|cwd| cwd.to_lowercase().contains(&search))
                    || session.updated_at.to_lowercase().contains(&search)
            })
            .enumerate()
            .map(|(index, session)| {
                let target = session.session_id.clone();
                let active = target == active_session_id;
                let semantic_name = session
                    .name
                    .as_deref()
                    .map(|name| truncate_label(name, 24))
                    .unwrap_or_else(|| {
                        if active {
                            "Current task".to_owned()
                        } else {
                            truncate_label(&target, 24)
                        }
                    });
                let relative_time = relative_session_time(&session.updated_at, now);
                let row_status = runtime_states
                    .iter()
                    .find(|state| state.session_id.as_ref() == target)
                    .map(|state| state.status);
                let (status_glyph, status, status_color) = if active || row_status.is_some() {
                    let semantic_status = if active {
                        active_semantic_status
                    } else {
                        row_status.unwrap_or(desktop::shell::SemanticStatus::Idle)
                    };
                    let label = semantic_status.label();
                    (
                        semantic_status.glyph(),
                        if active && label == "Idle" {
                            "current".to_owned()
                        } else {
                            label.to_lowercase()
                        },
                        semantic_status_color(semantic_status),
                    )
                } else {
                    ("○", "available".to_owned(), rgb(theme.muted_text.value()))
                };
                let accessible_label =
                    format!("{semantic_name}, {status}, updated {relative_time}");
                let close_label = format!("Close {semantic_name}");
                let row =
                    DesktopActionRow::new(("session-row", index), semantic_name, accessible_label)
                        .state(DesktopRowState {
                            selected: active,
                            disabled: active
                                || composer_running
                                || awaiting_prompt_start
                                || session_pending,
                            focus_visible: false,
                        })
                        .size(DesktopControlSize::Critical)
                        .leading(div().text_color(status_color).child(status_glyph));
                // The docked panel is intentionally compact: preserve the
                // primary session name and close action. The wide overlay has
                // enough room to add cwd metadata and relative time.
                let row = if context_is_overlay {
                    row.detail(format!(
                        "{} · {status}",
                        session
                            .cwd
                            .as_deref()
                            .map(|cwd| truncate_label(cwd, 28))
                            .unwrap_or_else(|| truncate_label(&target, 28))
                    ))
                    .trailing(
                        div()
                            .w(px(60.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_token(DesignText::Metadata)
                            .text_color(rgb(theme.muted_text.value()))
                            .child(relative_time),
                        60.,
                    )
                } else {
                    row
                };
                let row = row
                    .build(theme)
                    .debug_selector(move || format!("desktop-session-row-{index}"))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(SessionsPaneEvent::Open(target.clone()));
                    }));
                let close_target = session.session_id.clone();
                div()
                    .w_full()
                    .min_w_0()
                    .h(px(DesktopControlSize::Critical.pixels()))
                    .flex()
                    .items_center()
                    .gap_token(DesignSpace::Xs)
                    .child(div().flex_1().min_w_0().child(row))
                    // A GPUI Button cannot contain another Button. Keep the
                    // trailing tool as a fixed-width sibling. The docked row
                    // reserves 36 px; the overlay additionally reserves 60 px
                    // for time, keeping both responsive layouts stable.
                    .child(
                        DesktopIconButton::new(
                            ("close-session", index),
                            DesktopIcon::Close,
                            close_label,
                        )
                        .size(DesktopControlSize::Tool)
                        .build()
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(SessionsPaneEvent::CloseSession(close_target.clone()));
                        })),
                    )
            })
            .collect::<Vec<_>>();
        let empty_state = if visible_session_count > 0 {
            None
        } else if session_catalog_pending && view_model.catalog.is_empty() {
            Some("Loading sessions…".to_owned())
        } else if !search.is_empty() {
            Some(format!(
                "No sessions match “{}”.",
                truncate_label(&search, 24)
            ))
        } else {
            Some("No recent sessions yet. Create one to begin.".to_owned())
        };

        div()
            .id("sessions-panel")
            .role(Role::Navigation)
            .aria_label("Sessions")
            .debug_selector(|| "desktop-sessions-panel".into())
            .track_focus(&self.focus)
            .when(context_is_overlay, |panel| panel.w_full())
            .when(!context_is_overlay, |panel| panel.w(px(panel_width as f32)))
            .h_full()
            .flex()
            .flex_col()
            .when(!context_is_overlay, |panel| panel.border_r_1())
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
                                DesktopIconButton::new(
                                    "create-session",
                                    DesktopIcon::Plus,
                                    if view_model.workspace_limit_reached {
                                        "Session limit reached · close one first"
                                    } else {
                                        "Create a new session · Ctrl/Cmd+N"
                                    },
                                )
                                .build()
                                .debug_selector(|| "desktop-hit-create-session".into())
                                .disabled(
                                    composer_running
                                        || awaiting_prompt_start
                                        || session_pending
                                        || view_model.workspace_limit_reached,
                                )
                                .on_click(cx.listener(
                                    |_, _, _, cx| {
                                        cx.emit(SessionsPaneEvent::Create);
                                    },
                                )),
                            )
                            .child(
                                DesktopIconButton::new(
                                    "sessions-overflow",
                                    DesktopIcon::Overflow,
                                    "More Sessions actions",
                                )
                                .busy(session_catalog_pending)
                                .build()
                                .debug_selector(|| "desktop-hit-sessions-overflow".into())
                                .dropdown_menu(
                                    move |menu, _, _| {
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
                                    },
                                ),
                            )
                            .when(context_is_overlay, |actions| {
                                actions.child(
                                    DesktopIconButton::new(
                                        "close-narrow-sessions",
                                        DesktopIcon::Close,
                                        "Close Sessions",
                                    )
                                    .build()
                                    .debug_selector(|| "desktop-hit-close-narrow-sessions".into())
                                    .on_click(cx.listener(
                                        |_, _, _, cx| {
                                            cx.emit(SessionsPaneEvent::Dismiss);
                                        },
                                    )),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .id("sessions-list")
                    .role(Role::List)
                    .aria_label("Recent coding sessions")
                    .w_full()
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
                            .debug_selector(|| "sessions-search".into())
                            .role(Role::Search)
                            .aria_label("Search sessions")
                            .child(
                                Input::new(&search_input)
                                    .role(Role::SearchInput)
                                    .prefix(Icon::new(DesktopIcon::Search.name()).small())
                                    .when(!search.is_empty(), |input| {
                                        input.suffix(
                                            DesktopIconButton::new(
                                                "clear-session-search",
                                                DesktopIcon::Clear,
                                                "Clear session search",
                                            )
                                            .size(DesktopControlSize::Tool)
                                            .build()
                                            .on_click(
                                                move |_, window, cx| {
                                                    clear_search_input.update(cx, |input, cx| {
                                                        input.set_value("", window, cx);
                                                    });
                                                },
                                            ),
                                        )
                                    })
                                    .appearance(false),
                            ),
                    )
                    .children(session_rows)
                    .when_some(empty_state, |panel, message| {
                        panel.child(
                            div()
                                .debug_selector(|| "desktop-sessions-empty-state".into())
                                .p_token(DesignSpace::Sm)
                                .text_color(rgb(theme.muted_text.value()))
                                .child(message),
                        )
                    })
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
