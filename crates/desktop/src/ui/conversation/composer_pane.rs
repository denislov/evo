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
    menu::PopupMenuItem,
};
use std::{
    cell::Cell,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::app::native_shell::SessionWorkspace;
use crate::ui::components::{
    controls::{
        DesktopControlSize, DesktopControlWeight, DesktopIcon, DesktopIconButton,
        DesktopProjectDirectoryControl, DesktopProjectDirectoryState,
    },
    style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
};

pub(crate) const COMPOSER_PLACEHOLDER: &str = "What do you want to build or improve?";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComposerPaneEvent {
    InputChanged(String),
    Focused,
    AddAttachments,
    RemoveAttachment(usize),
    ChooseProjectDirectory,
    ClearProjectDirectory,
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
    pub(crate) project_directory: ComposerProjectDirectoryViewModel,
    pub(crate) attachments: Arc<[ComposerAttachmentViewModel]>,
    pub(crate) attachments_enabled: bool,
    pub(crate) attachment_disabled_reason: Option<Arc<str>>,
    pub(crate) rejection: Option<Arc<str>>,
}

pub(crate) fn view_model(workspace: &SessionWorkspace) -> ComposerPaneViewModel {
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
    let project_directory_state = if workspace.projection.is_some() {
        DesktopProjectDirectoryState::Locked
    } else if !workspace.project_directory_editable() || composer_pending || awaiting_prompt_start {
        DesktopProjectDirectoryState::Pending
    } else {
        DesktopProjectDirectoryState::Editable
    };
    let project_directory_path = workspace.project_directory();
    ComposerPaneViewModel {
        composer_pending,
        composer_running,
        awaiting_prompt_start,
        authorization_pending: snapshot
            .is_some_and(|snapshot| !snapshot.pending_authorizations.is_empty()),
        project_directory: ComposerProjectDirectoryViewModel {
            value: Arc::from(
                project_directory_path
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "无项目".into()),
            ),
            state: project_directory_state,
            is_projectless: project_directory_path.is_none(),
        },
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
    }
}

pub(crate) fn attachment_disabled_reason(workspace: &SessionWorkspace) -> Option<&'static str> {
    let supports_images = workspace
        .project
        .models
        .iter()
        .find(|model| model.id == workspace.project.selected_model_id)
        .is_some_and(|model| model.supports_images);
    if !supports_images {
        return Some("Selected model does not support image attachments.");
    }
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
pub(crate) struct ComposerProjectDirectoryViewModel {
    pub(crate) value: Arc<str>,
    pub(crate) state: DesktopProjectDirectoryState,
    pub(crate) is_projectless: bool,
}

impl ComposerProjectDirectoryViewModel {
    fn accessible_label(&self) -> String {
        match self.state {
            DesktopProjectDirectoryState::Editable if self.value.as_ref() == "无项目" => {
                "项目目录：无项目。按 Enter 或 Space 选择目录。".into()
            }
            DesktopProjectDirectoryState::Editable => {
                format!("项目目录：{}。按 Enter 或 Space 选择其他目录。", self.value)
            }
            DesktopProjectDirectoryState::Locked => format!(
                "项目目录：{}。项目目录在对话创建后固定。请新建对话以选择其他项目。",
                self.value
            ),
            DesktopProjectDirectoryState::Pending => {
                format!("项目目录：{}。正在提交，暂时不能更改。", self.value)
            }
        }
    }
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
        let project_directory = view_model.project_directory;
        let attachments = view_model.attachments;
        let attachments_enabled = view_model.attachments_enabled;
        let attachment_disabled_reason = view_model.attachment_disabled_reason;
        let rejection = view_model.rejection;
        let composer_disabled = composer_pending || awaiting_prompt_start;
        let theme = SemanticTheme::current(cx);
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
        let event_target_for_project_choose = cx.entity().downgrade();
        let event_target_for_project_clear = cx.entity().downgrade();
        let attachment_button_label = attachment_disabled_reason.as_deref().map_or_else(
            || "Add files or images".to_owned(),
            |reason| format!("Attachments unavailable: {reason}"),
        );
        let composer_notice = rejection
            .as_deref()
            .map(|notice| (notice, theme.danger))
            .or_else(|| state_notice.map(|notice| (notice, theme.muted_text)));
        let project_directory_accessible_label = project_directory.accessible_label();

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
            .px_token(DesignSpace::Lg)
            .py_token(DesignSpace::Md)
            .border_t_1()
            .border_color(rgb(theme.divider.value()))
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
                                .children(attachments.iter().enumerate().map(|(index, attachment)| {
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
                                                cx.emit(ComposerPaneEvent::RemoveAttachment(index));
                                            })),
                                        )
                                })),
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
                                    .items_center()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
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
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(ComposerPaneEvent::AddAttachments);
                                        })),
                                    )
                                    .child(
                                        DesktopProjectDirectoryControl::new(
                                            "composer-project-directory",
                                            project_directory.value,
                                            project_directory_accessible_label,
                                            project_directory.state,
                                        )
                                        .build_with_menu(move |menu, _, _| {
                                            let choose_target =
                                                event_target_for_project_choose.clone();
                                            let clear_target =
                                                event_target_for_project_clear.clone();
                                            menu.item(
                                                PopupMenuItem::new("选择项目目录…").on_click(
                                                    move |_, _, cx| {
                                                        if let Some(target) = choose_target.upgrade()
                                                        {
                                                            target.update(cx, |_, cx| {
                                                                cx.emit(
                                                                    ComposerPaneEvent::ChooseProjectDirectory,
                                                                );
                                                            });
                                                        }
                                                    },
                                                ),
                                            )
                                            .item(
                                                PopupMenuItem::new("无项目")
                                                    .checked(project_directory.is_projectless)
                                                    .on_click(move |_, _, cx| {
                                                        if let Some(target) = clear_target.upgrade() {
                                                            target.update(cx, |_, cx| {
                                                                cx.emit(
                                                                    ComposerPaneEvent::ClearProjectDirectory,
                                                                );
                                                            });
                                                        }
                                                    }),
                                            )
                                        }),
                                    )
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
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(ComposerPaneEvent::Send);
                                        })),
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
    fn project_directory_accessibility_preserves_full_value_and_state() {
        let full_path = Arc::<str>::from("/工作区/一个很长的项目目录/evo");
        let editable = ComposerProjectDirectoryViewModel {
            value: Arc::clone(&full_path),
            state: DesktopProjectDirectoryState::Editable,
            is_projectless: false,
        };
        let locked = ComposerProjectDirectoryViewModel {
            value: Arc::clone(&full_path),
            state: DesktopProjectDirectoryState::Locked,
            is_projectless: false,
        };
        let pending = ComposerProjectDirectoryViewModel {
            value: Arc::clone(&full_path),
            state: DesktopProjectDirectoryState::Pending,
            is_projectless: false,
        };

        assert!(editable.accessible_label().contains(full_path.as_ref()));
        assert!(editable.accessible_label().contains("Enter 或 Space"));
        assert!(locked.accessible_label().contains(full_path.as_ref()));
        assert!(locked.accessible_label().contains("对话创建后固定"));
        assert!(pending.accessible_label().contains(full_path.as_ref()));
        assert!(pending.accessible_label().contains("正在提交"));

        let projectless = ComposerProjectDirectoryViewModel {
            value: Arc::from("无项目"),
            state: DesktopProjectDirectoryState::Editable,
            is_projectless: true,
        };
        assert_eq!(
            projectless.accessible_label(),
            "项目目录：无项目。按 Enter 或 Space 选择目录。"
        );
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
