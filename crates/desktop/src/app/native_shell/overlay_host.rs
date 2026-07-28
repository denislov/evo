use coding_agent::api::authorization::{
    ToolAuthorizationDecision, ToolAuthorizationIdentity, ToolAuthorizationRequest,
    ToolAuthorizationScope,
};
use desktop::shell::{DESKTOP_OVERLAY_SCRIM_RGBA, MONOSPACE_FONT_FAMILY, SemanticTheme};
use gpui::{
    Entity, EventEmitter, FocusHandle, IntoElement, ParentElement as _, Render, Role, SharedString,
    Styled as _, Window, div, prelude::*, px, rgb, rgba,
};
use gpui_component::{Disableable as _, Selectable as _, button::Button};
use std::sync::Arc;

use super::{
    ConversationFullMessageView, DesktopPaletteCommand, InspectorPane, PALETTE_ENTRIES, actions,
    desktop_style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OverlayHostEvent {
    ExecutePalette(DesktopPaletteCommand),
    CopyFullMessage,
    CloseFullMessage,
    DecideAuthorization {
        identity: ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OverlayAuthorizationView {
    pub(super) request: ToolAuthorizationRequest,
    pub(super) decision_pending: bool,
}

#[derive(Debug, Clone)]
pub(super) struct OverlayViewModel {
    pub(super) palette_open: bool,
    pub(super) palette_selected: usize,
    pub(super) narrow_context_open: bool,
    pub(super) narrow_sessions_open: bool,
    pub(super) authorization: Option<OverlayAuthorizationView>,
    pub(super) full_message: Option<ConversationFullMessageView>,
}

pub(super) struct OverlayHost {
    inspector_pane: Entity<InspectorPane>,
    sessions_pane: Entity<super::SessionsPane>,
    authorization_focus: FocusHandle,
    command_palette_focus: FocusHandle,
    narrow_sessions_focus: FocusHandle,
    full_message_focus: FocusHandle,
    view_model: Option<OverlayViewModel>,
}

impl OverlayHost {
    pub(super) fn new(
        inspector_pane: Entity<InspectorPane>,
        sessions_pane: Entity<super::SessionsPane>,
        authorization_focus: FocusHandle,
        command_palette_focus: FocusHandle,
        narrow_sessions_focus: FocusHandle,
        full_message_focus: FocusHandle,
    ) -> Self {
        Self {
            inspector_pane,
            sessions_pane,
            authorization_focus,
            command_palette_focus,
            narrow_sessions_focus,
            full_message_focus,
            view_model: None,
        }
    }

    pub(super) fn set_view_model(&mut self, view_model: OverlayViewModel) {
        self.view_model = Some(view_model);
    }
}

impl EventEmitter<OverlayHostEvent> for OverlayHost {}

impl Render for OverlayHost {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(view_model) = self.view_model.clone() else {
            return div().into_any_element();
        };
        let theme = SemanticTheme::GEEK_DARK;
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
                                cx.emit(OverlayHostEvent::ExecutePalette(command));
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
        let narrow_context_overlay = view_model
            .narrow_context_open
            .then(|| self.inspector_pane.clone());
        let narrow_sessions_overlay = view_model.narrow_sessions_open.then(|| {
            overlay_surface("narrow-sessions-overlay", &self.narrow_sessions_focus)
                .role(Role::Dialog)
                .aria_label("Sessions")
                .aria_description(
                    "Create, refresh, or open a coding session. Escape closes this dialog.",
                )
                .key_context(actions::NARROW_SESSIONS_KEY_CONTEXT)
                .child(
                    div()
                        .id("narrow-sessions-dialog")
                        .w_full()
                        .max_w(px(520.))
                        .max_h(max_height)
                        .overflow_hidden()
                        .rounded_token(DesignRadius::Md)
                        .border_1()
                        .border_color(rgb(theme.focus_ring.value()))
                        .bg(rgb(theme.elevated.value()))
                        .flex()
                        .flex_col()
                        .child(self.sessions_pane.clone()),
                )
        });
        let authorization_overlay = view_model.authorization.map(|authorization| {
            let request = authorization.request;
            let decision_pending = authorization.decision_pending;
            let mut details = vec![
                format!("operation  {}", request.operation_id),
                format!(
                    "tool       {} · {}",
                    request.tool_name, request.tool_call_id
                ),
                format!("risk       {:?}", request.risk),
                format!("scope      {}", authorization_scope_text(&request.scope)),
            ];
            if let Some(path) = request.preview.path.as_ref() {
                details.push(format!("path       {path}"));
            }
            if let Some(cwd) = request.preview.cwd.as_ref() {
                details.push(format!("cwd        {cwd}"));
            }
            if let Some(command) = request.preview.command.as_ref() {
                details.push(format!("command\n{command}"));
            }
            if let Some(content) = request.preview.content_preview.as_ref() {
                details.push(format!("content preview\n{content}"));
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
                                .children(details.into_iter().map(|detail| {
                                    div()
                                        .whitespace_normal()
                                        .text_color(rgb(theme.muted_text.value()))
                                        .child(detail)
                                })),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_token(DesignSpace::Sm)
                                .child(authorization_button(
                                    "deny-authorization",
                                    "1 · Deny",
                                    "Deny this authorization request · 1",
                                    identity,
                                    ToolAuthorizationDecision::Deny {
                                        reason: Some("denied from native desktop".into()),
                                    },
                                    decision_pending,
                                    cx,
                                ))
                                .child(authorization_button(
                                    "allow-authorization-once",
                                    "2 · Allow once",
                                    "Allow this exact request once · 2",
                                    allow_once,
                                    ToolAuthorizationDecision::AllowOnce,
                                    decision_pending,
                                    cx,
                                ))
                                .child(authorization_button(
                                    "allow-authorization-operation",
                                    "3 · Allow for operation",
                                    "Allow this scope for the current operation · 3",
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
                                .rounded_token(DesignRadius::Md)
                                .border_1()
                                .border_color(rgb(theme.border.value()))
                                .bg(rgb(theme.canvas.value()))
                                .p_token(DesignSpace::Lg)
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
                                            cx.emit(OverlayHostEvent::CopyFullMessage);
                                        })),
                                )
                                .child(
                                    Button::new("close-full-message")
                                        .debug_selector(|| "desktop-close-full-message".into())
                                        .label("Close")
                                        .tooltip("Close full message · Escape")
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(OverlayHostEvent::CloseFullMessage);
                                        })),
                                ),
                        ),
                )
        });

        div()
            .id("overlay-host")
            .absolute()
            .size_full()
            .children(narrow_context_overlay)
            .children(narrow_sessions_overlay)
            .children(command_palette_overlay)
            .children(full_message_overlay)
            .children(authorization_overlay)
            .into_any_element()
    }
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
    id: &'static str,
    label: &'static str,
    tooltip: &'static str,
    identity: ToolAuthorizationIdentity,
    decision: ToolAuthorizationDecision,
    disabled: bool,
    cx: &gpui::Context<OverlayHost>,
) -> Button {
    Button::new(id)
        .label(label)
        .tooltip(tooltip)
        .disabled(disabled)
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.emit(OverlayHostEvent::DecideAuthorization {
                identity: identity.clone(),
                decision: decision.clone(),
            });
        }))
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
