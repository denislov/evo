use desktop::ui::shell::SemanticTheme;
use gpui::{
    Entity, EventEmitter, IntoElement, MouseButton, ParentElement as _, Render, Role, Styled as _,
    div, prelude::*, px, rgb,
};

use crate::actions;
use crate::app::native_shell::NativeDesktopState;
use crate::ui::{inspector::pane::InspectorPane, sessions::pane::SessionsPane};

use super::ShellUiState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CenterDrawerKind {
    Sessions,
    Inspector,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CenterDrawerHostEvent {
    Dismiss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CenterDrawerViewModel {
    pub(crate) active: Option<CenterDrawerKind>,
    pub(crate) sessions_width: u32,
    pub(crate) inspector_width: u32,
}

pub(crate) fn view_model(app: &NativeDesktopState, ui: &ShellUiState) -> CenterDrawerViewModel {
    CenterDrawerViewModel {
        active: ui.active_drawer,
        sessions_width: app.preferences.sessions_panel_width,
        inspector_width: app.preferences.context_panel_width,
    }
}

pub(crate) struct CenterDrawerHost {
    sessions_pane: Entity<SessionsPane>,
    inspector_pane: Entity<InspectorPane>,
    view_model: Option<CenterDrawerViewModel>,
}

impl CenterDrawerHost {
    pub(crate) fn new(
        sessions_pane: Entity<SessionsPane>,
        inspector_pane: Entity<InspectorPane>,
    ) -> Self {
        Self {
            sessions_pane,
            inspector_pane,
            view_model: None,
        }
    }

    pub(crate) fn set_view_model(&mut self, view_model: CenterDrawerViewModel) {
        self.view_model = Some(view_model);
    }
}

impl EventEmitter<CenterDrawerHostEvent> for CenterDrawerHost {}

impl Render for CenterDrawerHost {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(view_model) = self.view_model else {
            return div().into_any_element();
        };
        let Some(active) = view_model.active else {
            return div().into_any_element();
        };
        let theme = SemanticTheme::GEEK_DARK;
        let (id, label, key_context, width, pane, on_left) = match active {
            CenterDrawerKind::Sessions => (
                "sessions-drawer",
                "Sessions drawer",
                actions::SESSIONS_DRAWER_KEY_CONTEXT,
                view_model.sessions_width,
                self.sessions_pane.clone().into_any_element(),
                true,
            ),
            CenterDrawerKind::Inspector => (
                "inspector-drawer",
                "Inspector drawer",
                actions::INSPECTOR_DRAWER_KEY_CONTEXT,
                view_model.inspector_width,
                self.inspector_pane.clone().into_any_element(),
                false,
            ),
        };

        let drawer = div()
            .id(id)
            .debug_selector(move || format!("desktop-{id}"))
            .role(Role::Dialog)
            .aria_label(label)
            .key_context(key_context)
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(width as f32))
            .overflow_hidden()
            .border_1()
            .border_color(rgb(theme.focus_ring.value()))
            .bg(rgb(theme.elevated.value()))
            .when(on_left, |drawer| drawer.left_0())
            .when(!on_left, |drawer| drawer.right_0())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(pane);

        div()
            .id("center-drawer-host")
            .debug_selector(|| "desktop-center-drawer-host".into())
            .absolute()
            .size_full()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(CenterDrawerHostEvent::Dismiss)),
            )
            .child(drawer)
            .into_any_element()
    }
}
