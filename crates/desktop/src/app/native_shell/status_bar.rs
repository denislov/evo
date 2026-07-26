use desktop::shell::{MONOSPACE_FONT_FAMILY, STATUS_HEIGHT, SemanticTheme, truncate_label};
use gpui::{
    EventEmitter, IntoElement, ParentElement as _, Render, Styled as _, WeakEntity, Window, div,
    prelude::*, px, rgb,
};
use gpui_component::{Disableable as _, button::Button};

use super::{DesktopCommandIntent, NativeShell};

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
        let notice = owner.preference_notice.clone();
        let focused = owner.status_focus.is_focused(window) && owner.keyboard_focus_visible();

        div()
            .id("status-panel")
            .track_focus(&owner.status_focus)
            .h(px(STATUS_HEIGHT as f32))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(rgb(if focused {
                theme.focus_ring.value()
            } else {
                theme.border.value()
            }))
            .bg(rgb(theme.elevated.value()))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .text_color(owner.status_color(status))
                    .child(status.glyph())
                    .child(status.label()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .font_family(MONOSPACE_FONT_FAMILY)
                    .text_color(rgb(theme.muted_text.value()))
                    .child(
                        Button::new("cycle-model")
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
                    )
                    .child(format!(
                        "seq {}",
                        owner.projection.cursor().last_event_sequence
                    ))
                    .child(if owner.preferences.reduced_motion {
                        "motion reduced"
                    } else {
                        "motion static"
                    })
                    .child("commands Ctrl/Cmd+K · focus Ctrl/Cmd+Tab · messages ↑/↓")
                    .when_some(notice, |bar, notice| {
                        bar.child(
                            div()
                                .text_color(rgb(theme.warning.value()))
                                .child(truncate_label(&notice, 28)),
                        )
                    }),
            )
            .into_any_element()
    }
}
