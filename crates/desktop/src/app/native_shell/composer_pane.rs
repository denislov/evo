use desktop::conversation::ComposerAdmission;
use desktop::shell::{COMPOSER_MAX_HEIGHT, COMPOSER_MIN_HEIGHT, SemanticTheme};
use gpui::{
    EventEmitter, Focusable as _, IntoElement, ParentElement as _, Render, Role, Styled as _,
    WeakEntity, Window, div, prelude::*, px, rgb,
};
use gpui_component::{
    Disableable as _,
    button::{Button, DropdownButton},
    input::Input,
    menu::PopupMenuItem,
};

use super::{
    ComposerRunningMode, NativeShell,
    desktop_style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
};

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
            .gap_token(DesignSpace::Sm)
            .when_some(composer_notice, |composer, (notice, color)| {
                composer.child(
                    div()
                        .id("composer-state-notice")
                        .debug_selector(|| "desktop-composer-state-notice".into())
                        .role(Role::Status)
                        .aria_label(notice.to_owned())
                        .w_full()
                        .rounded_token(DesignRadius::Md)
                        .bg(rgb(theme.surface.value()))
                        .px_token(DesignSpace::Md)
                        .py_token(DesignSpace::Xs)
                        .text_token(DesignText::Metadata)
                        .text_color(rgb(color.value()))
                        .whitespace_normal()
                        .child(notice.to_owned()),
                )
            })
            .child(
                div()
                    .w_full()
                    .flex()
                    .gap_token(DesignSpace::Sm)
                    .rounded_token(DesignRadius::Lg)
                    .border_1()
                    .border_color(rgb(theme.border.value()))
                    .bg(rgb(theme.elevated.value()))
                    .p_token(DesignSpace::Sm)
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
                            .w(px(176.))
                            .flex()
                            .flex_col()
                            .gap_token(DesignSpace::Sm)
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
                                            .debug_selector(|| {
                                                "desktop-composer-running-mode-selector".into()
                                            })
                                            .child(
                                                DropdownButton::new("composer-running-mode-selector")
                                                    .compact()
                                                    .disabled(composer_pending)
                                                    .tooltip("Choose how this draft enters the active operation")
                                                    .button(
                                                        Button::new("composer-running-mode-current")
                                                            .label(running_action_label),
                                                    )
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
                                                                    if let Some(target) = steer_target.upgrade() {
                                                                        target.update(cx, |_, cx| {
                                                                            cx.emit(ComposerPaneEvent::SetRunningMode(
                                                                                ComposerRunningMode::SteerNow,
                                                                            ));
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
                                                                    if let Some(target) = queue_target.upgrade() {
                                                                        target.update(cx, |_, cx| {
                                                                            cx.emit(ComposerPaneEvent::SetRunningMode(
                                                                                ComposerRunningMode::QueueNext,
                                                                            ));
                                                                        });
                                                                    }
                                                                }),
                                                        )
                                                    }),
                                            ),
                                    )
                                    .child(
                                        Button::new("submit-running-composer")
                                            .debug_selector(|| {
                                                "desktop-hit-submit-running-composer".into()
                                            })
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
                            }),
                    ),
            )
            .into_any_element()
    }
}
