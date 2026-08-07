//! Inspector section chrome: section labels, tabs, and recovery actions.

use desktop::runtime::{DesktopRecoveryAction, DesktopRecoveryIdentity};
use desktop::ui::shell::{SemanticColor, SemanticTheme};
use gpui::{FocusHandle, KeyDownEvent, Role, ScrollHandle, div, prelude::*, px, rgb};
use gpui_component::button::Button;

use super::{InspectorPane, InspectorPaneEvent, InspectorSection};
use crate::ui::components::{
    controls::{DesktopControlSize, DesktopCriticalButton, DesktopCriticalTone},
    style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _},
};

pub(super) fn section(label: &'static str, theme: SemanticTheme) -> gpui::Div {
    colored_section(label, theme.accent)
}

pub(super) fn colored_section(label: &'static str, color: SemanticColor) -> gpui::Div {
    div()
        .mt_token(DesignSpace::Sm)
        .text_color(rgb(color.value()))
        .child(label)
}

const INSPECTOR_SECTIONS: [InspectorSection; 4] = [
    InspectorSection::Changes,
    InspectorSection::Task,
    InspectorSection::Usage,
    InspectorSection::Runtime,
];

pub(super) fn inspector_section_index(section: InspectorSection) -> usize {
    INSPECTOR_SECTIONS
        .iter()
        .position(|candidate| *candidate == section)
        .unwrap_or_default()
}

pub(super) fn inspector_section_tab(
    id: &'static str,
    label: &'static str,
    section: InspectorSection,
    selected: InspectorSection,
    tab_focus: [FocusHandle; 4],
    tab_scroll: ScrollHandle,
    cx: &gpui::Context<InspectorPane>,
) -> impl IntoElement {
    let active = section == selected;
    let index = inspector_section_index(section);
    let focus = tab_focus[index].clone().tab_stop(active);
    let click_focus = focus.clone();
    let click_scroll = tab_scroll.clone();
    let key_focus = tab_focus;
    let key_scroll = tab_scroll;
    let theme = SemanticTheme::current(cx);
    div()
        .id(id)
        .role(Role::Tab)
        .aria_label(label)
        .aria_selected(active)
        .track_focus(&focus)
        .h(px(DesktopControlSize::Compact.pixels()))
        .px_token(DesignSpace::Md)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_token(DesignRadius::Sm)
        .border_1()
        .border_color(rgb(if active {
            theme.accent.value()
        } else {
            theme.border.value()
        }))
        .bg(rgb(if active {
            theme.selection.value()
        } else {
            theme.surface.value()
        }))
        .text_token(DesignText::Metadata)
        .text_color(rgb(if active {
            theme.text.value()
        } else {
            theme.muted_text.value()
        }))
        .cursor_pointer()
        .hover(move |style| style.bg(rgb(theme.hover.value())))
        .focus(move |style| style.border_color(rgb(theme.focus_ring.value())))
        .child(label)
        .debug_selector(move || format!("desktop-inspector-tab-{}", label.to_lowercase()))
        .on_click(cx.listener(move |_, _, window, cx| {
            click_focus.focus(window, cx);
            click_scroll.scroll_to_item(index);
            cx.emit(InspectorPaneEvent::SelectSection(section));
        }))
        .on_key_down(cx.listener(move |_, event: &KeyDownEvent, window, cx| {
            let next_index = match event.keystroke.key.as_str() {
                "left" => Some(index.checked_sub(1).unwrap_or(INSPECTOR_SECTIONS.len() - 1)),
                "right" => Some((index + 1) % INSPECTOR_SECTIONS.len()),
                "enter" | "space" => Some(index),
                _ => None,
            };
            let Some(next_index) = next_index else {
                return;
            };
            window.prevent_default();
            cx.stop_propagation();
            key_focus[next_index].focus(window, cx);
            key_scroll.scroll_to_item(next_index);
            cx.emit(InspectorPaneEvent::SelectSection(
                INSPECTOR_SECTIONS[next_index],
            ));
        }))
}

pub(super) fn recovery_button(
    id: &'static str,
    label: &'static str,
    identity: DesktopRecoveryIdentity,
    action: DesktopRecoveryAction,
    disabled: bool,
    cx: &gpui::Context<InspectorPane>,
) -> Button {
    let tooltip = match action {
        DesktopRecoveryAction::Retry => "Retry this authoritative recovery",
        DesktopRecoveryAction::MarkFailed => "Resolve this recovery as failed",
        DesktopRecoveryAction::Abort => "Resolve this recovery as aborted",
    };
    let tone = match action {
        DesktopRecoveryAction::Retry => DesktopCriticalTone::Neutral,
        DesktopRecoveryAction::MarkFailed | DesktopRecoveryAction::Abort => {
            DesktopCriticalTone::Dangerous
        }
    };
    DesktopCriticalButton::new(id, label, tooltip, tone)
        .disabled(disabled)
        .build()
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.emit(InspectorPaneEvent::Recovery {
                identity: identity.clone(),
                action,
            });
        }))
}
