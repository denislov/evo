use coding_agent::api::authorization::{ToolAuthorizationDecision, ToolAuthorizationIdentity};
use coding_agent::api::embedding::{
    CodingAgentEmbeddingSnapshot, CodingAgentResourceCommand, CodingAgentWorkspaceSelection,
};
use coding_agent::api::event::CodingAgentRecoveryResolution;
use coding_agent::api::review::CodingAgentFileReviewRequest;
use desktop::platform::preferences::{PreferenceWriteResult, PreferenceWriter};
use desktop::preferences::{DesktopPreferences, DesktopThinkingLevel};
use desktop::projection::{DesktopProjection, DesktopRecoveryStatus};
use desktop::runtime::{
    DesktopRecoveryAction, DesktopRecoveryIdentity, DesktopRuntimeBridge,
    DesktopRuntimeSelectionKind, validate_prompt_attachments,
};
use desktop::ui::conversation::{
    ComposerState, ComposerSubmissionKind, ConversationBlockKind, DelegationStatus, MAX_COPY_BYTES,
    conversation_copy_text, conversation_width_bucket,
};
use desktop::ui::shell::{
    CONTEXT_PANEL_MAX_WIDTH, CONTEXT_PANEL_MIN_WIDTH, CONTEXT_PANEL_WIDTH,
    CONVERSATION_CONTENT_MAX_WIDTH, FocusTarget, MIN_CONVERSATION_WIDTH, PanelVisibility,
    SESSION_PANEL_MAX_WIDTH, SESSION_PANEL_MIN_WIDTH, SESSION_PANEL_WIDTH, SemanticColor,
    SemanticStatus, SemanticTheme, ShellLayout, UI_FONT_FAMILY, truncate_label,
};
use gpui::{
    ClipboardItem, Context, KeyDownEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathPromptOptions, ScrollStrategy, Window, WindowBounds, prelude::*, rgb,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::actions::{
    self, AbortActiveOperation, AuthorizationAllowForOperation, AuthorizationAllowOnce,
    AuthorizationDeny, CopySelectedConversation, DesktopPaletteCommand, EscapeHierarchy,
    FocusComposer, FocusNextRegion, FocusPreviousRegion, FollowLatestOutput, NewSession,
    OpenCommandPalette, OpenFileSurface, PaletteConfirm, PaletteNext, PalettePrevious,
    SelectNextConversation, SelectPreviousConversation, ToggleInspectorPanel,
    ToggleSelectedConversationDetails, TrapOverlayFocus,
};
use crate::application::{
    catalog::{ProjectCatalogController, ProjectCatalogState},
    change_set::{UiChangeSet, UiRegion},
    commands::{CommandTracker, DesktopCommandIntent},
    effect::{
        ClipboardFeedback, DesktopEffect, DesktopPickerKind, DesktopTimer, DesktopTimerKind,
        PlatformOutcome, PlatformResult,
    },
    reducer::{
        CatalogIntent, DesktopController, DesktopEvent, PlatformUpdatePort, PreferencePanel,
        PreferencesIntent, Transition,
    },
    runtime_state::{RuntimeProjectionPresentation, RuntimeWorkspacePresentation},
    state::DesktopState,
    workspace::{SessionId, WorkspaceKey, WorkspaceStore},
    workspace_state::{
        DesktopFileReviewState, MAX_SESSION_WORKSPACES, RuntimeWorkspaceDefaults, WorkspaceState,
        admitted_thinking_selection, workspace_selection_from_embedding,
    },
};
#[cfg(feature = "desktop-devtools")]
pub(super) use crate::ui::components::brand::{EvoBrandFixture, EvoBrandMode};
use crate::ui::shell::{
    ShellConnection, ShellUiState, ShellViews, presentation::recovery_action_label,
};

const MAX_RUNTIME_UPDATES_PER_FRAME: usize = 64;
const INSPECTOR_TELEMETRY_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
pub(crate) struct ConversationBlockVisual {
    pub(crate) glyph: &'static str,
    pub(crate) accent: SemanticColor,
    pub(crate) align_right: bool,
}

pub(crate) fn conversation_focus_accent(focused: bool, theme: SemanticTheme) -> SemanticColor {
    if focused { theme.accent } else { theme.divider }
}

pub(crate) fn semantic_status_color(status: SemanticStatus, theme: SemanticTheme) -> gpui::Rgba {
    rgb(match status {
        SemanticStatus::Idle => theme.muted_text.value(),
        SemanticStatus::Running => theme.accent.value(),
        SemanticStatus::Warning | SemanticStatus::Authorization => theme.warning.value(),
        SemanticStatus::Error => theme.danger.value(),
    })
}

/// Accent colour for a delegation's lifecycle state, mirroring
/// [`semantic_status_color`] for the delegation status vocabulary.
pub(crate) fn delegation_status_color(
    status: DelegationStatus,
    theme: SemanticTheme,
) -> gpui::Rgba {
    rgb(match status {
        DelegationStatus::Requested | DelegationStatus::Completed | DelegationStatus::Unknown => {
            theme.muted_text.value()
        }
        DelegationStatus::Running => theme.accent.value(),
        DelegationStatus::Failed => theme.danger.value(),
        DelegationStatus::Rejected
        | DelegationStatus::Cancelled
        | DelegationStatus::ConfirmationRequired => theme.warning.value(),
    })
}

pub(crate) fn conversation_block_visual(
    kind: ConversationBlockKind,
    is_error: bool,
    theme: SemanticTheme,
) -> ConversationBlockVisual {
    match kind {
        ConversationBlockKind::User => ConversationBlockVisual {
            glyph: "",
            accent: theme.accent,
            align_right: true,
        },
        ConversationBlockKind::Assistant => ConversationBlockVisual {
            glyph: "AI",
            accent: theme.text,
            align_right: false,
        },
        ConversationBlockKind::Tool => ConversationBlockVisual {
            glyph: "TOOL",
            accent: if is_error {
                theme.danger
            } else {
                theme.muted_text
            },
            align_right: false,
        },
        ConversationBlockKind::Delegation => ConversationBlockVisual {
            glyph: "AGENT",
            accent: theme.accent,
            align_right: false,
        },
        ConversationBlockKind::CompactionSummary | ConversationBlockKind::BranchSummary => {
            ConversationBlockVisual {
                glyph: "SUMMARY",
                accent: theme.muted_text,
                align_right: false,
            }
        }
        ConversationBlockKind::Diagnostic => ConversationBlockVisual {
            glyph: "ISSUE",
            accent: theme.danger,
            align_right: false,
        },
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum InspectorSection {
    #[default]
    Changes,
    Task,
    Usage,
    Runtime,
}

/// Typed states installed by the deterministic native visual replay.
///
/// These are presentation fixtures, not a second catalog implementation: each
/// variant drives the same [`ProjectCatalogController`] transitions used by
/// runtime updates, so reviewed images exercise production state rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "desktop-devtools")]
pub(super) enum NativeVisualCatalogFixture {
    NotLoaded,
    Loading,
    Ready,
    Error,
    Empty,
}

/// Responsive drawer selected by a deterministic native visual replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "desktop-devtools")]
pub(super) enum NativeVisualDrawerFixture {
    Sessions,
    Inspector,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResizablePanel {
    Sessions,
    Context,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PanelResizeState {
    pub(crate) panel: ResizablePanel,
    pub(crate) pointer_origin_x: f32,
    pub(crate) width_origin: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum FocusInputModality {
    Keyboard,
    #[default]
    Pointer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DesktopModalKind {
    Authorization,
    CommandPalette,
    FullMessage,
    Search,
    ConfirmDeleteSession,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionDeleteConfirm {
    pub(crate) session_id: String,
    pub(crate) name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationFullMessageView {
    pub(crate) block_id: String,
    pub(crate) title: Arc<str>,
    pub(crate) text: Arc<str>,
    pub(crate) source_truncated: bool,
}

#[derive(Default)]
pub(crate) struct SessionWorkspacePresentation {
    pub(crate) conversation_controller: ConversationController,
    pub(crate) inspector_section: InspectorSection,
}

impl RuntimeWorkspacePresentation for SessionWorkspacePresentation {
    fn mark_composer_accepted(&mut self) {
        self.conversation_controller.mark_live_dirty();
    }

    fn reconcile_projection(
        &mut self,
        composer: &mut ComposerState,
        update: RuntimeProjectionPresentation<'_>,
    ) -> bool {
        let RuntimeProjectionPresentation {
            projection,
            replaced,
            delta,
            sequence,
            completes_submitted_prompt,
            active_operation_after,
        } = update;
        self.conversation_controller
            .apply_projection_delta(replaced, delta, sequence);
        let mut composer_needs_sync = false;
        if replaced {
            if completes_submitted_prompt
                && !active_operation_after
                && composer.submitted().is_some()
            {
                if let Some((live_id, durable_id)) =
                    composer.reconcile_completed_submission(projection.conversation())
                {
                    self.conversation_controller
                        .reconcile_live_selection(&live_id, &durable_id);
                }
                composer_needs_sync = true;
            }
            let source = ConversationSource::new(projection, composer.submitted());
            self.conversation_controller
                .reconcile_hydration(&source, sequence);
        } else if delta.is_some_and(|delta| delta.conversation || delta.tools) {
            let source = ConversationSource::new(projection, composer.submitted());
            self.conversation_controller
                .reconcile_content(&source, sequence);
        }
        composer_needs_sync
    }
}

pub(crate) type SessionWorkspace = WorkspaceState<SessionWorkspacePresentation>;
pub(crate) type NativeDesktopState =
    DesktopState<SessionWorkspace, ProjectCatalogController, RuntimeWorkspaceDefaults>;

fn build_session_workspace(
    project: CodingAgentEmbeddingSnapshot,
    projection: Option<DesktopProjection>,
    preference_notice: Option<String>,
    thinking_selection: DesktopThinkingLevel,
    draft_workspace_selection: CodingAgentWorkspaceSelection,
) -> SessionWorkspace {
    let (thinking_selection, thinking_fallback) =
        admitted_thinking_selection(&project, thinking_selection);
    WorkspaceState::new(
        project,
        projection,
        draft_workspace_selection,
        preference_notice,
        thinking_selection,
        thinking_fallback,
        SessionWorkspacePresentation::default(),
    )
}

fn session_workspace_with_thinking(
    project: CodingAgentEmbeddingSnapshot,
    projection: Option<DesktopProjection>,
    preference_notice: Option<String>,
    thinking_selection: DesktopThinkingLevel,
) -> SessionWorkspace {
    let selection = workspace_selection_from_embedding(&project);
    build_session_workspace(
        project,
        projection,
        preference_notice,
        thinking_selection,
        selection,
    )
}

pub(super) struct NativeShell {
    connection: ShellConnection,
    app: NativeDesktopState,
    global_skills: Arc<[CodingAgentResourceCommand]>,
    views: ShellViews,
    ui: ShellUiState,
}

struct RuntimePoll {
    transition: Transition,
    running: bool,
}

pub(super) struct NativeShellInit {
    pub(super) runtime: DesktopRuntimeBridge,
    pub(super) workspace: NativeShellWorkspaceInit,
    pub(super) projectless_workspace_selection: CodingAgentWorkspaceSelection,
    pub(super) global_skills: Arc<[CodingAgentResourceCommand]>,
    pub(super) preferences: DesktopPreferences,
    pub(super) preference_writer: Option<PreferenceWriter>,
    pub(super) preference_notice: Option<String>,
}

pub(super) enum NativeShellWorkspaceInit {
    Home(Box<CodingAgentEmbeddingSnapshot>),
    Session(Box<DesktopProjection>),
}

impl NativeShell {
    pub(super) fn new(init: NativeShellInit, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let NativeShellInit {
            runtime,
            workspace,
            projectless_workspace_selection,
            global_skills,
            preferences,
            preference_writer,
            preference_notice,
        } = init;
        let (project, projection) = match workspace {
            NativeShellWorkspaceInit::Home(project) => (*project, None),
            NativeShellWorkspaceInit::Session(projection) => {
                (projection.project().clone(), Some(*projection))
            }
        };
        assert!(
            matches!(
                &projectless_workspace_selection,
                CodingAgentWorkspaceSelection::Projectless { .. }
            ),
            "the desktop Home clear target must be a managed Projectless workspace"
        );
        let (connection, mut runtime_executor) =
            ShellConnection::connect(runtime, preference_writer);
        let runtime_shutdown_signal = runtime_executor.shutdown_signal();
        let command_tracker = CommandTracker::default();
        let center_header_focus = cx.focus_handle().tab_stop(true).tab_index(1);
        let sidebar_focus = cx.focus_handle().tab_stop(true).tab_index(2);
        let center_body_focus = cx.focus_handle().tab_stop(true).tab_index(3);
        let inspector_focus = cx.focus_handle().tab_stop(true).tab_index(5);
        let authorization_focus = cx.focus_handle().tab_stop(true).tab_index(6);
        let command_palette_focus = cx.focus_handle().tab_stop(true).tab_index(6);
        let full_message_focus = cx.focus_handle().tab_stop(true).tab_index(6);
        let search_focus = cx.focus_handle().tab_stop(true).tab_index(6);
        let modal_focus = cx.focus_handle().tab_stop(true).tab_index(6);
        let conversation_pane = cx.new(|_| ConversationPane::new());
        let conversation_header = cx.new(|_| ConversationHeader::new(center_header_focus.clone()));
        let sessions_pane = cx.new(|cx| SessionsPane::new(sidebar_focus.clone(), window, cx));
        let composer_pane = cx.new(|cx| ComposerPane::new(window, cx));
        let home_pane = cx.new(|_| HomePane::new());
        let skills_pane = cx.new(|_| SkillsPane::new());
        let inspector_pane = cx.new(|cx| InspectorPane::new(inspector_focus.clone(), cx));
        let toast_host = cx.new(|cx| ToastHost::new(window, cx));
        let root_modal_host = cx.new(|cx| {
            RootModalHost::new(
                authorization_focus.clone(),
                command_palette_focus.clone(),
                full_message_focus.clone(),
                search_focus.clone(),
                modal_focus.clone(),
                window,
                cx,
            )
        });
        let center_drawer_host =
            cx.new(|_| CenterDrawerHost::new(sessions_pane.clone(), inspector_pane.clone()));

        let subscriptions = vec![
            cx.on_focus(&center_header_focus, window, |this, window, cx| {
                this.record_focus(FocusTarget::CenterHeader, window, cx);
            }),
            cx.on_focus(&sidebar_focus, window, |this, window, cx| {
                this.record_focus(FocusTarget::Sidebar, window, cx);
            }),
            cx.on_focus(&center_body_focus, window, |this, window, cx| {
                this.record_focus(FocusTarget::CenterBody, window, cx);
            }),
            cx.on_focus(&inspector_focus, window, |this, window, cx| {
                this.record_focus(FocusTarget::Inspector, window, cx);
            }),
            cx.subscribe_in(
                &conversation_pane,
                window,
                |this, _, event: &ConversationPaneEvent, window, cx| {
                    this.dispatch_ui_intent(event.into(), window, cx);
                },
            ),
            cx.subscribe_in(
                &conversation_header,
                window,
                |this, _, event: &ConversationHeaderEvent, window, cx| {
                    this.dispatch_ui_intent(event.into(), window, cx);
                },
            ),
            cx.subscribe_in(
                &sessions_pane,
                window,
                |this, _, event: &SessionsPaneEvent, window, cx| {
                    this.dispatch_ui_intent(event.into(), window, cx);
                },
            ),
            cx.subscribe_in(
                &composer_pane,
                window,
                |this, _, event: &ComposerPaneEvent, window, cx| {
                    this.dispatch_ui_intent(event.into(), window, cx);
                },
            ),
            cx.subscribe_in(
                &inspector_pane,
                window,
                |this, _, event: &InspectorPaneEvent, window, cx| {
                    this.dispatch_ui_intent(event.into(), window, cx);
                },
            ),
            cx.subscribe_in(
                &root_modal_host,
                window,
                |this, _, event: &RootModalHostEvent, window, cx| {
                    this.dispatch_ui_intent(event.into(), window, cx);
                },
            ),
            cx.subscribe_in(
                &center_drawer_host,
                window,
                |this, _, event: &CenterDrawerHostEvent, window, cx| {
                    this.dispatch_ui_intent(event.into(), window, cx);
                },
            ),
            cx.observe_window_bounds(window, Self::window_bounds_changed),
            cx.on_release(move |_, _| runtime_shutdown_signal.signal()),
        ];

        let composer_focus = composer_pane.read(cx).focus_handle().clone();
        composer_focus.focus(window, cx);
        cx.spawn(async move |this, cx| {
            while let Some(updates) = runtime_executor.next_update_batch().await {
                let Some(this) = this.upgrade() else {
                    break;
                };
                this.update(cx, |this, cx| {
                    this.connection.enqueue_runtime_updates(updates);
                    let poll = this.poll_runtime();
                    this.apply_runtime_poll(poll, cx)
                });
            }
            let _ = runtime_executor.shutdown().await;
        })
        .detach();

        let thinking_selection = projection
            .as_ref()
            .map(|projection| {
                preferences.thinking_level_for_session(&projection.snapshot().session.session_id)
            })
            .unwrap_or_default();
        let home_project = project.clone();
        let active_session_id = projection.as_ref().map(|projection| {
            SessionId::from_dto(projection.snapshot().session.session_id.clone())
        });
        let initial_workspace = if projection.is_none() {
            build_session_workspace(
                project,
                projection,
                preference_notice,
                thinking_selection,
                projectless_workspace_selection.clone(),
            )
        } else {
            session_workspace_with_thinking(
                project,
                projection,
                preference_notice,
                thinking_selection,
            )
        };
        let workspace_store = match active_session_id {
            Some(session_id) => {
                let mut store = WorkspaceStore::new(build_session_workspace(
                    home_project.clone(),
                    None,
                    None,
                    DesktopThinkingLevel::Default,
                    projectless_workspace_selection.clone(),
                ));
                store.insert_session(session_id.clone(), initial_workspace);
                assert!(store.activate(&WorkspaceKey::Session(session_id)));
                store
            }
            None => WorkspaceStore::new(initial_workspace),
        };
        let app = DesktopState::new_with_workspace_defaults(
            workspace_store,
            command_tracker,
            ProjectCatalogController::default(),
            preferences,
            RuntimeWorkspaceDefaults {
                home_project,
                projectless_selection: projectless_workspace_selection,
            },
        );
        let mut shell = Self {
            connection,
            app,
            global_skills,
            views: ShellViews::new(
                conversation_pane,
                conversation_header,
                sessions_pane,
                composer_pane,
                home_pane,
                skills_pane,
                inspector_pane,
                toast_host,
                root_modal_host,
                center_drawer_host,
                subscriptions,
            ),
            ui: ShellUiState::new(
                center_header_focus,
                sidebar_focus,
                center_body_focus,
                inspector_focus,
                authorization_focus,
                command_palette_focus,
                full_message_focus,
                search_focus,
                modal_focus,
            ),
        };
        debug_assert!(shell.views.subscription_count() > 0);
        shell.refresh_views(
            UiChangeSet::from_regions(&[
                UiRegion::Conversation,
                UiRegion::ConversationHeader,
                UiRegion::Composer,
                UiRegion::Sessions,
                UiRegion::Inspector,
                UiRegion::Skills,
                UiRegion::Modal,
                UiRegion::Drawer,
                UiRegion::Toast,
            ]),
            cx,
        );
        shell
    }

    fn dispatch_ui_intent(
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
                self.ui.pending_delete_session = Some(SessionDeleteConfirm {
                    session_id,
                    name,
                });
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

    fn active_command_contains(&self, intent: &DesktopCommandIntent) -> bool {
        self.app
            .commands
            .contains(self.app.workspaces.active_key(), intent)
    }

    fn active_command_contains_where(
        &self,
        predicate: impl Fn(&DesktopCommandIntent) -> bool,
    ) -> bool {
        self.app
            .commands
            .contains_where(self.app.workspaces.active_key(), predicate)
    }

    fn complete_active_command(&mut self, command_id: u64, intent: &DesktopCommandIntent) -> bool {
        let owner = self.app.workspaces.active_key().clone();
        self.app
            .complete_runtime_command(command_id, &owner, intent)
    }

    fn navigate_center(
        &mut self,
        target: CenterNavigationTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            CenterNavigationTarget::NewConversation => self.show_home_workspace(window, cx),
            CenterNavigationTarget::Skills => {
                self.ui.center_surface = CenterSurface::Skills;
                self.dismiss_drawer(window, cx, false);
                self.focus_target(FocusTarget::CenterBody, window, cx);
                self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
                cx.notify();
            }
            CenterNavigationTarget::Session(session_id) => {
                self.ui.center_surface = CenterSurface::Primary;
                self.dismiss_drawer(window, cx, false);
                self.focus_target(FocusTarget::CenterBody, window, cx);
                if self
                    .app
                    .workspaces
                    .active_mut()
                    .projection
                    .as_ref()
                    .is_some_and(|projection| {
                        projection.snapshot().session.session_id == session_id
                    })
                {
                    self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
                    cx.notify();
                } else {
                    self.open_session(session_id, cx);
                }
            }
        }
    }

    fn show_home_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ui.center_surface = CenterSurface::Primary;
        let activated = self.app.workspaces.activate(&WorkspaceKey::Home);
        debug_assert!(activated, "Home must remain a stable workspace entry");

        self.dismiss_drawer(window, cx, true);
        self.record_focus(FocusTarget::Composer, window, cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Conversation), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Modal), cx);
        cx.notify();
    }

    fn show_project_home_workspace(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let home = WorkspaceKey::Home;
        let Some(workspace) = self.app.workspaces.get_mut(&home) else {
            return;
        };
        if !workspace.project_directory_editable() {
            workspace.set_preference_notice(
                "The new conversation is still being prepared; try again when it is idle.".into(),
            );
            self.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            cx.notify();
            return;
        }
        workspace.draft_workspace_selection = CodingAgentWorkspaceSelection::project(path);
        self.show_home_workspace(window, cx);
    }

    fn open_workspace_count(&self) -> usize {
        self.app.workspaces.session_count()
    }

    fn reserve_session_command(
        &mut self,
        session_id: &str,
        intent: DesktopCommandIntent,
    ) -> Result<u64, String> {
        let key = WorkspaceKey::session(session_id);
        if !self.app.workspaces.contains(&key) {
            return Err("Cannot close an unavailable session.".to_owned());
        }
        self.app
            .commands
            .reserve(key, intent)
            .map_err(|error| error.to_string())
    }

    fn close_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let intent = DesktopCommandIntent::CloseSession {
            session_id: session_id.to_owned(),
        };
        let command_id = match self.reserve_session_command(session_id, intent.clone()) {
            Ok(command_id) => command_id,
            Err(error) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error);
                self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
                return;
            }
        };
        let admission = self
            .connection
            .runtime_client
            .as_ref()
            .ok_or_else(|| "desktop runtime is unavailable".to_owned())
            .and_then(|runtime| {
                runtime
                    .try_close_session(command_id, session_id)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = admission {
            let owner = WorkspaceKey::session(session_id);
            let _ = self
                .app
                .complete_runtime_command(command_id, &owner, &intent);
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice(error);
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
    }

    fn delete_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let intent = DesktopCommandIntent::DeleteSession {
            session_id: session_id.to_owned(),
        };
        let owner = WorkspaceKey::session(session_id);
        let command_id = match self.app.commands.reserve(owner.clone(), intent.clone()) {
            Ok(command_id) => command_id,
            Err(error) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
                return;
            }
        };
        let admission = self
            .connection
            .runtime_client
            .as_ref()
            .ok_or_else(|| "desktop runtime is unavailable".to_owned())
            .and_then(|runtime| {
                runtime
                    .try_delete_session(command_id, session_id)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = admission {
            let _ = self
                .app
                .complete_runtime_command(command_id, &owner, &intent);
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice(error);
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
    }

    #[cfg(feature = "desktop-devtools")]
    pub(super) fn install_native_visual_catalog_fixture(
        &mut self,
        fixture: NativeVisualCatalogFixture,
        cx: &mut Context<Self>,
    ) {
        self.app.catalog = ProjectCatalogController::default();
        match fixture {
            NativeVisualCatalogFixture::NotLoaded => {}
            NativeVisualCatalogFixture::Loading => self.app.catalog.begin_refresh(),
            NativeVisualCatalogFixture::Ready => {
                let Some(projection) = self.app.workspaces.active_mut().projection.as_ref() else {
                    return;
                };
                let session_id = projection.snapshot().session.session_id.clone();
                let mut entry = desktop::runtime::DesktopSessionCatalogEntry {
                    session_id,
                    name: Some("Current desktop task".into()),
                    // A future timestamp is clamped to zero elapsed time by the
                    // presentation helper, keeping the replay's `now` label
                    // deterministic across calendar dates.
                    created_at: "9999-12-31T23:59:59Z".into(),
                    updated_at: "9999-12-31T23:59:59Z".into(),
                    ..Default::default()
                };
                if let Some(workspace) = self.app.workspaces.active_mut().project.workspace.as_ref()
                {
                    entry.workspace = workspace.overview.clone();
                    entry.workspace_migration =
                        coding_agent::api::view::CodingAgentWorkspaceMigration {
                            outcome: coding_agent::api::view::CodingAgentWorkspaceMigrationOutcome::NotRequired,
                            diagnostic: None,
                        };
                }
                self.app.catalog.replace_catalog(vec![entry], 0);
            }
            NativeVisualCatalogFixture::Error => self
                .app
                .catalog
                .fail_refresh("The project catalog could not be loaded."),
            NativeVisualCatalogFixture::Empty => {
                self.app.catalog.replace_catalog(Vec::new(), 0);
            }
        }
        self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        cx.notify();
    }

    #[cfg(feature = "desktop-devtools")]
    pub(super) fn install_native_visual_drawer_fixture(
        &mut self,
        fixture: NativeVisualDrawerFixture,
        cx: &mut Context<Self>,
    ) {
        self.ui.active_drawer = Some(match fixture {
            NativeVisualDrawerFixture::Sessions => CenterDrawerKind::Sessions,
            NativeVisualDrawerFixture::Inspector => CenterDrawerKind::Inspector,
        });
        self.ui.drawer_restore_focus = Some(self.ui.focus.active());
        self.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Drawer), cx);
        cx.notify();
    }

    #[cfg(feature = "desktop-devtools")]
    pub(super) fn install_native_visual_home_project_fixture(
        &mut self,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        debug_assert!(self.app.workspaces.active_mut().projection.is_none());
        self.app.workspaces.active_mut().draft_workspace_selection =
            CodingAgentWorkspaceSelection::project(path);
        self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
        cx.notify();
    }

    #[cfg(feature = "desktop-devtools")]
    pub(super) fn install_native_visual_non_reasoning_fixture(&mut self, cx: &mut Context<Self>) {
        debug_assert!(self.app.workspaces.active_mut().projection.is_none());
        self.app.workspaces.active_mut().project.selected_model_id = "review-fixture".into();
        let selected_model_id = self
            .app
            .workspaces
            .active_mut()
            .project
            .selected_model_id
            .clone();
        for model in &mut self.app.workspaces.active_mut().project.models {
            model.selected = model.id == selected_model_id;
        }
        self.app.workspaces.active_mut().thinking_selection = DesktopThinkingLevel::High;
        self.reconcile_thinking_selection_with_project();
        self.refresh_views(UiChangeSet::one(UiRegion::ConversationHeader), cx);
        self.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
        cx.notify();
    }

    #[cfg(feature = "desktop-devtools")]
    fn reconcile_thinking_selection_for(&mut self, owner: &WorkspaceKey) {
        let Some(workspace) = self.app.workspaces.get_mut(owner) else {
            return;
        };
        let (selection, fallback) =
            admitted_thinking_selection(&workspace.project, workspace.thinking_selection);
        if !fallback {
            return;
        }
        workspace.thinking_selection = selection;
        workspace.thinking_hint = Some(Arc::from("Thinking reset to Auto for the selected model."));
        let session_id = workspace
            .projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.clone());
        if let Some(session_id) = session_id.as_deref() {
            self.remember_thinking_selection(session_id, selection);
        }
    }
}

fn focus_target_label(target: FocusTarget) -> &'static str {
    match target {
        FocusTarget::CenterHeader => "Center header",
        FocusTarget::Sidebar => "Sidebar",
        FocusTarget::CenterBody => "Center workspace",
        FocusTarget::Composer => "Composer",
        FocusTarget::Inspector => "Inspector",
        FocusTarget::Modal => "Modal",
    }
}

mod command_adapter;
mod commands;
#[path = "../ui/conversation/adapter.rs"]
mod conversation_adapter;
#[path = "../ui/conversation/layout_adapter.rs"]
mod conversation_layout_adapter;
mod intent;
#[path = "../ui/shell/layout_adapter.rs"]
mod layout_adapter;
#[path = "../ui/shell/overlay_adapter.rs"]
mod overlay_adapter;
mod platform_update;
#[path = "../ui/sessions/catalog_adapter.rs"]
mod project_catalog_controller;
mod review_adapter;
mod root_actions;
mod root_view;
mod runtime_adapter;
#[cfg(test)]
mod tests;

use crate::ui::conversation::composer_pane::{ComposerPane, ComposerPaneEvent};
use crate::ui::conversation::controller::{
    ConversationController, ConversationSource, RESIZE_DEBOUNCE as CONVERSATION_RESIZE_DEBOUNCE,
    message_block_id as message_conversation_block_id, tool_block_id as tool_conversation_block_id,
};
use crate::ui::conversation::header::{ConversationHeader, ConversationHeaderEvent};
use crate::ui::conversation::pane::{ConversationPane, ConversationPaneEvent};
use crate::ui::conversation::{
    composer_pane, header as conversation_header, pane as conversation_pane,
};
use crate::ui::home::HomePane;
use crate::ui::inspector::pane as inspector_pane;
use crate::ui::inspector::pane::{InspectorPane, InspectorPaneEvent};
use crate::ui::sessions::pane as sessions_pane;
use crate::ui::sessions::pane::{SessionsPane, SessionsPaneEvent};
use crate::ui::shell::drawer::{CenterDrawerHost, CenterDrawerHostEvent, CenterDrawerKind};
use crate::ui::shell::modal::{RootModalHost, RootModalHostEvent};
use crate::ui::shell::toast::{ToastHost, ToastNotice};
use crate::ui::shell::{CenterNavigationTarget, CenterSurface};
use crate::ui::shell::{drawer as center_drawer_host, modal as root_modal_host};
use crate::ui::skills as skills_pane;
use crate::ui::skills::SkillsPane;
use intent::UiIntent;
