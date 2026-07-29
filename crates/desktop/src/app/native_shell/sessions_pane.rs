use coding_agent::api::embedding::CodingAgentResourceCommand;
use desktop::shell::{SESSION_PANEL_WIDTH, SemanticTheme, truncate_label};
use gpui::{
    EventEmitter, FocusHandle, Focusable as _, IntoElement, ParentElement as _, Render, Role,
    Styled as _, Subscription, Window, div, prelude::*, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{
    Icon, Sizable as _,
    menu::{DropdownMenu as _, PopupMenuItem},
};
use std::sync::Arc;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{
    desktop_controls::{
        DesktopActionRow, DesktopControlSize, DesktopIcon, DesktopIconButton, DesktopRowState,
    },
    desktop_style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
    project_catalog_controller::{
        ProjectCatalogGroup, ProjectCatalogState, session_matches_query, workspace_matches_query,
    },
    semantic_status_color,
};

const MAX_SESSIONS_PANE_SKILLS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SessionsPaneEvent {
    NewConversation,
    Refresh,
    Open(String),
    Rename(String, String),
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
    pub(super) project_groups: Arc<[ProjectCatalogGroup]>,
    pub(super) omitted_sessions: usize,
    pub(super) catalog_state: ProjectCatalogState,
    pub(super) global_skills: Arc<[CodingAgentResourceCommand]>,
    pub(super) active_session_id: Arc<str>,
    pub(super) runtime_states: Arc<[SessionRuntimeState]>,
    pub(super) composer_running: bool,
    pub(super) awaiting_prompt_start: bool,
    pub(super) session_pending: bool,
    pub(super) active_status: desktop::shell::SemanticStatus,
    pub(super) keyboard_focus_visible: bool,
    pub(super) presented_as_drawer: bool,
}

pub(super) struct SessionsPane {
    focus: FocusHandle,
    search_input: gpui::Entity<InputState>,
    rename_input: gpui::Entity<InputState>,
    renaming_session_id: Option<String>,
    view_model: Option<SessionsPaneViewModel>,
    _search_subscription: Subscription,
    _rename_subscription: Subscription,
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
        let rename_input = cx.new(|cx| InputState::new(window, cx).placeholder("Session name"));
        let rename_subscription = cx.subscribe_in(
            &rename_input,
            window,
            |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => {
                    if let Some(session_id) = this.renaming_session_id.take() {
                        cx.emit(SessionsPaneEvent::Rename(
                            session_id,
                            input.read(cx).value().to_string(),
                        ));
                        cx.notify();
                    }
                }
                _ => {}
            },
        );
        Self {
            focus,
            search_input,
            rename_input,
            renaming_session_id: None,
            view_model: None,
            _search_subscription: search_subscription,
            _rename_subscription: rename_subscription,
        }
    }

    pub(super) fn set_view_model(&mut self, view_model: SessionsPaneViewModel) {
        self.view_model = Some(view_model);
    }

    fn begin_rename(
        &mut self,
        session_id: String,
        current_name: Option<String>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        self.renaming_session_id = Some(session_id);
        self.rename_input.update(cx, |input, cx| {
            input.set_value(current_name.unwrap_or_default(), window, cx)
        });
        self.rename_input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn commit_rename(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(session_id) = self.renaming_session_id.take() else {
            return;
        };
        let name = self.rename_input.read(cx).value().to_string();
        cx.emit(SessionsPaneEvent::Rename(session_id, name));
        cx.notify();
    }

    fn cancel_rename(&mut self, cx: &mut gpui::Context<Self>) {
        self.renaming_session_id = None;
        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn set_search_value(
        &mut self,
        value: &str,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        self.search_input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }

    #[cfg(test)]
    pub(super) fn set_rename_value(
        &mut self,
        value: &str,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        self.rename_input
            .update(cx, |input, cx| input.set_value(value, window, cx));
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
        let session_catalog_pending = view_model.catalog_state.is_loading();
        let presented_as_drawer = view_model.presented_as_drawer;
        let search_input = self.search_input.clone();
        let clear_search_input = search_input.clone();
        let rename_input = self.rename_input.clone();
        let renaming_session_id = self.renaming_session_id.clone();
        let search = search_input.read(cx).value().trim().to_lowercase();
        let omitted_sessions = view_model.omitted_sessions;
        let omitted_skills = view_model
            .global_skills
            .len()
            .saturating_sub(MAX_SESSIONS_PANE_SKILLS);
        let focused = self.focus.is_focused(window) && view_model.keyboard_focus_visible;
        let active_semantic_status = view_model.active_status;
        let runtime_states = Arc::clone(&view_model.runtime_states);
        let refresh_target = cx.entity().downgrade();
        let now = OffsetDateTime::now_utc();
        let catalog_rows = view_model
            .project_groups
            .iter()
            .flat_map(|group| {
                group
                    .sessions
                    .iter()
                    .map(move |session| (&group.workspace, session))
            })
            .filter(|(workspace, session)| {
                search.is_empty()
                    || workspace_matches_query(workspace, &search)
                    || session_matches_query(session, &search)
            })
            .collect::<Vec<_>>();
        let visible_session_count = catalog_rows.len();
        let session_rows = catalog_rows
            .into_iter()
            .enumerate()
            .map(|(index, (_, session))| {
                let target = session.session_id.clone();
                let active = target == active_session_id;
                let semantic_name = session
                    .name
                    .as_deref()
                    .map(|name| truncate_label(name, 24))
                    .unwrap_or_else(|| "Untitled".to_owned());
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
                let row = DesktopActionRow::new(
                    ("session-row", index),
                    semantic_name.clone(),
                    accessible_label,
                )
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
                let row = if presented_as_drawer {
                    row.detail(format!("{} · {status}", truncate_label(&target, 28)))
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
                    row.detail(truncate_label(&target, 14))
                };
                let row = row
                    .build(theme)
                    .debug_selector(move || format!("desktop-session-row-{index}"))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(SessionsPaneEvent::Open(target.clone()));
                    }));
                let close_target = session.session_id.clone();
                let rename_target = session.session_id.clone();
                let rename_name = session.name.clone();
                let rename_event_target = cx.entity().downgrade();
                if renaming_session_id.as_deref() == Some(session.session_id.as_str()) {
                    return div()
                        .debug_selector(move || format!("desktop-session-rename-{index}"))
                        .w_full()
                        .h(px(DesktopControlSize::Critical.pixels()))
                        .flex()
                        .items_center()
                        .gap_token(DesignSpace::Xs)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&rename_input).appearance(false)),
                        )
                        .child(
                            DesktopIconButton::new(
                                ("commit-session-rename", index),
                                DesktopIcon::Submit,
                                "Save session name",
                            )
                            .build()
                            .debug_selector(move || {
                                format!("desktop-hit-commit-session-rename-{index}")
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.commit_rename(cx))),
                        )
                        .child(
                            DesktopIconButton::new(
                                ("cancel-session-rename", index),
                                DesktopIcon::Close,
                                "Cancel session rename",
                            )
                            .build()
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_rename(cx))),
                        )
                        .into_any_element();
                }
                div()
                    .w_full()
                    .min_w_0()
                    .h(px(DesktopControlSize::Critical.pixels()))
                    .flex()
                    .items_center()
                    .gap_token(DesignSpace::Xs)
                    .child(div().flex_1().min_w_0().child(row))
                    .child(
                        DesktopIconButton::new(
                            ("rename-session", index),
                            DesktopIcon::Overflow,
                            format!("Rename {semantic_name}"),
                        )
                        .size(DesktopControlSize::Tool)
                        .build()
                        .debug_selector(move || format!("desktop-hit-rename-session-{index}"))
                        .dropdown_menu(move |menu, _, _| {
                            let event_target = rename_event_target.clone();
                            let target = rename_target.clone();
                            let name = rename_name.clone();
                            menu.item(PopupMenuItem::new("Rename session").on_click(
                                move |_, window, cx| {
                                    if let Some(event_target) = event_target.upgrade() {
                                        event_target.update(cx, |pane, cx| {
                                            pane.begin_rename(
                                                target.clone(),
                                                name.clone(),
                                                window,
                                                cx,
                                            )
                                        });
                                    }
                                },
                            ))
                        }),
                    )
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
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let skill_rows = view_model
            .global_skills
            .iter()
            .take(MAX_SESSIONS_PANE_SKILLS)
            .enumerate()
            .map(|(index, skill)| {
                div()
                    .id(("sessions-skill", index))
                    .debug_selector(move || format!("desktop-sessions-skill-{index}"))
                    .role(Role::ListItem)
                    .px_token(DesignSpace::Md)
                    .py_token(DesignSpace::Sm)
                    .rounded_token(DesignRadius::Sm)
                    .border_1()
                    .border_color(rgb(theme.divider.value()))
                    .child(
                        div()
                            .text_token(DesignText::Body)
                            .child(format!("/{}", truncate_label(&skill.name, 24))),
                    )
                    .child(
                        div()
                            .mt_token(DesignSpace::Xs)
                            .text_token(DesignText::Metadata)
                            .text_color(rgb(theme.muted_text.value()))
                            .child(truncate_label(&skill.description, 52)),
                    )
            })
            .collect::<Vec<_>>();
        let empty_state = if visible_session_count > 0 {
            None
        } else if session_catalog_pending && view_model.project_groups.is_empty() {
            Some("Loading sessions…".to_owned())
        } else if !search.is_empty() {
            Some(format!(
                "No sessions match “{}”.",
                truncate_label(&search, 24)
            ))
        } else {
            Some(match &view_model.catalog_state {
                ProjectCatalogState::NotLoaded => {
                    "Refresh to load projects and session history.".to_owned()
                }
                ProjectCatalogState::Error { message } => format!(
                    "Session history unavailable: {}. Use Refresh to retry.",
                    truncate_label(
                        view_model.catalog_state.error_message().unwrap_or(message),
                        72
                    )
                ),
                ProjectCatalogState::Stale {
                    error: Some(message),
                } => format!(
                    "Session history is stale: {}. Use Refresh to retry.",
                    truncate_label(
                        view_model.catalog_state.error_message().unwrap_or(message),
                        72
                    )
                ),
                ProjectCatalogState::Loading
                | ProjectCatalogState::Ready
                | ProjectCatalogState::Stale { error: None } => {
                    "No recent sessions yet. Create one to begin.".to_owned()
                }
            })
        };
        let new_conversation_row = DesktopActionRow::new(
            "new-conversation",
            "New conversation",
            "Open the new conversation home without creating a session",
        )
        .state(DesktopRowState {
            selected: active_session_id.is_empty(),
            disabled: false,
            focus_visible: false,
        })
        .size(DesktopControlSize::Critical)
        .leading(Icon::new(DesktopIcon::Plus.name()).small());
        let new_conversation_row = if presented_as_drawer {
            new_conversation_row.detail("Start from Home")
        } else {
            new_conversation_row
        };

        div()
            .id("sessions-panel")
            .role(Role::Navigation)
            .aria_label("Sessions")
            .debug_selector(|| "desktop-sessions-panel".into())
            .track_focus(&self.focus)
            .when(presented_as_drawer, |panel| panel.w_full())
            .when(!presented_as_drawer, |panel| {
                panel.w(px(panel_width as f32))
            })
            .h_full()
            .flex()
            .flex_col()
            .when(!presented_as_drawer, |panel| panel.border_r_1())
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
                            .when(presented_as_drawer, |actions| {
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
                    .aria_label("New conversation, global skills, and session history")
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_token(DesignSpace::Md)
                    .flex()
                    .flex_col()
                    .gap_token(DesignSpace::Lg)
                    .child(
                        div()
                            .id("new-conversation-section")
                            .debug_selector(|| "desktop-new-conversation-section".into())
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap_token(DesignSpace::Sm)
                            .child(
                                div()
                                    .text_token(DesignText::Metadata)
                                    .text_color(rgb(theme.muted_text.value()))
                                    .child("NEW CONVERSATION"),
                            )
                            .child(
                                new_conversation_row
                                    .build(theme)
                                    .debug_selector(|| "desktop-hit-new-conversation".into())
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(SessionsPaneEvent::NewConversation);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .id("global-skills-section")
                            .debug_selector(|| "desktop-global-skills-section".into())
                            .role(Role::List)
                            .aria_label("Global skills")
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap_token(DesignSpace::Sm)
                            .border_t_1()
                            .border_color(rgb(theme.divider.value()))
                            .py_token(DesignSpace::Lg)
                            .child(
                                div()
                                    .text_token(DesignText::Metadata)
                                    .text_color(rgb(theme.muted_text.value()))
                                    .child("GLOBAL SKILLS"),
                            )
                            .children(skill_rows)
                            .when(view_model.global_skills.is_empty(), |section| {
                                section.child(
                                    div()
                                        .p_token(DesignSpace::Sm)
                                        .text_color(rgb(theme.muted_text.value()))
                                        .child("No global skills installed."),
                                )
                            })
                            .when(omitted_skills > 0, |section| {
                                section.child(
                                    div()
                                        .text_token(DesignText::Metadata)
                                        .text_color(rgb(theme.muted_text.value()))
                                        .child(format!("+ {omitted_skills} more global skill(s)")),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("session-history-section")
                            .debug_selector(|| "desktop-session-history-section".into())
                            .role(Role::List)
                            .aria_label("Session history")
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap_token(DesignSpace::Sm)
                            .border_t_1()
                            .border_color(rgb(theme.divider.value()))
                            .py_token(DesignSpace::Lg)
                            .child(
                                div()
                                    .text_token(DesignText::Metadata)
                                    .text_color(rgb(theme.muted_text.value()))
                                    .child("HISTORY"),
                            )
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
                                                    .on_click(move |_, window, cx| {
                                                        clear_search_input.update(
                                                            cx,
                                                            |input, cx| {
                                                                input.set_value("", window, cx);
                                                            },
                                                        );
                                                    }),
                                                )
                                            })
                                            .appearance(false),
                                    ),
                            )
                            .children(session_rows)
                            .when_some(empty_state, |section, message| {
                                section.child(
                                    div()
                                        .debug_selector(|| "desktop-sessions-empty-state".into())
                                        .p_token(DesignSpace::Sm)
                                        .text_color(rgb(theme.muted_text.value()))
                                        .child(message),
                                )
                            })
                            .when(omitted_sessions > 0, |section| {
                                section.child(
                                    div()
                                        .text_token(DesignText::Body)
                                        .text_color(rgb(theme.warning.value()))
                                        .child(format!(
                                            "+ {omitted_sessions} older session(s) omitted"
                                        )),
                                )
                            }),
                    ),
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
