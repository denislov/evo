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
    controls::{
        DesktopCriticalButton, DesktopCriticalTone, DesktopIcon, DesktopIconButton, DesktopSelector,
    },
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
    SelectModel(Arc<str>),
    SelectSessionProfile(Arc<str>),
    SelectThinking(DesktopThinkingLevel),
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
pub(crate) struct ConversationHeaderViewModel {
    pub(crate) idle: bool,
    pub(crate) status: SemanticStatus,
    pub(crate) composer_running: bool,
    pub(crate) abort_pending: bool,
    pub(crate) reload_pending: bool,
    pub(crate) selector_disabled: bool,
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
    let project_name = project
        .cwd
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| desktop::ui::shell::truncate_label(name, 18))
        .unwrap_or_else(|| "Project".into());

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
        selector_disabled: composer_running
            || awaiting_prompt_start
            || reload_pending
            || selection_pending,
        model: Arc::from(desktop::ui::shell::truncate_label(model, 10)),
        profile: Arc::from(desktop::ui::shell::truncate_label(profile, 9)),
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
        let theme = SemanticTheme::GEEK_DARK;
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
        let (model_label, profile_label, thinking_label) = if expanded_chrome {
            (
                format!("Model · {}", view_model.model),
                format!("Profile · {}", view_model.profile),
                format!("Thinking · {}", view_model.thinking),
            )
        } else {
            (
                format!("M · {}", view_model.model),
                format!("P · {}", view_model.profile),
                format!("T · {}", view_model.thinking),
            )
        };
        let show_session_title = expanded_chrome || (view_model.idle && viewport_width >= 900);
        let compact_actions = viewport_width < 900;
        let status_slot_width = header_runtime_status_slot_width(viewport_width);
        let model_accessible_label =
            format!("Select model; current {}", view_model.current_model_id);
        let profile_accessible_label = format!(
            "Select session profile; current {}",
            view_model.current_profile_id
        );
        let thinking_accessible_label = format!(
            "Select session thinking level; current {}",
            view_model.thinking
        );
        let focused = self.focus.is_focused(window) && view_model.keyboard_focus_visible;
        let focus_accent = conversation_focus_accent(focused, theme);

        let model_target = cx.entity().downgrade();
        let profile_target = cx.entity().downgrade();
        let thinking_target = cx.entity().downgrade();
        let reload_target = cx.entity().downgrade();
        let model_groups = Arc::clone(&view_model.model_groups);
        let unavailable_current_model = view_model.unavailable_current_model.clone();
        let model_option_count = model_groups
            .iter()
            .map(|group| group.options.len())
            .sum::<usize>();
        let profile_options = Arc::clone(&view_model.profile_options);
        let thinking_hint = view_model.thinking_hint.clone();
        let current_model_id = Arc::clone(&view_model.current_model_id);
        let current_profile_id = Arc::clone(&view_model.current_profile_id);
        let selector_disabled = view_model.selector_disabled;

        div()
            .id("conversation-header")
            .debug_selector(|| "desktop-conversation-header".into())
            .track_focus(&self.focus)
            .h(px(CENTER_HEADER_HEIGHT as f32))
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
                    .when(show_session_title, |identity| identity.child(
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
                                    .child(if view_model.idle {
                                        "New task"
                                    } else {
                                        "Current task"
                                    }),
                            )
                            .child(
                                div()
                                    .text_token(DesignText::Metadata)
                                    .text_color(rgb(if view_model.thinking_hint.is_some() {
                                        theme.warning.value()
                                    } else {
                                        theme.subtle_text.value()
                                    }))
                                    .child(
                                        view_model
                                            .thinking_hint
                                            .as_deref()
                                            .unwrap_or(&view_model.project_name)
                                            .to_string(),
                                    ),
                            ),
                    )),
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
                                        .debug_selector(|| {
                                            "desktop-header-runtime-status".into()
                                        })
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
                                        .text_color(semantic_status_color(status))
                                        .child(status.glyph())
                                        .child(header_runtime_status_label(
                                            status,
                                            compact_actions,
                                        )),
                                )
                            }),
                    )
                    .child(
                        DesktopSelector::new(
                            "header-model-selector",
                            model_label,
                            model_accessible_label,
                        )
                        .disabled(selector_disabled)
                        .build()
                        .debug_selector(|| "desktop-header-model-selector".into())
                        .dropdown_menu(move |menu, _, _| {
                            let mut menu = menu
                                .min_w(px(320.))
                                .max_w(px(480.))
                                .max_h(px(320.))
                                .scrollable(model_option_count > 8);

                            if let Some(warning) = unavailable_current_model.as_ref() {
                                menu = menu
                                    .item(PopupMenuItem::label("Current model unavailable"))
                                    .item(PopupMenuItem::label(format!(
                                        "{} · {} · {}",
                                        desktop::ui::shell::truncate_label(&warning.name, 32),
                                        desktop::ui::shell::truncate_label(&warning.id, 36),
                                        warning.reason
                                    )))
                                    .separator();
                            }

                            if model_groups.is_empty() {
                                return menu
                                    .item(PopupMenuItem::label("No configured text models"))
                                    .item(PopupMenuItem::label(
                                        "Add keys to auth.toml or env, then Reload local resources.",
                                    ));
                            }

                            for (group_index, group) in model_groups.iter().enumerate() {
                                if group_index > 0 {
                                    menu = menu.separator();
                                }
                                menu = menu.item(PopupMenuItem::label(group.provider.to_string()));

                                for option in group.options.iter() {
                                    let target = model_target.clone();
                                    let id = Arc::clone(&option.id);
                                    let accessible_name = Arc::clone(&option.name);
                                    let display_name = Arc::clone(&option.display_name);
                                    let metadata = Arc::<str>::from(format!(
                                        "Model ID · {}",
                                        desktop::ui::shell::truncate_label(&option.id, 54)
                                    ));
                                    let row_id = Arc::clone(&option.id);
                                    menu = menu.item(
                                        PopupMenuItem::element(move |_, _| {
                                            div()
                                                .id(format!("header-model-menu-row-{row_id}"))
                                                .w_full()
                                                .min_w_0()
                                                .flex()
                                                .flex_col()
                                                .aria_label(format!(
                                                    "{}; model id {}",
                                                    accessible_name, row_id
                                                ))
                                                .child(
                                                    div()
                                                        .min_w_0()
                                                        .text_token(DesignText::Body)
                                                        .child(display_name.to_string()),
                                                )
                                                .child(
                                                    div()
                                                        .min_w_0()
                                                        .text_token(DesignText::Metadata)
                                                        .text_color(rgb(theme.subtle_text.value()))
                                                        .child(metadata.to_string()),
                                                )
                                        })
                                            .checked(option.id == current_model_id)
                                            .on_click(move |_, _, cx| {
                                                if let Some(target) = target.upgrade() {
                                                    let id = Arc::clone(&id);
                                                    target.update(cx, |_, cx| {
                                                        cx.emit(ConversationHeaderEvent::SelectModel(
                                                            id,
                                                        ));
                                                    });
                                                }
                                            }),
                                    );
                                }
                            }
                            menu
                            }),
                    )
                    .when_some(thinking_hint, |actions, hint| {
                        actions.child(
                            div()
                                .id("header-thinking-hint")
                                .debug_selector(|| "desktop-header-thinking-hint".into())
                                .role(Role::Status)
                                .aria_label(hint.to_string())
                                .max_w(px(148.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_token(DesignText::Metadata)
                                .text_color(rgb(theme.warning.value()))
                                .child("Thinking · Auto"),
                        )
                    })
                    .when(!view_model.thinking_options.is_empty(), |actions| actions.child(
                        DesktopSelector::new(
                            "header-thinking-selector",
                            thinking_label,
                            thinking_accessible_label,
                        )
                        .disabled(selector_disabled)
                        .build()
                        .debug_selector(|| "desktop-header-thinking-selector".into())
                        .dropdown_menu(move |menu, _, _| {
                            view_model.thinking_options.iter().fold(
                                menu.min_w(px(180.)).max_w(px(280.)),
                                |menu, option| {
                                    let target = thinking_target.clone();
                                    let level = option.selection;
                                    menu.item(
                                        PopupMenuItem::new(option.label)
                                        .checked(level == view_model.thinking_selection)
                                        .on_click(move |_, _, cx| {
                                            if let Some(target) = target.upgrade() {
                                                target.update(cx, |_, cx| {
                                                    cx.emit(
                                                        ConversationHeaderEvent::SelectThinking(
                                                            level,
                                                        ),
                                                    );
                                                });
                                            }
                                        }),
                                    )
                                },
                            )
                        }),
                    ))
                    .child(
                        DesktopSelector::new(
                            "header-profile-selector",
                            profile_label,
                            profile_accessible_label,
                        )
                        .disabled(selector_disabled)
                        .build()
                        .debug_selector(|| "desktop-header-profile-selector".into())
                        .dropdown_menu(move |menu, _, _| {
                            profile_options.iter().fold(
                                menu.min_w(px(240.))
                                    .max_w(px(420.))
                                    .max_h(px(320.))
                                    .scrollable(profile_options.len() > 8),
                                |menu, option| {
                                    let target = profile_target.clone();
                                    let id = Arc::clone(&option.id);
                                    menu.item(
                                        PopupMenuItem::new(option.label.to_string())
                                            .checked(option.id == current_profile_id)
                                            .disabled(!option.selectable)
                                            .on_click(move |_, _, cx| {
                                                if let Some(target) = target.upgrade() {
                                                    let id = Arc::clone(&id);
                                                    target.update(cx, |_, cx| {
                                                        cx.emit(
                                                            ConversationHeaderEvent::SelectSessionProfile(
                                                                id,
                                                            ),
                                                        );
                                                    });
                                                }
                                            }),
                                    )
                                },
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
                            menu.item(
                                PopupMenuItem::new(if view_model.reload_pending {
                                    "Reloading local resources…"
                                } else {
                                    "Reload local resources"
                                })
                                .disabled(view_model.selector_disabled)
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
