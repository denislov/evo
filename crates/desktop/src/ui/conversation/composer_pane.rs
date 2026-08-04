use desktop::preferences::DesktopThinkingLevel;
use desktop::runtime::MAX_PROMPT_ATTACHMENTS;
use desktop::ui::conversation::ComposerAdmission;
use desktop::ui::shell::{
    COMPOSER_MAX_HEIGHT, COMPOSER_MIN_HEIGHT, CONVERSATION_CONTENT_MAX_WIDTH, SemanticTheme,
};
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

use crate::app::native_shell::{NativeDesktopState, SessionWorkspace};
use crate::ui::components::{
    controls::{
        DesktopControlSize, DesktopControlWeight, DesktopIcon, DesktopIconButton, DesktopSelector,
    },
    style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
};
use crate::ui::conversation::header::{self, ConversationControlsViewModel};

pub(crate) const COMPOSER_PLACEHOLDER: &str = "What do you want to build or improve?";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComposerPaneEvent {
    InputChanged(String),
    Focused,
    AddAttachments,
    RemoveAttachment(usize),
    SelectModel(Arc<str>),
    SelectSessionProfile(Arc<str>),
    SelectThinking(DesktopThinkingLevel),
    Send,
    Insert,
}

fn enter_event(secondary: bool, shift: bool) -> Option<ComposerPaneEvent> {
    if shift {
        None
    } else if secondary {
        Some(ComposerPaneEvent::Insert)
    } else {
        Some(ComposerPaneEvent::Send)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComposerPaneViewModel {
    pub(crate) composer_pending: bool,
    pub(crate) composer_running: bool,
    pub(crate) awaiting_prompt_start: bool,
    pub(crate) authorization_pending: bool,
    pub(crate) attachments: Arc<[ComposerAttachmentViewModel]>,
    pub(crate) attachments_enabled: bool,
    pub(crate) attachment_disabled_reason: Option<Arc<str>>,
    pub(crate) rejection: Option<Arc<str>>,
    pub(crate) controls: ConversationControlsViewModel,
}

pub(crate) fn view_model(app: &NativeDesktopState) -> ComposerPaneViewModel {
    let workspace = app.workspaces.active();
    let snapshot = workspace
        .projection
        .as_ref()
        .map(|projection| projection.snapshot());
    let composer_running = snapshot.is_some_and(|snapshot| snapshot.active_operation.is_some());
    let composer_pending = matches!(
        workspace.composer.admission(),
        ComposerAdmission::Pending { .. }
    );
    let awaiting_prompt_start = workspace.composer.submitted().is_some() && !composer_running;
    let attachment_disabled_reason = attachment_disabled_reason(workspace);
    ComposerPaneViewModel {
        composer_pending,
        composer_running,
        awaiting_prompt_start,
        authorization_pending: snapshot
            .is_some_and(|snapshot| !snapshot.pending_authorizations.is_empty()),
        attachments: workspace
            .composer_attachments
            .iter()
            .map(|path| ComposerAttachmentViewModel {
                label: Arc::from(
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("attachment"),
                ),
                path: Arc::from(path.display().to_string()),
            })
            .collect::<Vec<_>>()
            .into(),
        attachments_enabled: attachment_disabled_reason.is_none()
            && workspace.composer_attachments.len() < MAX_PROMPT_ATTACHMENTS,
        attachment_disabled_reason: attachment_disabled_reason.map(Arc::from),
        rejection: workspace.composer.rejection().map(Arc::from),
        controls: header::controls_view_model(app),
    }
}

pub(crate) fn attachment_disabled_reason(workspace: &SessionWorkspace) -> Option<&'static str> {
    let snapshot = workspace
        .projection
        .as_ref()
        .map(|projection| projection.snapshot());
    if snapshot.is_some_and(|snapshot| snapshot.active_operation.is_some()) {
        return Some("Attachments are unavailable while an operation is running.");
    }
    if matches!(
        workspace.composer.admission(),
        ComposerAdmission::Pending { .. }
    ) || workspace.composer.submitted().is_some()
    {
        return Some("Attachments are unavailable while a prompt is starting.");
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComposerAttachmentViewModel {
    pub(crate) label: Arc<str>,
    pub(crate) path: Arc<str>,
}

#[derive(Debug, Default)]
pub(crate) struct InputRenderLatencyProbe {
    pending_change: Cell<Option<Instant>>,
    #[cfg(test)]
    last_observed: Cell<Option<Duration>>,
}

impl InputRenderLatencyProbe {
    fn mark_changed(&self) {
        self.mark_changed_at(Instant::now());
    }

    pub(crate) fn mark_changed_at(&self, now: Instant) {
        self.pending_change.set(Some(now));
    }

    fn observe_render(&self) {
        let _ = self.observe_render_at(Instant::now());
    }

    pub(crate) fn observe_render_at(&self, now: Instant) -> Option<Duration> {
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
    pub(crate) fn pending_is_empty(&self) -> bool {
        self.pending_change.get().is_none()
    }

    #[cfg(test)]
    pub(crate) fn last_observed(&self) -> Option<Duration> {
        self.last_observed.get()
    }

    #[cfg(test)]
    pub(crate) fn clear_last_observed(&self) {
        self.last_observed.set(None);
    }
}

pub(crate) struct ComposerPane {
    input: gpui::Entity<InputState>,
    focus: FocusHandle,
    latency: InputRenderLatencyProbe,
    view_model: Option<ComposerPaneViewModel>,
    _input_subscription: Subscription,
}

impl ComposerPane {
    pub(crate) fn new(window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder(COMPOSER_PLACEHOLDER)
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
                InputEvent::PressEnter { secondary, shift } => {
                    if let Some(event) = enter_event(*secondary, *shift) {
                        cx.emit(event);
                    }
                }
                InputEvent::Blur => cx.notify(),
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

    pub(crate) fn set_view_model(&mut self, view_model: ComposerPaneViewModel) {
        self.view_model = Some(view_model);
    }

    pub(crate) fn set_input_value(
        &mut self,
        value: String,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        self.input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }

    pub(crate) fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    #[cfg(test)]
    pub(crate) fn latency_probe(&self) -> &InputRenderLatencyProbe {
        &self.latency
    }
}

impl EventEmitter<ComposerPaneEvent> for ComposerPane {}

fn composer_model_selector(
    controls: &ConversationControlsViewModel,
    theme: SemanticTheme,
    cx: &gpui::Context<ComposerPane>,
) -> impl IntoElement {
    let target = cx.entity().downgrade();
    let model_groups = Arc::clone(&controls.model_groups);
    let unavailable_current_model = controls.unavailable_current_model.clone();
    let current_model_id = Arc::clone(&controls.current_model_id);
    let model_option_count = model_groups
        .iter()
        .map(|group| group.options.len())
        .sum::<usize>();

    DesktopSelector::new(
        "composer-model-selector",
        controls.model.to_string(),
        format!("Select model; current {}", controls.current_model_id),
    )
    .disabled(controls.selector_disabled)
    .build()
    .outline()
    .debug_selector(|| "desktop-composer-model-selector".into())
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
                let target = target.clone();
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
                            .id(format!("composer-model-menu-row-{row_id}"))
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .aria_label(format!("{}; model id {}", accessible_name, row_id))
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
                                cx.emit(ComposerPaneEvent::SelectModel(id));
                            });
                        }
                    }),
                );
            }
        }
        menu
    })
}

fn composer_thinking_selector(
    controls: &ConversationControlsViewModel,
    cx: &gpui::Context<ComposerPane>,
) -> impl IntoElement {
    let target = cx.entity().downgrade();
    let thinking_options = Arc::clone(&controls.thinking_options);
    let thinking_selection = controls.thinking_selection;
    let unavailable = thinking_options.is_empty();
    DesktopSelector::new(
        "composer-thinking-selector",
        controls.thinking.to_string(),
        if unavailable {
            "Thinking controls are unavailable for the current model".to_owned()
        } else {
            format!("Select thinking level; current {}", controls.thinking)
        },
    )
    .disabled(controls.selector_disabled || unavailable)
    .build()
    .outline()
    .debug_selector(|| "desktop-composer-thinking-selector".into())
    .dropdown_menu(move |menu, _, _| {
        thinking_options
            .iter()
            .fold(menu.min_w(px(180.)), |menu, option| {
                let target = target.clone();
                let level = option.selection;
                menu.item(
                    PopupMenuItem::new(option.label)
                        .checked(level == thinking_selection)
                        .on_click(move |_, _, cx| {
                            if let Some(target) = target.upgrade() {
                                target.update(cx, |_, cx| {
                                    cx.emit(ComposerPaneEvent::SelectThinking(level));
                                });
                            }
                        }),
                )
            })
    })
}

fn composer_profile_selector(
    controls: &ConversationControlsViewModel,
    cx: &gpui::Context<ComposerPane>,
) -> impl IntoElement {
    let target = cx.entity().downgrade();
    let profile_options = Arc::clone(&controls.profile_options);
    let current_profile_id = Arc::clone(&controls.current_profile_id);
    DesktopSelector::new(
        "composer-profile-selector",
        format!(
            "Profile · {}",
            desktop::ui::shell::truncate_label(&controls.profile, 16)
        ),
        format!(
            "Select session profile; current {}",
            controls.current_profile_id
        ),
    )
    .disabled(controls.profile_selector_disabled)
    .build()
    .outline()
    .debug_selector(|| "desktop-composer-profile-selector".into())
    .dropdown_menu(move |menu, _, _| {
        profile_options.iter().fold(
            menu.min_w(px(240.))
                .max_w(px(420.))
                .max_h(px(320.))
                .scrollable(profile_options.len() > 8),
            |menu, option| {
                let target = target.clone();
                let id = Arc::clone(&option.id);
                menu.item(
                    PopupMenuItem::new(option.label.to_string())
                        .checked(option.id == current_profile_id)
                        .disabled(!option.selectable)
                        .on_click(move |_, _, cx| {
                            if let Some(target) = target.upgrade() {
                                let id = Arc::clone(&id);
                                target.update(cx, |_, cx| {
                                    cx.emit(ComposerPaneEvent::SelectSessionProfile(id));
                                });
                            }
                        }),
                )
            },
        )
    })
}

impl Render for ComposerPane {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
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
        let attachments = view_model.attachments;
        let attachments_enabled = view_model.attachments_enabled;
        let attachment_disabled_reason = view_model.attachment_disabled_reason;
        let rejection = view_model.rejection;
        let controls = view_model.controls;
        let composer_disabled = composer_pending || awaiting_prompt_start;
        let theme = SemanticTheme::current(cx);
        let model_selector = composer_model_selector(&controls, theme, cx);
        let thinking_selector = composer_thinking_selector(&controls, cx);
        let profile_selector = composer_profile_selector(&controls, cx);
        let thinking_hint = controls.thinking_hint.clone();
        let submit_accessible_label = if composer_running {
            "Send after the active operation · Enter; insert into it now · Ctrl/Cmd+Enter"
        } else {
            "Send the composer draft · Enter; newline · Shift+Enter"
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
        let attachment_button_label = attachment_disabled_reason.as_deref().map_or_else(
            || "Add files or images".to_owned(),
            |reason| format!("Attachments unavailable: {reason}"),
        );
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
            .w_full()
            .max_w(px(CONVERSATION_CONTENT_MAX_WIDTH as f32))
            .mx_auto()
            .flex_shrink_0()
            .px_token(DesignSpace::Xl)
            .pt_token(DesignSpace::Sm)
            .pb_token(DesignSpace::Lg)
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
                    .border_color(rgb(theme.border.value()))
                    .bg(rgb(theme.elevated.value()))
                    .shadow_sm()
                    .p_token(DesignSpace::Md)
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
                            .min_h(px(56.))
                            .w_full()
                            .child(
                                div()
                                    .debug_selector(|| "desktop-composer-input-region".into())
                                    .w_full()
                                    .child(
                                        Input::new(&input)
                                            .appearance(false)
                                            .bordered(false)
                                            .focus_bordered(false)
                                            .disabled(composer_disabled),
                                    ),
                            ),
                    )
                    .when(!attachments.is_empty(), |surface| {
                        surface.child(
                            div()
                                .debug_selector(|| "desktop-composer-attachments".into())
                                .w_full()
                                .flex()
                                .flex_wrap()
                                .gap_token(DesignSpace::Xs)
                                .children(attachments.iter().enumerate().map(
                                    |(index, attachment)| {
                                        let path = attachment.path.clone();
                                        div()
                                            .id(format!("composer-attachment-{index}"))
                                            .role(Role::ListItem)
                                            .aria_label(format!("Attached file: {path}"))
                                            .max_w(px(280.))
                                            .flex()
                                            .items_center()
                                            .gap_token(DesignSpace::Xs)
                                            .px_token(DesignSpace::Sm)
                                            .py_token(DesignSpace::Xs)
                                            .rounded_token(DesignRadius::Md)
                                            .border_1()
                                            .border_color(rgb(theme.divider.value()))
                                            .bg(rgb(theme.surface.value()))
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .text_token(DesignText::Metadata)
                                                    .text_color(rgb(theme.text.value()))
                                                    .truncate()
                                                    .child(attachment.label.to_string()),
                                            )
                                            .child(
                                                DesktopIconButton::new(
                                                    format!("remove-composer-attachment-{index}"),
                                                    DesktopIcon::Close,
                                                    format!("Remove {}", attachment.label),
                                                )
                                                .disabled(composer_disabled)
                                                .build()
                                                .on_click(cx.listener(move |_, _, _, cx| {
                                                    cx.emit(ComposerPaneEvent::RemoveAttachment(
                                                        index,
                                                    ));
                                                })),
                                            )
                                    },
                                )),
                        )
                    })
                    .child(
                        div()
                            .debug_selector(|| "desktop-composer-bottom-row".into())
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_token(DesignSpace::Sm)
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_token(DesignSpace::Xs)
                                    .child(
                                        DesktopIconButton::new(
                                            "add-composer-attachments",
                                            DesktopIcon::Plus,
                                            attachment_button_label,
                                        )
                                        .size(DesktopControlSize::Standard)
                                        .disabled(!attachments_enabled || composer_disabled)
                                        .build()
                                        .debug_selector(|| {
                                            "desktop-hit-add-composer-attachments".into()
                                        })
                                        .on_click(
                                            cx.listener(|_, _, _, cx| {
                                                cx.emit(ComposerPaneEvent::AddAttachments);
                                            }),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .debug_selector(|| "desktop-composer-controls".into())
                                            .flex()
                                            .flex_wrap()
                                            .items_center()
                                            .gap_token(DesignSpace::Xs)
                                            .child(model_selector)
                                            .child(thinking_selector)
                                            .child(profile_selector),
                                    )
                                    .when_some(thinking_hint, |left, hint| {
                                        left.child(
                                            div()
                                                .id("composer-thinking-hint")
                                                .debug_selector(|| {
                                                    "desktop-composer-thinking-hint".into()
                                                })
                                                .role(Role::Status)
                                                .aria_label(hint.to_string())
                                                .max_w(px(148.))
                                                .overflow_hidden()
                                                .whitespace_nowrap()
                                                .text_ellipsis()
                                                .text_token(DesignText::Metadata)
                                                .text_color(rgb(theme.warning.value()))
                                                .child("Auto adjusted"),
                                        )
                                    })
                                    .when_some(attachment_disabled_reason, |left, reason| {
                                        left.child(
                                            div()
                                                .min_w_0()
                                                .text_token(DesignText::Metadata)
                                                .text_color(rgb(theme.muted_text.value()))
                                                .truncate()
                                                .child(reason.to_string()),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .debug_selector(|| "desktop-composer-actions".into())
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap_token(DesignSpace::Xs)
                                    .child(
                                        DesktopIconButton::new(
                                            "submit-composer",
                                            DesktopIcon::Submit,
                                            submit_accessible_label,
                                        )
                                        .size(DesktopControlSize::Standard)
                                        .weight(DesktopControlWeight::Primary)
                                        .busy(composer_disabled)
                                        .build()
                                        .debug_selector(|| "desktop-hit-submit-composer".into())
                                        .on_click(
                                            cx.listener(|_, _, _, cx| {
                                                cx.emit(ComposerPaneEvent::Send);
                                            }),
                                        ),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_uses_the_home_task_prompt_contract() {
        assert_eq!(
            COMPOSER_PLACEHOLDER,
            "What do you want to build or improve?"
        );
    }

    #[test]
    fn enter_shortcuts_have_fixed_send_insert_and_newline_semantics() {
        assert_eq!(enter_event(false, false), Some(ComposerPaneEvent::Send));
        assert_eq!(enter_event(true, false), Some(ComposerPaneEvent::Insert));
        assert_eq!(enter_event(false, true), None);
        assert_eq!(enter_event(true, true), None);
    }

    #[test]
    fn input_render_latency_uses_latest_change_and_consumes_it_once() {
        let probe = InputRenderLatencyProbe::default();
        let started = Instant::now();
        probe.mark_changed_at(started);
        probe.mark_changed_at(started + Duration::from_millis(3));

        assert_eq!(
            probe.observe_render_at(started + Duration::from_millis(8)),
            Some(Duration::from_millis(5))
        );
        assert_eq!(probe.last_observed(), Some(Duration::from_millis(5)));
        assert_eq!(
            probe.observe_render_at(started + Duration::from_millis(9)),
            None
        );
    }
}
