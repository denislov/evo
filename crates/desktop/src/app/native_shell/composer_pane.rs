use desktop::shell::{COMPOSER_MAX_HEIGHT, COMPOSER_MIN_HEIGHT, SemanticTheme};
use gpui::{
    EventEmitter, FocusHandle, Focusable as _, IntoElement, ParentElement as _, Render, Role,
    Styled as _, Subscription, Window, div, prelude::*, px, rgb,
};
use gpui_component::{
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
};
use std::{
    cell::Cell,
    sync::Arc,
    time::{Duration, Instant},
};

use super::{
    ComposerRunningMode,
    desktop_controls::{
        DesktopControlSize, DesktopControlWeight, DesktopIcon, DesktopIconButton, DesktopSelector,
    },
    desktop_style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ComposerPaneEvent {
    InputChanged(String),
    Focused,
    SubmitPrimary,
    Submit,
    SubmitRunning,
    SetRunningMode(ComposerRunningMode),
    CycleThinking,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ComposerPaneViewModel {
    pub(super) composer_pending: bool,
    pub(super) composer_running: bool,
    pub(super) awaiting_prompt_start: bool,
    pub(super) authorization_pending: bool,
    pub(super) running_mode: ComposerRunningMode,
    pub(super) thinking: Arc<str>,
    pub(super) rejection: Option<Arc<str>>,
    pub(super) keyboard_focus_visible: bool,
}

#[derive(Debug, Default)]
pub(super) struct InputRenderLatencyProbe {
    pending_change: Cell<Option<Instant>>,
    #[cfg(test)]
    last_observed: Cell<Option<Duration>>,
}

impl InputRenderLatencyProbe {
    fn mark_changed(&self) {
        self.mark_changed_at(Instant::now());
    }

    pub(super) fn mark_changed_at(&self, now: Instant) {
        self.pending_change.set(Some(now));
    }

    fn observe_render(&self) {
        let _ = self.observe_render_at(Instant::now());
    }

    pub(super) fn observe_render_at(&self, now: Instant) -> Option<Duration> {
        let latency = now.saturating_duration_since(self.pending_change.take()?);
        tracing::trace!(
            target: "desktop",
            latency_micros = u64::try_from(latency.as_micros()).unwrap_or(u64::MAX),
            "desktop.input.to_render"
        );
        #[cfg(test)]
        self.last_observed.set(Some(latency));
        Some(latency)
    }

    #[cfg(test)]
    pub(super) fn pending_is_empty(&self) -> bool {
        self.pending_change.get().is_none()
    }

    #[cfg(test)]
    pub(super) fn last_observed(&self) -> Option<Duration> {
        self.last_observed.get()
    }

    #[cfg(test)]
    pub(super) fn clear_last_observed(&self) {
        self.last_observed.set(None);
    }
}

pub(super) struct ComposerPane {
    input: gpui::Entity<InputState>,
    focus: FocusHandle,
    latency: InputRenderLatencyProbe,
    view_model: Option<ComposerPaneViewModel>,
    _input_subscription: Subscription,
}

impl ComposerPane {
    pub(super) fn new(window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(1, 8)
                .placeholder("Describe the change you want to make…")
        });
        let focus = input.focus_handle(cx).clone();
        let input_subscription = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    let _span = tracing::trace_span!("desktop.input.change").entered();
                    this.latency.mark_changed();
                    cx.emit(ComposerPaneEvent::InputChanged(
                        input.read(cx).value().to_string(),
                    ));
                    cx.notify();
                }
                InputEvent::Focus => cx.emit(ComposerPaneEvent::Focused),
                InputEvent::PressEnter {
                    secondary: true, ..
                } => cx.emit(ComposerPaneEvent::SubmitPrimary),
                InputEvent::Blur => cx.notify(),
                InputEvent::PressEnter {
                    secondary: false, ..
                } => {}
            },
        );
        Self {
            input,
            focus,
            latency: InputRenderLatencyProbe::default(),
            view_model: None,
            _input_subscription: input_subscription,
        }
    }

    pub(super) fn set_view_model(&mut self, view_model: ComposerPaneViewModel) {
        self.view_model = Some(view_model);
    }

    pub(super) fn set_input_value(
        &mut self,
        value: String,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        self.input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }

    pub(super) fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    #[cfg(test)]
    pub(super) fn latency_probe(&self) -> &InputRenderLatencyProbe {
        &self.latency
    }
}

impl EventEmitter<ComposerPaneEvent> for ComposerPane {}

impl Render for ComposerPane {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(view_model) = self.view_model.clone() else {
            return div()
                .min_h(px(COMPOSER_MIN_HEIGHT as f32))
                .into_any_element();
        };
        self.latency.observe_render();
        let input = self.input.clone();
        let composer_pending = view_model.composer_pending;
        let composer_running = view_model.composer_running;
        let awaiting_prompt_start = view_model.awaiting_prompt_start;
        let authorization_pending = view_model.authorization_pending;
        let running_mode = view_model.running_mode;
        let thinking = view_model.thinking;
        let rejection = view_model.rejection;
        let keyboard_focus_visible = view_model.keyboard_focus_visible;
        let composer_disabled = composer_pending || awaiting_prompt_start;
        let composer_focused = input.focus_handle(cx).is_focused(window) && keyboard_focus_visible;
        let theme = SemanticTheme::GEEK_DARK;
        let running_action_label = match running_mode {
            ComposerRunningMode::SteerNow => "Steer now",
            ComposerRunningMode::QueueNext => "Queue next",
        };
        let state_notice = if awaiting_prompt_start {
            Some("Waiting for the operation to start…")
        } else if authorization_pending {
            Some("Authorization required · draft remains editable")
        } else if composer_pending {
            Some("Submitting draft…")
        } else {
            None
        };
        let event_target_for_steer = cx.entity().downgrade();
        let event_target_for_queue = cx.entity().downgrade();
        let composer_notice = rejection
            .as_deref()
            .map(|notice| (notice, theme.danger))
            .or_else(|| state_notice.map(|notice| (notice, theme.muted_text)));

        div()
            .id("composer-panel")
            .role(Role::Form)
            .aria_label("Message composer")
            .aria_description(
                state_notice.unwrap_or("Describe the coding change, then send the message."),
            )
            .debug_selector(|| "desktop-composer-panel".into())
            .min_h(px(COMPOSER_MIN_HEIGHT as f32))
            .max_h(px(COMPOSER_MAX_HEIGHT as f32))
            .flex_shrink_0()
            .px_token(DesignSpace::Lg)
            .py_token(DesignSpace::Md)
            .border_t_1()
            .border_color(rgb(if composer_focused {
                theme.focus_ring.value()
            } else {
                theme.divider.value()
            }))
            .bg(rgb(theme.canvas.value()))
            .flex()
            .flex_col()
            .child(
                div()
                    .debug_selector(|| "desktop-composer-surface".into())
                    .w_full()
                    .flex()
                    .flex_col()
                    .rounded_token(DesignRadius::Lg)
                    .border_1()
                    .border_color(rgb(if composer_focused {
                        theme.focus_ring.value()
                    } else {
                        theme.border.value()
                    }))
                    .bg(rgb(theme.elevated.value()))
                    .p_token(DesignSpace::Sm)
                    .gap_token(DesignSpace::Xs)
                    .when_some(composer_notice, |surface, (notice, color)| {
                        surface.child(
                            div()
                                .id("composer-state-notice")
                                .debug_selector(|| "desktop-composer-state-notice".into())
                                .role(Role::Status)
                                .aria_label(notice.to_owned())
                                .w_full()
                                .px_token(DesignSpace::Sm)
                                .py_token(DesignSpace::Xs)
                                .border_b_1()
                                .border_color(rgb(theme.divider.value()))
                                .text_token(DesignText::Metadata)
                                .text_color(rgb(color.value()))
                                .whitespace_normal()
                                .child(notice.to_owned()),
                        )
                    })
                    .child(
                        div()
                            .debug_selector(|| "desktop-composer-content".into())
                            .min_h(px(48.))
                            .w_full()
                            .flex()
                            .items_end()
                            .gap_token(DesignSpace::Sm)
                            .child(
                                div()
                                    .debug_selector(|| "desktop-composer-input-region".into())
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        Input::new(&input)
                                            .appearance(false)
                                            .bordered(false)
                                            .focus_bordered(false)
                                            .disabled(composer_disabled),
                                    ),
                            )
                            .child(
                                div()
                                    .debug_selector(|| "desktop-composer-actions".into())
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap_token(DesignSpace::Xs)
                                    .child(
                                        DesktopSelector::new(
                                            "cycle-thinking",
                                            format!("T {thinking}"),
                                            "Cycle the composer thinking override",
                                        )
                                        .build()
                                        .debug_selector(|| "desktop-composer-thinking".into())
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(ComposerPaneEvent::CycleThinking);
                                        })),
                                    )
                                    .when(composer_running, |actions| {
                                        actions.child(
                                            DesktopSelector::new(
                                                "composer-running-mode-selector",
                                                running_action_label,
                                                "Choose how this draft enters the active operation",
                                            )
                                            .disabled(composer_pending)
                                            .build()
                                            .debug_selector(|| {
                                                "desktop-composer-running-mode-selector".into()
                                            })
                                            .dropdown_menu(move |menu, _, _| {
                                                let steer_target =
                                                    event_target_for_steer.clone();
                                                let queue_target =
                                                    event_target_for_queue.clone();
                                                menu.item(
                                                    PopupMenuItem::new("Steer now")
                                                        .checked(
                                                            running_mode
                                                                == ComposerRunningMode::SteerNow,
                                                        )
                                                        .on_click(move |_, _, cx| {
                                                            if let Some(target) =
                                                                steer_target.upgrade()
                                                            {
                                                                target.update(cx, |_, cx| {
                                                                    cx.emit(
                                                                        ComposerPaneEvent::SetRunningMode(
                                                                            ComposerRunningMode::SteerNow,
                                                                        ),
                                                                    );
                                                                });
                                                            }
                                                        }),
                                                )
                                                .item(
                                                    PopupMenuItem::new("Queue next")
                                                        .checked(
                                                            running_mode
                                                                == ComposerRunningMode::QueueNext,
                                                        )
                                                        .on_click(move |_, _, cx| {
                                                            if let Some(target) =
                                                                queue_target.upgrade()
                                                            {
                                                                target.update(cx, |_, cx| {
                                                                    cx.emit(
                                                                        ComposerPaneEvent::SetRunningMode(
                                                                            ComposerRunningMode::QueueNext,
                                                                        ),
                                                                    );
                                                                });
                                                            }
                                                        }),
                                                )
                                            }),
                                        )
                                    })
                                    .when(!composer_running, |actions| {
                                        actions.child(
                                            DesktopIconButton::new(
                                                "submit-composer",
                                                DesktopIcon::Submit,
                                                "Send the composer draft · Ctrl/Cmd+Enter",
                                            )
                                            .size(DesktopControlSize::Standard)
                                            .weight(DesktopControlWeight::Primary)
                                            .busy(composer_disabled)
                                            .build()
                                            .debug_selector(|| {
                                                "desktop-hit-submit-composer".into()
                                            })
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(ComposerPaneEvent::Submit);
                                            })),
                                        )
                                    })
                                    .when(composer_running, |actions| {
                                        actions.child(
                                            DesktopIconButton::new(
                                                "submit-running-composer",
                                                DesktopIcon::Submit,
                                                format!("Submit using {running_action_label}"),
                                            )
                                            .size(DesktopControlSize::Standard)
                                            .weight(DesktopControlWeight::Primary)
                                            .busy(composer_disabled)
                                            .build()
                                            .debug_selector(|| {
                                                "desktop-hit-submit-running-composer".into()
                                            })
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(ComposerPaneEvent::SubmitRunning);
                                            })),
                                        )
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }
}
