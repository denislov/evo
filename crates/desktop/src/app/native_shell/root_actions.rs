use super::{
    AbortActiveOperation, Arc, AuthorizationAllowForOperation, AuthorizationAllowOnce,
    AuthorizationDeny, CodingAgentFileReviewRequest, Context, CopySelectedConversation,
    DesktopFileReviewState, DesktopModalKind, DesktopPaletteCommand, DesktopRecoveryAction,
    DesktopRecoveryStatus, EscapeHierarchy, FocusComposer, FocusNextRegion, FocusPreviousRegion,
    FocusTarget, FollowLatestOutput, NativeShell, NewSession, OpenCommandPalette, OpenFileSurface,
    PaletteConfirm, PaletteNext, PalettePrevious, SelectNextConversation,
    SelectPreviousConversation, ToggleInspectorPanel, ToggleSelectedConversationDetails,
    ToolAuthorizationDecision, TrapOverlayFocus, UiChangeSet, UiRegion, Window, conversation_pane,
};

impl NativeShell {
    pub(super) fn follow_latest(&mut self, cx: &mut Context<Self>) {
        let visible_count = conversation_pane::visible_count(self.app.workspaces.active());
        self.app
            .workspaces
            .active_mut()
            .presentation
            .conversation_controller
            .follow_latest(visible_count);
        self.refresh_views(UiChangeSet::one(UiRegion::Conversation), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
    }

    pub(super) fn reconcile_conversation_scroll(&mut self, cx: &mut Context<Self>) {
        if self
            .app
            .workspaces
            .active_mut()
            .presentation
            .conversation_controller
            .reconcile_scroll()
        {
            self.refresh_views(UiChangeSet::one(UiRegion::Conversation), cx);
            self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        }
    }

    pub(super) fn review_next_file(&mut self, cx: &mut Context<Self>) {
        let Some(projection) = self.app.workspaces.active().projection.as_ref() else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("No session is open for file review.".into());
            cx.notify();
            return;
        };
        let changes = &projection.snapshot().context.changes;
        if changes.is_empty() {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("No changed file is available for review.".into());
            cx.notify();
            return;
        }
        let current = match self.app.workspaces.active().file_review.as_ref() {
            DesktopFileReviewState::Empty => None,
            DesktopFileReviewState::Loading(request)
            | DesktopFileReviewState::Failed { request, .. } => Some(&request.change),
            DesktopFileReviewState::Ready(document) => Some(&document.request.change),
        };
        let next = current
            .and_then(|current| {
                changes.iter().position(|change| {
                    change.operation_id == current.operation_id
                        && change.tool_call_id == current.tool_call_id
                        && change.path == current.path
                })
            })
            .map_or(0, |index| (index + 1) % changes.len());
        let request = CodingAgentFileReviewRequest::from(&changes[next]);
        self.request_file_review(request, cx);
    }

    pub(super) fn submit_latest_recovery(
        &mut self,
        action: DesktopRecoveryAction,
        cx: &mut Context<Self>,
    ) {
        let identity = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .and_then(|projection| {
                projection.recoveries().iter().find(|recovery| {
                    recovery.status == DesktopRecoveryStatus::Pending && recovery.authoritative
                })
            })
            .and_then(|recovery| recovery.identity.clone());
        let Some(identity) = identity else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("No authoritative pending recovery is available.".into());
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        };
        self.submit_recovery_action(identity, action, cx);
    }

    pub(super) fn execute_palette_command(
        &mut self,
        command: DesktopPaletteCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            DesktopPaletteCommand::NewSession => self.create_session(cx),
            DesktopPaletteCommand::SwitchNextSession => self.switch_next_session(cx),
            DesktopPaletteCommand::ToggleSessions => self.toggle_sessions(window, cx),
            DesktopPaletteCommand::ToggleInspector => self.toggle_context(window, cx),
            DesktopPaletteCommand::FocusSessions => {
                self.focus_target(FocusTarget::Sidebar, window, cx);
            }
            DesktopPaletteCommand::FocusConversation => {
                self.focus_target(FocusTarget::CenterBody, window, cx);
            }
            DesktopPaletteCommand::FocusComposer => {
                self.focus_target(FocusTarget::Composer, window, cx);
            }
            DesktopPaletteCommand::FocusInspector => {
                self.focus_target(FocusTarget::Inspector, window, cx);
            }
            DesktopPaletteCommand::SubmitPrompt => {
                self.send_composer(cx);
            }
            DesktopPaletteCommand::InsertMessage => {
                self.insert_composer(cx);
            }
            DesktopPaletteCommand::AbortOperation => self.abort_active_operation(cx),
            DesktopPaletteCommand::FollowLatest => self.follow_latest(cx),
            DesktopPaletteCommand::ReloadResources => self.reload_local_resources(cx),
            DesktopPaletteCommand::CopyConversation => self.copy_selected_conversation(cx),
            DesktopPaletteCommand::CycleThinking => self.cycle_thinking_selection(cx),
            DesktopPaletteCommand::ReviewNextFile => self.review_next_file(cx),
            DesktopPaletteCommand::CopyReviewPath => self.copy_review_path(cx),
            DesktopPaletteCommand::CopyFileReview => self.copy_file_review(cx),
            DesktopPaletteCommand::OpenExternalEditor => self.open_review_in_external_editor(cx),
            DesktopPaletteCommand::RetryRecovery => {
                self.submit_latest_recovery(DesktopRecoveryAction::Retry, cx);
            }
            DesktopPaletteCommand::MarkRecoveryFailed => {
                self.submit_latest_recovery(DesktopRecoveryAction::MarkFailed, cx);
            }
            DesktopPaletteCommand::AbortRecovery => {
                self.submit_latest_recovery(DesktopRecoveryAction::Abort, cx);
            }
            DesktopPaletteCommand::ToggleReducedMotion => {
                self.app.preferences.reduced_motion = !self.app.preferences.reduced_motion;
                self.schedule_preferences();
                let notice = if self.app.preferences.reduced_motion {
                    "Reduced motion enabled; desktop transitions remain static.".into()
                } else {
                    "Reduced motion disabled; idle presentation remains static.".into()
                };
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(notice);
                self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
                cx.notify();
            }
        }
    }

    pub(super) fn on_open_command_palette(
        &mut self,
        _: &OpenCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .is_some_and(|projection| !projection.snapshot().pending_authorizations.is_empty())
        {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Resolve authorization before opening commands.".into());
            self.ui.authorization_focus.focus(window, cx);
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        }
        self.ui.command_palette.open();
        self.activate_modal(DesktopModalKind::CommandPalette, window, cx);
    }

    pub(super) fn on_open_file_surface(
        &mut self,
        _: &OpenFileSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.root_action_blocked_by_modal(window, cx) {
            return;
        }
        self.review_next_file(cx);
        self.focus_target(FocusTarget::Inspector, window, cx);
    }

    pub(super) fn on_new_session(
        &mut self,
        _: &NewSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.root_action_blocked_by_modal(window, cx) {
            return;
        }
        self.create_session(cx);
    }

    pub(super) fn on_focus_composer(
        &mut self,
        _: &FocusComposer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.root_action_blocked_by_modal(window, cx) {
            return;
        }
        self.focus_target(FocusTarget::Composer, window, cx);
    }

    pub(super) fn on_abort_active_operation(
        &mut self,
        _: &AbortActiveOperation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.root_action_blocked_by_modal(window, cx) {
            return;
        }
        self.abort_active_operation(cx);
    }

    pub(super) fn on_escape_hierarchy(
        &mut self,
        _: &EscapeHierarchy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(modal) = self.ui.active_modal {
            match modal {
                DesktopModalKind::Authorization => {
                    self.app.workspaces.active_mut().set_preference_notice(
                        "Authorization requires Deny, Allow once, or Allow for operation.".into(),
                    );
                    self.ui.authorization_focus.focus(window, cx);
                    cx.notify();
                }
                DesktopModalKind::CommandPalette => {
                    self.ui.command_palette.close();
                    self.dismiss_modal(window, cx);
                }
                DesktopModalKind::FullMessage => {
                    self.close_full_conversation_message(window, cx);
                }
                DesktopModalKind::Search => self.dismiss_modal(window, cx),
                DesktopModalKind::ConfirmDeleteSession => {
                    self.ui.pending_delete_session = None;
                    self.dismiss_modal(window, cx);
                }
                DesktopModalKind::UpdateAvailable => {
                    if self
                        .ui
                        .available_update
                        .as_ref()
                        .is_some_and(|update| update.installing)
                    {
                        self.app.workspaces.active_mut().set_preference_notice(
                            "The verified update is still being installed. Please wait.".into(),
                        );
                        self.ui.modal_focus.focus(window, cx);
                        cx.notify();
                        return;
                    }
                    self.ui.available_update = None;
                    self.dismiss_modal(window, cx);
                }
            }
            return;
        }
        if self.ui.active_drawer.is_some() {
            self.dismiss_drawer(window, cx, true);
        } else if !matches!(
            self.app.workspaces.active_mut().file_review.as_ref(),
            DesktopFileReviewState::Empty
        ) {
            self.app.workspaces.active_mut().file_review = Arc::new(DesktopFileReviewState::Empty);
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Closed the changed-file review.".into());
            cx.notify();
        } else {
            self.focus_target(FocusTarget::Composer, window, cx);
        }
    }

    pub(super) fn on_follow_latest_output(
        &mut self,
        _: &FollowLatestOutput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.root_action_blocked_by_modal(window, cx) {
            return;
        }
        self.follow_latest(cx);
    }

    pub(super) fn on_toggle_inspector_panel(
        &mut self,
        _: &ToggleInspectorPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.root_action_blocked_by_modal(window, cx) {
            return;
        }
        self.toggle_context(window, cx);
    }

    pub(super) fn on_focus_next_region(
        &mut self,
        _: &FocusNextRegion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.root_action_blocked_by_modal(window, cx) {
            return;
        }
        self.cycle_focus(false, window, cx);
    }

    pub(super) fn on_focus_previous_region(
        &mut self,
        _: &FocusPreviousRegion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.root_action_blocked_by_modal(window, cx) {
            return;
        }
        self.cycle_focus(true, window, cx);
    }

    pub(super) fn on_select_previous_conversation(
        &mut self,
        _: &SelectPreviousConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.root_action_blocked_by_modal(window, cx) {
            self.select_adjacent_conversation(true, cx);
        }
    }

    pub(super) fn on_select_next_conversation(
        &mut self,
        _: &SelectNextConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.root_action_blocked_by_modal(window, cx) {
            self.select_adjacent_conversation(false, cx);
        }
    }

    pub(super) fn on_copy_selected_conversation(
        &mut self,
        _: &CopySelectedConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.root_action_blocked_by_modal(window, cx) {
            self.copy_keyboard_selected_conversation(cx);
        }
    }

    pub(super) fn on_toggle_selected_conversation_details(
        &mut self,
        _: &ToggleSelectedConversationDetails,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.root_action_blocked_by_modal(window, cx) {
            self.toggle_keyboard_selected_conversation_details(cx);
        }
    }

    pub(super) fn on_palette_previous(
        &mut self,
        _: &PalettePrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ui.command_palette.move_selection(true);
        self.refresh_views(UiChangeSet::one(UiRegion::Modal), cx);
        cx.notify();
    }

    pub(super) fn on_palette_next(
        &mut self,
        _: &PaletteNext,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ui.command_palette.move_selection(false);
        self.refresh_views(UiChangeSet::one(UiRegion::Modal), cx);
        cx.notify();
    }

    pub(super) fn on_palette_confirm(
        &mut self,
        _: &PaletteConfirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(command) = self.ui.command_palette.selected_command() else {
            return;
        };
        self.ui.command_palette.close();
        self.dismiss_modal(window, cx);
        self.execute_palette_command(command, window, cx);
    }

    pub(super) fn decide_current_authorization(
        &mut self,
        decision: ToolAuthorizationDecision,
        cx: &mut Context<Self>,
    ) {
        let Some(request) = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .and_then(|projection| projection.snapshot().pending_authorizations.first())
            .cloned()
        else {
            return;
        };
        self.decide_tool_authorization(request.identity(), decision, cx);
    }

    pub(super) fn on_authorization_deny(
        &mut self,
        _: &AuthorizationDeny,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.decide_current_authorization(
            ToolAuthorizationDecision::Deny {
                reason: Some("denied from native desktop keyboard action".into()),
            },
            cx,
        );
    }

    pub(super) fn on_authorization_allow_once(
        &mut self,
        _: &AuthorizationAllowOnce,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.decide_current_authorization(ToolAuthorizationDecision::AllowOnce, cx);
    }

    pub(super) fn on_authorization_allow_for_operation(
        &mut self,
        _: &AuthorizationAllowForOperation,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.decide_current_authorization(ToolAuthorizationDecision::AllowForOperation, cx);
    }

    pub(super) fn on_trap_overlay_focus(
        &mut self,
        _: &TrapOverlayFocus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_active_target(window, cx);
    }
}
