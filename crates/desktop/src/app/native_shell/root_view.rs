use gpui::{
    Context, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, Role,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
    rgb,
};

use super::{
    CONVERSATION_CONTENT_MAX_WIDTH, CONVERSATION_RESIZE_DEBOUNCE, CenterSurface, DesktopTimerKind,
    NativeShell, ResizablePanel, SemanticTheme, ShellLayout, UI_FONT_FAMILY, actions,
    conversation_width_bucket,
};
use crate::ui::components::style::{DesignRadius, DesignSpace, DesignText, DesktopStyledExt as _};

impl NativeShell {
    fn prepare_root_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (ShellLayout, SemanticTheme) {
        self.flush_queued_effects(cx);
        if self.app.workspaces.active_mut().composer_needs_sync {
            let draft = self.app.workspaces.active_mut().composer.draft().to_owned();
            self.views.composer_pane.update(cx, |pane, cx| {
                pane.set_input_value(draft, window, cx);
            });
            self.app.workspaces.active_mut().composer_needs_sync = false;
        }
        let theme = SemanticTheme::GEEK_DARK;
        let layout = self.layout(window);
        self.ui.focus.reconcile_layout(layout);
        if self.app.workspaces.active_mut().projection.is_some() {
            let requested_layout_width =
                conversation_width_bucket(layout.center.width.min(CONVERSATION_CONTENT_MAX_WIDTH));
            let (layout_width, width_refresh) = self
                .app
                .workspaces
                .active_mut()
                .presentation
                .conversation_controller
                .width_for_render(requested_layout_width);
            if width_refresh.is_some() {
                let owner = self.app.workspaces.active_key().clone();
                if let Ok(transition) = self.connection.controller.schedule_timer(
                    owner,
                    DesktopTimerKind::ConversationWidthCommit,
                    CONVERSATION_RESIZE_DEBOUNCE,
                ) {
                    self.apply_transition(transition, cx);
                }
            }
            self.refresh_conversation_rows_at_width(layout_width, cx);
        }
        let authorization_present = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .is_some_and(|projection| !projection.snapshot().pending_authorizations.is_empty());
        self.reconcile_authorization_modal(authorization_present, window, cx);
        (layout, theme)
    }
}

impl Render for NativeShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _span = tracing::trace_span!("desktop.render").entered();
        let (layout, theme) = self.prepare_root_view(window, cx);
        let sidebar_panel = layout.sidebar.map(|bounds| {
            div()
                .relative()
                .flex_none()
                .w(px(bounds.width as f32))
                .h_full()
                .child(self.views.sessions_pane.clone())
                .child(
                    div()
                        .id("sessions-resize-handle")
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(4.))
                        .cursor_ew_resize()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event, _, cx| {
                                this.begin_panel_resize(ResizablePanel::Sessions, event, cx);
                            }),
                        ),
                )
        });

        let inspector_panel = layout.inspector.map(|bounds| {
            div()
                .relative()
                .flex_none()
                .w(px(bounds.width as f32))
                .h_full()
                .child(self.views.inspector_pane.clone())
                .child(
                    div()
                        .id("inspector-resize-handle")
                        .absolute()
                        .top_0()
                        .left_0()
                        .bottom_0()
                        .w(px(4.))
                        .cursor_ew_resize()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event, _, cx| {
                                this.begin_panel_resize(ResizablePanel::Context, event, cx);
                            }),
                        ),
                )
        });

        let center = if self.ui.center_surface == CenterSurface::Skills {
            div()
                .id("skills-workspace")
                .role(Role::Main)
                .aria_label("Skills workspace")
                .debug_selector(|| "desktop-skills-workspace".into())
                .flex_none()
                .w(px(layout.center.width as f32))
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .bg(rgb(theme.canvas.value()))
                .child(self.views.conversation_header.clone())
                .child(
                    div()
                        .id("center-body")
                        .debug_selector(|| "desktop-center-body".into())
                        .relative()
                        .track_focus(&self.ui.center_body_focus)
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(self.views.skills_pane.clone())
                        .child(self.views.center_drawer_host.clone()),
                )
        } else if self.app.workspaces.active_mut().projection.is_some() {
            div()
                .id("conversation-panel")
                .role(Role::Main)
                .aria_label("Conversation workspace")
                .aria_description(
                    "Conversation history and message composer. Use Up and Down to select messages.",
                )
                .debug_selector(|| "desktop-conversation-panel".into())
                .flex_none()
                .w(px(layout.center.width as f32))
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .bg(rgb(theme.canvas.value()))
                .child(self.views.conversation_header.clone())
                .child(
                    div()
                        .id("center-body")
                        .debug_selector(|| "desktop-center-body".into())
                        .relative()
                        .key_context(actions::CONVERSATION_KEY_CONTEXT)
                        .track_focus(&self.ui.center_body_focus)
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(self.views.conversation_pane.clone())
                        .child(self.views.composer_pane.clone())
                        .child(self.views.center_drawer_host.clone()),
                )
        } else {
            div()
                .id("home-workspace")
                .role(Role::Main)
                .aria_label("Home workspace")
                .debug_selector(|| "desktop-home-workspace".into())
                .flex_none()
                .w(px(layout.center.width as f32))
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .bg(rgb(theme.canvas.value()))
                .child(self.views.conversation_header.clone())
                .child(
                    div()
                        .id("center-body")
                        .debug_selector(|| "desktop-center-body".into())
                        .relative()
                        .track_focus(&self.ui.center_body_focus)
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(self.views.home_pane.clone())
                        .child(
                            div()
                                .w_full()
                                .max_w(px(900.))
                                .mx_auto()
                                .px_6()
                                .pb_8()
                                .child(self.views.composer_pane.clone()),
                        )
                        .child(self.views.center_drawer_host.clone()),
                )
        };

        let root_modal_host = self.views.root_modal_host.clone();
        let toast_host = self.views.toast_host.clone();
        let conversation_announcement = self
            .ui
            .conversation_announcement
            .as_ref()
            .map(|(_, _, message)| message.clone());

        div()
            .id("desktop-application")
            .role(Role::Application)
            .aria_label("Evo native coding agent")
            .key_context(actions::ROOT_KEY_CONTEXT)
            .on_action(cx.listener(Self::on_open_command_palette))
            .on_action(cx.listener(Self::on_open_file_surface))
            .on_action(cx.listener(Self::on_new_session))
            .on_action(cx.listener(Self::on_focus_composer))
            .on_action(cx.listener(Self::on_submit_composer))
            .on_action(cx.listener(Self::on_abort_active_operation))
            .on_action(cx.listener(Self::on_escape_hierarchy))
            .on_action(cx.listener(Self::on_follow_latest_output))
            .on_action(cx.listener(Self::on_toggle_inspector_panel))
            .on_action(cx.listener(Self::on_focus_next_region))
            .on_action(cx.listener(Self::on_focus_previous_region))
            .on_action(cx.listener(Self::on_select_previous_conversation))
            .on_action(cx.listener(Self::on_select_next_conversation))
            .on_action(cx.listener(Self::on_copy_selected_conversation))
            .on_action(cx.listener(Self::on_toggle_selected_conversation_details))
            .on_action(cx.listener(Self::on_palette_previous))
            .on_action(cx.listener(Self::on_palette_next))
            .on_action(cx.listener(Self::on_palette_confirm))
            .on_action(cx.listener(Self::on_authorization_deny))
            .on_action(cx.listener(Self::on_authorization_allow_once))
            .on_action(cx.listener(Self::on_authorization_allow_for_operation))
            .on_action(cx.listener(Self::on_trap_overlay_focus))
            .capture_any_mouse_down(cx.listener(Self::note_pointer_input))
            .capture_key_down(cx.listener(Self::note_keyboard_input))
            .on_mouse_move(cx.listener(Self::update_panel_resize))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::finish_panel_resize))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .font_family(UI_FONT_FAMILY)
            .text_token(DesignText::Body)
            .bg(rgb(theme.canvas.value()))
            .text_color(rgb(theme.text.value()))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .children(sidebar_panel)
                    .child(center)
                    .children(inspector_panel),
            )
            .child(root_modal_host)
            .child(toast_host)
            .when_some(conversation_announcement, |app, message| {
                app.child(
                    div()
                        .id("conversation-copy-announcement")
                        .debug_selector(|| "desktop-conversation-copy-announcement".into())
                        .role(Role::Status)
                        .aria_label(message.clone())
                        .absolute()
                        .top_4()
                        .right_4()
                        .rounded_token(DesignRadius::Md)
                        .border_1()
                        .border_color(rgb(theme.success.value()))
                        .bg(rgb(theme.elevated.value()))
                        .px_token(DesignSpace::Md)
                        .py_token(DesignSpace::Sm)
                        .text_color(rgb(theme.text.value()))
                        .child(message),
                )
            })
    }
}
