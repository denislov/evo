use desktop::conversation::ComposerAdmission;
use desktop::shell::{SemanticTheme, truncate_label};
use gpui::{
    EventEmitter, Focusable as _, IntoElement, ParentElement as _, Render, Styled as _, WeakEntity,
    Window, div, prelude::*, px, rgb,
};
use gpui_component::{Disableable as _, button::Button, input::Input};

use super::{COMPOSER_MAX_HEIGHT, COMPOSER_MIN_HEIGHT, NativeShell};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ComposerPaneEvent {
    Submit,
    Steer,
    FollowUp,
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
            return div().min_h(px(COMPOSER_MIN_HEIGHT)).into_any_element();
        };
        let (input, composer_pending, composer_running, awaiting_prompt_start, rejection) = {
            let owner = owner.read(cx);
            (
                owner.composer_input.clone(),
                matches!(
                    owner.composer.admission(),
                    ComposerAdmission::Pending { .. }
                ),
                owner.projection.snapshot().active_operation.is_some(),
                owner.composer.submitted().is_some()
                    && owner.projection.snapshot().active_operation.is_none(),
                owner.composer.rejection().map(str::to_owned),
            )
        };
        let composer_disabled = composer_pending || awaiting_prompt_start;
        let composer_focused = input.focus_handle(cx).is_focused(window);
        let theme = SemanticTheme::GEEK_DARK;

        div()
            .id("composer-panel")
            .min_h(px(COMPOSER_MIN_HEIGHT))
            .max_h(px(COMPOSER_MAX_HEIGHT))
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
                            .w(px(116.))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .justify_end()
                            .when(!composer_running, |actions| {
                                actions.child(
                                    Button::new("submit-composer")
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
                                        Button::new("steer-operation")
                                            .compact()
                                            .label(if composer_pending {
                                                "Sending…"
                                            } else {
                                                "Steer"
                                            })
                                            .tooltip("Send the composer draft as steering input")
                                            .disabled(composer_disabled)
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(ComposerPaneEvent::Steer);
                                            })),
                                    )
                                    .child(
                                        Button::new("follow-up-operation")
                                            .compact()
                                            .label("Follow up")
                                            .tooltip("Queue the composer draft as a follow-up")
                                            .disabled(composer_disabled)
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(ComposerPaneEvent::FollowUp);
                                            })),
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
