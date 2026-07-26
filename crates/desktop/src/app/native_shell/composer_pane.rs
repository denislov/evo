use desktop::conversation::ComposerAdmission;
use desktop::shell::{COMPOSER_MAX_HEIGHT, COMPOSER_MIN_HEIGHT, SemanticTheme, truncate_label};
use gpui::{
    EventEmitter, Focusable as _, IntoElement, ParentElement as _, Render, Role, Styled as _,
    WeakEntity, Window, div, prelude::*, px, rgb,
};
use gpui_component::{Disableable as _, button::Button, input::Input};

use super::{ComposerRunningMode, NativeShell};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ComposerPaneEvent {
    Submit,
    SubmitRunning,
    SetRunningMode(ComposerRunningMode),
}

pub(super) struct ComposerPane {
    owner: WeakEntity<NativeShell>,
}

impl ComposerPane {
    pub(super) fn new(owner: WeakEntity<NativeShell>) -> Self {
        Self { owner }
    }
}

impl EventEmitter<ComposerPaneEvent> for ComposerPane {}

impl Render for ComposerPane {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(owner) = self.owner.upgrade() else {
            return div()
                .min_h(px(COMPOSER_MIN_HEIGHT as f32))
                .into_any_element();
        };
        let (
            input,
            composer_pending,
            composer_running,
            awaiting_prompt_start,
            authorization_pending,
            running_mode,
            rejection,
            keyboard_focus_visible,
        ) = {
            let owner = owner.read(cx);
            owner.composer_input_latency.observe_render();
            (
                owner.composer_input.clone(),
                matches!(
                    owner.composer.admission(),
                    ComposerAdmission::Pending { .. }
                ),
                owner.projection.snapshot().active_operation.is_some(),
                owner.composer.submitted().is_some()
                    && owner.projection.snapshot().active_operation.is_none(),
                !owner
                    .projection
                    .snapshot()
                    .pending_authorizations
                    .is_empty(),
                owner.active_composer_running_mode(),
                owner.composer.rejection().map(str::to_owned),
                owner.keyboard_focus_visible(),
            )
        };
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
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(rgb(if composer_focused {
                theme.focus_ring.value()
            } else {
                theme.border.value()
            }))
            .bg(rgb(theme.canvas.value()))
            .child(
                div()
                    .w_full()
                    .flex()
                    .gap_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(theme.border.value()))
                    .bg(rgb(theme.elevated.value()))
                    .p_2()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&input)
                                .appearance(false)
                                .bordered(false)
                                .focus_bordered(false)
                                .disabled(composer_disabled),
                        ),
                    )
                    .child(
                        div()
                            .w(px(188.))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .justify_end()
                            .when(!composer_running, |actions| {
                                actions.child(
                                    Button::new("submit-composer")
                                        .debug_selector(|| "desktop-hit-submit-composer".into())
                                        .label(if composer_pending {
                                            "Sending…"
                                        } else {
                                            "Send"
                                        })
                                        .tooltip("Send the composer draft · Ctrl/Cmd+Enter")
                                        .disabled(composer_disabled)
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(ComposerPaneEvent::Submit);
                                        })),
                                )
                            })
                            .when(composer_running, |actions| {
                                actions
                                    .child(
                                        div()
                                            .flex()
                                            .gap_1()
                                            .child(
                                                Button::new("composer-mode-steer")
                                                    .compact()
                                                    .label(
                                                        if running_mode
                                                            == ComposerRunningMode::SteerNow
                                                        {
                                                            "● Steer now"
                                                        } else {
                                                            "○ Steer now"
                                                        },
                                                    )
                                                    .tooltip(
                                                        "Send input to the active operation now",
                                                    )
                                                    .disabled(composer_pending)
                                                    .on_click(cx.listener(|_, _, _, cx| {
                                                        cx.emit(ComposerPaneEvent::SetRunningMode(
                                                            ComposerRunningMode::SteerNow,
                                                        ));
                                                    })),
                                            )
                                            .child(
                                                Button::new("composer-mode-follow-up")
                                                    .compact()
                                                    .label(
                                                        if running_mode
                                                            == ComposerRunningMode::QueueNext
                                                        {
                                                            "● Queue next"
                                                        } else {
                                                            "○ Queue next"
                                                        },
                                                    )
                                                    .tooltip(
                                                        "Queue input after the active operation",
                                                    )
                                                    .disabled(composer_pending)
                                                    .on_click(cx.listener(|_, _, _, cx| {
                                                        cx.emit(ComposerPaneEvent::SetRunningMode(
                                                            ComposerRunningMode::QueueNext,
                                                        ));
                                                    })),
                                            ),
                                    )
                                    .child(
                                        Button::new("submit-running-composer")
                                            .label(if composer_pending {
                                                "Sending…"
                                            } else {
                                                running_action_label
                                            })
                                            .tooltip("Submit using the selected running mode")
                                            .disabled(composer_disabled)
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(ComposerPaneEvent::SubmitRunning);
                                            })),
                                    )
                            })
                            .when_some(state_notice, |actions, notice| {
                                actions.child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(theme.muted_text.value()))
                                        .child(notice),
                                )
                            })
                            .when_some(rejection, |actions, rejection| {
                                actions.child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(theme.danger.value()))
                                        .child(truncate_label(&rejection, 22)),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }
}
