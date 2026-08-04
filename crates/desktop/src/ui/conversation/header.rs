use coding_agent::api::{
    embedding::{CodingAgentModelChoice, CodingAgentThinkingLevel},
    view::ProfileKind,
};
use desktop::preferences::DesktopThinkingLevel;
use desktop::ui::shell::{
    CENTER_HEADER_HEIGHT, PanelVisibility, SemanticStatus, SemanticTheme, ShellLayout,
};
use gpui::{
    EventEmitter, FocusHandle, IntoElement, ParentElement as _, Render, Role, Styled as _, Window,
    div, prelude::*, px, rgb,
};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use std::{collections::BTreeMap, sync::Arc};

use crate::app::native_shell::{
    NativeDesktopState, conversation_focus_accent, semantic_status_color,
};
use crate::application::commands::DesktopCommandIntent;
use crate::ui::components::{
    controls::{DesktopCriticalButton, DesktopCriticalTone, DesktopIcon, DesktopIconButton},
    style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
};
use crate::ui::shell::drawer::CenterDrawerKind;
use crate::ui::shell::{ShellUiState, presentation::semantic_status};

/// Stable space for the attention-only runtime indicator. Idle leaves this
/// slot empty so status changes never move the selectors or panel actions.
pub(crate) const HEADER_RUNTIME_STATUS_SLOT_WIDTH: f32 = 104.;
const HEADER_RUNTIME_STATUS_COMPACT_SLOT_WIDTH: f32 = 80.;

pub(crate) const fn header_runtime_status_slot_width(viewport_width: u32) -> f32 {
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
pub(crate) enum ConversationHeaderEvent {
    ToggleSessions,
    ToggleInspector,
    ChooseProjectDirectory,
    ClearProjectDirectory,
    Reload,
    Abort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationHeaderSelectorOption {
    pub(crate) id: Arc<str>,
    pub(crate) label: Arc<str>,
    pub(crate) selectable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationHeaderModelOption {
    pub(crate) id: Arc<str>,
    pub(crate) name: Arc<str>,
    pub(crate) display_name: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationHeaderModelGroup {
    pub(crate) provider: Arc<str>,
    pub(crate) options: Arc<[ConversationHeaderModelOption]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationHeaderModelWarning {
    pub(crate) id: Arc<str>,
    pub(crate) name: Arc<str>,
    pub(crate) reason: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationHeaderThinkingOption {
    pub(crate) selection: DesktopThinkingLevel,
    pub(crate) label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationControlsViewModel {
    pub(crate) selector_disabled: bool,
    pub(crate) profile_selector_disabled: bool,
    pub(crate) model: Arc<str>,
    pub(crate) profile: Arc<str>,
    pub(crate) thinking: Arc<str>,
    pub(crate) thinking_selection: DesktopThinkingLevel,
    pub(crate) thinking_options: Arc<[ConversationHeaderThinkingOption]>,
    pub(crate) thinking_hint: Option<Arc<str>>,
    pub(crate) current_model_id: Arc<str>,
    pub(crate) current_profile_id: Arc<str>,
    pub(crate) model_groups: Arc<[ConversationHeaderModelGroup]>,
    pub(crate) unavailable_current_model: Option<ConversationHeaderModelWarning>,
    pub(crate) profile_options: Arc<[ConversationHeaderSelectorOption]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationHeaderViewModel {
    pub(crate) idle: bool,
    pub(crate) status: SemanticStatus,
    pub(crate) composer_running: bool,
    pub(crate) abort_pending: bool,
    pub(crate) reload_pending: bool,
    pub(crate) reload_disabled: bool,
    pub(crate) project_directory_editable: bool,
    pub(crate) project_directory_selected: bool,
    pub(crate) session_name: Arc<str>,
    pub(crate) project_name: Arc<str>,
    pub(crate) keyboard_focus_visible: bool,
    pub(crate) panel_visibility: PanelVisibility,
    pub(crate) sessions_drawer_open: bool,
    pub(crate) inspector_drawer_open: bool,
    pub(crate) sessions_panel_width: u32,
    pub(crate) context_panel_width: u32,
}

pub(crate) fn view_model(
    app: &NativeDesktopState,
    ui: &ShellUiState,
) -> ConversationHeaderViewModel {
    let workspace = app.workspaces.active();
    let snapshot = workspace
        .projection
        .as_ref()
        .map(|projection| projection.snapshot());
    let project = &workspace.project;
    let composer_running = snapshot.is_some_and(|snapshot| snapshot.active_operation.is_some());
    let awaiting_prompt_start = workspace.composer.submitted().is_some() && !composer_running;
    let reload_pending = app
        .commands
        .contains(app.workspaces.active_key(), &DesktopCommandIntent::Reload);
    let selection_pending = app
        .commands
        .contains_where(app.workspaces.active_key(), |intent| {
            matches!(intent, DesktopCommandIntent::Selection(_))
        });
    let project_name = project
        .cwd
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| desktop::ui::shell::truncate_label(name, 18))
        .unwrap_or_else(|| "Project".into());
    let session_name = snapshot.map_or_else(
        || "New task".to_owned(),
        |snapshot| {
            app.catalog
                .project_groups()
                .into_iter()
                .flat_map(|group| group.sessions)
                .find(|session| session.session_id == snapshot.session.session_id)
                .and_then(|session| session.name)
                .or_else(|| snapshot.session.name.clone())
                .filter(|name| !name.trim().is_empty())
                .map(|name| desktop::ui::shell::truncate_label(&name, 32))
                .unwrap_or_else(|| "Untitled".into())
        },
    );

    ConversationHeaderViewModel {
        idle: workspace.projection.is_none(),
        status: semantic_status(workspace.projection.as_ref()),
        composer_running,
        abort_pending: app
            .commands
            .contains_where(app.workspaces.active_key(), |intent| {
                matches!(intent, DesktopCommandIntent::Abort { .. })
            }),
        reload_pending,
        reload_disabled: composer_running
            || awaiting_prompt_start
            || reload_pending
            || selection_pending,
        project_directory_editable: workspace.project_directory_editable(),
        project_directory_selected: workspace.project_directory().is_some(),
        session_name: Arc::from(session_name),
        project_name: Arc::from(project_name),
        keyboard_focus_visible: ui.keyboard_focus_visible(),
        panel_visibility: PanelVisibility {
            sessions: app.preferences.sessions_panel_visible,
            context: app.preferences.context_panel_visible,
        },
        sessions_drawer_open: ui.active_drawer == Some(CenterDrawerKind::Sessions),
        inspector_drawer_open: ui.active_drawer == Some(CenterDrawerKind::Inspector),
        sessions_panel_width: app.preferences.sessions_panel_width,
        context_panel_width: app.preferences.context_panel_width,
    }
}

pub(crate) fn controls_view_model(app: &NativeDesktopState) -> ConversationControlsViewModel {
    let workspace = app.workspaces.active();
    let snapshot = workspace
        .projection
        .as_ref()
        .map(|projection| projection.snapshot());
    let project = &workspace.project;
    let composer_running = snapshot.is_some_and(|snapshot| snapshot.active_operation.is_some());
    let awaiting_prompt_start = workspace.composer.submitted().is_some() && !composer_running;
    let reload_pending = app
        .commands
        .contains(app.workspaces.active_key(), &DesktopCommandIntent::Reload);
    let selection_pending = app
        .commands
        .contains_where(app.workspaces.active_key(), |intent| {
            matches!(intent, DesktopCommandIntent::Selection(_))
        });
    let current_model_id = project.selected_model_id.as_str();
    let current_profile_id = snapshot
        .map(|snapshot| snapshot.session.default_agent_profile_id.as_str())
        .unwrap_or_else(|| project.default_agent_profile_id.as_str());
    let model = project
        .models
        .iter()
        .find(|model| model.id == current_model_id)
        .map(|model| model.name.as_str())
        .unwrap_or(current_model_id);
    let current_model = project
        .models
        .iter()
        .find(|model| model.id == current_model_id);
    let profile = project
        .profiles
        .iter()
        .find(|profile| profile.id.as_str() == current_profile_id)
        .map(|profile| profile.display_name.as_str())
        .unwrap_or(current_profile_id);
    let (model_groups, unavailable_current_model) = model_menu(&project.models, current_model_id);
    let profile_options = project
        .profiles
        .iter()
        .filter(|profile| !is_built_in_helper(profile.id.as_str()))
        .map(|profile| ConversationHeaderSelectorOption {
            id: Arc::from(profile.id.as_str()),
            label: Arc::from(format!(
                "{} · {}{}",
                profile.display_name,
                profile.id.as_str(),
                if profile.kind == ProfileKind::Team {
                    " · team profile"
                } else {
                    ""
                }
            )),
            selectable: profile.kind == ProfileKind::Agent,
        })
        .collect::<Vec<_>>();

    ConversationControlsViewModel {
        selector_disabled: composer_running
            || awaiting_prompt_start
            || reload_pending
            || selection_pending,
        profile_selector_disabled: workspace.projection.is_some()
            || composer_running
            || awaiting_prompt_start
            || reload_pending
            || selection_pending,
        model: Arc::from(desktop::ui::shell::truncate_label(model, 18)),
        profile: Arc::from(desktop::ui::shell::truncate_label(profile, 14)),
        thinking: Arc::from(
            if workspace.thinking_selection == DesktopThinkingLevel::Default {
                "Auto".to_owned()
            } else {
                desktop::ui::shell::truncate_label(&workspace.thinking_selection.label(None), 12)
            },
        ),
        thinking_selection: workspace.thinking_selection,
        thinking_options: thinking_menu(current_model).into(),
        thinking_hint: workspace.thinking_hint.clone(),
        current_model_id: Arc::from(current_model_id),
        current_profile_id: Arc::from(current_profile_id),
        model_groups: model_groups.into(),
        unavailable_current_model,
        profile_options: profile_options.into(),
    }
}

pub(crate) fn thinking_menu(
    model: Option<&CodingAgentModelChoice>,
) -> Vec<ConversationHeaderThinkingOption> {
    let Some(capability) = model.map(|model| &model.thinking_capability) else {
        return Vec::new();
    };
    if !capability.supported {
        return Vec::new();
    }
    let mut options = vec![ConversationHeaderThinkingOption {
        selection: DesktopThinkingLevel::Default,
        label: "Auto",
    }];
    if capability.can_disable {
        options.push(ConversationHeaderThinkingOption {
            selection: DesktopThinkingLevel::Off,
            label: "Off",
        });
    }
    for level in &capability.explicit_levels {
        if *level == CodingAgentThinkingLevel::Off {
            continue;
        }
        let selection = DesktopThinkingLevel::from_explicit(Some(*level));
        if options.iter().any(|option| option.selection == selection) {
            continue;
        }
        options.push(ConversationHeaderThinkingOption {
            selection,
            label: match level {
                CodingAgentThinkingLevel::Off => "Off",
                CodingAgentThinkingLevel::Minimal => "Minimal",
                CodingAgentThinkingLevel::Low => "Low",
                CodingAgentThinkingLevel::Medium => "Medium",
                CodingAgentThinkingLevel::High => "High",
                CodingAgentThinkingLevel::XHigh => "XHigh",
            },
        });
    }
    options
}

pub(crate) fn model_menu(
    models: &[CodingAgentModelChoice],
    current_model_id: &str,
) -> (
    Vec<ConversationHeaderModelGroup>,
    Option<ConversationHeaderModelWarning>,
) {
    let mut grouped = BTreeMap::<&str, Vec<ConversationHeaderModelOption>>::new();
    for model in models
        .iter()
        .filter(|model| model.configured && model.supports_text)
    {
        grouped
            .entry(model.provider.as_str())
            .or_default()
            .push(ConversationHeaderModelOption {
                id: Arc::from(model.id.as_str()),
                name: Arc::from(model.name.as_str()),
                display_name: Arc::from(desktop::ui::shell::truncate_label(&model.name, 44)),
            });
    }

    let groups = grouped
        .into_iter()
        .map(|(provider, mut options)| {
            options.sort_by(|left, right| {
                left.name
                    .cmp(&right.name)
                    .then_with(|| left.id.cmp(&right.id))
            });
            ConversationHeaderModelGroup {
                provider: Arc::from(provider),
                options: options.into(),
            }
        })
        .collect();
    let unavailable_current_model = models
        .iter()
        .find(|model| model.id == current_model_id)
        .filter(|model| !(model.configured && model.supports_text))
        .map(|model| ConversationHeaderModelWarning {
            id: Arc::from(model.id.as_str()),
            name: Arc::from(model.name.as_str()),
            reason: Arc::from(if !model.supports_text {
                "No text input"
            } else {
                "Authentication required"
            }),
        })
        .or_else(|| {
            (!models.iter().any(|model| model.id == current_model_id)).then(|| {
                ConversationHeaderModelWarning {
                    id: Arc::from(current_model_id),
                    name: Arc::from(current_model_id),
                    reason: Arc::from("Not in model catalog"),
                }
            })
        });

    (groups, unavailable_current_model)
}

pub(crate) struct ConversationHeader {
    focus: FocusHandle,
    view_model: Option<ConversationHeaderViewModel>,
}

impl ConversationHeader {
    pub(crate) fn new(focus: FocusHandle) -> Self {
        Self {
            focus,
            view_model: None,
        }
    }

    pub(crate) fn set_view_model(&mut self, view_model: ConversationHeaderViewModel) {
        self.view_model = Some(view_model);
    }
}

impl EventEmitter<ConversationHeaderEvent> for ConversationHeader {}

impl Render for ConversationHeader {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(view_model) = self.view_model.clone() else {
            return div().h(px(CENTER_HEADER_HEIGHT as f32)).into_any_element();
        };
        let theme = SemanticTheme::current(cx);
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
        let show_session_title = expanded_chrome || (view_model.idle && viewport_width >= 900);
        let compact_actions = viewport_width < 900;
        let status_slot_width = header_runtime_status_slot_width(viewport_width);
        let focused = self.focus.is_focused(window) && view_model.keyboard_focus_visible;
        let focus_accent = conversation_focus_accent(focused, theme);

        let reload_target = cx.entity().downgrade();
        let choose_project_target = cx.entity().downgrade();
        let clear_project_target = cx.entity().downgrade();
        let project_directory_editable = view_model.project_directory_editable;
        let project_directory_selected = view_model.project_directory_selected;

        div()
            .id("conversation-header")
            .debug_selector(|| "desktop-conversation-header".into())
            .track_focus(&self.focus)
            .h(px(CENTER_HEADER_HEIGHT as f32))
            .px_token(DesignSpace::Xl)
            .flex()
            .items_center()
            .gap_token(DesignSpace::Lg)
            .border_b_1()
            .border_color(rgb(focus_accent.value()))
            .bg(rgb(theme.elevated.value()))
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
                    .when(show_session_title, |identity| {
                        identity.child(
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
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(view_model.session_name.to_string()),
                                )
                                .child(
                                    div()
                                        .text_token(DesignText::Metadata)
                                        .text_color(rgb(theme.subtle_text.value()))
                                        .child(view_model.project_name.to_string()),
                                ),
                        )
                    }),
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
                                        .debug_selector(|| "desktop-header-runtime-status".into())
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
                                        .text_color(semantic_status_color(status, theme))
                                        .child(status.glyph())
                                        .child(header_runtime_status_label(
                                            status,
                                            compact_actions,
                                        )),
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
                            let mut menu = menu;
                            if project_directory_editable {
                                let choose_project_target = choose_project_target.clone();
                                menu = menu.item(
                                    PopupMenuItem::new("Choose project directory…").on_click(
                                        move |_, _, cx| {
                                            if let Some(target) = choose_project_target.upgrade() {
                                                target.update(cx, |_, cx| {
                                                    cx.emit(
                                                        ConversationHeaderEvent::ChooseProjectDirectory,
                                                    );
                                                });
                                            }
                                        },
                                    ),
                                );
                                if project_directory_selected {
                                    let clear_project_target = clear_project_target.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new("Use no project").on_click(
                                            move |_, _, cx| {
                                                if let Some(target) = clear_project_target.upgrade()
                                                {
                                                    target.update(cx, |_, cx| {
                                                        cx.emit(
                                                            ConversationHeaderEvent::ClearProjectDirectory,
                                                        );
                                                    });
                                                }
                                            },
                                        ),
                                    );
                                }
                                menu = menu.separator();
                            }
                            menu.item(
                                PopupMenuItem::new(if view_model.reload_pending {
                                    "Reloading local resources…"
                                } else {
                                    "Reload local resources"
                                })
                                .disabled(view_model.reload_disabled)
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

/// The three built-in helper agents exist only as delegation targets; they
/// cannot be chosen as a session's profile.
fn is_built_in_helper(profile_id: &str) -> bool {
    matches!(profile_id, "explore" | "review" | "check")
}

#[cfg(test)]
mod tests {
    use super::*;
    use coding_agent::api::embedding::CodingAgentThinkingCapability;

    fn model_fixture(
        id: &str,
        name: &str,
        provider: &str,
        configured: bool,
        supports_text: bool,
    ) -> CodingAgentModelChoice {
        CodingAgentModelChoice {
            id: id.into(),
            name: name.into(),
            provider: provider.into(),
            reasoning: false,
            thinking_capability: CodingAgentThinkingCapability::default(),
            supports_text,
            supports_images: !supports_text,
            context_window: 32_000,
            max_output_tokens: 4_000,
            configured,
            selected: false,
        }
    }

    #[test]
    fn model_menu_filters_and_stably_orders_provider_groups_and_rows() {
        let models = vec![
            model_fixture("z-current", "Zulu Current", "z-provider", true, true),
            model_fixture("a-second", "Second Alpha", "a-provider", true, true),
            model_fixture(
                "unconfigured",
                "Unavailable Alpha",
                "a-provider",
                false,
                true,
            ),
            model_fixture("image-only", "Image Alpha", "a-provider", true, false),
            model_fixture("a-first", "First Alpha", "a-provider", true, true),
        ];

        let (groups, warning) = model_menu(&models, "z-current");
        assert!(warning.is_none());
        assert_eq!(
            groups
                .iter()
                .map(|group| group.provider.as_ref())
                .collect::<Vec<_>>(),
            ["a-provider", "z-provider"]
        );
        assert_eq!(
            groups[0]
                .options
                .iter()
                .map(|option| option.id.as_ref())
                .collect::<Vec<_>>(),
            ["a-first", "a-second"]
        );
        assert_eq!(
            groups
                .iter()
                .flat_map(|group| group.options.iter())
                .map(|option| option.id.as_ref())
                .collect::<Vec<_>>(),
            ["a-first", "a-second", "z-current"]
        );

        let mut reordered = models;
        reordered.reverse();
        let (reordered_groups, _) = model_menu(&reordered, "z-current");
        assert_eq!(groups, reordered_groups);
    }

    #[test]
    fn model_menu_bounds_long_names_and_isolates_unavailable_current_model() {
        let long_name =
            "A deliberately very long model name used to prove bounded popup rows ".repeat(3);
        let models = vec![
            model_fixture(
                "lost-auth-model",
                "Lost Authentication",
                "z-provider",
                false,
                true,
            ),
            model_fixture(
                "configured-model-with-a-very-long-identifier-that-remains-typed",
                &long_name,
                "a-provider",
                true,
                true,
            ),
        ];

        let (groups, warning) = model_menu(&models, "lost-auth-model");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].options.len(), 1);
        assert_eq!(groups[0].options[0].name.as_ref(), long_name);
        assert_ne!(groups[0].options[0].display_name.as_ref(), long_name);
        assert!(groups[0].options[0].display_name.ends_with('…'));
        assert_eq!(
            warning,
            Some(ConversationHeaderModelWarning {
                id: Arc::from("lost-auth-model"),
                name: Arc::from("Lost Authentication"),
                reason: Arc::from("Authentication required"),
            })
        );

        let unavailable = models
            .into_iter()
            .map(|mut model| {
                model.configured = false;
                model
            })
            .collect::<Vec<_>>();
        let (empty_groups, warning) = model_menu(&unavailable, "lost-auth-model");
        assert!(empty_groups.is_empty());
        assert!(warning.is_some());
    }

    #[test]
    fn thinking_menu_exactly_matches_the_product_capability() {
        let mut model = model_fixture("reasoner", "Reasoner", "fixture", true, true);
        model.thinking_capability = CodingAgentThinkingCapability {
            supported: true,
            explicit_levels: vec![
                CodingAgentThinkingLevel::High,
                CodingAgentThinkingLevel::Low,
                CodingAgentThinkingLevel::High,
                CodingAgentThinkingLevel::Off,
            ],
            can_disable: false,
        };
        let options = thinking_menu(Some(&model));
        assert_eq!(
            options
                .iter()
                .map(|option| (option.selection, option.label))
                .collect::<Vec<_>>(),
            [
                (DesktopThinkingLevel::Default, "Auto"),
                (DesktopThinkingLevel::High, "High"),
                (DesktopThinkingLevel::Low, "Low"),
            ]
        );

        model.thinking_capability.can_disable = true;
        assert_eq!(
            thinking_menu(Some(&model))
                .iter()
                .map(|option| option.selection)
                .collect::<Vec<_>>(),
            [
                DesktopThinkingLevel::Default,
                DesktopThinkingLevel::Off,
                DesktopThinkingLevel::High,
                DesktopThinkingLevel::Low,
            ]
        );
        assert!(thinking_menu(None).is_empty());
        model.thinking_capability = CodingAgentThinkingCapability::default();
        assert!(thinking_menu(Some(&model)).is_empty());
    }
}
