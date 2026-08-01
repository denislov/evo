use super::{
    CenterDrawerKind, Context, DesktopModalKind, FocusTarget, NativeShell, UiChangeSet, UiRegion,
    Window, focus_target_label,
};

impl NativeShell {
    pub(super) fn activate_modal(
        &mut self,
        modal: DesktopModalKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_drawer(window, cx, false);
        if self.ui.active_modal.is_none() {
            self.ui.focus.open_modal();
        }
        self.ui.active_modal = Some(modal);
        match modal {
            DesktopModalKind::Authorization => self.ui.authorization_focus.focus(window, cx),
            DesktopModalKind::CommandPalette => self.ui.command_palette_focus.focus(window, cx),
            DesktopModalKind::FullMessage => self.ui.full_message_focus.focus(window, cx),
            DesktopModalKind::Search => self.ui.search_focus.focus(window, cx),
        }
        self.refresh_views(
            UiChangeSet::from_regions(&[UiRegion::ConversationHeader, UiRegion::Modal]),
            cx,
        );
        cx.notify();
    }

    pub(super) fn dismiss_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ui.active_modal = None;
        self.ui.focus.close_modal(self.layout(window));
        self.focus_active_target(window, cx);
        self.refresh_views(
            UiChangeSet::from_regions(&[UiRegion::ConversationHeader, UiRegion::Modal]),
            cx,
        );
        cx.notify();
    }

    pub(super) fn activate_drawer(
        &mut self,
        drawer: CenterDrawerKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ui.active_modal.is_some() {
            self.focus_active_target(window, cx);
            return;
        }
        if self.ui.active_drawer.is_none() {
            self.ui.drawer_restore_focus = Some(self.ui.focus.active());
        }
        self.ui.active_drawer = Some(drawer);
        match drawer {
            CenterDrawerKind::Sessions => self.ui.sidebar_focus.focus(window, cx),
            CenterDrawerKind::Inspector => self.ui.inspector_focus.focus(window, cx),
        }
        self.refresh_views(
            UiChangeSet::from_regions(&[
                UiRegion::Sessions,
                UiRegion::Inspector,
                UiRegion::ConversationHeader,
                UiRegion::Drawer,
            ]),
            cx,
        );
        cx.notify();
    }

    pub(super) fn dismiss_drawer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        restore_focus: bool,
    ) {
        if self.ui.active_drawer.take().is_none() {
            if !restore_focus {
                self.ui.drawer_restore_focus = None;
            }
            return;
        }
        let restore_target = self.ui.drawer_restore_focus.take();
        if restore_focus {
            let layout = self.layout(window);
            let restored =
                restore_target.is_some_and(|target| self.ui.focus.request(target, layout));
            if !restored {
                let _ = self.ui.focus.request(FocusTarget::Composer, layout);
            }
            self.focus_active_target(window, cx);
        }
        self.refresh_views(
            UiChangeSet::from_regions(&[
                UiRegion::Sessions,
                UiRegion::Inspector,
                UiRegion::ConversationHeader,
                UiRegion::Drawer,
            ]),
            cx,
        );
        cx.notify();
    }

    pub(super) fn reconcile_authorization_modal(
        &mut self,
        authorization_present: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if authorization_present {
            self.ui.command_palette.close();
            self.ui.conversation_full_message = None;
            if self.ui.active_modal != Some(DesktopModalKind::Authorization) {
                self.activate_modal(DesktopModalKind::Authorization, window, cx);
            }
        } else if self.ui.active_modal == Some(DesktopModalKind::Authorization) {
            self.dismiss_modal(window, cx);
        }
    }

    pub(super) fn focus_target(
        &mut self,
        target: FocusTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ui.active_modal.is_some() {
            return;
        }
        let layout = self.layout(window);
        if target == FocusTarget::Sidebar && !layout.is_visible(target) {
            self.activate_drawer(CenterDrawerKind::Sessions, window, cx);
            return;
        }
        if target == FocusTarget::Inspector && !layout.is_visible(target) {
            self.activate_drawer(CenterDrawerKind::Inspector, window, cx);
            return;
        }
        if !self.ui.focus.request(target, layout) {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice(format!(
                    "{} is unavailable at the current window width.",
                    focus_target_label(target)
                ));
            cx.notify();
            return;
        }
        self.focus_active_target(window, cx);
        cx.notify();
    }

    pub(super) fn cycle_focus(
        &mut self,
        reverse: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ui.active_modal.is_some() {
            self.focus_active_target(window, cx);
            return;
        }
        self.ui.focus.cycle(self.layout(window), reverse);
        self.focus_active_target(window, cx);
        cx.notify();
    }

    pub(super) fn root_action_blocked_by_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(modal) = self.ui.active_modal else {
            return false;
        };
        self.app.workspaces.active_mut().set_preference_notice(
            match modal {
                DesktopModalKind::Authorization => {
                    "Resolve the authorization dialog before using workspace shortcuts."
                }
                DesktopModalKind::CommandPalette => {
                    "Choose a typed command or close the command palette first."
                }
                DesktopModalKind::FullMessage => {
                    "Close the full message viewer before using workspace shortcuts."
                }
                DesktopModalKind::Search => {
                    "Choose a search result or close search before using workspace shortcuts."
                }
            }
            .into(),
        );
        self.focus_active_target(window, cx);
        cx.notify();
        true
    }
}
