//! NativeShell UI intent dispatch: routing typed UI events into command
//! reservations and workspace navigation.

use desktop::runtime::DesktopRuntimeSelectionKind;
use desktop::ui::shell::FocusTarget;
use gpui::{Context, Window};

use crate::application::{
    catalog::ProjectCatalogState,
    change_set::{UiChangeSet, UiRegion},
    effect::ClipboardFeedback,
    reducer::{CatalogIntent, DesktopEvent, Transition},
};

use super::intent::UiIntent;
use super::{DesktopModalKind, NativeShell, SessionDeleteConfirm};
use crate::ui::shell::CenterNavigationTarget;

impl NativeShell {
    pub(in crate::app) fn dispatch_ui_intent(
        &mut self,
        intent: UiIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match intent {
            UiIntent::Navigate(target) => self.navigate_center(target, window, cx),
            UiIntent::NewConversationForProject(path) => {
                self.show_project_home_workspace(path, window, cx)
            }
            UiIntent::RefreshSessions => self.request_session_catalog(cx),
            UiIntent::SetProjectCollapsed {
                group_id,
                collapsed,
            } => {
                let transition = self.connection.controller.reduce(
                    &mut self.app,
                    DesktopEvent::Ui(CatalogIntent::SetProjectCollapsed {
                        group_id,
                        collapsed,
                    }),
                    |state, event| {
                        let DesktopEvent::Ui(CatalogIntent::SetProjectCollapsed {
                            group_id,
                            collapsed,
                        }) = event
                        else {
                            unreachable!("catalog disclosure receives one typed intent")
                        };
                        if state.catalog.set_group_collapsed(&group_id, collapsed) {
                            Transition::changed(UiRegion::Sessions)
                        } else {
                            Transition::default()
                        }
                    },
                );
                if transition.changes().contains(UiRegion::Sessions) {
                    self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
                    cx.notify();
                }
            }
            UiIntent::RenameSession { session_id, name } => {
                self.rename_session(session_id, name, cx);
            }
            UiIntent::CloseSession(session_id) => self.close_session(&session_id, cx),
            UiIntent::DeleteSession(session_id) => {
                let name = self
                    .app
                    .catalog
                    .project_groups()
                    .into_iter()
                    .flat_map(|group| group.sessions)
                    .find(|session| session.session_id == session_id)
                    .and_then(|session| session.name);
                self.ui.pending_delete_session = Some(SessionDeleteConfirm { session_id, name });
                self.activate_modal(DesktopModalKind::ConfirmDeleteSession, window, cx);
            }
            UiIntent::ConfirmDeleteSession => {
                let session_id = self
                    .ui
                    .pending_delete_session
                    .take()
                    .map(|confirm| confirm.session_id);
                self.dismiss_modal(window, cx);
                if let Some(session_id) = session_id {
                    self.delete_session(&session_id, cx);
                }
            }
            UiIntent::CancelDeleteSession => {
                self.ui.pending_delete_session = None;
                self.dismiss_modal(window, cx);
            }
            UiIntent::OpenSearch => {
                if matches!(self.app.catalog.state(), ProjectCatalogState::NotLoaded) {
                    self.request_session_catalog(cx);
                }
                self.activate_modal(DesktopModalKind::Search, window, cx);
                self.views.root_modal_host.update(cx, |host, cx| {
                    host.open_search(window, cx);
                });
            }
            UiIntent::DismissDrawer => self.dismiss_drawer(window, cx, true),
            UiIntent::ToggleSessions => self.toggle_sessions(window, cx),
            UiIntent::ToggleInspector => self.toggle_context(window, cx),
            UiIntent::Reload => self.reload_local_resources(cx),
            UiIntent::SelectModel(model_id) => {
                self.submit_selection(DesktopRuntimeSelectionKind::Model, model_id.to_string(), cx)
            }
            UiIntent::SelectSessionProfile(profile_id) => self.submit_selection(
                DesktopRuntimeSelectionKind::SessionProfile,
                profile_id.to_string(),
                cx,
            ),
            UiIntent::SelectThinking(level) => self.select_thinking_level(level, cx),
            UiIntent::Abort => self.abort_active_operation(cx),
            UiIntent::ComposerInputChanged(value) => {
                self.app.workspaces.active_mut().composer.edit(value);
                self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
            }
            UiIntent::ComposerFocused => self.record_focus(FocusTarget::Composer, window, cx),
            UiIntent::AddAttachments => self.choose_composer_attachments(cx),
            UiIntent::RemoveAttachment(index) => self.remove_composer_attachment(index, cx),
            UiIntent::ChooseProjectDirectory => self.choose_project_directory(cx),
            UiIntent::ClearProjectDirectory => {
                self.clear_project_directory(cx);
            }
            UiIntent::InsertComposer => {
                if !self.root_action_blocked_by_modal(window, cx) {
                    self.insert_composer(cx);
                }
            }
            UiIntent::SendComposer => self.send_composer(cx),
            UiIntent::SelectConversation { block_id, durable } => {
                self.record_focus(FocusTarget::CenterBody, window, cx);
                let workspace = self.app.workspaces.active_mut();
                let Some(projection) = workspace.projection.as_ref() else {
                    return;
                };
                workspace.presentation.conversation_controller.select_row(
                    block_id,
                    durable,
                    projection.conversation(),
                );
                self.refresh_views(UiChangeSet::one(UiRegion::Conversation), cx);
                self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
            }
            UiIntent::ConversationScrolled => {
                cx.defer_in(window, |this, _, cx| {
                    this.reconcile_conversation_scroll(cx);
                });
            }
            UiIntent::CopyConversation(block_id) => self.copy_conversation_row(&block_id, cx),
            UiIntent::CopyToolDetails(block_id) => self.copy_tool_details(&block_id, cx),
            UiIntent::CopyCodeCompleted => self.announce_conversation_copy("Code copied.", cx),
            UiIntent::ToggleConversationDetails(block_id) => {
                self.toggle_conversation_details(&block_id, cx);
            }
            UiIntent::OpenFullConversation(block_id) => {
                self.open_full_conversation_message(&block_id, window, cx);
            }
            UiIntent::Recovery { identity, action } => {
                self.submit_recovery_action(identity, action, cx);
            }
            UiIntent::FollowLatest => self.follow_latest(cx),
            UiIntent::RequestFileReview(request) => self.request_file_review(request, cx),
            UiIntent::CopyReviewPath => self.copy_review_path(cx),
            UiIntent::CopyFileReview => self.copy_file_review(cx),
            UiIntent::OpenExternalEditor => self.open_review_in_external_editor(cx),
            UiIntent::RefreshMergeProposals => self.refresh_merge_proposals(cx),
            UiIntent::MergeProposal(worktree_id) => {
                self.decide_merge_proposal(worktree_id, true, cx);
            }
            UiIntent::DiscardProposal(worktree_id) => {
                self.decide_merge_proposal(worktree_id, false, cx);
            }
            UiIntent::SelectInspectorSection(section) => {
                self.app
                    .workspaces
                    .active_mut()
                    .presentation
                    .inspector_section = section;
                self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
            }
            UiIntent::ExecutePalette(command) => {
                self.ui.command_palette.close();
                self.dismiss_modal(window, cx);
                self.execute_palette_command(command, window, cx);
            }
            UiIntent::DecideAuthorization { identity, decision } => {
                self.decide_tool_authorization(identity, decision, cx);
            }
            UiIntent::CopyFullMessage => {
                if let Some(message) = &self.ui.conversation_full_message {
                    self.write_clipboard(
                        Some(message.text.to_string()),
                        ClipboardFeedback::ConversationAnnouncement("Full message copied.".into()),
                        cx,
                    );
                }
            }
            UiIntent::CloseFullMessage => self.close_full_conversation_message(window, cx),
            UiIntent::NavigateSearch(session_id) => {
                self.dismiss_modal(window, cx);
                self.navigate_center(CenterNavigationTarget::Session(session_id), window, cx);
            }
            UiIntent::CloseSearch => self.dismiss_modal(window, cx),
        }
    }
}
