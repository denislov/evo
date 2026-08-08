use crate::app::native_shell::{
    Context, ConversationSource, DesktopModalKind, FocusTarget, NativeShell, UiChangeSet, UiRegion,
    Window,
};

impl NativeShell {
    pub(super) fn refresh_conversation_rows_at_width(
        &mut self,
        layout_width: u32,
        cx: &mut Context<Self>,
    ) {
        let workspace = &mut self.app.workspaces.active_mut();
        let Some(projection) = workspace.projection.as_ref() else {
            return;
        };
        let pane_dirty = workspace
            .presentation
            .conversation_controller
            .needs_row_refresh()
            || workspace
                .presentation
                .conversation_controller
                .active_width_bucket()
                != Some(layout_width);
        let source = ConversationSource::new(projection, workspace.composer.submitted());
        workspace
            .presentation
            .conversation_controller
            .prepare_rows(&source, layout_width);
        if pane_dirty {
            self.refresh_views(UiChangeSet::one(UiRegion::Conversation), cx);
        }
    }

    pub(super) fn refresh_conversation_rows_at_current_width(
        &mut self,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(layout_width) = self
            .app
            .workspaces
            .active_mut()
            .presentation
            .conversation_controller
            .active_width_bucket()
        else {
            return false;
        };
        self.refresh_conversation_rows_at_width(layout_width, cx);
        true
    }

    pub(in crate::app) fn focus_composer_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.views.composer_pane.read(cx).focus_handle().clone();
        focus.focus(window, cx);
    }

    pub(super) fn focus_active_target(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.ui.focus.active() {
            FocusTarget::CenterHeader => self.ui.center_header_focus.focus(window, cx),
            FocusTarget::Sidebar => self.ui.sidebar_focus.focus(window, cx),
            FocusTarget::CenterBody => self.ui.center_body_focus.focus(window, cx),
            FocusTarget::Composer => self.focus_composer_input(window, cx),
            FocusTarget::Inspector => self.ui.inspector_focus.focus(window, cx),
            FocusTarget::Modal => match self.ui.active_modal {
                Some(DesktopModalKind::Authorization) => {
                    self.ui.authorization_focus.focus(window, cx)
                }
                Some(DesktopModalKind::CommandPalette) => {
                    self.ui.command_palette_focus.focus(window, cx);
                }
                Some(DesktopModalKind::FullMessage) => self.ui.full_message_focus.focus(window, cx),
                Some(DesktopModalKind::Search) => self.ui.search_focus.focus(window, cx),
                Some(DesktopModalKind::ConfirmDeleteSession) => {
                    self.ui.modal_focus.focus(window, cx)
                }
                Some(DesktopModalKind::UpdateAvailable) => self.ui.modal_focus.focus(window, cx),
                None => self.focus_composer_input(window, cx),
            },
        }
    }
}
