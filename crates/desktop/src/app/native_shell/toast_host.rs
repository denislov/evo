use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use desktop::shell::{SemanticTheme, UI_FONT_FAMILY};
use gpui::{
    Context, FocusHandle, IntoElement, ParentElement as _, Render, Role, Styled as _, Subscription,
    Window, div, prelude::*, px, rgb,
};

use super::{
    desktop_controls::{DesktopControlSize, DesktopIcon, DesktopIconButton},
    desktop_style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
};

/// Product choice for VUI-302. Keep these policy values centralized so the
/// presentation can change without touching any notice-producing path.
pub(super) const MAX_VISIBLE_TOASTS: usize = 3;
pub(super) const TOAST_LIFETIME: Duration = Duration::from_secs(6);
const MAX_NOTICE_SOURCES: usize = 32;
const MAX_TOAST_WIDTH: u32 = 420;
const TOAST_EDGE_INSET: u32 = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ToastNotice {
    pub(super) session_id: Arc<str>,
    pub(super) revision: u64,
    pub(super) message: Arc<str>,
}

#[derive(Debug, Clone)]
struct ToastEntry {
    id: u64,
    message: Arc<str>,
    expires_at: Instant,
}

pub(super) struct ToastHost {
    focus: FocusHandle,
    toasts: VecDeque<ToastEntry>,
    seen_sources: VecDeque<(Arc<str>, u64)>,
    next_id: u64,
    hovered: bool,
    focus_within: bool,
    paused_at: Option<Instant>,
    scheduled_deadline: Option<Instant>,
    expiry_generation: u64,
    _subscriptions: Vec<Subscription>,
}

impl ToastHost {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let focus_in = cx.on_focus_in(&focus, window, |this, _, cx| {
            this.set_focus_within(true, cx);
        });
        let focus_out = cx.on_focus_out(&focus, window, |this, _, _, cx| {
            this.set_focus_within(false, cx);
        });
        Self {
            focus,
            toasts: VecDeque::with_capacity(MAX_VISIBLE_TOASTS),
            seen_sources: VecDeque::with_capacity(MAX_NOTICE_SOURCES),
            next_id: 1,
            hovered: false,
            focus_within: false,
            paused_at: None,
            scheduled_deadline: None,
            expiry_generation: 0,
            _subscriptions: vec![focus_in, focus_out],
        }
    }

    pub(super) fn observe_notice(&mut self, notice: Option<ToastNotice>, cx: &mut Context<Self>) {
        let Some(notice) = notice else {
            return;
        };
        if let Some(index) = self
            .seen_sources
            .iter()
            .position(|(session_id, _)| session_id == &notice.session_id)
        {
            let Some((session_id, revision)) = self.seen_sources.remove(index) else {
                return;
            };
            self.seen_sources
                .push_back((Arc::clone(&session_id), notice.revision));
            if revision == notice.revision {
                return;
            }
        } else {
            if self.seen_sources.len() == MAX_NOTICE_SOURCES {
                self.seen_sources.pop_front();
            }
            self.seen_sources
                .push_back((Arc::clone(&notice.session_id), notice.revision));
        }
        self.push(notice.message, cx);
    }

    fn push(&mut self, message: Arc<str>, cx: &mut Context<Self>) {
        if self.toasts.len() == MAX_VISIBLE_TOASTS {
            self.toasts.pop_front();
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let now = Instant::now();
        let expiry_origin = self.paused_at.unwrap_or(now);
        self.toasts.push_back(ToastEntry {
            id,
            message,
            expires_at: expiry_origin + TOAST_LIFETIME,
        });
        self.arm_expiry(cx);
        cx.notify();
    }

    fn dismiss(&mut self, id: u64, cx: &mut Context<Self>) {
        self.toasts.retain(|toast| toast.id != id);
        self.invalidate_expiry();
        self.arm_expiry(cx);
        cx.notify();
    }

    fn set_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if self.hovered == hovered {
            return;
        }
        let was_paused = self.is_paused();
        self.hovered = hovered;
        self.reconcile_pause(was_paused, cx);
    }

    fn set_focus_within(&mut self, focus_within: bool, cx: &mut Context<Self>) {
        if self.focus_within == focus_within {
            return;
        }
        let was_paused = self.is_paused();
        self.focus_within = focus_within;
        self.reconcile_pause(was_paused, cx);
    }

    const fn is_paused(&self) -> bool {
        self.hovered || self.focus_within
    }

    fn reconcile_pause(&mut self, was_paused: bool, cx: &mut Context<Self>) {
        let is_paused = self.is_paused();
        if was_paused == is_paused {
            return;
        }
        let now = Instant::now();
        if is_paused {
            self.paused_at = Some(now);
            self.invalidate_expiry();
        } else if let Some(paused_at) = self.paused_at.take() {
            let paused_for = now.saturating_duration_since(paused_at);
            for toast in &mut self.toasts {
                toast.expires_at += paused_for;
            }
            self.arm_expiry(cx);
        }
        cx.notify();
    }

    fn invalidate_expiry(&mut self) {
        self.expiry_generation = self.expiry_generation.wrapping_add(1);
        self.scheduled_deadline = None;
    }

    fn arm_expiry(&mut self, cx: &mut Context<Self>) {
        if self.is_paused() {
            return;
        }
        let Some(deadline) = self.toasts.iter().map(|toast| toast.expires_at).min() else {
            self.scheduled_deadline = None;
            return;
        };
        if self.scheduled_deadline == Some(deadline) {
            return;
        }
        self.expiry_generation = self.expiry_generation.wrapping_add(1);
        let generation = self.expiry_generation;
        self.scheduled_deadline = Some(deadline);
        let delay = deadline.saturating_duration_since(Instant::now());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                if this.expiry_generation != generation || this.scheduled_deadline != Some(deadline)
                {
                    return;
                }
                let now = Instant::now();
                this.scheduled_deadline = None;
                this.toasts.retain(|toast| toast.expires_at > now);
                this.arm_expiry(cx);
                cx.notify();
            });
        })
        .detach();
    }

    #[cfg(test)]
    pub(super) fn messages(&self) -> Vec<Arc<str>> {
        self.toasts
            .iter()
            .map(|toast| Arc::clone(&toast.message))
            .collect()
    }
}

impl Render for ToastHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = SemanticTheme::GEEK_DARK;
        let width = u32::from(window.viewport_size().width)
            .saturating_sub(TOAST_EDGE_INSET.saturating_mul(2))
            .min(MAX_TOAST_WIDTH);
        let rows = self
            .toasts
            .iter()
            .map(|toast| {
                let id = toast.id;
                let message = Arc::clone(&toast.message);
                div()
                    .id(("desktop-toast", id as usize))
                    .debug_selector(move || format!("desktop-toast-{id}"))
                    .role(Role::Status)
                    .aria_label(message.clone())
                    .w_full()
                    .min_h(px(44.))
                    .px_token(DesignSpace::Md)
                    .py_token(DesignSpace::Sm)
                    .flex()
                    .items_start()
                    .gap_token(DesignSpace::Sm)
                    .rounded_token(DesignRadius::Md)
                    .border_1()
                    .border_color(rgb(theme.divider.value()))
                    .bg(rgb(theme.elevated.value()))
                    .text_color(rgb(theme.text.value()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_token(DesignText::Body)
                            .child(message.to_string()),
                    )
                    .child(
                        DesktopIconButton::new(
                            ("dismiss-toast", id as usize),
                            DesktopIcon::Close,
                            "Dismiss notification",
                        )
                        .size(DesktopControlSize::Tool)
                        .build()
                        .tab_index(5)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dismiss(id, cx);
                        })),
                    )
            })
            .collect::<Vec<_>>();

        div()
            .id("desktop-toast-host")
            .debug_selector(|| "desktop-toast-host".into())
            .track_focus(&self.focus)
            .absolute()
            .right(px(TOAST_EDGE_INSET as f32))
            .bottom(px(TOAST_EDGE_INSET as f32))
            .w(px(width as f32))
            .flex()
            .flex_col()
            .gap_token(DesignSpace::Sm)
            .font_family(UI_FONT_FAMILY)
            .on_hover(cx.listener(|this, hovered, _, cx| {
                this.set_hovered(*hovered, cx);
            }))
            .children(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_is_bounded_and_transient() {
        assert_eq!(MAX_VISIBLE_TOASTS, 3);
        assert_eq!(TOAST_LIFETIME, Duration::from_secs(6));
    }
}
