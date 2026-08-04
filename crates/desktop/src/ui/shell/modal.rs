use coding_agent::api::authorization::{
    ToolAuthorizationDecision, ToolAuthorizationIdentity, ToolAuthorizationRequest,
    ToolAuthorizationScope,
};
use desktop::ui::shell::{DESKTOP_OVERLAY_SCRIM_RGBA, MONOSPACE_FONT_FAMILY, SemanticTheme};
use gpui::{
    EventEmitter, FocusHandle, Focusable as _, IntoElement, ParentElement as _, Render, Role,
    SharedString, Styled as _, Subscription, Window, div, prelude::*, px, rgb, rgba,
};
use gpui_component::{
    Icon, Selectable as _, Sizable as _,
    button::Button,
    input::{Input, InputEvent, InputState},
};
use std::sync::Arc;

use super::ShellUiState;
use crate::actions::{self, DesktopPaletteCommand, PALETTE_ENTRIES};
use crate::app::native_shell::{
    ConversationFullMessageView, NativeDesktopState, SessionDeleteConfirm,
};
use crate::ui::components::{
    controls::{
        DesktopActionRow, DesktopCriticalButton, DesktopCriticalTone, DesktopIcon,
        DesktopIconButton, DesktopRowState,
    },
    style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RootModalHostEvent {
    ExecutePalette(DesktopPaletteCommand),
    CopyFullMessage,
    CloseFullMessage,
    NavigateSearch(String),
    CloseSearch,
    ConfirmDeleteSession,
    CancelDeleteSession,
    DecideAuthorization {
        identity: ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootModalAuthorizationView {
    pub(crate) request: ToolAuthorizationRequest,
    pub(crate) decision_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootModalViewModel {
    pub(crate) palette_open: bool,
    pub(crate) palette_selected: usize,
    pub(crate) authorization: Option<RootModalAuthorizationView>,
    pub(crate) full_message: Option<ConversationFullMessageView>,
    pub(crate) search_open: bool,
    pub(crate) search_loading: bool,
    pub(crate) search_sessions: Arc<[GlobalSearchSession]>,
    pub(crate) active_session_id: Arc<str>,
    pub(crate) delete_confirm: Option<SessionDeleteConfirm>,
}

/// One result category in the global search surface.
///
/// Keeping the dialog model independent from the sidebar tree means settings
/// and other searchable resources can be added as sibling categories later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GlobalSearchSession {
    pub(crate) session_id: Arc<str>,
    pub(crate) name: Arc<str>,
    pub(crate) workspace: Arc<str>,
}

pub(crate) fn view_model(app: &NativeDesktopState, ui: &ShellUiState) -> RootModalViewModel {
    let authorization = app
        .workspaces
        .active()
        .projection
        .as_ref()
        .and_then(|projection| projection.snapshot().pending_authorizations.first())
        .cloned()
        .map(|request| {
            let decision_pending = app
                .commands
                .authorization(app.workspaces.active_key())
                .is_some_and(|(_, authorization_id, operation_id)| {
                    authorization_id == request.authorization_id
                        && operation_id == request.operation_id
                });
            RootModalAuthorizationView {
                request,
                decision_pending,
            }
        });
    let active_session_id = app
        .workspaces
        .active()
        .projection
        .as_ref()
        .map(|projection| projection.snapshot().session.session_id.as_str())
        .unwrap_or_default();
    let search_sessions = app
        .catalog
        .project_groups()
        .into_iter()
        .flat_map(|group| {
            let workspace: Arc<str> =
                Arc::from(if group.workspace.display_name.trim().is_empty() {
                    "No project".to_owned()
                } else {
                    group.workspace.display_name
                });
            group
                .sessions
                .into_iter()
                .map(move |session| GlobalSearchSession {
                    session_id: Arc::from(session.session_id),
                    name: Arc::from(
                        session
                            .name
                            .filter(|name| !name.trim().is_empty())
                            .unwrap_or_else(|| "Untitled".into()),
                    ),
                    workspace: Arc::clone(&workspace),
                })
        })
        .collect::<Vec<_>>();
    RootModalViewModel {
        palette_open: ui.command_palette.is_open(),
        palette_selected: ui.command_palette.selected(),
        authorization,
        full_message: ui.conversation_full_message.clone(),
        search_open: ui.active_modal == Some(crate::app::native_shell::DesktopModalKind::Search),
        search_loading: app.catalog.state().is_loading(),
        search_sessions: search_sessions.into(),
        active_session_id: Arc::from(active_session_id),
        delete_confirm: ui.pending_delete_session.clone(),
    }
}

pub(crate) struct RootModalHost {
    authorization_focus: FocusHandle,
    command_palette_focus: FocusHandle,
    full_message_focus: FocusHandle,
    search_focus: FocusHandle,
    modal_focus: FocusHandle,
    search_input: gpui::Entity<InputState>,
    view_model: Option<RootModalViewModel>,
    _search_subscription: Subscription,
}

impl RootModalHost {
    pub(crate) fn new(
        authorization_focus: FocusHandle,
        command_palette_focus: FocusHandle,
        full_message_focus: FocusHandle,
        search_focus: FocusHandle,
        modal_focus: FocusHandle,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search sessions, settings, and more…")
        });
        let search_subscription = cx.subscribe_in(
            &search_input,
            window,
            |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => {
                    let query = input.read(cx).value();
                    if let Some(session) = this.view_model.as_ref().and_then(|view_model| {
                        filtered_search_sessions(&view_model.search_sessions, &query)
                            .into_iter()
                            .next()
                    }) {
                        cx.emit(RootModalHostEvent::NavigateSearch(
                            session.session_id.to_string(),
                        ));
                    }
                }
                _ => {}
            },
        );
        Self {
            authorization_focus,
            command_palette_focus,
            full_message_focus,
            search_focus,
            modal_focus,
            search_input,
            view_model: None,
            _search_subscription: search_subscription,
        }
    }

    pub(crate) fn set_view_model(&mut self, view_model: RootModalViewModel) {
        self.view_model = Some(view_model);
    }

    pub(crate) fn open_search(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        self.search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.search_input.focus_handle(cx).focus(window, cx);
    }

    #[cfg(test)]
    pub(crate) fn set_search_value(
        &mut self,
        value: &str,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        self.search_input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }
}

impl EventEmitter<RootModalHostEvent> for RootModalHost {}

impl Render for RootModalHost {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(view_model) = self.view_model.clone() else {
            return div().into_any_element();
        };
        let theme = SemanticTheme::current(cx);
        let search_input = self.search_input.clone();
        let search_query = search_input.read(cx).value().to_string();
        let search_results = filtered_search_sessions(&view_model.search_sessions, &search_query);
        let search_result_count = search_results.len();
        let search_rows = search_results
            .into_iter()
            .enumerate()
            .map(|(index, session)| {
                let target = session.session_id.to_string();
                DesktopActionRow::new(
                    ("global-search-session", index),
                    session.name.to_string(),
                    format!("Open session {} in {}", session.name, session.workspace),
                )
                .state(DesktopRowState {
                    selected: session.session_id == view_model.active_session_id,
                    disabled: false,
                    focus_visible: false,
                })
                .detail(format!("{} · {}", session.workspace, session.session_id))
                .selection_background_only()
                .build(theme)
                .debug_selector(move || format!("desktop-global-search-session-{index}"))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(RootModalHostEvent::NavigateSearch(target.clone()));
                }))
            })
            .collect::<Vec<_>>();
        let palette_rows = PALETTE_ENTRIES
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let command = entry.command;
                let selected = view_model.palette_selected == index;
                let label = entry.shortcut.map_or_else(
                    || entry.label.to_owned(),
                    |shortcut| format!("{}    {shortcut}", entry.label),
                );
                div()
                    .id(("palette-option", index))
                    .role(Role::ListItem)
                    .aria_label(entry.semantic_label)
                    .aria_selected(selected)
                    .aria_position_in_set(index + 1)
                    .aria_size_of_set(PALETTE_ENTRIES.len())
                    .when(selected, |row| row.aria_active_descendant())
                    .font_family(MONOSPACE_FONT_FAMILY)
                    .rounded_token(DesignRadius::Md)
                    .border_l_2()
                    .border_color(rgb(if selected {
                        theme.focus_ring.value()
                    } else {
                        theme.border.value()
                    }))
                    .child(
                        Button::new(("palette-command", index))
                            .selected(selected)
                            .label(label)
                            .tooltip(entry.semantic_label)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(RootModalHostEvent::ExecutePalette(command));
                            })),
                    )
            })
            .collect::<Vec<_>>();
        let max_height = px((f32::from(window.viewport_size().height) * 0.8).max(320.));
        let command_palette_overlay = view_model.palette_open.then(|| {
            overlay_surface("command-palette-overlay", &self.command_palette_focus)
                .role(Role::Dialog)
                .aria_label("Command palette")
                .aria_description(
                    "Use Up or Down to select a command, Enter to run it, and Escape to close.",
                )
                .key_context(actions::PALETTE_KEY_CONTEXT)
                .child(
                    div()
                        .id("command-palette-dialog")
                        .w_full()
                        .max_w(px(680.))
                        .max_h(max_height)
                        .overflow_y_scroll()
                        .rounded_token(DesignRadius::Md)
                        .border_1()
                        .border_color(rgb(theme.focus_ring.value()))
                        .bg(rgb(theme.elevated.value()))
                        .p_token(DesignSpace::Lg)
                        .flex()
                        .flex_col()
                        .gap_token(DesignSpace::Sm)
                        .child(
                            div()
                                .text_color(rgb(theme.accent.value()))
                                .child("COMMAND PALETTE · typed desktop actions"),
                        )
                        .child(
                            div()
                                .text_token(DesignText::Body)
                                .text_color(rgb(theme.muted_text.value()))
                                .child("Up/Down or Tab selects · Enter runs · Esc closes"),
                        )
                        .child(
                            div()
                                .id("command-palette-options")
                                .role(Role::List)
                                .aria_label("Available commands")
                                .children(palette_rows),
                        ),
                )
        });
        let search_overlay = view_model.search_open.then(|| {
            overlay_surface("global-search-overlay", &self.search_focus)
                .role(Role::Dialog)
                .aria_label("Search Evo")
                .aria_description(
                    "Search every session. Additional categories such as settings can be added here.",
                )
                .child(
                    div()
                        .id("global-search-dialog")
                        .debug_selector(|| "desktop-global-search-dialog".to_owned())
                        .w_full()
                        .max_w(px(720.))
                        .max_h(max_height)
                        .overflow_hidden()
                        .rounded_token(DesignRadius::Lg)
                        .border_1()
                        .border_color(rgb(theme.focus_ring.value()))
                        .bg(rgb(theme.elevated.value()))
                        .p_token(DesignSpace::Lg)
                        .flex()
                        .flex_col()
                        .gap_token(DesignSpace::Md)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_token(DesignSpace::Md)
                                .child(
                                    div()
                                        .text_token(DesignText::Title)
                                        .text_color(rgb(theme.text.value()))
                                        .child("Search"),
                                )
                                .child(
                                    DesktopIconButton::new(
                                        "close-global-search",
                                        DesktopIcon::Close,
                                        "Close search · Escape",
                                    )
                                    .build()
                                    .debug_selector(|| "desktop-close-global-search".into())
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(RootModalHostEvent::CloseSearch);
                                    })),
                                ),
                        )
                        .child(
                            div()
                                .id("global-search-input")
                                .role(Role::Search)
                                .aria_label("Search all sessions")
                                .child(
                                    Input::new(&search_input)
                                        .role(Role::SearchInput)
                                        .prefix(Icon::new(DesktopIcon::Search.name()).small())
                                        .appearance(false),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .text_token(DesignText::Metadata)
                                .text_color(rgb(theme.muted_text.value()))
                                .child("SESSIONS")
                                .child(format!("{search_result_count} results")),
                        )
                        .child(
                            div()
                                .id("global-search-results")
                                .role(Role::List)
                                .aria_label("Session search results")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap_token(DesignSpace::Xs)
                                .when(search_result_count == 0, |results| {
                                    results.child(
                                        div()
                                            .id("global-search-empty")
                                            .role(Role::Status)
                                            .p_token(DesignSpace::Lg)
                                            .text_color(rgb(theme.muted_text.value()))
                                            .child(if view_model.search_loading {
                                                "Loading sessions…"
                                            } else {
                                                "No sessions match this search."
                                            }),
                                    )
                                })
                                .children(search_rows),
                        ),
                )
        });
        let authorization_overlay =
            view_model.authorization.map(|authorization| {
                let request = authorization.request;
                let decision_pending = authorization.decision_pending;
                let mut details = vec![
                    ("operation", request.operation_id.clone()),
                    (
                        "tool",
                        format!("{} · {}", request.tool_name, request.tool_call_id),
                    ),
                    ("risk", format!("{:?}", request.risk)),
                    ("scope", authorization_scope_text(&request.scope)),
                ];
                if let Some(path) = request.preview.path.as_ref() {
                    details.push(("path", path.clone()));
                }
                if let Some(cwd) = request.preview.cwd.as_ref() {
                    details.push(("cwd", cwd.clone()));
                }
                if let Some(command) = request.preview.command.as_ref() {
                    details.push(("command", command.clone()));
                }
                if let Some(content) = request.preview.content_preview.as_ref() {
                    details.push(("content preview", content.clone()));
                }
                let identity = request.identity();
                let allow_once = identity.clone();
                let allow_operation = identity.clone();
                overlay_surface("authorization-overlay", &self.authorization_focus)
                    .role(Role::AlertDialog)
                    .aria_label("Authorization required")
                    .aria_description(request.preview.summary.clone())
                    .key_context(actions::AUTHORIZATION_KEY_CONTEXT)
                    .child(
                        div()
                            .id("authorization-dialog")
                            .w_full()
                            .max_w(px(720.))
                            .max_h(max_height)
                            .overflow_hidden()
                            .rounded_token(DesignRadius::Md)
                            .border_1()
                            .border_color(rgb(theme.warning.value()))
                            .bg(rgb(theme.elevated.value()))
                            .p_token(DesignSpace::Xl)
                            .flex()
                            .flex_col()
                            .gap_token(DesignSpace::Md)
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .text_color(rgb(theme.warning.value()))
                                    .child("AUTHORIZATION REQUIRED")
                                    .child(if decision_pending {
                                        "decision pending…"
                                    } else {
                                        "explicit decision required"
                                    }),
                            )
                            .child(
                                div()
                                    .text_color(rgb(theme.text.value()))
                                    .whitespace_normal()
                                    .child(request.preview.summary),
                            )
                            .child(
                                div()
                                    .id("authorization-details")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .font_family(MONOSPACE_FONT_FAMILY)
                                    .flex()
                                    .flex_col()
                                    .gap_token(DesignSpace::Sm)
                                    .children(details.into_iter().map(|(term, value)| {
                                        authorization_detail(term, value, theme)
                                    })),
                            )
                            .child(
                                div()
                                    .debug_selector(|| "desktop-authorization-actions".into())
                                    .flex()
                                    .justify_end()
                                    .gap_token(DesignSpace::Sm)
                                    .child(authorization_button(
                                        identity,
                                        ToolAuthorizationDecision::Deny {
                                            reason: Some("denied from native desktop".into()),
                                        },
                                        decision_pending,
                                        cx,
                                    ))
                                    .child(authorization_button(
                                        allow_once,
                                        ToolAuthorizationDecision::AllowOnce,
                                        decision_pending,
                                        cx,
                                    ))
                                    .child(authorization_button(
                                        allow_operation,
                                        ToolAuthorizationDecision::AllowForOperation,
                                        decision_pending,
                                        cx,
                                    )),
                            ),
                    )
            });
        let full_message_overlay = view_model.full_message.as_ref().map(|message| {
            let text = SharedString::new(Arc::clone(&message.text));
            overlay_surface("full-message-overlay", &self.full_message_focus)
                .role(Role::Dialog)
                .aria_label("Full conversation message")
                .aria_description(
                    "Complete bounded message source. Use Copy full message or Escape to close.",
                )
                .child(
                    div()
                        .id("full-message-dialog")
                        .debug_selector(|| "desktop-full-message-dialog".to_owned())
                        .w_full()
                        .max_w(px(1_100.))
                        .max_h(max_height)
                        .overflow_hidden()
                        .rounded_token(DesignRadius::Md)
                        .border_1()
                        .border_color(rgb(theme.focus_ring.value()))
                        .bg(rgb(theme.elevated.value()))
                        .p_token(DesignSpace::Lg)
                        .flex()
                        .flex_col()
                        .gap_token(DesignSpace::Md)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_token(DesignSpace::Md)
                                .child(
                                    div()
                                        .min_w_0()
                                        .text_color(rgb(theme.text.value()))
                                        .child(SharedString::new(Arc::clone(&message.title))),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_token(DesignText::Metadata)
                                        .text_color(rgb(if message.source_truncated {
                                            theme.warning.value()
                                        } else {
                                            theme.muted_text.value()
                                        }))
                                        .child(if message.source_truncated {
                                            "bounded source · additional content unavailable"
                                        } else {
                                            "complete bounded source"
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .id("full-message-scroll")
                                .debug_selector(|| "desktop-full-message-scroll".to_owned())
                                .role(Role::Document)
                                .aria_label("Full message source")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .border_t_1()
                                .border_b_1()
                                .border_color(rgb(theme.divider.value()))
                                .bg(rgb(theme.canvas.value()))
                                .px_token(DesignSpace::Lg)
                                .py_token(DesignSpace::Md)
                                .font_family(MONOSPACE_FONT_FAMILY)
                                .whitespace_normal()
                                .text_color(rgb(theme.text.value()))
                                .child(text),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_token(DesignSpace::Sm)
                                .child(
                                    Button::new("copy-full-message")
                                        .debug_selector(|| "desktop-copy-full-message".into())
                                        .label("Copy full message")
                                        .tooltip("Copy the complete bounded message source")
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(RootModalHostEvent::CopyFullMessage);
                                        })),
                                )
                                .child(
                                    Button::new("close-full-message")
                                        .debug_selector(|| "desktop-close-full-message".into())
                                        .label("Close")
                                        .tooltip("Close full message · Escape")
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(RootModalHostEvent::CloseFullMessage);
                                        })),
                                ),
                        ),
                )
        });

        let delete_confirm_overlay = view_model.delete_confirm.as_ref().map(|confirm| {
            let title = confirm
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| "this session".into());
            overlay_surface("delete-session-overlay", &self.modal_focus)
                .role(Role::AlertDialog)
                .aria_label("Delete session")
                .aria_description(format!(
                    "Delete {title}? This permanently removes the session and its event log."
                ))
                .child(
                    div()
                        .id("delete-session-dialog")
                        .debug_selector(|| "desktop-delete-session-dialog".to_owned())
                        .w_full()
                        .max_w(px(480.))
                        .rounded_token(DesignRadius::Md)
                        .border_1()
                        .border_color(rgb(theme.danger.value()))
                        .bg(rgb(theme.elevated.value()))
                        .p_token(DesignSpace::Xl)
                        .flex()
                        .flex_col()
                        .gap_token(DesignSpace::Md)
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .text_color(rgb(theme.danger.value()))
                                .child("DELETE SESSION")
                                .child(
                                    DesktopIconButton::new(
                                        "close-delete-session-dialog",
                                        DesktopIcon::Close,
                                        "Cancel session deletion",
                                    )
                                    .build()
                                    .debug_selector(|| "desktop-close-delete-session-dialog".into())
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(RootModalHostEvent::CancelDeleteSession);
                                    })),
                                ),
                        )
                        .child(
                            div()
                                .text_color(rgb(theme.text.value()))
                                .whitespace_normal()
                                .child(format!(
                                    "Delete {title}? The session and its event log are removed permanently and cannot be recovered."
                                )),
                        )
                        .child(
                            div()
                                .id("delete-session-identity")
                                .font_family(MONOSPACE_FONT_FAMILY)
                                .text_token(DesignText::Metadata)
                                .text_color(rgb(theme.muted_text.value()))
                                .child(SharedString::new(Arc::from(confirm.session_id.as_str()))),
                        )
                        .child(
                            div()
                                .debug_selector(|| "desktop-delete-session-actions".into())
                                .flex()
                                .justify_end()
                                .gap_token(DesignSpace::Sm)
                                .child(
                                    Button::new("cancel-delete-session")
                                        .debug_selector(|| "desktop-cancel-delete-session".into())
                                        .label("Cancel")
                                        .tooltip("Keep the session · Escape")
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(RootModalHostEvent::CancelDeleteSession);
                                        })),
                                )
                                .child(
                                    DesktopCriticalButton::new(
                                        "confirm-delete-session",
                                        "Delete",
                                        "Permanently delete this session",
                                        DesktopCriticalTone::Dangerous,
                                    )
                                    .build()
                                    .debug_selector(|| "desktop-confirm-delete-session".into())
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(RootModalHostEvent::ConfirmDeleteSession);
                                    })),
                                ),
                        ),
                )
        });

        div()
            .id("root-modal-host")
            .absolute()
            .size_full()
            .children(command_palette_overlay)
            .children(search_overlay)
            .children(full_message_overlay)
            .children(authorization_overlay)
            .children(delete_confirm_overlay)
            .into_any_element()
    }
}

fn filtered_search_sessions<'a>(
    sessions: &'a [GlobalSearchSession],
    query: &str,
) -> Vec<&'a GlobalSearchSession> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return sessions.iter().collect();
    }
    sessions
        .iter()
        .filter(|session| {
            session.name.to_lowercase().contains(&query)
                || session.session_id.to_lowercase().contains(&query)
                || session.workspace.to_lowercase().contains(&query)
        })
        .collect()
}

fn overlay_surface(id: &'static str, focus: &gpui::FocusHandle) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .absolute()
        .size_full()
        .occlude()
        .track_focus(focus)
        .bg(rgba(DESKTOP_OVERLAY_SCRIM_RGBA))
        .p_token(DesignSpace::Lg)
        .flex()
        .items_center()
        .justify_center()
}

fn authorization_button(
    identity: ToolAuthorizationIdentity,
    decision: ToolAuthorizationDecision,
    disabled: bool,
    cx: &gpui::Context<RootModalHost>,
) -> Button {
    let presentation = authorization_decision_presentation(&decision);
    DesktopCriticalButton::new(
        presentation.id,
        presentation.label,
        presentation.tooltip,
        presentation.tone,
    )
    .disabled(disabled)
    .build()
    .debug_selector(move || format!("desktop-hit-{}", presentation.id))
    .child(
        div()
            .rounded_token(DesignRadius::Sm)
            .border_1()
            .px_token(DesignSpace::Xs)
            .font_family(MONOSPACE_FONT_FAMILY)
            .text_token(DesignText::Metadata)
            .child(presentation.shortcut),
    )
    .on_click(cx.listener(move |_, _, _, cx| {
        cx.emit(RootModalHostEvent::DecideAuthorization {
            identity: identity.clone(),
            decision: decision.clone(),
        });
    }))
}

#[derive(Clone, Copy)]
struct AuthorizationDecisionPresentation {
    id: &'static str,
    label: &'static str,
    shortcut: &'static str,
    tooltip: &'static str,
    tone: DesktopCriticalTone,
}

const fn authorization_decision_presentation(
    decision: &ToolAuthorizationDecision,
) -> AuthorizationDecisionPresentation {
    match decision {
        ToolAuthorizationDecision::Deny { .. } => AuthorizationDecisionPresentation {
            id: "deny-authorization",
            label: "Deny",
            shortcut: "1",
            tooltip: "Deny this authorization request · 1",
            tone: DesktopCriticalTone::Dangerous,
        },
        ToolAuthorizationDecision::AllowOnce => AuthorizationDecisionPresentation {
            id: "allow-authorization-once",
            label: "Allow once",
            shortcut: "2",
            tooltip: "Allow this exact request once · 2",
            tone: DesktopCriticalTone::Neutral,
        },
        ToolAuthorizationDecision::AllowForOperation => AuthorizationDecisionPresentation {
            id: "allow-authorization-operation",
            label: "Allow for operation",
            shortcut: "3",
            tooltip: "Allow this scope for the current operation · 3",
            tone: DesktopCriticalTone::Affirmative,
        },
    }
}

fn authorization_detail(term: &'static str, value: String, theme: SemanticTheme) -> gpui::Div {
    div()
        .flex()
        .items_start()
        .gap_token(DesignSpace::Md)
        .child(
            div()
                .debug_selector(move || {
                    format!("desktop-authorization-term-{}", term.replace(' ', "-"))
                })
                .w(px(112.))
                .flex_none()
                .text_color(rgb(theme.subtle_text.value()))
                .child(term),
        )
        .child(
            div()
                .debug_selector(move || {
                    format!("desktop-authorization-value-{}", term.replace(' ', "-"))
                })
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .text_color(rgb(theme.muted_text.value()))
                .child(value),
        )
}

fn authorization_scope_text(scope: &ToolAuthorizationScope) -> String {
    match scope {
        ToolAuthorizationScope::Path { path } => format!("path · {path}"),
        ToolAuthorizationScope::FilesystemTarget {
            path,
            target_fingerprint,
        } => format!("filesystem target · {path} · {target_fingerprint}"),
        ToolAuthorizationScope::Shell {
            cwd,
            command_fingerprint,
        } => format!("shell · {cwd} · {command_fingerprint}"),
        ToolAuthorizationScope::ToolArguments { fingerprint } => {
            format!("tool arguments · {fingerprint}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search_session(id: &str, name: &str, workspace: &str) -> GlobalSearchSession {
        GlobalSearchSession {
            session_id: Arc::from(id),
            name: Arc::from(name),
            workspace: Arc::from(workspace),
        }
    }

    #[test]
    fn global_search_matches_every_session_field_case_insensitively() {
        let sessions = [
            search_session("session-alpha", "Fix Parser", "Compiler"),
            search_session("session-beta", "Write docs", "Website"),
        ];

        assert_eq!(filtered_search_sessions(&sessions, "parser").len(), 1);
        assert_eq!(filtered_search_sessions(&sessions, "SESSION-BETA").len(), 1);
        assert_eq!(filtered_search_sessions(&sessions, "compiler").len(), 1);
        assert_eq!(filtered_search_sessions(&sessions, "").len(), 2);
        assert!(filtered_search_sessions(&sessions, "settings").is_empty());
    }
}
