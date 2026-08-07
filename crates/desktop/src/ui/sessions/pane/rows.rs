use coding_agent::api::view::CodingAgentWorkspaceKind;
use desktop::ui::shell::{SemanticTheme, truncate_label};
use gpui::{
    IntoElement, KeyDownEvent, ParentElement as _, Role, Styled as _, div, prelude::*, px, rgb,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{
    Icon, Sizable as _,
    menu::{DropdownMenu as _, PopupMenuItem},
};
use std::sync::Arc;
use time::OffsetDateTime;

use super::{
    SessionsPane, SessionsPaneEvent, SessionsPaneViewModel, count_label, is_keyboard_activation,
    project_runtime_summary, project_title, relative_session_time, runtime_status_label,
    session_runtime_status,
};
use crate::app::native_shell::semantic_status_color;
use crate::ui::components::{
    controls::{
        DesktopActionRow, DesktopControlSize, DesktopIcon, DesktopIconButton, DesktopRowState,
    },
    style::{DesignSpace, DesignText, DesktopStyledExt as _},
};
use crate::ui::shell::CenterNavigationTarget;
use gpui::{Entity, WeakEntity};

impl SessionsPane {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn catalog_group_elements(
        &self,
        view_model: &SessionsPaneViewModel,
        _panel_width: u32,
        theme: SemanticTheme,
        presented_as_drawer: bool,
        rename_input: Entity<InputState>,
        renaming_session_id: Option<String>,
        _focused: bool,
        _refresh_target: WeakEntity<SessionsPane>,
        now: OffsetDateTime,
        composer_running: bool,
        awaiting_prompt_start: bool,
        session_pending: bool,
        _session_catalog_pending: bool,
        home_project_directory_editable: bool,
        _omitted_sessions: usize,
        cx: &mut gpui::Context<Self>,
    ) -> Vec<(bool, gpui::AnyElement)> {
        let active_session_id = view_model.active_session_id.as_ref();
        let active_semantic_status = view_model.active_status;
        let runtime_states = Arc::clone(&view_model.runtime_states);
        let visible_groups = view_model.project_groups.to_vec();
        let mut session_index = 0usize;
        visible_groups
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
            .collect::<Vec<_>>()
    }
}
