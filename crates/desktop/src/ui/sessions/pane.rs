use coding_agent::api::view::CodingAgentWorkspaceKind;
use desktop::ui::shell::{SESSION_PANEL_WIDTH, SemanticStatus, SemanticTheme, truncate_label};
use gpui::{
    EventEmitter, FocusHandle, Focusable as _, IntoElement, KeyDownEvent, ParentElement as _,
    Render, Role, Styled as _, Subscription, Window, div, prelude::*, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{
    Icon, Sizable as _,
    menu::{DropdownMenu as _, PopupMenuItem},
};
use std::{path::PathBuf, sync::Arc};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::app::native_shell::{NativeDesktopState, semantic_status_color};
use crate::application::catalog::{ProjectCatalogGroup, ProjectCatalogState};
use crate::application::{commands::DesktopCommandIntent, workspace::WorkspaceKey};
use crate::ui::components::{
    brand::{EvoBrand, EvoBrandMode},
    controls::{
        DesktopActionRow, DesktopControlSize, DesktopIcon, DesktopIconButton, DesktopRowState,
    },
    style::{DesignSpace, DesignText, DesktopStyledExt as _},
};
use crate::ui::shell::drawer::CenterDrawerKind;
use crate::ui::shell::{
    CenterNavigationTarget, CenterSurface, ShellUiState, presentation::semantic_status,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionsPaneEvent {
    Navigate(CenterNavigationTarget),
    NewConversationForProject(PathBuf),
    Refresh,
    SetProjectCollapsed { group_id: String, collapsed: bool },
    Rename(String, String),
    CloseSession(String),
    DeleteSession(String),
    OpenSearch,
    Dismiss,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionRuntimeState {
    pub(crate) session_id: Arc<str>,
    pub(crate) status: desktop::ui::shell::SemanticStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionsPaneViewModel {
    pub(crate) panel_width: u32,
    pub(crate) project_groups: Arc<[ProjectCatalogGroup]>,
    pub(crate) omitted_sessions: usize,
    pub(crate) catalog_state: ProjectCatalogState,
    pub(crate) active_session_id: Arc<str>,
    pub(crate) skills_active: bool,
    pub(crate) runtime_states: Arc<[SessionRuntimeState]>,
    pub(crate) composer_running: bool,
    pub(crate) awaiting_prompt_start: bool,
    pub(crate) session_pending: bool,
    pub(crate) home_project_directory_editable: bool,
    pub(crate) active_status: desktop::ui::shell::SemanticStatus,
    pub(crate) keyboard_focus_visible: bool,
    pub(crate) presented_as_drawer: bool,
    pub(crate) reduced_motion: bool,
}

pub(crate) fn view_model(app: &NativeDesktopState, ui: &ShellUiState) -> SessionsPaneViewModel {
    let workspace = app.workspaces.active();
    let snapshot = workspace
        .projection
        .as_ref()
        .map(|projection| projection.snapshot());
    let composer_running = snapshot.is_some_and(|snapshot| snapshot.active_operation.is_some());
    let mut runtime_states = app
        .workspaces
        .iter()
        .filter_map(|(key, workspace)| {
            let WorkspaceKey::Session(session_id) = key else {
                return None;
            };
            workspace.projection.as_ref()?;
            Some(SessionRuntimeState {
                session_id: Arc::from(session_id.as_str()),
                status: semantic_status(workspace.projection.as_ref()),
            })
        })
        .collect::<Vec<_>>();
    runtime_states.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    SessionsPaneViewModel {
        panel_width: app.preferences.sessions_panel_width,
        project_groups: Arc::from(app.catalog.project_groups()),
        omitted_sessions: app.catalog.omitted(),
        catalog_state: app.catalog.state().clone(),
        active_session_id: Arc::from(
            snapshot
                .map(|snapshot| snapshot.session.session_id.as_str())
                .unwrap_or_default(),
        ),
        skills_active: ui.center_surface == CenterSurface::Skills,
        runtime_states: Arc::from(runtime_states),
        composer_running,
        awaiting_prompt_start: workspace.composer.submitted().is_some() && !composer_running,
        session_pending: app.commands.contains_anywhere(|intent| {
            matches!(
                intent,
                DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
            )
        }),
        home_project_directory_editable: app
            .workspaces
            .get(&WorkspaceKey::Home)
            .is_some_and(|workspace| workspace.project_directory_editable()),
        active_status: semantic_status(workspace.projection.as_ref()),
        keyboard_focus_visible: ui.keyboard_focus_visible(),
        presented_as_drawer: ui.active_drawer == Some(CenterDrawerKind::Sessions),
        reduced_motion: app.preferences.reduced_motion,
    }
}

pub(crate) struct SessionsPane {
    focus: FocusHandle,
    rename_input: gpui::Entity<InputState>,
    renaming_session_id: Option<String>,
    view_model: Option<SessionsPaneViewModel>,
    _rename_subscription: Subscription,
}

impl SessionsPane {
    pub(crate) fn new(
        focus: FocusHandle,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
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
            rename_input,
            renaming_session_id: None,
            view_model: None,
            _rename_subscription: rename_subscription,
        }
    }

    pub(crate) fn set_view_model(&mut self, view_model: SessionsPaneViewModel) {
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
    pub(crate) fn set_rename_value(
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

fn semantic_status_priority(status: SemanticStatus) -> u8 {
    match status {
        SemanticStatus::Error => 5,
        SemanticStatus::Authorization => 4,
        SemanticStatus::Running => 3,
        SemanticStatus::Warning => 2,
        SemanticStatus::Idle => 1,
    }
}

fn runtime_status_label(status: Option<SemanticStatus>, contains_active: bool) -> String {
    match status {
        Some(SemanticStatus::Idle) if contains_active => "current".into(),
        Some(status) => status.label().to_lowercase(),
        None => "available".into(),
    }
}

fn session_runtime_status(
    session_id: &str,
    active_session_id: &str,
    active_status: SemanticStatus,
    runtime_states: &[SessionRuntimeState],
) -> Option<SemanticStatus> {
    if session_id == active_session_id {
        return Some(active_status);
    }
    runtime_states
        .iter()
        .find(|state| state.session_id.as_ref() == session_id)
        .map(|state| state.status)
}

fn project_runtime_summary(
    group: &ProjectCatalogGroup,
    active_session_id: &str,
    active_status: SemanticStatus,
    runtime_states: &[SessionRuntimeState],
) -> (Option<SemanticStatus>, bool) {
    let contains_active = group
        .sessions
        .iter()
        .any(|session| session.session_id == active_session_id);
    let status = group
        .sessions
        .iter()
        .filter_map(|session| {
            session_runtime_status(
                &session.session_id,
                active_session_id,
                active_status,
                runtime_states,
            )
        })
        .max_by_key(|status| semantic_status_priority(*status));
    (status, contains_active)
}

fn project_title(group: &ProjectCatalogGroup) -> String {
    match group.workspace.kind {
        CodingAgentWorkspaceKind::Projectless => "无项目".into(),
        CodingAgentWorkspaceKind::Legacy if group.workspace.display_name.trim().is_empty() => {
            "Legacy sessions".into()
        }
        CodingAgentWorkspaceKind::Project | CodingAgentWorkspaceKind::Legacy => {
            group.workspace.display_name.clone()
        }
    }
}

fn count_label(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

fn is_keyboard_activation(event: &KeyDownEvent) -> bool {
    matches!(event.keystroke.key.as_str(), "enter" | "space")
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
        let theme = SemanticTheme::current(cx);
        let active_session_id = view_model.active_session_id.as_ref();
        let composer_running = view_model.composer_running;
        let awaiting_prompt_start = view_model.awaiting_prompt_start;
        let session_pending = view_model.session_pending;
        let session_catalog_pending = view_model.catalog_state.is_loading();
        let presented_as_drawer = view_model.presented_as_drawer;
        let rename_input = self.rename_input.clone();
        let renaming_session_id = self.renaming_session_id.clone();
        let omitted_sessions = view_model.omitted_sessions;
        let home_project_directory_editable = view_model.home_project_directory_editable;
        let focused = self.focus.is_focused(window) && view_model.keyboard_focus_visible;
        let active_semantic_status = view_model.active_status;
        let runtime_states = Arc::clone(&view_model.runtime_states);
        let refresh_target = cx.entity().downgrade();
        let now = OffsetDateTime::now_utc();
        let visible_groups = view_model.project_groups.to_vec();
        let visible_project_count = visible_groups
            .iter()
            .filter(|group| group.workspace.kind != CodingAgentWorkspaceKind::Projectless)
            .count();
        let visible_conversation_count = visible_groups
            .iter()
            .filter(|group| group.workspace.kind == CodingAgentWorkspaceKind::Projectless)
            .map(|group| group.sessions.len())
            .sum::<usize>();
        let visible_project_session_count = visible_groups
            .iter()
            .filter(|group| group.workspace.kind != CodingAgentWorkspaceKind::Projectless)
            .map(|group| group.sessions.len())
            .sum::<usize>();
        let mut session_index = 0usize;
        let catalog_group_elements = visible_groups
            .into_iter()
            .enumerate()
            .map(|(group_index, group)| {
                let is_conversations =
                    group.workspace.kind == CodingAgentWorkspaceKind::Projectless;
                let new_project_path = (group.workspace.kind == CodingAgentWorkspaceKind::Project)
                    .then(|| group.workspace.display_path.clone())
                    .flatten();
                let group_id = group.workspace.group_id.clone();
                let expanded = !group.collapsed;
                let title = project_title(&group);
                let (project_status, contains_active) = project_runtime_summary(
                    &group,
                    active_session_id,
                    active_semantic_status,
                    &runtime_states,
                );
                let project_status_label = runtime_status_label(project_status, contains_active);
                let project_status_color = project_status.map_or_else(
                    || rgb(theme.muted_text.value()),
                    |status| semantic_status_color(status, theme),
                );
                let scope_detail = match group.workspace.kind {
                    CodingAgentWorkspaceKind::Project => group
                        .workspace
                        .display_path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "Project path unavailable".into()),
                    CodingAgentWorkspaceKind::Projectless => "Global config only".into(),
                    CodingAgentWorkspaceKind::Legacy => "Legacy scope · migration required".into(),
                };
                let session_count_label = count_label(group.sessions.len(), "session");
                let project_detail =
                    format!("{scope_detail} · {session_count_label} · {project_status_label}");
                let project_accessible_label = format!(
                    "{title}, {project_detail}, {}",
                    if expanded { "expanded" } else { "collapsed" }
                );
                let project_icon = if expanded {
                    DesktopIcon::ProjectDirectoryOpen
                } else {
                    DesktopIcon::ProjectDirectoryClosed
                };
                let keyboard_group_id = group_id.clone();
                let project_row = DesktopActionRow::new(
                    ("project-group", group_index),
                    truncate_label(&title, 24),
                    project_accessible_label,
                )
                .size(DesktopControlSize::Critical)
                .expanded(expanded)
                .leading(
                    div().flex().items_center().child(
                        div()
                            .text_color(project_status_color)
                            .child(Icon::new(project_icon.name()).small()),
                    ),
                )
                .detail(project_detail)
                .build(theme)
                .debug_selector(move || format!("desktop-project-row-{group_index}"))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(SessionsPaneEvent::SetProjectCollapsed {
                        group_id: group_id.clone(),
                        collapsed: expanded,
                    });
                }))
                .on_key_down(cx.listener(
                    move |_, event: &KeyDownEvent, window, cx| {
                        if !is_keyboard_activation(event) {
                            return;
                        }
                        window.prevent_default();
                        cx.stop_propagation();
                        cx.emit(SessionsPaneEvent::SetProjectCollapsed {
                            group_id: keyboard_group_id.clone(),
                            collapsed: expanded,
                        });
                    },
                ));

                let mut nested_sessions = Vec::with_capacity(group.sessions.len());
                let group_session_start = session_index;
                session_index = session_index.saturating_add(group.sessions.len());
                if expanded {
                    for (group_session_index, session) in group.sessions.iter().enumerate() {
                        let index = group_session_start.saturating_add(group_session_index);
                        let target = session.session_id.clone();
                        let active = target == active_session_id;
                        let selected = active && !view_model.skills_active;
                        let semantic_name = session
                            .name
                            .as_deref()
                            .map(|name| truncate_label(name, 24))
                            .unwrap_or_else(|| "Untitled".to_owned());
                        let relative_time = relative_session_time(&session.updated_at, now);
                        let row_status = session_runtime_status(
                            &target,
                            active_session_id,
                            active_semantic_status,
                            &runtime_states,
                        );
                        let status = if active || row_status.is_some() {
                            let semantic_status = if active {
                                active_semantic_status
                            } else {
                                row_status.unwrap_or(desktop::ui::shell::SemanticStatus::Idle)
                            };
                            runtime_status_label(Some(semantic_status), active)
                        } else {
                            runtime_status_label(None, false)
                        };
                        let accessible_label =
                            format!("{semantic_name}, {status}, updated {relative_time}");
                        let row = DesktopActionRow::new(
                            ("session-row", index),
                            semantic_name.clone(),
                            accessible_label,
                        )
                        .state(DesktopRowState {
                            selected,
                            disabled: selected
                                || composer_running
                                || awaiting_prompt_start
                                || session_pending,
                            focus_visible: false,
                        })
                        .size(DesktopControlSize::Critical)
                        .selection_background_only();
                        // The docked tree keeps status and time visible; the drawer
                        // additionally exposes the full session identity.
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
                            row.detail(format!("{status} · {relative_time}"))
                        };
                        let keyboard_target = target.clone();
                        let row = row
                            .build(theme)
                            .debug_selector(move || format!("desktop-session-row-{index}"))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(SessionsPaneEvent::Navigate(
                                    CenterNavigationTarget::Session(target.clone()),
                                ));
                            }))
                            .on_key_down(cx.listener(
                                move |_, event: &KeyDownEvent, window, cx| {
                                    if !is_keyboard_activation(event) {
                                        return;
                                    }
                                    window.prevent_default();
                                    cx.stop_propagation();
                                    cx.emit(SessionsPaneEvent::Navigate(
                                        CenterNavigationTarget::Session(keyboard_target.clone()),
                                    ));
                                },
                            ));
                        let rename_target = session.session_id.clone();
                        let rename_name = session.name.clone();
                        let rename_event_target = cx.entity().downgrade();
                        if renaming_session_id.as_deref() == Some(session.session_id.as_str()) {
                            nested_sessions.push(
                                div()
                                    .id(("session-tree-item", index))
                                    .role(Role::ListItem)
                                    .debug_selector(move || {
                                        format!("desktop-session-rename-{index}")
                                    })
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
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.commit_rename(cx)),
                                        ),
                                    )
                                    .child(
                                        DesktopIconButton::new(
                                            ("cancel-session-rename", index),
                                            DesktopIcon::Close,
                                            "Cancel session rename",
                                        )
                                        .build()
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.cancel_rename(cx)),
                                        ),
                                    )
                                    .into_any_element(),
                            );
                            continue;
                        }
                        let session_actions_target = cx.entity().downgrade();
                        let close_target = session.session_id.clone();
                        let delete_target = session.session_id.clone();
                        nested_sessions.push(
                            div()
                                .id(("session-tree-item", index))
                                .role(Role::ListItem)
                                .w_full()
                                .min_w_0()
                                .h(px(DesktopControlSize::Critical.pixels()))
                                .flex()
                                .items_center()
                                .gap_token(DesignSpace::Xs)
                                .child(div().flex_1().min_w_0().child(row))
                                .child(
                                    DesktopIconButton::new(
                                        ("session-actions", index),
                                        DesktopIcon::Overflow,
                                        format!("More actions for {semantic_name}"),
                                    )
                                    .size(DesktopControlSize::Compact)
                                    .build()
                                    .debug_selector(move || {
                                        format!("desktop-hit-session-actions-{index}")
                                    })
                                    .dropdown_menu(
                                        move |menu, _, _| {
                                            let event_target = rename_event_target.clone();
                                            let target = rename_target.clone();
                                            let name = rename_name.clone();
                                            let close_event_target = session_actions_target.clone();
                                            let close_target = close_target.clone();
                                            let delete_event_target =
                                                session_actions_target.clone();
                                            let delete_target = delete_target.clone();
                                            menu.item(
                                                PopupMenuItem::new("Rename session").on_click(
                                                    move |_, window, cx| {
                                                        if let Some(event_target) =
                                                            event_target.upgrade()
                                                        {
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
                                                ),
                                            )
                                            .item(PopupMenuItem::new("Close session").on_click(
                                                move |_, _, cx| {
                                                    if let Some(event_target) =
                                                        close_event_target.upgrade()
                                                    {
                                                        event_target.update(cx, |_, cx| {
                                                            cx.emit(
                                                                SessionsPaneEvent::CloseSession(
                                                                    close_target.clone(),
                                                                ),
                                                            );
                                                        });
                                                    }
                                                },
                                            ))
                                            .item(
                                                PopupMenuItem::new("Delete session").on_click(
                                                    move |_, _, cx| {
                                                        if let Some(event_target) =
                                                            delete_event_target.upgrade()
                                                        {
                                                            event_target.update(cx, |_, cx| {
                                                                cx.emit(
                                                                SessionsPaneEvent::DeleteSession(
                                                                    delete_target.clone(),
                                                                ),
                                                            );
                                                            });
                                                        }
                                                    },
                                                ),
                                            )
                                        },
                                    ),
                                )
                                .into_any_element(),
                        );
                    }
                }

                if is_conversations {
                    (
                        true,
                        div()
                            .id("conversation-session-list")
                            .debug_selector(|| "desktop-conversation-sessions".into())
                            .role(Role::List)
                            .aria_label("Conversations without a project directory")
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_token(DesignSpace::Xs)
                            .children(nested_sessions)
                            .into_any_element(),
                    )
                } else {
                    let project_header = div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_token(DesignSpace::Xs)
                        .child(div().flex_1().min_w_0().child(project_row))
                        .when_some(new_project_path, |header, path| {
                            let new_project_title = title.clone();
                            header.child(
                                DesktopIconButton::new(
                                    ("new-project-conversation", group_index),
                                    DesktopIcon::Plus,
                                    format!("Start a new conversation in {new_project_title}"),
                                )
                                .size(DesktopControlSize::Compact)
                                .disabled(!home_project_directory_editable)
                                .build()
                                .debug_selector(move || {
                                    format!("desktop-hit-new-project-conversation-{group_index}")
                                })
                                .on_click(cx.listener(
                                    move |_, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(SessionsPaneEvent::NewConversationForProject(
                                            path.clone(),
                                        ));
                                    },
                                )),
                            )
                        });
                    (
                        false,
                        div()
                            .id(("project-tree-item", group_index))
                            .role(Role::ListItem)
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_token(DesignSpace::Xs)
                            .child(project_header)
                            .when(expanded, |project| {
                                project.child(
                                    div()
                                        .id(("project-session-list", group_index))
                                        .debug_selector(move || {
                                            format!("desktop-project-sessions-{group_index}")
                                        })
                                        .role(Role::List)
                                        .aria_label(format!("Sessions in {title}"))
                                        .w_full()
                                        .min_w_0()
                                        .pl_token(DesignSpace::Lg)
                                        .flex()
                                        .flex_col()
                                        .gap_token(DesignSpace::Xs)
                                        .children(nested_sessions),
                                )
                            })
                            .into_any_element(),
                    )
                }
            })
            .collect::<Vec<_>>();
        let mut conversation_group_elements = Vec::new();
        let mut project_group_elements = Vec::new();
        for (is_conversations, element) in catalog_group_elements {
            if is_conversations {
                conversation_group_elements.push(element);
            } else {
                project_group_elements.push(element);
            }
        }
        let catalog_group_count = view_model
            .project_groups
            .iter()
            .filter(|group| group.workspace.kind != CodingAgentWorkspaceKind::Projectless)
            .count();
        let (catalog_status, catalog_status_color) = match &view_model.catalog_state {
            ProjectCatalogState::NotLoaded => ("Not loaded".to_owned(), theme.muted_text),
            ProjectCatalogState::Loading => ("Loading".to_owned(), theme.accent),
            ProjectCatalogState::Ready => (
                count_label(catalog_group_count, "project"),
                theme.muted_text,
            ),
            ProjectCatalogState::Error { .. } => ("Error".to_owned(), theme.danger),
            ProjectCatalogState::Stale { .. } => ("Stale".to_owned(), theme.warning),
        };
        let catalog_notice = match &view_model.catalog_state {
            ProjectCatalogState::NotLoaded => Some((
                "not-loaded",
                "Projects not loaded".to_owned(),
                Some("Refresh when you want to load project and session history.".into()),
                theme.muted_text,
            )),
            ProjectCatalogState::Loading if view_model.project_groups.is_empty() => Some((
                "loading",
                "Loading projects…".to_owned(),
                Some("The current Home draft remains available.".into()),
                theme.accent,
            )),
            ProjectCatalogState::Loading => Some((
                "loading",
                "Refreshing projects…".to_owned(),
                Some("The previous project tree remains available while loading.".into()),
                theme.accent,
            )),
            ProjectCatalogState::Error { message } => Some((
                "error",
                "Projects unavailable".to_owned(),
                Some(format!(
                    "{}. Use Refresh to retry.",
                    truncate_label(
                        view_model.catalog_state.error_message().unwrap_or(message),
                        72
                    )
                )),
                theme.danger,
            )),
            ProjectCatalogState::Stale {
                error: Some(message),
            } => Some((
                "stale",
                "Project history may be stale".to_owned(),
                Some(format!(
                    "{}. Refresh to reconcile the tree.",
                    truncate_label(
                        view_model.catalog_state.error_message().unwrap_or(message),
                        72
                    )
                )),
                theme.warning,
            )),
            ProjectCatalogState::Stale { error: None } => Some((
                "stale",
                "Project history changed locally".to_owned(),
                Some("Refresh to reconcile with durable history.".into()),
                theme.warning,
            )),
            ProjectCatalogState::Ready if view_model.project_groups.is_empty() => Some((
                "empty",
                "No projects yet".to_owned(),
                Some("Start a conversation to create the first session.".into()),
                theme.muted_text,
            )),
            ProjectCatalogState::Ready => None,
        };
        let new_conversation_row = DesktopActionRow::new(
            "new-conversation",
            "New conversation",
            "Open the new conversation home without creating a session",
        )
        .state(DesktopRowState {
            selected: active_session_id.is_empty() && !view_model.skills_active,
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
        let skills_row = DesktopActionRow::new("skills", "Skills", "Open global skills")
            .state(DesktopRowState {
                selected: view_model.skills_active,
                disabled: false,
                focus_visible: false,
            })
            .size(DesktopControlSize::Critical)
            .leading(div().child("◇"));
        let skills_row = if presented_as_drawer {
            skills_row.detail("Available to every project")
        } else {
            skills_row
        };

        div()
            .id("sessions-panel")
            .role(Role::Navigation)
            .aria_label("Evo workspace navigation")
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
                    .child(
                        div()
                            .debug_selector(|| "desktop-sidebar-evo-mark".into())
                            .flex()
                            .items_center()
                            .gap_token(DesignSpace::Xs)
                            .child(
                                EvoBrand::compact("sidebar-evo-loop", 24., EvoBrandMode::Dark)
                                    .build(),
                            )
                            .child(
                                div()
                                    .text_token(DesignText::Metadata)
                                    .text_color(rgb(theme.muted_text.value()))
                                    .child("workspace"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_token(DesignSpace::Xs)
                            .child(
                                DesktopIconButton::new(
                                    "open-global-search",
                                    DesktopIcon::Search,
                                    "Search sessions",
                                )
                                .build()
                                .debug_selector(|| "desktop-hit-global-search".into())
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(SessionsPaneEvent::OpenSearch);
                                })),
                            )
                            .when(presented_as_drawer, |actions| {
                                actions.child(
                                    DesktopIconButton::new(
                                        "close-narrow-sessions",
                                        DesktopIcon::Close,
                                        "Close workspace navigation",
                                    )
                                    .build()
                                    .debug_selector(|| {
                                        "desktop-hit-close-narrow-sessions".into()
                                    })
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(SessionsPaneEvent::Dismiss);
                                    })),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .id("sessions-list")
                    .aria_label("New conversation, Skills, Conversations, and Projects")
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
                                    .child("WORKSPACE"),
                            )
                            .child(
                                new_conversation_row
                                    .build(theme)
                                    .debug_selector(|| "desktop-hit-new-conversation".into())
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(SessionsPaneEvent::Navigate(
                                            CenterNavigationTarget::NewConversation,
                                        ));
                                    }))
                                    .on_key_down(cx.listener(
                                        |_, event: &KeyDownEvent, window, cx| {
                                            if !is_keyboard_activation(event) {
                                                return;
                                            }
                                            window.prevent_default();
                                            cx.stop_propagation();
                                            cx.emit(SessionsPaneEvent::Navigate(
                                                CenterNavigationTarget::NewConversation,
                                            ));
                                        },
                                    )),
                            )
                            .child(
                                skills_row
                                    .build(theme)
                                    .debug_selector(|| "desktop-hit-skills".into())
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(SessionsPaneEvent::Navigate(
                                            CenterNavigationTarget::Skills,
                                        ));
                                    }))
                                    .on_key_down(cx.listener(
                                        |_, event: &KeyDownEvent, window, cx| {
                                            if !is_keyboard_activation(event) {
                                                return;
                                            }
                                            window.prevent_default();
                                            cx.stop_propagation();
                                            cx.emit(SessionsPaneEvent::Navigate(
                                                CenterNavigationTarget::Skills,
                                            ));
                                        },
                                    )),
                            ),
                    )
                    .when(visible_conversation_count > 0, |list| {
                        list.child(
                            div()
                                .id("conversations-section")
                                .debug_selector(|| "desktop-conversations-section".into())
                                .w_full()
                                .flex()
                                .flex_col()
                                .gap_token(DesignSpace::Sm)
                                .border_t_1()
                                .border_color(rgb(theme.divider.value()))
                                .py_token(DesignSpace::Lg)
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_token(DesignSpace::Xs)
                                        .child(
                                            div()
                                                .text_token(DesignText::Metadata)
                                                .text_color(rgb(theme.muted_text.value()))
                                                .child("CONVERSATIONS"),
                                        )
                                        .child(
                                            div()
                                                .id("conversations-count")
                                                .role(Role::Status)
                                                .aria_label(format!(
                                                    "{visible_conversation_count} conversations without a project directory"
                                                ))
                                                .text_token(DesignText::Metadata)
                                                .text_color(rgb(theme.muted_text.value()))
                                                .child(count_label(
                                                    visible_conversation_count,
                                                    "session",
                                                )),
                                        ),
                                )
                                .children(conversation_group_elements),
                        )
                    })
                    .child(
                        div()
                            .id("projects-section")
                            .debug_selector(|| "desktop-projects-section".into())
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap_token(DesignSpace::Sm)
                            .border_t_1()
                            .border_color(rgb(theme.divider.value()))
                            .py_token(DesignSpace::Lg)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_token(DesignSpace::Sm)
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_token(DesignSpace::Xs)
                                            .child(
                                                div()
                                                    .text_token(DesignText::Metadata)
                                                    .text_color(rgb(theme.muted_text.value()))
                                                    .child("PROJECTS"),
                                            )
                                            .child(
                                                div()
                                                    .id("projects-catalog-status")
                                                    .debug_selector(|| {
                                                        "desktop-projects-status".into()
                                                    })
                                                    .role(Role::Status)
                                                    .aria_label(format!(
                                                        "Project catalog status: {catalog_status}"
                                                    ))
                                                    .text_token(DesignText::Metadata)
                                                    .text_color(rgb(
                                                        catalog_status_color.value(),
                                                    ))
                                                    .child(catalog_status),
                                            ),
                                    )
                                    .child(
                                        DesktopIconButton::new(
                                            "refresh-projects",
                                            DesktopIcon::Refresh,
                                            if session_catalog_pending {
                                                "Loading projects"
                                            } else {
                                                "Refresh projects and sessions"
                                            },
                                        )
                                        .size(DesktopControlSize::Compact)
                                        .busy(session_catalog_pending)
                                        .reduced_motion(view_model.reduced_motion)
                                        .disabled(session_catalog_pending || composer_running)
                                        .build()
                                        .debug_selector(|| {
                                            "desktop-hit-refresh-projects".into()
                                        })
                                        .on_click(move |_, _, cx| {
                                            if let Some(target) = refresh_target.upgrade() {
                                                target.update(cx, |_, cx| {
                                                    cx.emit(SessionsPaneEvent::Refresh);
                                                });
                                            }
                                        }),
                                    ),
                            )
                            .when_some(catalog_notice, |section, notice| {
                                let (selector, title, detail, color) = notice;
                                let debug_selector = format!("desktop-projects-state-{selector}");
                                section.child(
                                    div()
                                        .id("projects-state-notice")
                                        .debug_selector(move || debug_selector.clone())
                                        .role(Role::Status)
                                        .aria_label(detail.as_ref().map_or_else(
                                            || title.clone(),
                                            |detail| format!("{title}. {detail}"),
                                        ))
                                        .p_token(DesignSpace::Sm)
                                        .border_l_2()
                                        .border_color(rgb(color.value()))
                                        .flex()
                                        .flex_col()
                                        .gap_token(DesignSpace::Xs)
                                        .child(
                                            div()
                                                .text_token(DesignText::Body)
                                                .text_color(rgb(theme.text.value()))
                                                .child(title),
                                        )
                                        .when_some(detail, |notice, detail| {
                                            notice.child(
                                                div()
                                                    .text_token(DesignText::Metadata)
                                                    .text_color(rgb(theme.muted_text.value()))
                                                    .child(detail),
                                            )
                                        }),
                                )
                            })
                            .when(visible_project_count > 0, |section| {
                                section.child(
                                    div()
                                        .id("projects-tree")
                                        .debug_selector(|| "desktop-projects-tree".into())
                                        .role(Role::List)
                                        .aria_label(format!(
                                            "{visible_project_count} projects containing {visible_project_session_count} sessions"
                                        ))
                                        .w_full()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_token(DesignSpace::Sm)
                                        .children(project_group_elements),
                                )
                            })
                            .when(omitted_sessions > 0, |section| {
                                section.child(
                                    div()
                                        .id("projects-omitted-notice")
                                        .debug_selector(|| "desktop-projects-state-omitted".into())
                                        .role(Role::Status)
                                        .aria_label(format!(
                                            "{omitted_sessions} older sessions omitted from the project tree"
                                        ))
                                        .text_token(DesignText::Metadata)
                                        .text_color(rgb(theme.warning.value()))
                                        .child(format!(
                                            "+ {omitted_sessions} older session(s) omitted from this view"
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
    use coding_agent::api::view::CodingAgentWorkspaceOverview;
    use desktop::runtime::DesktopSessionCatalogEntry;
    use std::path::PathBuf;

    fn project_group(
        group_id: &str,
        kind: CodingAgentWorkspaceKind,
        display_name: &str,
        session_ids: &[&str],
        collapsed: bool,
    ) -> ProjectCatalogGroup {
        let workspace = CodingAgentWorkspaceOverview {
            group_id: group_id.into(),
            kind,
            display_name: display_name.into(),
            display_path: (kind == CodingAgentWorkspaceKind::Project)
                .then(|| PathBuf::from(format!("/work/{display_name}"))),
        };
        ProjectCatalogGroup {
            sessions: session_ids
                .iter()
                .map(|session_id| DesktopSessionCatalogEntry {
                    session_id: (*session_id).into(),
                    name: Some(format!("{display_name} session")),
                    workspace: workspace.clone(),
                    ..Default::default()
                })
                .collect(),
            workspace,
            collapsed,
        }
    }

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

    #[test]
    fn project_tree_exposes_four_concurrent_runtime_presentations() {
        let groups = [
            project_group(
                "project:current",
                CodingAgentWorkspaceKind::Project,
                "Current",
                &["current-session"],
                false,
            ),
            project_group(
                "project:running",
                CodingAgentWorkspaceKind::Project,
                "Running",
                &["running-session"],
                false,
            ),
            project_group(
                "project:error",
                CodingAgentWorkspaceKind::Project,
                "Error",
                &["error-session"],
                false,
            ),
            project_group(
                "project:available",
                CodingAgentWorkspaceKind::Project,
                "Available",
                &["available-session"],
                false,
            ),
        ];
        let runtime_states: Arc<[SessionRuntimeState]> = Arc::from([
            SessionRuntimeState {
                session_id: Arc::from("running-session"),
                status: SemanticStatus::Running,
            },
            SessionRuntimeState {
                session_id: Arc::from("error-session"),
                status: SemanticStatus::Error,
            },
        ]);

        let labels = groups
            .iter()
            .map(|group| {
                let (status, contains_active) = project_runtime_summary(
                    group,
                    "current-session",
                    SemanticStatus::Idle,
                    &runtime_states,
                );
                runtime_status_label(status, contains_active)
            })
            .collect::<Vec<_>>();

        assert_eq!(labels, ["current", "running", "error", "available"]);
        assert_eq!(
            session_runtime_status(
                "current-session",
                "current-session",
                SemanticStatus::Idle,
                &runtime_states,
            ),
            Some(SemanticStatus::Idle)
        );
        assert_eq!(
            session_runtime_status(
                "available-session",
                "current-session",
                SemanticStatus::Idle,
                &runtime_states,
            ),
            None
        );
    }

    #[test]
    fn project_status_uses_highest_attention_descendant() {
        let group = project_group(
            "project:mixed",
            CodingAgentWorkspaceKind::Project,
            "Mixed",
            &["idle", "running", "error"],
            false,
        );
        let runtime_states: Arc<[SessionRuntimeState]> = Arc::from([
            SessionRuntimeState {
                session_id: Arc::from("idle"),
                status: SemanticStatus::Idle,
            },
            SessionRuntimeState {
                session_id: Arc::from("running"),
                status: SemanticStatus::Running,
            },
            SessionRuntimeState {
                session_id: Arc::from("error"),
                status: SemanticStatus::Error,
            },
        ]);

        assert_eq!(
            project_runtime_summary(&group, "elsewhere", SemanticStatus::Idle, &runtime_states),
            (Some(SemanticStatus::Error), false)
        );
    }

    #[test]
    fn projectless_and_legacy_groups_have_explicit_titles() {
        let projectless = project_group(
            "projectless:global",
            CodingAgentWorkspaceKind::Projectless,
            "Managed scratch path",
            &["projectless-session"],
            false,
        );
        let legacy = project_group(
            "legacy:unscoped",
            CodingAgentWorkspaceKind::Legacy,
            "",
            &["legacy-session"],
            false,
        );

        assert_eq!(project_title(&projectless), "无项目");
        assert_eq!(project_title(&legacy), "Legacy sessions");
    }

    #[test]
    fn project_tree_keyboard_activation_is_limited_to_enter_and_space() {
        let mut enter = KeyDownEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
            is_held: false,
            prefer_character_input: false,
        };
        assert!(is_keyboard_activation(&enter));
        enter.keystroke = gpui::Keystroke::parse("space").unwrap();
        assert!(is_keyboard_activation(&enter));
        enter.keystroke = gpui::Keystroke::parse("down").unwrap();
        assert!(!is_keyboard_activation(&enter));
    }
}
