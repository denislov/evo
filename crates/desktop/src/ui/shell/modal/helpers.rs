//! Modal overlay construction helpers: search filtering, focus surfaces, and
//! authorization decision presentation.

use coding_agent::api::authorization::{
    ToolAuthorizationDecision, ToolAuthorizationIdentity, ToolAuthorizationScope,
};
use desktop::ui::shell::{DESKTOP_OVERLAY_SCRIM_RGBA, MONOSPACE_FONT_FAMILY, SemanticTheme};
use gpui::{ParentElement as _, Styled as _, div, prelude::*, px, rgb, rgba};
use gpui_component::button::Button;

use super::{GlobalSearchSession, RootModalHost, RootModalHostEvent};
use crate::ui::components::{
    controls::{DesktopCriticalButton, DesktopCriticalTone},
    style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
};

pub(super) fn filtered_search_sessions<'a>(
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

pub(super) fn overlay_surface(
    id: &'static str,
    focus: &gpui::FocusHandle,
) -> gpui::Stateful<gpui::Div> {
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

pub(super) fn authorization_button(
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
pub(super) struct AuthorizationDecisionPresentation {
    id: &'static str,
    label: &'static str,
    shortcut: &'static str,
    tooltip: &'static str,
    tone: DesktopCriticalTone,
}

pub(super) const fn authorization_decision_presentation(
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

pub(super) fn authorization_detail(
    term: &'static str,
    value: String,
    theme: SemanticTheme,
) -> gpui::Div {
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

pub(super) fn authorization_scope_text(scope: &ToolAuthorizationScope) -> String {
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
