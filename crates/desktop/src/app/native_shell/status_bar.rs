use desktop::shell::{MONOSPACE_FONT_FAMILY, STATUS_HEIGHT, SemanticTheme, truncate_label};
use gpui::{
    EventEmitter, IntoElement, ParentElement as _, Render, Role, Styled as _, WeakEntity, Window,
    div, prelude::*, px, rgb,
};
use gpui_component::{
    Disableable as _,
    button::Button,
    menu::{DropdownMenu as _, PopupMenuItem},
};

use super::{
    DesktopCommandIntent, NativeShell,
    desktop_style::{DesignSpace, DesignText, DesktopStyledExt as _},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusBarEvent {
    SelectNextModel,
    SelectNextSessionProfile,
    CycleThinking,
}

pub(super) struct StatusBar {
    owner: WeakEntity<NativeShell>,
}

impl StatusBar {
    pub(super) fn new(owner: WeakEntity<NativeShell>) -> Self {
        Self { owner }
    }
}

impl EventEmitter<StatusBarEvent> for StatusBar {}

impl Render for StatusBar {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(owner) = self.owner.upgrade() else {
            return div().h(px(STATUS_HEIGHT as f32)).into_any_element();
        };
        let owner = owner.read(cx);
        let theme = SemanticTheme::GEEK_DARK;
        let snapshot = owner.projection.snapshot();
        let project = owner.projection.project();
        let status = owner.semantic_status();
        let composer_running = snapshot.active_operation.is_some();
        let awaiting_prompt_start = owner.composer.submitted().is_some() && !composer_running;
        let reload_pending = owner.command_ledger.contains(&DesktopCommandIntent::Reload);
        let selection_pending = owner
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Selection(_)));
        let selector_disabled =
            composer_running || awaiting_prompt_start || reload_pending || selection_pending;
        let model_cycle_available = project
            .models
            .iter()
            .filter(|model| model.supports_text && (model.configured || model.selected))
            .take(2)
            .count()
            > 1;
        let profile_cycle_available = project.profiles.len() > 1;
        let status_model = truncate_label(&project.selected_model_id, 14);
        let status_profile = truncate_label(snapshot.session.default_agent_profile_id.as_str(), 12);
        let thinking = owner
            .thinking_selection
            .label(project.settings.default_thinking_level.as_deref());
        let status_thinking = truncate_label(&thinking, 12);
        let change_count = snapshot.context.changes.len();
        let notice = owner.preference_notice.clone();
        let notice_for_menu = notice.clone();
        let focused = owner.status_focus.is_focused(window) && owner.keyboard_focus_visible();
        let viewport_width = u32::from(window.viewport_size().width);
        let show_configuration = viewport_width >= 1_200;

        div()
            .id("status-panel")
            .role(Role::Status)
            .aria_label(format!("Desktop status: {}", status.label()))
            .when_some(notice.clone(), |bar, notice| bar.aria_description(notice))
            .debug_selector(|| "desktop-status-panel".into())
            .track_focus(&owner.status_focus)
            .h(px(STATUS_HEIGHT as f32))
            .px_token(DesignSpace::Md)
            .flex()
            .items_center()
            .justify_between()
            .gap_token(DesignSpace::Md)
            .border_t_1()
            .border_color(rgb(if focused {
                theme.focus_ring.value()
            } else {
                theme.divider.value()
            }))
            .bg(rgb(theme.elevated.value()))
            .child(
                div()
                    .debug_selector(|| "desktop-status-primary".into())
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_token(DesignSpace::Sm)
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .gap_token(DesignSpace::Xs)
                            .text_color(owner.status_color(status))
                            .child(status.glyph())
                            .child(status.label()),
                    )
                    .child(
                        div()
                            .debug_selector(|| "desktop-status-changes".into())
                            .flex_none()
                            .text_token(DesignText::Metadata)
                            .text_color(rgb(theme.subtle_text.value()))
                            .child(if change_count == 1 {
                                "1 changed file".to_owned()
                            } else {
                                format!("{change_count} changed files")
                            }),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "desktop-status-secondary".into())
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_token(DesignSpace::Sm)
                    .font_family(MONOSPACE_FONT_FAMILY)
                    .text_color(rgb(theme.subtle_text.value()))
                    .when(show_configuration, |bar| {
                        bar.child(
                            div()
                                .debug_selector(|| "desktop-status-configuration".into())
                                .flex()
                                .items_center()
                                .gap_token(DesignSpace::Sm)
                                .child(
                                    Button::new("cycle-model")
                                        .debug_selector(|| "desktop-hit-cycle-model".into())
                                        .compact()
                                        .label(format!("M {status_model}"))
                                        .tooltip("Select the next configured text model")
                                        .disabled(selector_disabled || !model_cycle_available)
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(StatusBarEvent::SelectNextModel);
                                        })),
                                )
                                .child(
                                    Button::new("cycle-session-profile")
                                        .compact()
                                        .label(format!("P {status_profile}"))
                                        .tooltip("Select the next session agent profile")
                                        .disabled(selector_disabled || !profile_cycle_available)
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(StatusBarEvent::SelectNextSessionProfile);
                                        })),
                                )
                                .child(
                                    Button::new("cycle-thinking")
                                        .compact()
                                        .label(format!("T {status_thinking}"))
                                        .tooltip("Cycle the composer thinking override")
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(StatusBarEvent::CycleThinking);
                                        })),
                                ),
                        )
                    })
                    .when_some(notice_for_menu, |bar, notice| {
                        bar.child(
                            Button::new("status-details")
                                .debug_selector(|| "desktop-status-details".into())
                                .compact()
                                .label("Notice")
                                .tooltip("Open status details")
                                .dropdown_menu(move |menu, _, _| {
                                    menu.item(PopupMenuItem::label(notice.clone()))
                                }),
                        )
                    })
                    .child(
                        div()
                            .debug_selector(|| "desktop-command-palette-hint".into())
                            .flex_none()
                            .text_token(DesignText::Metadata)
                            .child("Commands Ctrl/Cmd+K"),
                    ),
            )
            .into_any_element()
    }
}
