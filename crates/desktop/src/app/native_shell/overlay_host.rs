use coding_agent::api::authorization::{
    ToolAuthorizationDecision, ToolAuthorizationIdentity, ToolAuthorizationScope,
};
use desktop::shell::{SemanticTheme, truncate_label};
use gpui::{
    EventEmitter, IntoElement, ParentElement as _, Render, Styled as _, WeakEntity, Window, div,
    prelude::*, px, rgb, rgba,
};
use gpui_component::{Disableable as _, button::Button};

use super::{DesktopCommandIntent, DesktopPaletteCommand, NativeShell, PALETTE_ENTRIES, actions};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OverlayHostEvent {
    ExecutePalette(DesktopPaletteCommand),
    CreateSession,
    RefreshSessions,
    OpenSession(String),
    DecideAuthorization {
        identity: ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
    },
}

pub(super) struct OverlayHost {
    owner: WeakEntity<NativeShell>,
}

impl OverlayHost {
    pub(super) fn new(owner: WeakEntity<NativeShell>) -> Self {
        Self { owner }
    }
}

impl EventEmitter<OverlayHostEvent> for OverlayHost {}

impl Render for OverlayHost {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(owner) = self.owner.upgrade() else {
            return div().into_any_element();
        };
        let owner = owner.read(cx);
        let theme = SemanticTheme::GEEK_DARK;
        let snapshot = owner.projection.snapshot();
        let composer_running = snapshot.active_operation.is_some();
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
        let active_session_id = snapshot.session.session_id.as_str();
        let narrow_session_rows = owner
            .session_catalog
            .iter()
            .enumerate()
            .map(|(index, session)| {
                let target = session.session_id.clone();
                let active = target == active_session_id;
                Button::new(("narrow-open-session", index))
                    .label(format!(
                        "{} {} · {}",
                        if active { "●" } else { "○" },
                        truncate_label(&target, 32),
                        truncate_label(&session.updated_at, 20)
                    ))
                    .tooltip(if active {
                        "Active coding-agent session"
                    } else {
                        "Open this coding-agent session"
                    })
                    .disabled(
                        active || composer_running || awaiting_prompt_start || session_pending,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(OverlayHostEvent::OpenSession(target.clone()));
                    }))
            })
            .collect::<Vec<_>>();
        let palette_rows = PALETTE_ENTRIES
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let command = entry.command;
                let selected = owner.command_palette.selected() == index;
                let label = entry.shortcut.map_or_else(
                    || entry.label.to_owned(),
                    |shortcut| format!("{}    {shortcut}", entry.label),
                );
                div()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if selected {
                        theme.focus_ring.value()
                    } else {
                        theme.border.value()
                    }))
                    .child(
                        Button::new(("palette-command", index))
                            .label(label)
                            .tooltip(entry.semantic_label)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(OverlayHostEvent::ExecutePalette(command));
                            })),
                    )
            })
            .collect::<Vec<_>>();
        let max_height = px((f32::from(window.viewport_size().height) * 0.8).max(320.));
        let command_palette_overlay = owner.command_palette.is_open().then(|| {
            overlay_surface("command-palette-overlay", &owner.command_palette_focus)
                .key_context(actions::PALETTE_KEY_CONTEXT)
                .child(
                    div()
                        .id("command-palette-dialog")
                        .w_full()
                        .max_w(px(680.))
                        .max_h(max_height)
                        .overflow_y_scroll()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(theme.focus_ring.value()))
                        .bg(rgb(theme.elevated.value()))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_color(rgb(theme.accent.value()))
                                .child("COMMAND PALETTE · typed desktop actions"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(theme.muted_text.value()))
                                .child("Up/Down or Tab selects · Enter runs · Esc closes"),
                        )
                        .children(palette_rows),
                )
        });
        let omitted_sessions = owner.omitted_sessions;
        let narrow_context_overlay = owner
            .narrow_context_open
            .then(|| owner.inspector_pane.clone());
        let narrow_sessions_overlay = owner.narrow_sessions_open.then(|| {
            overlay_surface("narrow-sessions-overlay", &owner.narrow_sessions_focus)
                .key_context(actions::NARROW_SESSIONS_KEY_CONTEXT)
                .child(
                    div()
                        .id("narrow-sessions-dialog")
                        .w_full()
                        .max_w(px(520.))
                        .max_h(max_height)
                        .overflow_y_scroll()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(theme.focus_ring.value()))
                        .bg(rgb(theme.elevated.value()))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .text_color(rgb(theme.accent.value()))
                                .child("SESSIONS · narrow layout dialog")
                                .child("Esc closes"),
                        )
                        .child(
                            Button::new("narrow-create-session")
                                .label("New session · Ctrl/Cmd+N")
                                .tooltip("Create a new coding-agent session")
                                .disabled(
                                    composer_running || awaiting_prompt_start || session_pending,
                                )
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(OverlayHostEvent::CreateSession);
                                })),
                        )
                        .child(
                            Button::new("narrow-refresh-sessions")
                                .label(if session_catalog_pending {
                                    "Loading sessions…"
                                } else {
                                    "Refresh sessions"
                                })
                                .tooltip("Load the bounded project session catalog")
                                .disabled(session_catalog_pending || composer_running)
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(OverlayHostEvent::RefreshSessions);
                                })),
                        )
                        .children(narrow_session_rows)
                        .when(omitted_sessions > 0, |dialog| {
                            dialog
                                .child(div().text_color(rgb(theme.warning.value())).child(format!(
                                "{omitted_sessions} older session(s) omitted at the desktop limit"
                            )))
                        }),
                )
        });
        let authorization_overlay =
            snapshot
                .pending_authorizations
                .first()
                .cloned()
                .map(|request| {
                    let decision_pending = owner.command_ledger.authorization().is_some_and(
                        |(_, authorization_id, operation_id)| {
                            authorization_id == request.authorization_id
                                && operation_id == request.operation_id
                        },
                    );
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
                    overlay_surface("authorization-overlay", &owner.authorization_focus)
                        .key_context(actions::AUTHORIZATION_KEY_CONTEXT)
                        .child(
                            div()
                                .w_full()
                                .max_w(px(720.))
                                .max_h(max_height)
                                .overflow_hidden()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(theme.warning.value()))
                                .bg(rgb(theme.elevated.value()))
                                .p_5()
                                .flex()
                                .flex_col()
                                .gap_3()
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
                                        .flex()
                                        .flex_col()
                                        .gap_2()
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
                                        .gap_2()
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

        div()
            .id("overlay-host")
            .absolute()
            .size_full()
            .children(narrow_context_overlay)
            .children(narrow_sessions_overlay)
            .children(command_palette_overlay)
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
        .bg(rgba(0x0b0e14dd))
        .p_4()
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
