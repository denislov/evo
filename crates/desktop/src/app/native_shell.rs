use coding_agent::api::authorization::{ToolAuthorizationDecision, ToolAuthorizationIdentity};
use coding_agent::api::embedding::{
    CodingAgentEmbeddingSnapshot, CodingAgentResourceCommand, CodingAgentWorkspaceSelection,
};
#[cfg(test)]
use coding_agent::api::embedding::{CodingAgentResourceCommandKind, CodingAgentThinkingLevel};
use coding_agent::api::event::CodingAgentRecoveryResolution;
use coding_agent::api::review::CodingAgentFileReviewRequest;
#[cfg(test)]
use desktop::conversation::{
    ComposerAdmission, TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT, conversation_block_height,
};
use desktop::conversation::{
    ComposerState, ComposerSubmissionKind, ConversationBlockKind, ConversationRowMeasurement,
    MAX_COPY_BYTES, conversation_copy_text, conversation_width_bucket,
};
use desktop::platform::preferences::{PreferenceWriteResult, PreferenceWriter};
use desktop::preferences::{DesktopPreferences, DesktopThinkingLevel};
use desktop::projection::{DesktopProjection, DesktopRecoveryStatus};
#[cfg(test)]
use desktop::projection::{DesktopProjectionLifecycle, ProjectionEvent};
#[cfg(test)]
use desktop::runtime::MAX_PROMPT_ATTACHMENTS;
use desktop::runtime::{
    DesktopRecoveryAction, DesktopRecoveryIdentity, DesktopRuntimeBridge,
    DesktopRuntimeSelectionKind, validate_prompt_attachments,
};
use desktop::shell::{
    CONTEXT_PANEL_MAX_WIDTH, CONTEXT_PANEL_MIN_WIDTH, CONTEXT_PANEL_WIDTH,
    CONVERSATION_CONTENT_MAX_WIDTH, FocusTarget, MIN_CONVERSATION_WIDTH, PanelVisibility,
    SESSION_PANEL_MAX_WIDTH, SESSION_PANEL_MIN_WIDTH, SESSION_PANEL_WIDTH, SemanticColor,
    SemanticStatus, SemanticTheme, ShellLayout, UI_FONT_FAMILY, truncate_label,
};
#[cfg(test)]
use desktop::ui::inspector::review::DesktopFileReviewDocument;
use gpui::{
    ClipboardItem, Context, KeyDownEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathPromptOptions, ScrollStrategy, Window, WindowBounds, prelude::*, rgb,
};
#[cfg(test)]
use gpui::{Role, div, px};
use std::path::PathBuf;
#[cfg(test)]
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) use self::evo_brand::{EvoBrandFixture, EvoBrandMode};
use crate::actions::{
    self, AbortActiveOperation, AuthorizationAllowForOperation, AuthorizationAllowOnce,
    AuthorizationDeny, CopySelectedConversation, DesktopPaletteCommand, EscapeHierarchy,
    FocusComposer, FocusNextRegion, FocusPreviousRegion, FollowLatestOutput, NewSession,
    OpenCommandPalette, OpenFileSurface, PALETTE_ENTRIES, PaletteConfirm, PaletteNext,
    PalettePrevious, SelectNextConversation, SelectPreviousConversation, SubmitComposer,
    ToggleInspectorPanel, ToggleSelectedConversationDetails, TrapOverlayFocus,
};
#[cfg(test)]
use crate::application::catalog::ProjectCatalogState;
#[cfg(test)]
use crate::application::reducer::safe_runtime_rejection_notice;
use crate::application::{
    catalog::ProjectCatalogController,
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
#[cfg(test)]
use crate::ui::shell::presentation::{
    recovery_status_label, runtime_state_label, usage_cost_label,
};
use crate::ui::shell::{
    ShellConnection, ShellUiState, ShellViews, presentation::recovery_action_label,
};

const MAX_RUNTIME_UPDATES_PER_FRAME: usize = 64;
const INSPECTOR_TELEMETRY_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
struct ConversationBlockVisual {
    glyph: &'static str,
    accent: SemanticColor,
    align_right: bool,
}

fn conversation_focus_accent(focused: bool, theme: SemanticTheme) -> SemanticColor {
    if focused { theme.accent } else { theme.divider }
}

fn semantic_status_color(status: SemanticStatus) -> gpui::Rgba {
    let theme = SemanticTheme::GEEK_DARK;
    rgb(match status {
        SemanticStatus::Idle => theme.muted_text.value(),
        SemanticStatus::Running => theme.accent.value(),
        SemanticStatus::Warning | SemanticStatus::Authorization => theme.warning.value(),
        SemanticStatus::Error => theme.danger.value(),
    })
}

fn inspector_telemetry_refresh_delay(last_refresh: Option<Instant>, now: Instant) -> Duration {
    last_refresh.map_or(Duration::ZERO, |last_refresh| {
        INSPECTOR_TELEMETRY_REFRESH_INTERVAL
            .saturating_sub(now.saturating_duration_since(last_refresh))
    })
}

fn conversation_block_visual(
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
pub(super) enum ComposerRunningMode {
    #[default]
    SteerNow,
    QueueNext,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum InspectorSection {
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
pub(super) enum NativeVisualCatalogFixture {
    NotLoaded,
    Loading,
    Ready,
    Error,
    Empty,
}

/// Responsive drawer selected by a deterministic native visual replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl ComposerRunningMode {
    const fn submission_kind(self) -> ComposerSubmissionKind {
        match self {
            Self::SteerNow => ComposerSubmissionKind::Steer,
            Self::QueueNext => ComposerSubmissionKind::FollowUp,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DesktopModalKind {
    Authorization,
    CommandPalette,
    FullMessage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationFullMessageView {
    pub(crate) block_id: String,
    pub(crate) title: Arc<str>,
    pub(crate) text: Arc<str>,
    pub(crate) source_truncated: bool,
}

#[derive(Default)]
pub(super) struct SessionWorkspacePresentation {
    conversation_controller: ConversationController,
    inspector_section: InspectorSection,
    composer_running_mode: ComposerRunningMode,
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

pub(super) type SessionWorkspace = WorkspaceState<SessionWorkspacePresentation>;
pub(super) type NativeDesktopState =
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

#[cfg(test)]
fn make_session_workspace(
    project: CodingAgentEmbeddingSnapshot,
    projection: Option<DesktopProjection>,
    preference_notice: Option<String>,
) -> SessionWorkspace {
    session_workspace_with_thinking(
        project,
        projection,
        preference_notice,
        DesktopThinkingLevel::Default,
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
        let conversation_pane = cx.new(|_| ConversationPane::new());
        let conversation_header = cx.new(|_| ConversationHeader::new(center_header_focus.clone()));
        let sessions_pane = cx.new(|cx| SessionsPane::new(sidebar_focus.clone(), window, cx));
        let composer_pane = cx.new(|cx| ComposerPane::new(window, cx));
        let home_pane = cx.new(|_| HomePane::new());
        let skills_pane = cx.new(|_| SkillsPane::new());
        let inspector_pane = cx.new(|cx| InspectorPane::new(inspector_focus.clone(), cx));
        let toast_host = cx.new(|cx| ToastHost::new(window, cx));
        let root_modal_host = cx.new(|_| {
            RootModalHost::new(
                authorization_focus.clone(),
                command_palette_focus.clone(),
                full_message_focus.clone(),
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
            UiIntent::SubmitPrimary => {
                if !self.root_action_blocked_by_modal(window, cx) {
                    self.submit_primary_composer(cx);
                }
            }
            UiIntent::Submit => self.submit_composer(cx),
            UiIntent::SubmitRunning => self
                .submit_active_control(self.active_composer_running_mode().submission_kind(), cx),
            UiIntent::SetRunningMode(mode) => self.set_active_composer_running_mode(mode, cx),
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
            UiIntent::ConversationMeasured(measurement) => {
                self.submit_conversation_row_measurement(&measurement, cx);
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

fn adjacent_conversation_index(
    row_count: usize,
    current_index: Option<usize>,
    reverse: bool,
) -> Option<usize> {
    let last_index = row_count.checked_sub(1)?;
    Some(
        match (current_index.filter(|index| *index < row_count), reverse) {
            (Some(index), true) => index.saturating_sub(1),
            (Some(index), false) => index.saturating_add(1).min(last_index),
            (None, true) => last_index,
            (None, false) => 0,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{borrow::Cow, cell::RefCell, collections::HashSet, fs};

    use crate::runtime::{DesktopPromptTarget, DesktopRuntimeOwnerTarget};

    use desktop::conversation::{
        ConversationItemKey, ConversationItemKind, ConversationRowRenderCache,
        ConversationRowRenderSource,
    };
    use gpui::size;

    use coding_agent::api::authorization::{
        ToolAuthorizationPreview, ToolAuthorizationRequest, ToolAuthorizationRisk,
        ToolAuthorizationScope,
    };
    use coding_agent::api::client::{
        CodingAgentContextSnapshot, CodingAgentFileChangeSnapshot, CodingAgentRecoveryPending,
        CodingAgentSnapshot, CodingAgentSnapshotCursor, UI_SNAPSHOT_PROTOCOL_VERSION,
    };
    use coding_agent::api::embedding::{
        CodingAgentEmbeddingSnapshot, CodingAgentModelChoice, CodingAgentProfileChoice,
        CodingAgentResourceSummary, CodingAgentSettingsSummary, CodingAgentThinkingCapability,
    };
    use coding_agent::api::review::CodingAgentFileReview;
    use coding_agent::api::view::{
        CodingAgentCapabilities, CodingAgentSessionTranscriptItem, CodingAgentSessionView,
        CodingAgentTranscriptSnapshot, ProfileId, ProfileKind, ProfileSource,
    };
    use gpui::TestAppContext;
    use gpui_component::{Theme, ThemeMode, text::TextViewState};

    use desktop::shell::{
        COMPOSER_MAX_HEIGHT, COMPOSER_MIN_HEIGHT, CONVERSATION_ROW_VERTICAL_PADDING_PX,
    };

    fn session_key(session_id: &str) -> WorkspaceKey {
        WorkspaceKey::session(session_id)
    }

    fn active_session_id(shell: &NativeShell) -> Option<&str> {
        shell
            .app
            .workspaces
            .active_key()
            .session_id()
            .map(SessionId::as_str)
    }

    fn insert_session_workspace(
        shell: &mut NativeShell,
        session_id: &str,
        workspace: SessionWorkspace,
    ) {
        assert!(
            shell
                .app
                .workspaces
                .insert_session(SessionId::from_dto(session_id), workspace)
                .is_none(),
            "test session IDs must be unique"
        );
    }

    fn workspace_for_session<'a>(
        shell: &'a NativeShell,
        session_id: &str,
    ) -> Option<&'a SessionWorkspace> {
        shell.app.workspaces.get(&session_key(session_id))
    }

    fn activate_session(shell: &mut NativeShell, session_id: &str) -> bool {
        shell.app.workspaces.activate(&session_key(session_id))
    }

    fn set_project_directory_for_test(shell: &mut NativeShell, path: PathBuf) -> bool {
        let owner = shell.app.workspaces.active_key().clone();
        PlatformUpdatePort::set_project_directory(shell, &owner, path)
    }

    fn apply_picker_result_for_test(
        shell: &mut NativeShell,
        picker: DesktopPickerKind,
        outcome: PlatformOutcome<Vec<PathBuf>>,
        cx: &mut Context<NativeShell>,
    ) {
        let owner = shell.app.workspaces.active_key().clone();
        let transition = shell
            .connection
            .controller
            .pick_paths(owner, picker)
            .expect("test picker effect identity is available");
        let identity = match transition.effects().first() {
            Some(DesktopEffect::PickPaths { identity, .. }) => identity.clone(),
            _ => panic!("picker request must emit one typed picker effect"),
        };
        shell.dispatch_platform_result(
            PlatformResult::PathsPicked {
                identity,
                picker,
                outcome,
            },
            cx,
        );
    }

    fn session_workspace_ids(shell: &NativeShell) -> HashSet<String> {
        shell
            .app
            .workspaces
            .iter()
            .filter_map(|(key, _)| key.session_id().map(|id| id.as_str().to_owned()))
            .collect()
    }

    fn visual_test_snapshot() -> desktop::runtime::DesktopRuntimeHydratedSnapshot {
        visual_test_snapshot_for("desktop-visual-test")
    }

    fn model_menu_fixture(
        id: &str,
        name: &str,
        provider: &str,
        configured: bool,
        supports_text: bool,
    ) -> CodingAgentModelChoice {
        CodingAgentModelChoice {
            id: id.into(),
            name: name.into(),
            provider: provider.into(),
            reasoning: false,
            thinking_capability: CodingAgentThinkingCapability::default(),
            supports_text,
            supports_images: !supports_text,
            context_window: 32_000,
            max_output_tokens: 4_000,
            configured,
            selected: false,
        }
    }

    fn visual_test_snapshot_for(
        session_id: &str,
    ) -> desktop::runtime::DesktopRuntimeHydratedSnapshot {
        let session_id = session_id.to_owned();
        let stream_id = format!("{session_id}-stream");
        desktop::runtime::DesktopRuntimeHydratedSnapshot {
            project: CodingAgentEmbeddingSnapshot {
                cwd: std::path::PathBuf::from("/desktop-visual-test"),
                workspace: None,
                global_config_dir: std::path::PathBuf::from("/desktop-visual-test/config"),
                selected_model_id: "test-model".into(),
                default_agent_profile_id: ProfileId::from("default"),
                models: vec![
                    CodingAgentModelChoice {
                        id: "test-model".into(),
                        name: "Test Model".into(),
                        provider: "fixture".into(),
                        reasoning: true,
                        thinking_capability: CodingAgentThinkingCapability {
                            supported: true,
                            explicit_levels: vec![
                                CodingAgentThinkingLevel::Minimal,
                                CodingAgentThinkingLevel::Low,
                                CodingAgentThinkingLevel::Medium,
                                CodingAgentThinkingLevel::High,
                                CodingAgentThinkingLevel::XHigh,
                            ],
                            can_disable: true,
                        },
                        supports_text: true,
                        supports_images: true,
                        context_window: 200_000,
                        max_output_tokens: 32_000,
                        configured: true,
                        selected: true,
                    },
                    CodingAgentModelChoice {
                        id: "adjacent-model".into(),
                        name: "Adjacent Model".into(),
                        provider: "fixture".into(),
                        reasoning: false,
                        thinking_capability: CodingAgentThinkingCapability::default(),
                        supports_text: true,
                        supports_images: false,
                        context_window: 80_000,
                        max_output_tokens: 8_000,
                        configured: true,
                        selected: false,
                    },
                    CodingAgentModelChoice {
                        id: "exact-target-model".into(),
                        name: "Exact Target".into(),
                        provider: "fixture".into(),
                        reasoning: false,
                        thinking_capability: CodingAgentThinkingCapability::default(),
                        supports_text: true,
                        supports_images: false,
                        context_window: 100_000,
                        max_output_tokens: 16_000,
                        configured: true,
                        selected: false,
                    },
                    CodingAgentModelChoice {
                        id: "image-only-model".into(),
                        name: "Image Only".into(),
                        provider: "fixture".into(),
                        reasoning: false,
                        thinking_capability: CodingAgentThinkingCapability::default(),
                        supports_text: false,
                        supports_images: true,
                        context_window: 32_000,
                        max_output_tokens: 4_000,
                        configured: true,
                        selected: false,
                    },
                ],
                profiles: vec![
                    CodingAgentProfileChoice {
                        id: ProfileId::from("default"),
                        display_name: "Default".into(),
                        description: Some("General coding work".into()),
                        kind: ProfileKind::Agent,
                        source: ProfileSource::BuiltIn,
                        model_id: None,
                    },
                    CodingAgentProfileChoice {
                        id: ProfileId::from("exact-reviewer"),
                        display_name: "Exact Reviewer".into(),
                        description: Some("Review changes before completion".into()),
                        kind: ProfileKind::Agent,
                        source: ProfileSource::Project,
                        model_id: Some("exact-target-model".into()),
                    },
                    CodingAgentProfileChoice {
                        id: ProfileId::from("review-team"),
                        display_name: "Review Team".into(),
                        description: Some("Delegated review team".into()),
                        kind: ProfileKind::Team,
                        source: ProfileSource::Project,
                        model_id: None,
                    },
                ],
                resources: CodingAgentResourceSummary {
                    skill_names: Vec::new(),
                    prompt_template_names: Vec::new(),
                    commands: Vec::new(),
                    context_files: Vec::new(),
                },
                settings: CodingAgentSettingsSummary {
                    default_provider: None,
                    default_model: None,
                    default_thinking_level: None,
                    session_dir: None,
                    no_context_files: true,
                },
                diagnostics: Vec::new(),
            },
            session: CodingAgentSnapshot {
                cursor: CodingAgentSnapshotCursor {
                    stream_id,
                    snapshot_protocol_major: UI_SNAPSHOT_PROTOCOL_VERSION.major,
                    last_event_sequence: 0,
                    last_session_sequence: 0,
                    capability_generation: 0,
                },
                version: UI_SNAPSHOT_PROTOCOL_VERSION,
                session: CodingAgentSessionView {
                    session_id: session_id.clone(),
                    default_agent_profile_id: ProfileId::from("default"),
                },
                capabilities: CodingAgentCapabilities::idle(false),
                active_operation: None,
                drafts: Vec::new(),
                submitted_operation: None,
                pending_authorizations: Vec::new(),
                context: CodingAgentContextSnapshot::default(),
            },
            transcript: CodingAgentTranscriptSnapshot {
                session_id,
                active_leaf_id: None,
                items: Vec::new(),
            },
            pending_recoveries: Vec::new(),
        }
    }

    fn visual_test_projection() -> DesktopProjection {
        DesktopProjection::new(visual_test_snapshot())
            .expect("visual test fixture is a valid product projection")
    }

    fn visual_performance_projection(block_count: usize) -> DesktopProjection {
        let mut snapshot = visual_test_snapshot();
        let payload = "headless frame replay 中文 🙂 ".repeat(8);
        snapshot.transcript.items = (0..block_count)
            .map(|index| CodingAgentSessionTranscriptItem::User {
                text: format!("message {index}: {payload}"),
            })
            .collect();
        DesktopProjection::new(snapshot)
            .expect("headless frame replay fixture is a valid product projection")
    }

    fn clipping_regression_projection() -> DesktopProjection {
        let mut snapshot = visual_test_snapshot();
        let mut text = String::from(
            "# Complete final response\n\n> The tail marker must remain inside the measured row.\n\n",
        );
        for line in 1..=60 {
            text.push_str(&format!(
                "{line}. Layout line {line} — 长中文内容用于验证系统字体回退和换行 🙂 e\u{301}\n"
            ));
        }
        text.push_str(
            "\n- list item one\n- list item two\n\n| column | value |\n|---|---|\n| 中文 | 🙂 |\n\n```rust\nfn tail() {\n    println!(\"visible\");\n}\n```\n\nFINAL TAIL TEXT",
        );
        snapshot
            .transcript
            .items
            .push(CodingAgentSessionTranscriptItem::Assistant {
                id: "clipping-regression-final".into(),
                text,
                thinking: String::new(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: None,
            });
        DesktopProjection::new(snapshot)
            .expect("clipping regression fixture is a valid product projection")
    }

    fn long_integrity_text(label: &str) -> String {
        let mut text = format!("# {label}\n\n> Every final line must remain measurable.\n\n");
        for line in 1..=60 {
            text.push_str(&format!(
                "{line}. {label} line {line} — 中文换行 🙂 e\u{301} {}\n",
                "unbroken-width-probe".repeat(8)
            ));
        }
        text.push_str("\nFINAL TYPE-SPECIFIC TAIL");
        text
    }

    fn projection_with_last_item(item: CodingAgentSessionTranscriptItem) -> DesktopProjection {
        let mut snapshot = visual_test_snapshot();
        snapshot.transcript.items.push(item);
        DesktopProjection::new(snapshot)
            .expect("message-integrity fixture is a valid product projection")
    }

    fn projection_with_items(items: Vec<CodingAgentSessionTranscriptItem>) -> DesktopProjection {
        let mut snapshot = visual_test_snapshot();
        snapshot.transcript.items = items;
        DesktopProjection::new(snapshot)
            .expect("multi-item conversation fixture is a valid product projection")
    }

    fn settle_visual_measurements(cx: &mut gpui::VisualTestContext) {
        cx.executor().advance_clock(Duration::from_millis(100));
        for _ in 0..4 {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
    }

    fn assert_last_row_matches_card_and_tail(cx: &mut gpui::VisualTestContext, label: &str) {
        let row = cx
            .debug_bounds("conversation-last-row")
            .unwrap_or_else(|| panic!("{label}: final virtual row is mounted"));
        let card = cx
            .debug_bounds("conversation-last-card")
            .unwrap_or_else(|| panic!("{label}: final card is laid out"));
        let tail = cx
            .debug_bounds("conversation-tail-marker")
            .unwrap_or_else(|| panic!("{label}: tail marker is laid out"));
        let composer = cx
            .debug_bounds("desktop-composer-panel")
            .unwrap_or_else(|| panic!("{label}: Composer remains visible"));

        assert!(
            (f32::from(row.size.height)
                - (f32::from(card.size.height) + CONVERSATION_ROW_VERTICAL_PADDING_PX as f32))
                .abs()
                <= 1.,
            "{label}: virtual row must match the actual card: row={row:?}, card={card:?}"
        );
        assert!(
            tail.bottom() <= row.bottom() + px(1.),
            "{label}: tail must remain inside the row: tail={tail:?}, row={row:?}"
        );
        assert!(
            tail.bottom() <= composer.top() + px(1.),
            "{label}: tail must remain above the Composer: tail={tail:?}, composer={composer:?}"
        );
    }

    fn initialize_visual_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            actions::bind_keys(cx);
            Theme::change(ThemeMode::Dark, None, cx);
        });
    }

    fn visual_global_skills() -> Arc<[CodingAgentResourceCommand]> {
        Arc::from([CodingAgentResourceCommand {
            name: "review-plan".into(),
            command: "/review-plan".into(),
            description: "Review an implementation plan before coding.".into(),
            kind: CodingAgentResourceCommandKind::Skill,
            model_invocable: true,
        }])
    }

    fn add_visual_shell(
        cx: &mut TestAppContext,
        runtime: DesktopRuntimeBridge,
        projection: DesktopProjection,
    ) -> (gpui::Entity<NativeShell>, &mut gpui::VisualTestContext) {
        add_visual_shell_with_preferences(cx, runtime, projection, DesktopPreferences::default())
    }

    fn visual_preferences_with_inspector() -> DesktopPreferences {
        DesktopPreferences {
            context_panel_visible: true,
            ..DesktopPreferences::default()
        }
    }

    fn add_visual_shell_with_preferences(
        cx: &mut TestAppContext,
        runtime: DesktopRuntimeBridge,
        projection: DesktopProjection,
        preferences: DesktopPreferences,
    ) -> (gpui::Entity<NativeShell>, &mut gpui::VisualTestContext) {
        let shell_slot = Rc::new(RefCell::new(None));
        let shell_slot_for_window = Rc::clone(&shell_slot);
        let (_, visual_cx) = cx.add_window_view(move |window, cx| {
            let shell = cx.new(|cx| {
                NativeShell::new(
                    NativeShellInit {
                        runtime,
                        workspace: NativeShellWorkspaceInit::Session(Box::new(projection)),
                        projectless_workspace_selection: CodingAgentWorkspaceSelection::projectless(
                            "workspace-native-fixture",
                        ),
                        global_skills: visual_global_skills(),
                        preferences,
                        preference_writer: None,
                        preference_notice: None,
                    },
                    window,
                    cx,
                )
            });
            shell_slot_for_window.replace(Some(shell.clone()));
            gpui_component::Root::new(shell, window, cx)
        });
        let shell = shell_slot
            .borrow_mut()
            .take()
            .expect("visual shell entity was captured");
        (shell, visual_cx)
    }

    fn add_idle_visual_shell(
        cx: &mut TestAppContext,
    ) -> (gpui::Entity<NativeShell>, &mut gpui::VisualTestContext) {
        add_idle_visual_shell_with_runtime(cx, DesktopRuntimeBridge::disconnected_for_test())
    }

    fn add_idle_visual_shell_with_runtime(
        cx: &mut TestAppContext,
        runtime: DesktopRuntimeBridge,
    ) -> (gpui::Entity<NativeShell>, &mut gpui::VisualTestContext) {
        add_idle_visual_shell_with_preferences(cx, runtime, DesktopPreferences::default())
    }

    fn add_idle_visual_shell_with_preferences(
        cx: &mut TestAppContext,
        runtime: DesktopRuntimeBridge,
        preferences: DesktopPreferences,
    ) -> (gpui::Entity<NativeShell>, &mut gpui::VisualTestContext) {
        let shell_slot = Rc::new(RefCell::new(None));
        let shell_slot_for_window = Rc::clone(&shell_slot);
        let mut project = visual_test_snapshot().project;
        project.global_config_dir = std::path::PathBuf::from("/desktop-global");
        project.cwd = project
            .global_config_dir
            .join("scratch/workspace-native-fixture");
        let (_, visual_cx) = cx.add_window_view(move |window, cx| {
            let shell = cx.new(|cx| {
                NativeShell::new(
                    NativeShellInit {
                        runtime,
                        workspace: NativeShellWorkspaceInit::Home(Box::new(project)),
                        projectless_workspace_selection: CodingAgentWorkspaceSelection::projectless(
                            "workspace-native-fixture",
                        ),
                        global_skills: visual_global_skills(),
                        preferences,
                        preference_writer: None,
                        preference_notice: None,
                    },
                    window,
                    cx,
                )
            });
            shell_slot_for_window.replace(Some(shell.clone()));
            gpui_component::Root::new(shell, window, cx)
        });
        let shell = shell_slot
            .borrow_mut()
            .take()
            .expect("idle visual shell entity was captured");
        (shell, visual_cx)
    }

    #[gpui::test]
    fn panel_resize_commits_through_the_typed_preferences_transition(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);

        shell.update(cx, |shell, cx| {
            shell.apply_panel_width(ResizablePanel::Sessions, SESSION_PANEL_MIN_WIDTH, cx);
            assert_eq!(
                shell.app.preferences.sessions_panel_width,
                SESSION_PANEL_MIN_WIDTH
            );
            assert!(shell.ui.runtime_ui_notification_count > 0);
        });
    }

    #[gpui::test]
    fn idle_session_catalog_is_loaded_only_by_explicit_refresh(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);

        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.app.catalog.state().clone()),
            ProjectCatalogState::NotLoaded
        );
        assert_eq!(
            runtime_harness.drain_command_kinds(),
            [],
            "NativeShell::new must not auto-load the session catalog"
        );
        cx.executor().advance_clock(Duration::from_secs(60));
        cx.run_until_parked();
        assert_eq!(
            runtime_harness.drain_command_kinds(),
            [],
            "an idle shell must not arm a catalog refresh timer"
        );

        cx.simulate_resize(size(px(700.), px(800.)));
        cx.run_until_parked();
        let toggle = cx
            .debug_bounds("desktop-hit-toggle-sessions")
            .expect("idle Header exposes the Sessions drawer toggle");
        cx.simulate_click(toggle.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            runtime_harness.drain_command_kinds(),
            [],
            "opening the Sessions surface must remain read-free"
        );

        assert!(
            cx.debug_bounds("desktop-projects-state-not-loaded")
                .is_some(),
            "the unloaded catalog has a local Projects state"
        );
        let refresh = cx
            .debug_bounds("desktop-hit-refresh-projects")
            .expect("Projects exposes its direct explicit refresh action");
        cx.simulate_click(refresh.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            runtime_harness.drain_command_kinds(),
            [desktop::runtime::DesktopRuntimeCommandKind::ListSessions]
        );
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.app.catalog.state().clone()),
            ProjectCatalogState::Loading
        );
        assert!(
            cx.debug_bounds("desktop-projects-state-loading").is_some(),
            "the pending catalog has a local loading state"
        );
        shell.update(cx, |shell, cx| shell.request_session_catalog(cx));
        assert_eq!(
            runtime_harness.drain_command_kinds(),
            [],
            "a pending explicit refresh must be deduplicated"
        );
        shell.update(cx, |shell, cx| {
            let command_id = shell
                .app
                .commands
                .command_id_for(
                    shell.app.workspaces.active_key(),
                    &DesktopCommandIntent::ListSessions,
                )
                .expect("the explicit refresh remains pending");
            shell.connection.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::SessionsListed {
                    command_id,
                    sessions: vec![desktop::runtime::DesktopSessionCatalogEntry {
                        session_id: "explicit-refresh-session".into(),
                        ..Default::default()
                    }],
                    omitted: 0,
                },
            );
            assert!(shell.poll_runtime_for_test(cx));
            assert_eq!(
                shell.app.catalog.catalog()[0].session_id,
                "explicit-refresh-session"
            );
            assert_eq!(shell.app.catalog.state(), &ProjectCatalogState::Ready);
            assert!(shell.app.workspaces.active().preference_notice.is_none());
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-projects-tree").is_some());
        assert!(cx.debug_bounds("desktop-projects-state-loading").is_none());
        cx.executor().advance_clock(Duration::from_secs(60));
        cx.run_until_parked();
        assert_eq!(
            runtime_harness.drain_command_kinds(),
            [],
            "a successful explicit refresh must not schedule another load"
        );
    }

    #[gpui::test]
    fn observed_automatic_name_updates_the_local_session_catalog(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, cx| {
            shell.app.catalog.replace_catalog(
                vec![desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "auto-named-session".into(),
                    name: None,
                    ..Default::default()
                }],
                0,
            );
            shell.connection.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::SessionNameObserved {
                    session_id: "auto-named-session".into(),
                    name: Some("询问助手名字".into()),
                    updated_at: "2026-07-30T02:24:11Z".into(),
                },
            );

            assert!(shell.poll_runtime_for_test(cx));
            let session = &shell.app.catalog.catalog()[0];
            assert_eq!(session.name.as_deref(), Some("询问助手名字"));
            assert_eq!(session.updated_at, "2026-07-30T02:24:11Z");
            assert!(shell.app.workspaces.active().preference_notice.is_none());
        });
    }

    #[gpui::test]
    fn explicit_session_catalog_refresh_failure_reports_error_without_retry(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);

        shell.update(cx, |shell, cx| {
            shell.request_session_catalog(cx);
            assert_eq!(
                shell.app.workspaces.active().preference_notice.as_deref(),
                Some("desktop runtime command queue is closed")
            );
            assert_eq!(
                shell.app.catalog.state(),
                &ProjectCatalogState::Error {
                    message: "desktop runtime command queue is closed".into()
                }
            );
            assert!(
                !shell.active_command_contains(&DesktopCommandIntent::ListSessions),
                "failed admission must release the pending refresh"
            );
        });
        cx.executor().advance_clock(Duration::from_secs(60));
        cx.run_until_parked();
        shell.update(cx, |shell, _cx| {
            assert_eq!(
                shell.app.workspaces.active().preference_notice.as_deref(),
                Some("desktop runtime command queue is closed")
            );
            assert!(matches!(
                shell.app.catalog.state(),
                ProjectCatalogState::Error { .. }
            ));
            assert!(
                !shell.active_command_contains(&DesktopCommandIntent::ListSessions),
                "failed refresh must not schedule another attempt"
            );
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-projects-state-error").is_some());
    }

    #[gpui::test]
    fn rejected_session_catalog_refresh_keeps_typed_error_state(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
        cx.run_until_parked();

        shell.update(cx, |shell, cx| shell.request_session_catalog(cx));
        assert_eq!(
            runtime_harness.drain_command_kinds(),
            [desktop::runtime::DesktopRuntimeCommandKind::ListSessions]
        );
        shell.update(cx, |shell, cx| {
            let command_id = shell
                .app
                .commands
                .command_id_for(
                    shell.app.workspaces.active_key(),
                    &DesktopCommandIntent::ListSessions,
                )
                .expect("refresh is pending before rejection");
            shell.connection.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command: desktop::runtime::DesktopRuntimeCommandKind::ListSessions,
                    code: "catalog_unavailable".into(),
                    message: "private runtime detail must not become catalog state".into(),
                },
            );
            assert!(shell.poll_runtime_for_test(cx));
            assert_eq!(
                shell.app.catalog.state(),
                &ProjectCatalogState::Error {
                    message: "ListSessions rejected (catalog_unavailable)".into()
                }
            );
            assert!(
                !shell
                    .app
                    .catalog
                    .state()
                    .error_message()
                    .unwrap()
                    .contains("private runtime detail")
            );
        });
    }

    #[gpui::test]
    fn projects_local_empty_omitted_and_legacy_states_are_explicit(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        cx.simulate_resize(size(px(1_300.), px(900.)));
        cx.run_until_parked();

        shell.update(cx, |shell, cx| {
            shell.app.catalog.replace_catalog(Vec::new(), 0);
            shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-projects-state-empty").is_some());
        assert!(cx.debug_bounds("desktop-projects-tree").is_none());

        shell.update(cx, |shell, cx| {
            shell.app.catalog.replace_catalog(
                vec![desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "legacy-visible-session".into(),
                    name: Some("Legacy visible session".into()),
                    updated_at: "2026-07-29T08:00:00Z".into(),
                    ..Default::default()
                }],
                4,
            );
            shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-projects-state-empty").is_none());
        assert!(cx.debug_bounds("desktop-projects-tree").is_some());
        assert!(cx.debug_bounds("desktop-project-row-0").is_some());
        assert!(cx.debug_bounds("desktop-session-row-0").is_some());
        assert!(cx.debug_bounds("desktop-projects-state-omitted").is_some());
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.app.catalog.project_groups()[0]
                .workspace
                .kind),
            coding_agent::api::view::CodingAgentWorkspaceKind::Legacy
        );
    }

    #[gpui::test]
    fn projects_tree_disclosure_preserves_order_at_minimum_width_and_in_drawer(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let preferences = DesktopPreferences {
            sessions_panel_width: SESSION_PANEL_MIN_WIDTH,
            ..DesktopPreferences::default()
        };
        let (shell, cx) = add_idle_visual_shell_with_preferences(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            preferences,
        );
        shell.update(cx, |shell, cx| {
            shell.app.catalog.replace_catalog(
                vec![
                    desktop::runtime::DesktopSessionCatalogEntry {
                        session_id: "stable-first-session".into(),
                        name: Some("First session with a long label".into()),
                        updated_at: "2026-07-29T09:00:00Z".into(),
                        ..Default::default()
                    },
                    desktop::runtime::DesktopSessionCatalogEntry {
                        session_id: "stable-second-session".into(),
                        name: Some("Second session".into()),
                        updated_at: "2026-07-29T08:00:00Z".into(),
                        ..Default::default()
                    },
                ],
                0,
            );
            shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        });

        cx.simulate_resize(size(px(1_300.), px(900.)));
        cx.run_until_parked();
        let panel = cx
            .debug_bounds("desktop-sessions-panel")
            .expect("minimum-width Sidebar remains docked");
        assert_eq!(f32::from(panel.size.width), SESSION_PANEL_MIN_WIDTH as f32);
        let new_conversation = cx
            .debug_bounds("desktop-hit-new-conversation")
            .expect("New conversation remains first");
        let skills = cx
            .debug_bounds("desktop-hit-skills")
            .expect("Skills remains second");
        let project = cx
            .debug_bounds("desktop-project-row-0")
            .expect("project disclosure follows fixed navigation");
        let session = cx
            .debug_bounds("desktop-session-row-0")
            .expect("nested session follows its project");
        assert!(new_conversation.origin.y < skills.origin.y);
        assert!(skills.origin.y < project.origin.y);
        assert!(project.origin.y < session.origin.y);
        for selector in [
            "desktop-hit-refresh-projects",
            "desktop-project-row-0",
            "desktop-session-row-0",
            "desktop-hit-session-actions-0",
        ] {
            assert_minimum_hit_target(cx, selector);
            let bounds = cx.debug_bounds(selector).unwrap();
            assert!(bounds.origin.x >= panel.origin.x);
            assert!(bounds.origin.x + bounds.size.width <= panel.origin.x + panel.size.width);
        }

        cx.simulate_click(project.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-project-sessions-0").is_none());
        assert!(cx.debug_bounds("desktop-session-row-0").is_none());
        assert!(shell.read_with(cx, |shell, _| {
            shell.app.catalog.project_groups()[0].collapsed
        }));

        let collapsed_project = cx.debug_bounds("desktop-project-row-0").unwrap();
        cx.simulate_click(collapsed_project.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-project-sessions-0").is_some());
        assert!(cx.debug_bounds("desktop-session-row-0").is_some());
        assert!(!shell.read_with(cx, |shell, _| {
            shell.app.catalog.project_groups()[0].collapsed
        }));

        cx.simulate_resize(size(px(700.), px(900.)));
        cx.run_until_parked();
        let toggle = cx.debug_bounds("desktop-hit-toggle-sessions").unwrap();
        cx.simulate_click(toggle.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-sessions-drawer").is_some());
        assert!(cx.debug_bounds("desktop-sidebar-evo-mark").is_some());
        assert!(cx.debug_bounds("desktop-project-row-0").is_some());
        assert!(cx.debug_bounds("desktop-session-row-1").is_some());
        assert_minimum_hit_target(cx, "desktop-hit-close-narrow-sessions");
        assert_minimum_hit_target(cx, "desktop-hit-refresh-projects");
        assert_minimum_hit_target(cx, "desktop-hit-session-actions-1");
    }

    #[gpui::test]
    fn idle_shell_constructs_all_bounded_view_models_without_session_facts(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, cx| {
            assert!(shell.app.workspaces.active().projection.is_none());
            assert!(
                sessions_pane::view_model(&shell.app, &shell.ui)
                    .active_session_id
                    .is_empty()
            );
            assert!(!composer_pane::view_model(shell.app.workspaces.active()).composer_running);
            let inspector =
                inspector_pane::view_model(&shell.app, &shell.ui, shell.global_skills.len());
            assert_eq!(inspector.active_operation, "—");
            assert_eq!(inspector.stream_id, "—");
            assert!(
                root_modal_host::view_model(&shell.app, &shell.ui)
                    .authorization
                    .is_none()
            );
            assert!(shell.views.toast_host.read(cx).messages().len() <= 3);
            assert_eq!(
                conversation_pane::view_model(shell.app.workspaces.active(), &shell.ui)
                    .visible_count,
                0
            );
            let header = conversation_header::view_model(&shell.app, &shell.ui);
            assert_eq!(header.profile.as_ref(), "Default");
            assert_eq!(header.current_profile_id.as_ref(), "default");
            assert_eq!(
                skills_pane::view_model(&shell.global_skills).skills.len(),
                1
            );
            assert!(!sessions_pane::view_model(&shell.app, &shell.ui).skills_active);
        });

        for (width, height, expected_center_width, sidebar_visible) in [
            (1_300., 900., 1_060., true),
            (900., 800., 660., true),
            (700., 800., 700., false),
        ] {
            cx.simulate_resize(size(px(width), px(height)));
            cx.run_until_parked();
            let home = cx
                .debug_bounds("desktop-home-workspace")
                .expect("idle workspace is visible");
            assert_eq!(f32::from(home.size.width), expected_center_width);
            assert_eq!(
                cx.debug_bounds("desktop-sessions-panel").is_some(),
                sidebar_visible
            );
            assert!(cx.debug_bounds("desktop-conversation-panel").is_none());
            assert!(cx.debug_bounds("desktop-inspector-panel").is_none());
            let header = cx
                .debug_bounds("desktop-conversation-header")
                .expect("center header remains mounted on Home");
            let body = cx
                .debug_bounds("desktop-center-body")
                .expect("center body remains mounted on Home");
            assert_eq!(f32::from(header.size.width), expected_center_width);
            assert_eq!(f32::from(header.size.height), 48.);
            assert_eq!(f32::from(body.size.width), expected_center_width);
            assert_eq!(f32::from(body.origin.y - header.origin.y), 48.);
            assert!(cx.debug_bounds("desktop-evo-wordmark").is_some());
            assert!(cx.debug_bounds("desktop-composer-panel").is_some());
        }
    }

    #[gpui::test]
    fn feature_presenters_are_pure_and_repeatable(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, _| {
            assert_eq!(
                sessions_pane::view_model(&shell.app, &shell.ui),
                sessions_pane::view_model(&shell.app, &shell.ui)
            );
            assert_eq!(
                composer_pane::view_model(shell.app.workspaces.active()),
                composer_pane::view_model(shell.app.workspaces.active())
            );
            assert_eq!(
                conversation_header::view_model(&shell.app, &shell.ui),
                conversation_header::view_model(&shell.app, &shell.ui)
            );
            assert_eq!(
                root_modal_host::view_model(&shell.app, &shell.ui),
                root_modal_host::view_model(&shell.app, &shell.ui)
            );
            assert_eq!(
                center_drawer_host::view_model(&shell.app, &shell.ui),
                center_drawer_host::view_model(&shell.app, &shell.ui)
            );
            assert_eq!(
                skills_pane::view_model(&shell.global_skills),
                skills_pane::view_model(&shell.global_skills)
            );

            let first_inspector =
                inspector_pane::view_model(&shell.app, &shell.ui, shell.global_skills.len());
            let second_inspector =
                inspector_pane::view_model(&shell.app, &shell.ui, shell.global_skills.len());
            assert_eq!(first_inspector, second_inspector);

            let first_conversation =
                conversation_pane::view_model(shell.app.workspaces.active(), &shell.ui);
            let second_conversation =
                conversation_pane::view_model(shell.app.workspaces.active(), &shell.ui);
            assert_eq!(
                first_conversation.snapshot(),
                second_conversation.snapshot()
            );
        });
    }

    #[gpui::test]
    fn home_hero_scales_across_idle_viewports_and_yields_height_to_the_composer(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (_, cx) = add_idle_visual_shell(cx);

        for (width, height, expected_wordmark_width) in [
            (1_300., 900., 360.),
            (900., 800., 320.),
            (700., 800., 280.),
            (1_300., 480., 280.),
        ] {
            cx.simulate_resize(size(px(width), px(height)));
            cx.run_until_parked();

            let body = cx.debug_bounds("desktop-center-body").unwrap();
            let home = cx.debug_bounds("desktop-home-pane").unwrap();
            let hero = cx.debug_bounds("desktop-home-hero").unwrap();
            let wordmark = cx.debug_bounds("desktop-evo-wordmark").unwrap();
            let headline = cx.debug_bounds("desktop-home-headline").unwrap();
            let description = cx.debug_bounds("desktop-home-description").unwrap();
            let composer = cx.debug_bounds("desktop-composer-panel").unwrap();

            assert_eq!(f32::from(wordmark.size.width), expected_wordmark_width);
            assert!(
                (f32::from(wordmark.size.height) - expected_wordmark_width * 128. / 360.).abs()
                    <= 1.,
                "wordmark must retain its vector aspect ratio at {width}x{height}"
            );
            assert!(hero.top() >= home.top());
            assert!(wordmark.top() >= hero.top());
            assert!(headline.top() >= wordmark.bottom());
            assert!(description.top() >= headline.bottom());
            assert!(description.bottom() <= hero.bottom());
            assert!(home.bottom() <= composer.top() + px(1.));
            assert!(composer.bottom() <= body.bottom() + px(1.));
            assert!(f32::from(composer.size.height) >= COMPOSER_MIN_HEIGHT as f32);
        }
    }

    #[gpui::test]
    fn home_geometry_is_independent_of_sidebar_catalog_refresh_state(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        cx.simulate_resize(size(px(1_300.), px(900.)));
        cx.run_until_parked();

        let selectors = [
            "desktop-home-pane",
            "desktop-home-hero",
            "desktop-evo-wordmark",
            "desktop-home-headline",
            "desktop-home-description",
            "desktop-composer-panel",
        ];
        let initial = selectors.map(|selector| {
            cx.debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing Home geometry selector {selector}"))
        });

        shell.update(cx, |shell, cx| {
            shell.app.catalog.begin_refresh();
            shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-projects-state-loading").is_some());
        let loading = selectors.map(|selector| cx.debug_bounds(selector).unwrap());
        assert_eq!(loading, initial);

        shell.update(cx, |shell, cx| {
            shell.app.catalog.replace_catalog(
                vec![desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "catalog-layout-probe".into(),
                    name: Some("Catalog layout probe".into()),
                    ..Default::default()
                }],
                0,
            );
            shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-projects-tree").is_some());
        let ready = selectors.map(|selector| cx.debug_bounds(selector).unwrap());
        assert_eq!(ready, initial);
    }

    #[gpui::test]
    fn home_respects_an_explicit_inspector_preference(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let preferences = visual_preferences_with_inspector();
        let (shell, cx) = add_idle_visual_shell_with_preferences(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            preferences,
        );
        cx.simulate_resize(size(px(1_300.), px(900.)));
        cx.run_until_parked();

        assert!(cx.debug_bounds("desktop-sessions-panel").is_some());
        assert!(cx.debug_bounds("desktop-inspector-panel").is_some());
        assert_eq!(
            f32::from(
                cx.debug_bounds("desktop-home-workspace")
                    .expect("Home center remains visible")
                    .size
                    .width
            ),
            740.
        );
        assert!(shell.read_with(cx, |shell, _| {
            shell.app.preferences.context_panel_visible
        }));
    }

    #[gpui::test]
    fn idle_sessions_drawer_renders_new_conversation_skills_and_history(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, cx| {
            shell.app.catalog.replace_catalog(
                vec![desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "idle-recent-session".into(),
                    name: Some("Idle recent session".into()),
                    updated_at: "2026-07-29T08:00:00Z".into(),
                    ..Default::default()
                }],
                0,
            );
            shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        });
        cx.simulate_resize(size(px(700.), px(800.)));
        cx.run_until_parked();

        let toggle = cx
            .debug_bounds("desktop-hit-toggle-sessions")
            .expect("idle Header exposes the Sessions drawer toggle");
        cx.simulate_click(toggle.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert!(
            cx.debug_bounds("desktop-new-conversation-section")
                .is_some()
        );
        assert!(cx.debug_bounds("desktop-hit-skills").is_some());
        assert!(cx.debug_bounds("desktop-skill-row-0").is_none());
        assert!(cx.debug_bounds("desktop-projects-section").is_some());
        assert!(cx.debug_bounds("desktop-project-row-0").is_some());
        assert!(cx.debug_bounds("desktop-session-row-0").is_some());
        assert!(cx.debug_bounds("sessions-search").is_some());
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.active_drawer),
            Some(CenterDrawerKind::Sessions)
        );
    }

    #[gpui::test]
    fn typed_navigation_switches_skills_session_and_home_without_runtime_commands(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();
        shell.update(cx, |shell, cx| {
            shell.app.catalog.replace_catalog(
                vec![desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "desktop-visual-test".into(),
                    name: Some("Active visual session".into()),
                    updated_at: "2026-07-29T08:00:00Z".into(),
                    ..Default::default()
                }],
                0,
            );
            shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        });
        cx.simulate_resize(size(px(1_300.), px(900.)));
        cx.run_until_parked();

        assert!(
            cx.debug_bounds("desktop-new-conversation-section")
                .is_some()
        );
        assert!(cx.debug_bounds("desktop-hit-skills").is_some());
        assert!(cx.debug_bounds("desktop-projects-section").is_some());
        assert!(cx.debug_bounds("desktop-session-row-0").is_some());

        let skills = cx
            .debug_bounds("desktop-hit-skills")
            .expect("the panel exposes the Skills route");
        cx.simulate_click(skills.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert!(cx.debug_bounds("desktop-skills-workspace").is_some());
        assert!(cx.debug_bounds("desktop-skills-pane").is_some());
        assert!(cx.debug_bounds("desktop-skill-row-0").is_some());
        assert!(cx.debug_bounds("desktop-conversation-panel").is_none());
        assert!(cx.debug_bounds("desktop-composer-panel").is_none());
        assert!(shell.read_with(cx, |shell, _| {
            shell.ui.center_surface == CenterSurface::Skills
                && sessions_pane::view_model(&shell.app, &shell.ui).skills_active
        }));
        assert_eq!(runtime_harness.drain_command_kinds(), []);

        let active_session = cx
            .debug_bounds("desktop-session-row-0")
            .expect("the active session remains a typed navigation target");
        cx.simulate_click(active_session.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert!(cx.debug_bounds("desktop-conversation-panel").is_some());
        assert!(cx.debug_bounds("desktop-skills-workspace").is_none());
        assert_eq!(runtime_harness.drain_command_kinds(), []);

        let skills = cx
            .debug_bounds("desktop-hit-skills")
            .expect("the Skills route remains available");
        cx.simulate_click(skills.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        let new_conversation = cx
            .debug_bounds("desktop-hit-new-conversation")
            .expect("the panel exposes the new-conversation row");
        cx.simulate_click(new_conversation.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert!(shell.read_with(cx, |shell, _| {
            shell.app.workspaces.active().projection.is_none()
        }));
        assert!(shell.read_with(cx, |shell, _| {
            workspace_for_session(shell, "desktop-visual-test").is_some()
        }));
        assert!(cx.debug_bounds("desktop-home-workspace").is_some());
        assert_eq!(
            runtime_harness.drain_command_kinds(),
            [],
            "entering Home must not dispatch any runtime command or touch session persistence"
        );
    }

    #[gpui::test]
    fn preference_notices_preserve_repeated_messages_and_bound_the_toast_stack(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, cx| {
            shell
                .app
                .workspaces
                .active_mut()
                .set_preference_notice("Repeated notice".into());
            shell.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            shell
                .app
                .workspaces
                .active_mut()
                .set_preference_notice("Repeated notice".into());
            shell.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);

            let repeated = shell.views.toast_host.read(cx).messages();
            assert_eq!(
                repeated
                    .iter()
                    .rev()
                    .take(2)
                    .map(AsRef::as_ref)
                    .collect::<Vec<_>>(),
                ["Repeated notice", "Repeated notice"]
            );

            shell
                .app
                .workspaces
                .active_mut()
                .set_preference_notice("Third notice".into());
            shell.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);
            shell
                .app
                .workspaces
                .active_mut()
                .set_preference_notice("Fourth notice".into());
            shell.refresh_views(UiChangeSet::one(UiRegion::Toast), cx);

            let bounded = shell.views.toast_host.read(cx).messages();
            assert_eq!(bounded.len(), 3);
            assert_eq!(
                bounded.iter().map(AsRef::as_ref).collect::<Vec<_>>(),
                ["Repeated notice", "Third notice", "Fourth notice"]
            );
        });
    }

    #[gpui::test]
    fn idle_home_draft_moves_into_the_first_established_session(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, _| {
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .edit("keep this home draft");
            assert!(
                shell
                    .app
                    .install_hydrated_workspace(&visual_test_snapshot(), true, true)
            );
            assert_eq!(
                shell.app.workspaces.active().composer.draft(),
                "keep this home draft"
            );
            assert_eq!(active_session_id(shell), Some("desktop-visual-test"));
            assert!(
                shell
                    .app
                    .workspaces
                    .get(&WorkspaceKey::Home)
                    .is_some_and(|home| home.composer.draft().is_empty())
            );
        });
    }

    #[gpui::test]
    fn first_session_change_rekeys_the_home_workspace_and_completes_its_command(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
        cx.run_until_parked();
        assert_eq!(runtime_harness.drain_command_kinds(), []);
        shell.update(cx, |shell, cx| {
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .edit("home draft");
            let intent = DesktopCommandIntent::OpenSession {
                session_id: "session-first".into(),
            };
            let command_id = shell
                .app
                .commands
                .reserve(WorkspaceKey::session("session-first"), intent.clone())
                .expect("the first open command fits the global tracker");
            shell.ui.runtime_ui_notification_count = 0;
            shell.connection.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::SessionChanged {
                    command_id,
                    snapshot: visual_test_snapshot_for("session-first"),
                },
            );

            assert!(shell.poll_runtime_for_test(cx));
            assert_eq!(active_session_id(shell), Some("session-first"));
            assert_eq!(shell.app.workspaces.active().composer.draft(), "home draft");
            assert!(shell.app.commands.pending(command_id).is_none());
            assert!(shell.ui.runtime_ui_notification_count > 0);
        });
        assert_eq!(
            runtime_harness.drain_command_kinds(),
            [],
            "opening a session must not trigger a full catalog request"
        );
    }

    #[gpui::test]
    fn runtime_command_owner_mismatch_is_rejected_and_requires_resync(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let projection = DesktopProjection::new(visual_test_snapshot_for("owner-session-a"))
            .expect("owner session A fixture is valid");
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection,
        );

        shell.update(cx, |shell, cx| {
            let owner = WorkspaceKey::session("owner-session-a");
            let command_id = shell
                .app
                .commands
                .reserve(owner.clone(), DesktopCommandIntent::Reload)
                .expect("reload command fits the global tracker");
            let mut foreign = visual_test_snapshot_for("owner-session-b");
            foreign.project.selected_model_id = "foreign-model-must-not-apply".into();
            shell.connection.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::Reloaded {
                    command_id,
                    metadata: desktop::runtime::DesktopRuntimeMetadataSnapshot {
                        project: foreign.project,
                        session: Some(foreign.session),
                    },
                },
            );

            assert!(shell.poll_runtime_for_test(cx));
            assert!(
                shell
                    .app
                    .commands
                    .matches(command_id, &owner, &DesktopCommandIntent::Reload,)
            );
            assert_ne!(
                shell.app.workspaces.active().project.selected_model_id,
                "foreign-model-must-not-apply"
            );
            let projection = shell
                .app
                .workspaces
                .active()
                .projection
                .as_ref()
                .expect("owner session remains hydrated");
            assert_eq!(
                projection.lifecycle(),
                DesktopProjectionLifecycle::NeedsResync
            );
            assert!(
                projection
                    .issues()
                    .iter()
                    .any(|issue| issue.code == "command_owner_mismatch")
            );
        });
    }

    #[gpui::test]
    fn create_and_resync_update_local_state_without_loading_the_catalog(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
        cx.run_until_parked();
        assert_eq!(runtime_harness.drain_command_kinds(), []);

        shell.update(cx, |shell, cx| {
            let create_id = shell
                .reserve_command(DesktopCommandIntent::CreateSession)
                .expect("create command fits the Home ledger");
            shell.connection.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::SessionChanged {
                    command_id: create_id,
                    snapshot: visual_test_snapshot_for("session-created-locally"),
                },
            );
            assert!(shell.poll_runtime_for_test(cx));
            assert_eq!(
                shell.app.catalog.catalog()[0].session_id,
                "session-created-locally"
            );

            let resync_id = shell
                .reserve_command(DesktopCommandIntent::Resync)
                .expect("resync command fits the session ledger");
            shell.connection.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::Resynced {
                    command_id: resync_id,
                    replacement: desktop::runtime::DesktopRuntimeResyncSnapshot::Hydrated(
                        visual_test_snapshot_for("session-created-locally"),
                    ),
                },
            );
            assert!(shell.poll_runtime_for_test(cx));
        });
        assert_eq!(
            runtime_harness.drain_command_kinds(),
            [],
            "create and resync completions must use local state only"
        );
    }

    #[gpui::test]
    fn rejected_new_prompt_promotes_home_owner_with_background_sessions(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, cx| {
            let background_snapshot = visual_test_snapshot_for("session-background");
            let background_projection = DesktopProjection::new(background_snapshot.clone())
                .expect("the background fixture is valid");
            insert_session_workspace(
                shell,
                "session-background",
                make_session_workspace(
                    background_snapshot.project,
                    Some(background_projection),
                    None,
                ),
            );
            shell.app.catalog.replace_catalog(
                vec![desktop::runtime::DesktopSessionCatalogEntry {
                    session_id: "close-session-b".into(),
                    ..Default::default()
                }],
                0,
            );
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .edit("retain this exact Home draft");
            shell
                .app
                .workspaces
                .active_mut()
                .composer_attachments
                .push(PathBuf::from("/tmp/retained-home-attachment.txt"));
            let command_id = shell
                .reserve_command(DesktopCommandIntent::Prompt)
                .expect("the Home prompt command fits the ledger");
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .begin_submit(command_id, ComposerSubmissionKind::Prompt)
                .expect("the Home draft enters pending admission");
            shell.connection.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::PromptRejectedWithSession {
                    command_id,
                    snapshot: visual_test_snapshot_for("session-created"),
                    error: desktop::runtime::DesktopRuntimeError {
                        code: "prompt_prepare".into(),
                        message: "the created session retained the rejected prompt".into(),
                    },
                },
            );

            assert!(shell.poll_runtime_for_test(cx));
            assert_eq!(active_session_id(shell), Some("session-created"));
            assert!(workspace_for_session(shell, "session-background").is_some());
            assert!(shell.app.workspaces.get(&WorkspaceKey::Home).is_some());
            assert_eq!(
                shell.app.workspaces.active().composer.draft(),
                "retain this exact Home draft"
            );
            assert_eq!(
                shell.app.workspaces.active().composer_attachments,
                [PathBuf::from("/tmp/retained-home-attachment.txt")]
            );
            assert_eq!(
                shell.app.workspaces.active().composer.admission(),
                &ComposerAdmission::Idle
            );
            assert!(shell.app.workspaces.active().composer.rejection().is_some());
            assert!(shell.app.commands.pending(command_id).is_none());
            assert!(
                shell
                    .app
                    .workspaces
                    .active()
                    .projection
                    .as_ref()
                    .unwrap()
                    .issues()
                    .iter()
                    .any(|issue| issue.code == "prompt_prepare")
            );
            assert_eq!(
                shell.app.catalog.catalog()[0].session_id,
                "session-created",
                "the first prompt must add its newly-created session locally"
            );
        });
    }

    #[gpui::test]
    fn runtime_stop_rejects_the_pending_composer_admission(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, _| {
            let owner = shell.app.workspaces.active_key().clone();
            let command_id = shell
                .app
                .commands
                .reserve(owner, DesktopCommandIntent::Prompt)
                .expect("test prompt fits the command tracker");
            let workspace = shell.app.workspaces.active_mut();
            workspace.composer.edit("retain this exact draft");
            workspace
                .composer
                .begin_submit(command_id, ComposerSubmissionKind::Prompt)
                .expect("test prompt enters admission");

            let transition = shell.with_controller(|controller, shell| {
                controller.reduce_runtime(
                    &mut shell.app,
                    desktop::runtime::DesktopRuntimeUpdate::Stopped,
                )
            });

            assert!(transition.changes().contains(UiRegion::Sessions));
            assert_eq!(
                shell.app.workspaces.active().composer.draft(),
                "retain this exact draft"
            );
            assert!(matches!(
                shell.app.workspaces.active().composer.admission(),
                ComposerAdmission::Idle
            ));
            assert_eq!(
                shell.app.workspaces.active().composer.rejection(),
                Some("desktop runtime stopped")
            );
            assert!(!shell.app.commands.contains_anywhere(|_| true));
        });
    }

    #[gpui::test]
    fn background_workspace_advances_silently_and_switching_restores_scoped_state(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let session_a_snapshot = visual_test_snapshot_for("session-a");
        let session_a_projection = DesktopProjection::new(session_a_snapshot)
            .expect("session A fixture is a valid product projection");
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, session_a_projection);
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        shell.update(cx, |shell, cx| {
            let mut session_b_snapshot = visual_test_snapshot_for("session-b");
            session_b_snapshot.session.active_operation = Some("operation-session-b".into());
            let session_b_projection = DesktopProjection::new(session_b_snapshot.clone())
                .expect("session B fixture is a valid product projection");
            let mut session_b = make_session_workspace(
                session_b_snapshot.project.clone(),
                Some(session_b_projection),
                None,
            );
            let change = CodingAgentFileChangeSnapshot {
                path: "session-b-only.rs".into(),
                mutation_kind: "edit".into(),
                operation_id: "operation-session-b".into(),
                tool_call_id: None,
                updated_sequence: 3,
                first_changed_line: Some(4),
                added_lines: Some(1),
                removed_lines: Some(0),
                diff: None,
            };
            let review_request = CodingAgentFileReviewRequest::from(&change);
            session_b.composer.edit("draft b");
            session_b.presentation.inspector_section = InspectorSection::Task;
            session_b.file_review =
                Arc::new(DesktopFileReviewState::Loading(review_request.clone()));

            shell.app.workspaces.active_mut().composer.edit("draft a");
            shell
                .app
                .workspaces
                .active_mut()
                .presentation
                .inspector_section = InspectorSection::Runtime;
            insert_session_workspace(shell, "session-b", session_b);
            let review_intent = DesktopCommandIntent::FileReview {
                request: review_request.clone(),
            };
            let review_command_id = shell
                .app
                .commands
                .reserve(WorkspaceKey::session("session-b"), review_intent.clone())
                .expect("session B test command fits the global tracker");
            let sessions = sessions_pane::view_model(&shell.app, &shell.ui);
            assert_eq!(
                sessions
                    .runtime_states
                    .iter()
                    .find(|state| state.session_id.as_ref() == "session-b")
                    .map(|state| state.status),
                Some(SemanticStatus::Running)
            );
            shell.refresh_conversation_rows_at_width(800, cx);
            shell.ui.runtime_ui_notification_count = 0;

            let mut finished_snapshot = visual_test_snapshot_for("session-b");
            finished_snapshot.session.cursor.last_event_sequence = 7;
            finished_snapshot.session.cursor.last_session_sequence = 7;
            finished_snapshot.session.context.changes.push(change);
            shell.connection.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::PromptFinished {
                    command_id: 9_002,
                    operation_id: "operation-session-b".into(),
                    snapshot: finished_snapshot,
                    error: None,
                },
            );
            assert!(shell.poll_runtime_for_test(cx));

            assert_eq!(active_session_id(shell), Some("session-a"));
            assert_eq!(shell.ui.runtime_ui_notification_count, 0);
            assert_eq!(shell.app.workspaces.active().composer.draft(), "draft a");
            assert_eq!(
                shell.app.workspaces.active().presentation.inspector_section,
                InspectorSection::Runtime
            );
            assert!(matches!(
                shell.app.workspaces.active().file_review.as_ref(),
                DesktopFileReviewState::Empty
            ));
            assert!(
                !shell
                    .app
                    .commands
                    .contains(shell.app.workspaces.active_key(), &review_intent,)
            );

            let background = workspace_for_session(shell, "session-b")
                .expect("session B remains parked after its background update");
            assert_eq!(
                background
                    .projection
                    .as_ref()
                    .expect("session B remains hydrated")
                    .cursor()
                    .last_event_sequence,
                7
            );
            assert_eq!(background.composer.draft(), "draft b");
            assert_eq!(
                background.presentation.inspector_section,
                InspectorSection::Task
            );
            assert!(matches!(
                background.file_review.as_ref(),
                DesktopFileReviewState::Loading(request) if *request == review_request
            ));
            assert!(shell.app.commands.matches(
                review_command_id,
                &WorkspaceKey::session("session-b"),
                &review_intent,
            ));

            assert!(activate_session(shell, "session-b"));
            assert_eq!(shell.app.workspaces.active().composer.draft(), "draft b");
            assert_eq!(
                shell.app.workspaces.active().presentation.inspector_section,
                InspectorSection::Task
            );
            assert!(activate_session(shell, "session-a"));
            assert_eq!(shell.app.workspaces.active().composer.draft(), "draft a");
            assert_eq!(
                shell.app.workspaces.active().presentation.inspector_section,
                InspectorSection::Runtime
            );

            for session_id in ["session-c", "session-d"] {
                let snapshot = visual_test_snapshot_for(session_id);
                let projection = DesktopProjection::new(snapshot.clone())
                    .expect("workspace-cap fixture is a valid projection");
                insert_session_workspace(
                    shell,
                    session_id,
                    make_session_workspace(snapshot.project, Some(projection), None),
                );
            }
            assert_eq!(shell.app.workspaces.session_count(), MAX_SESSION_WORKSPACES);
            let session_e = visual_test_snapshot_for("session-e");
            assert!(
                !shell
                    .app
                    .install_hydrated_workspace(&session_e, false, true)
            );
            assert!(workspace_for_session(shell, "session-e").is_none());
            let workspace_ids_before = session_workspace_ids(shell);
            shell.open_session("session-e".into(), cx);
            assert_eq!(session_workspace_ids(shell), workspace_ids_before);
            assert!(
                shell
                    .app
                    .workspaces
                    .active()
                    .preference_notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("close one first"))
            );
        });
        assert!(
            !runtime_harness
                .drain_command_kinds()
                .contains(&desktop::runtime::DesktopRuntimeCommandKind::OpenSession)
        );
    }

    #[gpui::test]
    fn closing_a_background_workspace_removes_only_its_owner(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let snapshot_a = visual_test_snapshot_for("close-session-a");
        let projection_a = DesktopProjection::new(snapshot_a)
            .expect("close-session A fixture is a valid projection");
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, projection_a);
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        shell.update(cx, |shell, cx| {
            let snapshot_b = visual_test_snapshot_for("close-session-b");
            let projection_b = DesktopProjection::new(snapshot_b.clone())
                .expect("close-session B fixture is a valid projection");
            insert_session_workspace(
                shell,
                "close-session-b",
                make_session_workspace(snapshot_b.project, Some(projection_b), None),
            );
            let owner = WorkspaceKey::session("close-session-b");
            let abandoned_command_id = shell
                .app
                .commands
                .reserve(owner.clone(), DesktopCommandIntent::Reload)
                .expect("background pending command fits the global tracker");
            let intent = DesktopCommandIntent::CloseSession {
                session_id: "close-session-b".into(),
            };
            shell.close_session("close-session-b", cx);
            let command_id = shell
                .app
                .commands
                .command_id_for(&owner, &intent)
                .expect("close command is owned by the target workspace");
            shell.connection.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::SessionClosed {
                    command_id,
                    session_id: "close-session-b".into(),
                },
            );
            assert!(shell.poll_runtime_for_test(cx));
            assert_eq!(active_session_id(shell), Some("close-session-a"));
            assert!(workspace_for_session(shell, "close-session-b").is_none());
            assert!(shell.app.commands.pending(abandoned_command_id).is_none());
            assert_eq!(
                shell.app.workspaces.active().preference_notice.as_deref(),
                Some("Session closed; 1 pending command(s) cancelled.")
            );
            assert!(shell.app.catalog.catalog().is_empty());
        });
        assert_eq!(
            runtime_harness.drain_command_kinds(),
            [desktop::runtime::DesktopRuntimeCommandKind::CloseSession],
            "close must remove the local entry without loading the catalog"
        );
    }

    #[gpui::test]
    fn closing_the_active_workspace_falls_back_to_home(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let snapshot = visual_test_snapshot_for("close-active-session");
        let projection =
            DesktopProjection::new(snapshot).expect("close-active fixture is a valid projection");
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, projection);
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        shell.update(cx, |shell, cx| {
            shell
                .app
                .workspaces
                .get_mut(&WorkspaceKey::Home)
                .expect("Home remains in the store")
                .composer
                .edit("deterministic fallback draft");
            shell.close_session("close-active-session", cx);
            let intent = DesktopCommandIntent::CloseSession {
                session_id: "close-active-session".into(),
            };
            let command_id = shell
                .app
                .commands
                .command_id_for(&WorkspaceKey::session("close-active-session"), &intent)
                .expect("close command remains owned by the active session");
            shell.connection.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::SessionClosed {
                    command_id,
                    session_id: "close-active-session".into(),
                },
            );

            assert!(shell.poll_runtime_for_test(cx));
            assert_eq!(shell.app.workspaces.active_key(), &WorkspaceKey::Home);
            assert_eq!(
                shell.app.workspaces.active().composer.draft(),
                "deterministic fallback draft"
            );
            assert!(workspace_for_session(shell, "close-active-session").is_none());
        });
        assert_eq!(
            runtime_harness.drain_command_kinds(),
            [desktop::runtime::DesktopRuntimeCommandKind::CloseSession]
        );
    }

    fn desktop_region_bounds(
        cx: &mut gpui::VisualTestContext,
    ) -> [Option<gpui::Bounds<gpui::Pixels>>; 4] {
        [
            cx.debug_bounds("desktop-sessions-panel"),
            cx.debug_bounds("desktop-conversation-panel"),
            cx.debug_bounds("desktop-composer-panel"),
            cx.debug_bounds("desktop-inspector-panel"),
        ]
    }

    fn assert_minimum_hit_target(cx: &mut gpui::VisualTestContext, selector: &'static str) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing hit-target selector {selector}"));
        assert!(
            f32::from(bounds.size.width) >= 32. && f32::from(bounds.size.height) >= 32.,
            "{selector} must retain a 32x32 desktop hit target, got {:?}",
            bounds.size
        );
    }

    fn choose_popup_item(cx: &mut gpui::VisualTestContext, index: usize) {
        for key in std::iter::repeat_n("down", index + 1).chain(std::iter::once("enter")) {
            let keystroke = gpui::Keystroke::parse(key)
                .unwrap_or_else(|error| panic!("popup-menu key {key} is valid: {error}"));
            let dispatched = cx.update(|window, cx| window.dispatch_keystroke(keystroke, cx));
            assert!(dispatched, "popup menu handles {key}");
            cx.run_until_parked();
        }
    }

    fn test_percentile_95(samples: &mut [u128]) -> u128 {
        samples.sort_unstable();
        let index = (samples.len() * 95).div_ceil(100).saturating_sub(1);
        samples[index]
    }

    #[gpui::test]
    fn native_shell_focus_and_responsive_bounds_are_stable(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell_with_preferences(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            visual_test_projection(),
            visual_preferences_with_inspector(),
        );

        cx.simulate_resize(size(px(1_300.), px(900.)));
        cx.run_until_parked();
        let wide_before_focus = desktop_region_bounds(cx);
        assert!(wide_before_focus.iter().all(Option::is_some));
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.focus.active()),
            FocusTarget::Composer
        );
        cx.dispatch_action(FocusNextRegion);
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.focus.active()),
            FocusTarget::Inspector
        );
        cx.dispatch_action(FocusNextRegion);
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.focus.active()),
            FocusTarget::CenterHeader
        );
        assert_eq!(desktop_region_bounds(cx), wide_before_focus);

        cx.simulate_resize(size(px(1_000.), px(900.)));
        cx.run_until_parked();
        let medium = desktop_region_bounds(cx);
        assert!(medium[0].is_some());
        assert!(medium[1].is_some());
        assert!(medium[2].is_some());
        assert!(medium[3].is_none());
        assert_eq!(f32::from(medium[0].unwrap().size.width), 240.);
        assert_eq!(f32::from(medium[1].unwrap().size.width), 760.);
        assert_eq!(f32::from(medium[2].unwrap().size.width), 760.);

        cx.simulate_resize(size(px(700.), px(900.)));
        cx.run_until_parked();
        let narrow = desktop_region_bounds(cx);
        assert!(narrow[0].is_none());
        assert!(narrow[1].is_some());
        assert!(narrow[2].is_some());
        assert!(narrow[3].is_none());
        assert_eq!(f32::from(narrow[1].unwrap().size.width), 700.);
        assert_eq!(f32::from(narrow[2].unwrap().size.width), 700.);

        for (window_width, expected_workspace_width) in
            [(1_080., 520.), (1_079., 839.), (760., 520.), (759., 759.)]
        {
            cx.simulate_resize(size(px(window_width), px(900.)));
            cx.run_until_parked();
            let actual = cx
                .debug_bounds("desktop-conversation-panel")
                .expect("workspace remains visible at every responsive breakpoint");
            assert_eq!(f32::from(actual.size.width), expected_workspace_width);
        }

        let medium_layout = ShellLayout::resolve(1_000, 900, PanelVisibility::default());
        assert!(medium_layout.sidebar.is_some());
        assert!(medium_layout.inspector.is_none());
        let narrow_layout = ShellLayout::resolve(700, 900, PanelVisibility::default());
        assert!(narrow_layout.sidebar.is_none());
        assert!(narrow_layout.inspector.is_none());
    }

    #[gpui::test]
    fn shell_header_and_toast_host_stay_bounded_at_all_viewports(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (_, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            visual_test_projection(),
        );

        for width in [1_300., 1_000., 700.] {
            cx.simulate_resize(size(px(width), px(900.)));
            cx.run_until_parked();

            let header = cx
                .debug_bounds("desktop-conversation-header")
                .expect("conversation header remains visible");
            let identity = cx
                .debug_bounds("desktop-header-identity")
                .expect("header identity region remains visible");
            let title = cx.debug_bounds("desktop-header-session-title");
            let actions = cx
                .debug_bounds("desktop-header-actions")
                .expect("header actions remain visible");
            let runtime_slot = cx
                .debug_bounds("desktop-header-runtime-status-slot")
                .expect("the attention-only status slot remains reserved");
            assert!(identity.right() <= actions.left());
            if let Some(title) = &title {
                assert!(title.left() >= identity.left() && title.right() <= identity.right());
            }
            if width == 700. {
                assert!(
                    title.is_none(),
                    "narrow chrome reserves space for selectors"
                );
            }
            assert!(
                runtime_slot.left() >= actions.left() && runtime_slot.right() <= actions.right()
            );
            assert_eq!(
                f32::from(runtime_slot.size.width),
                header_runtime_status_slot_width(width as u32)
            );
            assert!(
                cx.debug_bounds("desktop-header-runtime-status").is_none(),
                "idle does not render a status indicator"
            );
            assert!(
                actions.right() <= header.right(),
                "Header actions must stay bounded at {width}px: header={header:?}, actions={actions:?}"
            );

            assert!(cx.debug_bounds("desktop-status-panel").is_none());
            assert!(cx.debug_bounds("desktop-status-primary").is_none());
            assert!(cx.debug_bounds("desktop-status-secondary").is_none());

            let composer = cx
                .debug_bounds("desktop-composer-panel")
                .expect("Composer remains visible");
            assert_eq!(composer.bottom(), px(900.));
            let toast_host = cx
                .debug_bounds("desktop-toast-host")
                .expect("the transient notice host remains mounted");
            assert!(toast_host.left() >= px(0.));
            assert!(toast_host.right() <= px(width));
            assert!(toast_host.bottom() <= px(900.));
            assert!(
                cx.debug_bounds("desktop-header-thinking-selector")
                    .is_some(),
                "the session thinking selector remains available in the Header"
            );
            assert!(cx.debug_bounds("desktop-composer-thinking").is_none());
        }
    }

    #[gpui::test]
    fn idle_and_running_header_status_keep_every_other_control_stationary(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            visual_test_projection(),
        );
        let stable_selectors = [
            "desktop-header-identity",
            "desktop-header-actions",
            "desktop-header-model-selector",
            "desktop-header-thinking-selector",
            "desktop-header-profile-selector",
            "desktop-hit-toggle-inspector",
            "desktop-header-overflow",
        ];

        for width in [1_300., 1_000., 700.] {
            shell.update(cx, |shell, cx| {
                let mut view_model = conversation_header::view_model(&shell.app, &shell.ui);
                view_model.status = SemanticStatus::Idle;
                shell.views.conversation_header.update(cx, |header, cx| {
                    header.set_view_model(view_model);
                    cx.notify();
                });
            });
            cx.simulate_resize(size(px(width), px(900.)));
            cx.run_until_parked();
            assert!(cx.debug_bounds("desktop-header-runtime-status").is_none());
            let idle_slot = cx
                .debug_bounds("desktop-header-runtime-status-slot")
                .expect("idle keeps the horizontal status reservation");
            let idle_bounds = stable_selectors.map(|selector| {
                cx.debug_bounds(selector)
                    .unwrap_or_else(|| panic!("idle header is missing {selector} at {width}px"))
            });

            for status in [
                SemanticStatus::Running,
                SemanticStatus::Authorization,
                SemanticStatus::Warning,
                SemanticStatus::Error,
            ] {
                shell.update(cx, |shell, cx| {
                    let mut view_model = conversation_header::view_model(&shell.app, &shell.ui);
                    view_model.status = status;
                    // Isolate the status transition from the independently
                    // conditional Abort action so this regression measures only
                    // the attention indicator's geometry contract.
                    view_model.composer_running = false;
                    shell.views.conversation_header.update(cx, |header, cx| {
                        header.set_view_model(view_model);
                        cx.notify();
                    });
                });
                cx.run_until_parked();
                let indicator = cx
                    .debug_bounds("desktop-header-runtime-status")
                    .unwrap_or_else(|| panic!("{status:?} renders the attention indicator"));
                let slot = cx
                    .debug_bounds("desktop-header-runtime-status-slot")
                    .expect("attention states keep the reserved status slot");
                assert!(
                    indicator.left() >= slot.left() && indicator.right() <= slot.right(),
                    "{status:?} indicator must fit its slot: indicator={indicator:?}, slot={slot:?}"
                );
                assert_eq!(idle_slot.left(), slot.left());
                assert_eq!(idle_slot.size.width, slot.size.width);
                let attention_bounds = stable_selectors.map(|selector| {
                    cx.debug_bounds(selector).unwrap_or_else(|| {
                        panic!("{status:?} header is missing {selector} at {width}px")
                    })
                });
                assert_eq!(
                    idle_bounds, attention_bounds,
                    "{status:?} appearance must not move any other Header control at {width}px"
                );
            }
        }
    }

    #[gpui::test]
    fn inspector_tabs_stay_on_one_line_in_docked_and_drawer_layouts(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell_with_preferences(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            visual_test_projection(),
            visual_preferences_with_inspector(),
        );

        for (width, open_modal) in [(1_300., false), (700., true)] {
            cx.simulate_resize(size(px(width), px(900.)));
            cx.run_until_parked();
            if open_modal {
                cx.update(|window, app| {
                    shell.update(app, |shell, app| shell.toggle_context(window, app));
                });
                cx.run_until_parked();
            }

            let tabs = cx
                .debug_bounds("desktop-inspector-tabs")
                .unwrap_or_else(|| {
                    panic!(
                        "Inspector tab strip is visible at width {width}; panel={:?}, details={:?}",
                        cx.debug_bounds("desktop-inspector-panel"),
                        cx.debug_bounds("inspector-details")
                    )
                });
            let tab_bounds = [
                "desktop-inspector-tab-changes",
                "desktop-inspector-tab-task",
                "desktop-inspector-tab-usage",
                "desktop-inspector-tab-runtime",
            ]
            .map(|selector| {
                cx.debug_bounds(selector)
                    .unwrap_or_else(|| panic!("missing Inspector tab {selector}"))
            });
            let first = tab_bounds[0];
            for bounds in tab_bounds {
                assert_eq!(bounds.top(), first.top());
                assert_eq!(bounds.bottom(), first.bottom());
                assert_eq!(f32::from(bounds.size.height), 32.);
            }
            assert!(tab_bounds[0].size.width > tab_bounds[1].size.width);
            assert!(tab_bounds[3].size.width > tab_bounds[2].size.width);
            assert!(tab_bounds[0].left() >= tabs.left());

            shell.update(cx, |shell, cx| {
                shell
                    .app
                    .workspaces
                    .active_mut()
                    .presentation
                    .inspector_section = InspectorSection::Runtime;
                shell.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
            });
            cx.run_until_parked();
            let runtime = cx
                .debug_bounds("desktop-inspector-tab-runtime")
                .expect("selected Runtime tab remains mounted");
            assert!(runtime.left() >= tabs.left() && runtime.right() <= tabs.right());
            assert!(shell.read_with(cx, |shell, cx| {
                shell.views.inspector_pane.read(cx).tab_scroll_offset().x <= px(0.)
            }));

            cx.update(|window, app| {
                shell.update(app, |shell, app| {
                    shell.views.inspector_pane.update(app, |pane, app| {
                        pane.focus_tab(InspectorSection::Runtime, window, app)
                    });
                });
            });
            let left = gpui::Keystroke::parse("left").expect("left is a valid keystroke");
            assert!(cx.update(|window, app| window.dispatch_keystroke(left, app)));
            cx.run_until_parked();
            assert_eq!(
                shell.read_with(cx, |shell, _| shell
                    .app
                    .workspaces
                    .active()
                    .presentation
                    .inspector_section),
                InspectorSection::Usage
            );
            let usage = cx
                .debug_bounds("desktop-inspector-tab-usage")
                .expect("keyboard-selected Usage tab remains mounted");
            assert!(usage.left() >= tabs.left() && usage.right() <= tabs.right());
        }
    }

    #[gpui::test]
    fn responsive_drawers_preserve_conversation_geometry_scroll_and_owner_focus(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(CodingAgentSessionTranscriptItem::User {
                text: "Drawer geometry must remain stable.".into(),
            }),
        );

        cx.simulate_resize(size(px(1_000.), px(900.)));
        settle_visual_measurements(cx);
        let medium_conversation = cx
            .debug_bounds("desktop-conversation-panel")
            .expect("medium conversation remains visible");
        let medium_row = cx
            .debug_bounds("conversation-last-row")
            .expect("medium conversation row remains mounted");
        let medium_scroll = shell.read_with(cx, |shell, _| {
            shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .scroll
                .offset()
        });

        cx.dispatch_action(ToggleInspectorPanel);
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.active_drawer),
            Some(CenterDrawerKind::Inspector)
        );
        assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_modal), None);
        assert!(cx.debug_bounds("desktop-inspector-panel").is_some());
        assert_minimum_hit_target(cx, "desktop-hit-close-inspector");
        let center_header = cx
            .debug_bounds("desktop-conversation-header")
            .expect("center header remains mounted above the drawer host");
        let center_body = cx
            .debug_bounds("desktop-center-body")
            .expect("center body owns the drawer host");
        let inspector_drawer = cx
            .debug_bounds("desktop-inspector-drawer")
            .expect("Inspector is rendered by the center-body drawer host");
        assert_eq!(inspector_drawer.top(), center_body.top());
        assert_eq!(inspector_drawer.bottom(), center_body.bottom());
        assert_eq!(inspector_drawer.right(), center_body.right());
        assert!(center_header.bottom() <= inspector_drawer.top());
        assert_eq!(
            cx.debug_bounds("desktop-conversation-panel"),
            Some(medium_conversation)
        );
        assert_eq!(cx.debug_bounds("conversation-last-row"), Some(medium_row));
        assert_eq!(
            shell.read_with(cx, |shell, _| shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .scroll
                .offset()),
            medium_scroll
        );

        let model_selector = cx
            .debug_bounds("desktop-header-model-selector")
            .expect("the model selector stays exposed while Inspector is open");
        cx.simulate_click(model_selector.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let down = gpui::Keystroke::parse("down").expect("down is a valid popup keystroke");
        assert!(cx.update(|window, app| window.dispatch_keystroke(down, app)));
        let escape = gpui::Keystroke::parse("escape").expect("escape is a valid popup keystroke");
        assert!(cx.update(|window, app| window.dispatch_keystroke(escape, app)));
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.active_drawer),
            Some(CenterDrawerKind::Inspector),
            "selector interaction must not implicitly close the non-modal drawer"
        );

        cx.simulate_click(center_body.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_drawer), None);
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.focus.active()),
            FocusTarget::Composer
        );

        cx.simulate_resize(size(px(700.), px(900.)));
        settle_visual_measurements(cx);
        let narrow_conversation = cx
            .debug_bounds("desktop-conversation-panel")
            .expect("narrow conversation remains visible");
        let narrow_row = cx
            .debug_bounds("conversation-last-row")
            .expect("narrow conversation row remains mounted");
        let narrow_scroll = shell.read_with(cx, |shell, _| {
            shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .scroll
                .offset()
        });
        let sessions_toggle = cx
            .debug_bounds("desktop-hit-toggle-sessions")
            .expect("narrow layout retains the Sessions drawer toggle");
        cx.simulate_click(sessions_toggle.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.active_drawer),
            Some(CenterDrawerKind::Sessions)
        );
        assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_modal), None);
        assert!(cx.debug_bounds("desktop-sessions-drawer").is_some());
        assert_minimum_hit_target(cx, "desktop-hit-refresh-projects");
        assert_minimum_hit_target(cx, "desktop-hit-close-narrow-sessions");
        assert!(
            cx.debug_bounds("desktop-projects-state-not-loaded")
                .is_some(),
            "narrow drawer reuses the Projects-local unloaded state"
        );
        assert!(
            cx.debug_bounds("sessions-search").is_none(),
            "search remains optional until the project catalog has entries"
        );
        assert_eq!(
            cx.debug_bounds("desktop-conversation-panel"),
            Some(narrow_conversation)
        );
        assert_eq!(cx.debug_bounds("conversation-last-row"), Some(narrow_row));
        assert_eq!(
            shell.read_with(cx, |shell, _| shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .scroll
                .offset()),
            narrow_scroll
        );

        let inspector_toggle = cx
            .debug_bounds("desktop-hit-toggle-inspector")
            .expect("the Header keeps the primary Inspector toggle above either drawer");
        cx.simulate_click(inspector_toggle.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.active_drawer),
            Some(CenterDrawerKind::Inspector)
        );
        assert!(cx.debug_bounds("desktop-sessions-drawer").is_none());
        assert!(cx.debug_bounds("desktop-inspector-drawer").is_some());

        cx.simulate_click(sessions_toggle.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.active_drawer),
            Some(CenterDrawerKind::Sessions)
        );
        assert!(cx.debug_bounds("desktop-inspector-drawer").is_none());
        assert!(cx.debug_bounds("desktop-sessions-drawer").is_some());

        cx.dispatch_action(EscapeHierarchy);
        cx.run_until_parked();
        assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_drawer), None);
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.focus.active()),
            FocusTarget::Composer
        );
    }

    fn assert_profile_dropdown_usable_with_inspector_drawer(
        cx: &mut TestAppContext,
        viewport_width: f32,
    ) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        cx.simulate_resize(size(px(viewport_width), px(900.)));
        settle_visual_measurements(cx);

        let inspector_toggle = cx
            .debug_bounds("desktop-hit-toggle-inspector")
            .expect("the center Header exposes the primary Inspector toggle");
        cx.simulate_click(inspector_toggle.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.active_drawer),
            Some(CenterDrawerKind::Inspector)
        );
        let center_header = cx
            .debug_bounds("desktop-conversation-header")
            .expect("the center Header remains mounted above the drawer host");
        let inspector_drawer = cx
            .debug_bounds("desktop-inspector-drawer")
            .expect("Inspector opens as a center-body drawer");
        assert!(center_header.bottom() <= inspector_drawer.top());
        assert!(inspector_toggle.bottom() <= inspector_drawer.top());
        assert_minimum_hit_target(cx, "desktop-hit-close-inspector");

        let profile_selector = cx
            .debug_bounds("desktop-header-profile-selector")
            .expect("the Profile selector stays exposed while Inspector is open");
        assert!(profile_selector.bottom() <= inspector_drawer.top());
        cx.simulate_click(profile_selector.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        choose_popup_item(cx, 1);

        assert_eq!(
            runtime_harness.drain_selections(),
            [(
                desktop::runtime::DesktopRuntimeCommandKind::SelectSessionProfile,
                DesktopRuntimeOwnerTarget::session("desktop-visual-test"),
                "exact-reviewer".into(),
                None,
            )]
        );
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.active_drawer),
            Some(CenterDrawerKind::Inspector),
            "Profile selection must not implicitly close the non-modal drawer"
        );

        let close = cx
            .debug_bounds("desktop-hit-close-inspector")
            .expect("the drawer exposes its auxiliary close control");
        cx.simulate_click(close.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_drawer), None);
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.focus.active()),
            FocusTarget::Composer,
            "the auxiliary close control restores the pre-drawer focus owner"
        );
    }

    #[gpui::test]
    fn medium_profile_dropdown_remains_usable_with_inspector_drawer(cx: &mut TestAppContext) {
        assert_profile_dropdown_usable_with_inspector_drawer(cx, 1_000.);
    }

    #[gpui::test]
    fn narrow_profile_dropdown_remains_usable_with_inspector_drawer(cx: &mut TestAppContext) {
        assert_profile_dropdown_usable_with_inspector_drawer(cx, 700.);
    }

    #[gpui::test]
    fn final_long_markdown_tail_is_inside_measured_row_at_all_viewports(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            clipping_regression_projection(),
        );

        for (width, height) in [(1_300., 900.), (1_000., 800.), (700., 800.)] {
            cx.simulate_resize(size(px(width), px(height)));
            cx.executor().advance_clock(Duration::from_millis(100));
            for _ in 0..4 {
                cx.update(|window, _| window.refresh());
                cx.run_until_parked();
            }

            let shell_state = cx.update(|_, app| {
                let shell = shell.read(app);
                (
                    shell
                        .app
                        .workspaces
                        .active()
                        .presentation
                        .conversation_controller
                        .row_count(),
                    shell
                        .app
                        .workspaces
                        .active()
                        .presentation
                        .conversation_controller
                        .render_heights_for_tests(),
                    shell
                        .app
                        .workspaces
                        .active()
                        .presentation
                        .conversation_controller
                        .scroll
                        .offset(),
                )
            });
            let row = cx.debug_bounds("conversation-last-row").unwrap_or_else(|| {
                panic!(
                    "final virtual row is mounted: state={shell_state:?}, card={:?}, tail={:?}, panel={:?}",
                    cx.debug_bounds("conversation-last-card"),
                    cx.debug_bounds("conversation-tail-marker"),
                    cx.debug_bounds("desktop-conversation-panel"),
                )
            });
            let card = cx
                .debug_bounds("conversation-last-card")
                .expect("final conversation card is laid out");
            let tail = cx
                .debug_bounds("conversation-tail-marker")
                .expect("tail layout marker is laid out");
            let composer = cx
                .debug_bounds("desktop-composer-panel")
                .expect("composer remains visible");

            assert!(
                f32::from(card.size.height) > TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT,
                "{width}px fixture must exceed the former silent clipping limit"
            );
            assert!(
                (f32::from(row.size.height)
                    - (f32::from(card.size.height) + CONVERSATION_ROW_VERTICAL_PADDING_PX as f32))
                    .abs()
                    <= 1.,
                "{width}px virtual row must match actual card bounds: row={row:?}, card={card:?}"
            );
            assert!(
                tail.bottom() <= row.bottom() + px(1.),
                "{width}px tail marker must remain inside the virtual row"
            );
            assert!(
                tail.bottom() <= composer.top() + px(1.),
                "{width}px final tail must not be hidden below the Composer: tail={tail:?}, composer={composer:?}, row={row:?}"
            );
        }
    }

    #[gpui::test]
    fn final_long_user_tail_is_inside_its_measured_row(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (_, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(CodingAgentSessionTranscriptItem::User {
                text: long_integrity_text("User"),
            }),
        );
        cx.simulate_resize(size(px(700.), px(800.)));
        settle_visual_measurements(cx);

        assert_last_row_matches_card_and_tail(cx, "User");
        assert!(
            f32::from(
                cx.debug_bounds("conversation-last-card")
                    .expect("User card remains mounted")
                    .size
                    .height
            ) > TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT,
            "long User content must not inherit the former silent height cap"
        );
    }

    #[gpui::test]
    fn final_long_diagnostic_tail_is_inside_its_measured_row(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (_, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(CodingAgentSessionTranscriptItem::Diagnostic {
                message: long_integrity_text("Diagnostic"),
            }),
        );
        cx.simulate_resize(size(px(700.), px(800.)));
        settle_visual_measurements(cx);

        assert_last_row_matches_card_and_tail(cx, "Diagnostic");
        assert!(
            f32::from(
                cx.debug_bounds("conversation-last-card")
                    .expect("Diagnostic card remains mounted")
                    .size
                    .height
            ) > TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT,
            "long Diagnostic content must not inherit the former silent height cap"
        );
    }

    #[gpui::test]
    fn final_long_tool_expands_without_losing_its_tail(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(CodingAgentSessionTranscriptItem::Tool {
                call_id: "long-tool-output".into(),
                name: "shell".into(),
                args: serde_json::json!({
                    "command": "cargo test --workspace",
                    "notes": "参数 中文 🙂".repeat(80),
                }),
                result: Some(long_integrity_text("Tool output")),
                is_error: false,
                duration_millis: Some(1_240),
            }),
        );
        cx.simulate_resize(size(px(700.), px(800.)));
        settle_visual_measurements(cx);
        assert_last_row_matches_card_and_tail(cx, "collapsed Tool");
        let collapsed_height = f32::from(
            cx.debug_bounds("conversation-last-card")
                .expect("collapsed Tool card is laid out")
                .size
                .height,
        );

        let block_id = shell.read_with(cx, |shell, _| {
            shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .last_row_id_for_tests()
                .expect("Tool row exists")
        });
        assert_minimum_hit_target(cx, "desktop-toggle-tool-details");
        let tool_header = cx
            .debug_bounds("desktop-tool-toggle-header")
            .expect("the complete tool header is a disclosure action");
        cx.simulate_click(tool_header.center(), gpui::Modifiers::default());
        settle_visual_measurements(cx);

        assert_eq!(
            shell.read_with(cx, |shell, _| {
                shell
                    .app
                    .workspaces
                    .active()
                    .presentation
                    .conversation_controller
                    .selected_block_id()
                    .map(str::to_owned)
            }),
            Some(block_id.clone()),
            "clicking the tool disclosure preserves the typed row-selection path"
        );
        assert_last_row_matches_card_and_tail(cx, "expanded Tool");
        let expanded_height = f32::from(
            cx.debug_bounds("conversation-last-card")
                .expect("expanded Tool card is laid out")
                .size
                .height,
        );
        assert!(
            expanded_height > collapsed_height + 100.,
            "expanded Tool output must contribute its real content height: collapsed={collapsed_height}, expanded={expanded_height}"
        );
        let output_region = cx
            .debug_bounds("desktop-tool-output-region")
            .expect("expanded Tool output uses its dedicated region");
        assert!(
            f32::from(output_region.size.height) <= 402.,
            "expanded Tool output must stay height-bounded and scroll internally: region={output_region:?}"
        );
    }

    #[gpui::test]
    fn expanded_shell_tool_copies_the_displayed_command_and_output(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let command = "cargo test -p desktop";
        let output = "desktop tests passed\n";
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(CodingAgentSessionTranscriptItem::Tool {
                call_id: "tool-actions".into(),
                name: "bash".into(),
                args: serde_json::json!({ "command": command, "timeout": 120 }),
                result: Some(output.into()),
                is_error: false,
                duration_millis: Some(320),
            }),
        );
        let block_id = shell.read_with(cx, |shell, _| {
            shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .last_row_id_for_tests()
                .expect("Tool row exists")
        });
        assert_minimum_hit_target(cx, "desktop-toggle-tool-details");
        let disclosure = cx
            .debug_bounds("desktop-toggle-tool-details")
            .expect("tool chevron exposes the typed disclosure path");
        cx.simulate_click(disclosure.center(), gpui::Modifiers::default());
        settle_visual_measurements(cx);
        assert_eq!(
            shell.read_with(cx, |shell, _| {
                shell
                    .app
                    .workspaces
                    .active()
                    .presentation
                    .conversation_controller
                    .selected_block_id()
                    .map(str::to_owned)
            }),
            Some(block_id.clone())
        );

        assert!(
            cx.debug_bounds("desktop-tool-output-region").is_some(),
            "expanded Shell output uses the dedicated bordered region"
        );
        assert_minimum_hit_target(cx, "desktop-copy-tool-details");
        let copy_details = cx
            .debug_bounds("desktop-copy-tool-details")
            .expect("the expanded region exposes one hover copy action");
        cx.simulate_click(copy_details.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(format!("$ {command}\n{output}"))
        );
    }

    #[gpui::test]
    fn read_tool_remains_a_single_collapsed_summary(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (_, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(CodingAgentSessionTranscriptItem::Tool {
                call_id: "read-summary".into(),
                name: "read".into(),
                args: serde_json::json!({
                    "path": "src/main.rs",
                    "offset": 40,
                    "limit": 80,
                }),
                result: Some("read output remains hidden".into()),
                is_error: false,
                duration_millis: Some(20),
            }),
        );
        settle_visual_measurements(cx);

        assert!(cx.debug_bounds("conversation-last-card").is_some());
        assert!(
            cx.debug_bounds("desktop-toggle-tool-details").is_none(),
            "Read does not expose a disclosure chevron"
        );
        assert!(
            cx.debug_bounds("desktop-tool-toggle-header").is_none(),
            "Read header is not presented as an expandable surface"
        );
        assert!(
            cx.debug_bounds("desktop-tool-output-region").is_none(),
            "Read has no expanded output region"
        );
        assert!(
            cx.debug_bounds("desktop-copy-conversation-row").is_none(),
            "Tool rows do not inherit the generic message copy footer"
        );
    }

    #[gpui::test]
    fn assistant_after_tool_continues_without_repeating_the_identity_header(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (_, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_items(vec![
                CodingAgentSessionTranscriptItem::Tool {
                    call_id: "identity-tool".into(),
                    name: "shell".into(),
                    args: serde_json::json!({ "command": "git status" }),
                    result: Some("working tree clean".into()),
                    is_error: false,
                    duration_millis: Some(20),
                },
                CodingAgentSessionTranscriptItem::Assistant {
                    id: "identity-answer".into(),
                    text: "This answer is part of the same assistant output.".into(),
                    thinking: String::new(),
                    images: Vec::new(),
                    done: true,
                    reasoning_duration_millis: None,
                },
            ]),
        );
        settle_visual_measurements(cx);

        assert!(
            cx.debug_bounds("conversation-last-card").is_some(),
            "the final Assistant answer remains rendered"
        );
        assert!(
            cx.debug_bounds("desktop-last-conversation-row-header")
                .is_none(),
            "a Tool row must not restart the Assistant identity group"
        );
        assert!(
            cx.debug_bounds("desktop-copy-conversation-row").is_some(),
            "the final Assistant segment keeps the group's copy action"
        );
    }

    #[gpui::test]
    fn assistant_segment_before_tool_does_not_insert_a_middle_copy_button(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (_, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_items(vec![
                CodingAgentSessionTranscriptItem::Assistant {
                    id: "pre-tool-answer".into(),
                    text: "I will inspect the workspace.".into(),
                    thinking: String::new(),
                    images: Vec::new(),
                    done: true,
                    reasoning_duration_millis: None,
                },
                CodingAgentSessionTranscriptItem::Tool {
                    call_id: "copy-group-tool".into(),
                    name: "shell".into(),
                    args: serde_json::json!({ "command": "git status" }),
                    result: Some("working tree clean".into()),
                    is_error: false,
                    duration_millis: Some(20),
                },
            ]),
        );
        settle_visual_measurements(cx);

        assert!(
            cx.debug_bounds("desktop-copy-conversation-row").is_none(),
            "an Assistant segment immediately before Tool must not paint an in-between copy action"
        );
    }

    #[gpui::test]
    fn tool_content_aligns_with_assistant_and_selection_has_no_focus_rail(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (_, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_items(vec![
                CodingAgentSessionTranscriptItem::Assistant {
                    id: "alignment-answer".into(),
                    text: "Assistant content alignment reference.".into(),
                    thinking: String::new(),
                    images: Vec::new(),
                    done: true,
                    reasoning_duration_millis: None,
                },
                CodingAgentSessionTranscriptItem::Tool {
                    call_id: "alignment-tool".into(),
                    name: "shell".into(),
                    args: serde_json::json!({ "command": "git status" }),
                    result: Some("working tree clean".into()),
                    is_error: false,
                    duration_millis: Some(20),
                },
            ]),
        );
        cx.simulate_resize(size(px(1_200.), px(900.)));
        settle_visual_measurements(cx);

        let assistant_header = cx
            .debug_bounds("desktop-conversation-row-header")
            .expect("Assistant header is available as the alignment reference");
        let tool_header = cx
            .debug_bounds("desktop-tool-toggle-header")
            .expect("Tool header is laid out");
        assert_eq!(
            (assistant_header.left(), assistant_header.right()),
            (tool_header.left(), tool_header.right()),
            "Tool and Assistant content must share the same horizontal bounds"
        );

        cx.simulate_click(tool_header.center(), gpui::Modifiers::default());
        settle_visual_measurements(cx);
        assert!(
            cx.debug_bounds("conversation-selected-rail").is_none(),
            "selecting a Tool row must not paint the conversation focus rail"
        );
        let output = cx
            .debug_bounds("desktop-tool-output-region")
            .expect("selected Tool still expands normally");
        assert_eq!(
            (output.left(), output.right()),
            (tool_header.left(), tool_header.right()),
            "expanded Tool details must stay aligned with the collapsed summary"
        );
    }

    #[gpui::test]
    fn assistant_reasoning_expands_downward_without_moving_its_top(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(CodingAgentSessionTranscriptItem::Assistant {
                id: "reasoning-layout".into(),
                text: "Final answer tail remains visible.".into(),
                thinking: long_integrity_text("Reasoning"),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: Some(2_430),
            }),
        );
        cx.simulate_resize(size(px(700.), px(800.)));
        settle_visual_measurements(cx);
        assert_last_row_matches_card_and_tail(cx, "collapsed Reasoning");
        let collapsed_height = f32::from(
            cx.debug_bounds("conversation-last-card")
                .expect("collapsed reasoning card is laid out")
                .size
                .height,
        );
        let collapsed_top = cx
            .debug_bounds("conversation-last-card")
            .expect("collapsed reasoning card is laid out")
            .top();

        assert_minimum_hit_target(cx, "desktop-toggle-reasoning-details");
        let reasoning_header = cx
            .debug_bounds("desktop-reasoning-toggle-header")
            .expect("the complete reasoning header is a disclosure action");
        cx.simulate_click(reasoning_header.center(), gpui::Modifiers::default());
        settle_visual_measurements(cx);
        assert!(shell.read_with(cx, |shell, _| {
            shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .toggle_anchor_active_for_tests()
        }));

        let row = cx
            .debug_bounds("conversation-last-row")
            .expect("expanded reasoning row remains mounted");
        let card = cx
            .debug_bounds("conversation-last-card")
            .expect("expanded reasoning card is laid out");
        let tail = cx
            .debug_bounds("conversation-tail-marker")
            .expect("expanded reasoning tail remains laid out");
        let expanded_height = f32::from(card.size.height);
        assert!(
            expanded_height > collapsed_height + 100.,
            "expanded reasoning must contribute its real content height: collapsed={collapsed_height}, expanded={expanded_height}"
        );
        assert_eq!(
            card.top(),
            collapsed_top,
            "expanding details must keep the message top fixed and grow downward"
        );
        assert!(
            (f32::from(row.size.height)
                - (expanded_height + CONVERSATION_ROW_VERTICAL_PADDING_PX as f32))
                .abs()
                <= 1.,
            "the expanded virtual row must match its measured card: row={row:?}, card={card:?}"
        );
        assert!(
            tail.bottom() <= row.bottom() + px(1.),
            "the expanded tail must remain inside its own row even when below the viewport: tail={tail:?}, row={row:?}"
        );
    }

    #[gpui::test]
    fn assistant_reasoning_chevron_toggles_once_without_reflow(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(CodingAgentSessionTranscriptItem::Assistant {
                id: "reasoning-chevron".into(),
                text: "The final answer remains below the disclosure.".into(),
                thinking: "A bounded reasoning detail line.\n".repeat(12),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: Some(640),
            }),
        );
        cx.simulate_resize(size(px(700.), px(800.)));
        settle_visual_measurements(cx);
        let block_id = shell.read_with(cx, |shell, _| {
            shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .last_row_id_for_tests()
                .expect("Assistant row exists")
        });
        let collapsed_height = f32::from(
            cx.debug_bounds("conversation-last-card")
                .expect("collapsed reasoning card is laid out")
                .size
                .height,
        );

        assert_minimum_hit_target(cx, "desktop-toggle-reasoning-details");
        let expand = cx
            .debug_bounds("desktop-toggle-reasoning-details")
            .expect("collapsed reasoning retains its trailing disclosure icon");
        cx.simulate_click(expand.center(), gpui::Modifiers::default());
        settle_visual_measurements(cx);
        assert!(shell.read_with(cx, |shell, _| {
            shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .expanded_details()
                .contains(&block_id)
        }));
        let expanded_height = f32::from(
            cx.debug_bounds("conversation-last-card")
                .expect("expanded reasoning card is laid out")
                .size
                .height,
        );
        assert!(expanded_height > collapsed_height);

        assert_minimum_hit_target(cx, "desktop-toggle-reasoning-details");
        let collapse = cx
            .debug_bounds("desktop-toggle-reasoning-details")
            .expect("expanded reasoning retains its trailing disclosure icon");
        cx.simulate_click(collapse.center(), gpui::Modifiers::default());
        settle_visual_measurements(cx);
        let collapsed_again = f32::from(
            cx.debug_bounds("conversation-last-card")
                .expect("collapsed reasoning card remains laid out")
                .size
                .height,
        );
        assert!(
            (collapsed_again - collapsed_height).abs() <= 1.,
            "the standalone icon must emit exactly one collapse: initial={collapsed_height}, final={collapsed_again}"
        );
        assert!(
            !shell.read_with(cx, |shell, _| {
                shell
                    .app
                    .workspaces
                    .active()
                    .presentation
                    .conversation_controller
                    .expanded_details()
                    .contains(&block_id)
            }),
            "the reasoning disclosure returns to its collapsed state"
        );
    }

    #[gpui::test]
    fn conversation_row_copy_selection_is_typed_and_geometry_stable(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let message = "Copy the complete bounded conversation row.";
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(CodingAgentSessionTranscriptItem::Assistant {
                id: "row-copy-selection".into(),
                text: message.into(),
                thinking: String::new(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: None,
            }),
        );
        cx.simulate_resize(size(px(700.), px(800.)));
        settle_visual_measurements(cx);
        let card_before_selection = cx
            .debug_bounds("conversation-last-card")
            .expect("conversation row card remains mounted");
        assert_minimum_hit_target(cx, "desktop-copy-conversation-row");

        let row_header = cx
            .debug_bounds("desktop-last-conversation-row-header")
            .expect("conversation row header exposes its typed selection path");
        cx.simulate_click(row_header.center(), gpui::Modifiers::default());
        settle_visual_measurements(cx);
        assert_eq!(
            shell.read_with(cx, |shell, _| {
                shell
                    .app
                    .workspaces
                    .active()
                    .presentation
                    .conversation_controller
                    .selected_block_id()
                    .map(str::to_owned)
            }),
            Some("assistant:row-copy-selection".into())
        );
        assert_eq!(
            cx.debug_bounds("conversation-last-card"),
            Some(card_before_selection),
            "revealing the selected-row copy icon must not reflow the card"
        );

        assert_minimum_hit_target(cx, "desktop-copy-conversation-row");
        let copy = cx
            .debug_bounds("desktop-copy-conversation-row")
            .expect("selected row exposes its copy icon");
        cx.simulate_click(copy.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(message.into())
        );
    }

    #[gpui::test]
    fn truncated_preview_opens_and_copies_the_complete_bounded_message(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let mut snapshot = visual_test_snapshot();
        let full_text = format!(
            "BEGIN FULL MESSAGE\n{}END FULL MESSAGE",
            "完整消息内容 🙂 e\u{301}\n".repeat(24_000)
        );
        assert!(full_text.len() > desktop::conversation::MAX_MARKDOWN_PREVIEW_BYTES);
        assert!(full_text.len() < MAX_COPY_BYTES);
        snapshot
            .transcript
            .items
            .push(CodingAgentSessionTranscriptItem::Assistant {
                id: "full-message-regression".into(),
                text: full_text.clone(),
                thinking: String::new(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: None,
            });
        let projection = DesktopProjection::new(snapshot)
            .expect("full-message fixture is a valid product projection");
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection,
        );
        cx.simulate_resize(size(px(700.), px(800.)));
        cx.executor().advance_clock(Duration::from_millis(100));
        for _ in 0..4 {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }

        let open = cx
            .debug_bounds("desktop-open-full-message")
            .expect("truncated preview exposes an explicit full-message action");
        assert_minimum_hit_target(cx, "desktop-open-full-message");
        assert_minimum_hit_target(cx, "desktop-copy-conversation-row");
        let composer = cx
            .debug_bounds("desktop-composer-panel")
            .expect("Composer remains visible below the preview");
        let row = cx
            .debug_bounds("conversation-last-row")
            .expect("truncated preview row is mounted");
        assert!(
            open.top() >= row.top()
                && open.bottom() <= row.bottom()
                && open.bottom() <= composer.top(),
            "full-message action must be reachable inside its row and above the Composer: open={open:?}, row={row:?}, composer={composer:?}, offset={:?}",
            shell.read_with(cx, |shell, _| shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .scroll
                .offset())
        );
        cx.simulate_click(open.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.active_modal),
            Some(DesktopModalKind::FullMessage)
        );
        let dialog = cx
            .debug_bounds("desktop-full-message-dialog")
            .expect("full message uses a modal dialog");
        let scroll = cx
            .debug_bounds("desktop-full-message-scroll")
            .expect("full message uses one explicit scroll container");
        assert!(scroll.size.height < dialog.size.height);
        assert!(shell.read_with(cx, |shell, _| {
            shell
                .ui
                .conversation_full_message
                .as_ref()
                .is_some_and(|message| {
                    message.text.starts_with("BEGIN FULL MESSAGE")
                        && message.text.ends_with("END FULL MESSAGE")
                        && !message.source_truncated
                })
        }));

        let copy = cx
            .debug_bounds("desktop-copy-full-message")
            .expect("full viewer exposes its complete-source copy action");
        cx.simulate_click(copy.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(full_text)
        );

        let close = cx
            .debug_bounds("desktop-close-full-message")
            .expect("full viewer exposes a close action");
        cx.simulate_click(close.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_modal), None);
        assert!(shell.read_with(cx, |shell, _| shell.ui.conversation_full_message.is_none()));
    }

    #[gpui::test]
    fn native_shell_primary_controls_keep_minimum_hit_targets(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            visual_test_projection(),
        );
        shell.update(cx, |shell, cx| {
            let active_session_id = shell
                .app
                .workspaces
                .active()
                .projection
                .as_ref()
                .expect("the visual shell owns a session projection")
                .snapshot()
                .session
                .session_id
                .clone();
            shell.app.catalog.replace_catalog(
                vec![
                    desktop::runtime::DesktopSessionCatalogEntry {
                        session_id: active_session_id,
                        updated_at: "2026-07-28T09:00:00Z".into(),
                        ..Default::default()
                    },
                    desktop::runtime::DesktopSessionCatalogEntry {
                        session_id: "recent-session-with-a-stable-action-row".into(),
                        updated_at: "2026-07-28T08:00:00Z".into(),
                        ..Default::default()
                    },
                ],
                0,
            );
            shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        });

        for width in [1_300., 700.] {
            cx.simulate_resize(size(px(width), px(900.)));
            cx.run_until_parked();
            for selector in [
                "desktop-hit-toggle-sessions",
                "desktop-hit-toggle-inspector",
                "desktop-hit-submit-composer",
                "desktop-header-model-selector",
                "desktop-header-profile-selector",
                "desktop-header-thinking-selector",
            ] {
                assert_minimum_hit_target(cx, selector);
            }
        }

        cx.simulate_resize(size(px(1_300.), px(900.)));
        cx.run_until_parked();
        assert_minimum_hit_target(cx, "desktop-hit-new-conversation");
        assert_minimum_hit_target(cx, "desktop-hit-refresh-projects");
        assert_minimum_hit_target(cx, "desktop-project-row-0");
        assert_minimum_hit_target(cx, "desktop-session-row-1");
        assert_minimum_hit_target(cx, "desktop-hit-session-actions-1");
    }

    #[gpui::test]
    fn sessions_show_names_search_name_and_id_and_offer_manual_rename(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();
        shell.update(cx, |shell, cx| {
            shell.app.catalog.replace_catalog(
                vec![
                    desktop::runtime::DesktopSessionCatalogEntry {
                        session_id: "named-session-id".into(),
                        name: Some("Release plan".into()),
                        updated_at: "9999-12-31T23:59:59Z".into(),
                        ..Default::default()
                    },
                    desktop::runtime::DesktopSessionCatalogEntry {
                        session_id: "unnamed-session-id".into(),
                        name: None,
                        updated_at: "9999-12-31T23:59:59Z".into(),
                        ..Default::default()
                    },
                ],
                0,
            );
            shell.refresh_views(UiChangeSet::one(UiRegion::Sessions), cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-session-row-0").is_some());
        assert!(cx.debug_bounds("desktop-session-row-1").is_some());

        cx.update(|window, app| {
            shell.update(app, |shell, app| {
                shell.views.sessions_pane.update(app, |pane, app| {
                    pane.set_search_value("Release", window, app)
                });
            });
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-session-row-0").is_some());
        assert!(cx.debug_bounds("desktop-session-row-1").is_none());

        cx.update(|window, app| {
            shell.update(app, |shell, app| {
                shell
                    .views
                    .sessions_pane
                    .update(app, |pane, app| pane.set_search_value("", window, app));
            });
        });
        cx.run_until_parked();
        let rename = cx
            .debug_bounds("desktop-hit-session-actions-1")
            .expect("unnamed session exposes its compact actions menu");
        cx.simulate_click(rename.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        choose_popup_item(cx, 0);
        assert!(cx.debug_bounds("desktop-session-rename-1").is_some());
        cx.update(|window, app| {
            shell.update(app, |shell, app| {
                shell.views.sessions_pane.update(app, |pane, app| {
                    pane.set_rename_value("Recovered name", window, app)
                });
            });
        });
        cx.run_until_parked();
        let commit = cx
            .debug_bounds("desktop-hit-commit-session-rename-1")
            .expect("inline rename exposes a save action");
        cx.simulate_click(commit.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            runtime_harness.drain_session_renames(),
            [("unnamed-session-id".into(), Some("Recovered name".into()))]
        );
    }

    #[gpui::test]
    fn idle_model_selector_groups_configured_text_models_and_submits_the_exact_id(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        let view_model = shell.read_with(cx, |shell, _| {
            conversation_header::view_model(&shell.app, &shell.ui)
        });
        assert!(view_model.idle);
        assert!(shell.read_with(cx, |shell, _| {
            shell.app.workspaces.active().projection.is_none()
        }));
        assert_eq!(
            view_model
                .model_groups
                .iter()
                .map(|group| group.provider.as_ref())
                .collect::<Vec<_>>(),
            ["fixture"]
        );
        assert_eq!(
            view_model
                .model_groups
                .iter()
                .flat_map(|group| group.options.iter())
                .map(|option| option.id.as_ref())
                .collect::<Vec<_>>(),
            ["adjacent-model", "exact-target-model", "test-model"]
        );
        assert!(view_model.unavailable_current_model.is_none());

        let selector = cx
            .debug_bounds("desktop-header-model-selector")
            .expect("the model selector is visible");
        cx.simulate_click(selector.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        choose_popup_item(cx, 2);

        assert_eq!(
            runtime_harness.drain_selections(),
            [(
                desktop::runtime::DesktopRuntimeCommandKind::SelectModel,
                DesktopRuntimeOwnerTarget::home(),
                "exact-target-model".into(),
                None,
            )]
        );
    }

    #[test]
    fn model_menu_filters_and_stably_orders_provider_groups_and_rows() {
        let models = vec![
            model_menu_fixture("z-current", "Zulu Current", "z-provider", true, true),
            model_menu_fixture("a-second", "Second Alpha", "a-provider", true, true),
            model_menu_fixture(
                "unconfigured",
                "Unavailable Alpha",
                "a-provider",
                false,
                true,
            ),
            model_menu_fixture("image-only", "Image Alpha", "a-provider", true, false),
            model_menu_fixture("a-first", "First Alpha", "a-provider", true, true),
        ];

        let (groups, warning) = conversation_header::model_menu(&models, "z-current");
        assert!(warning.is_none());
        assert_eq!(
            groups
                .iter()
                .map(|group| group.provider.as_ref())
                .collect::<Vec<_>>(),
            ["a-provider", "z-provider"]
        );
        assert_eq!(
            groups[0]
                .options
                .iter()
                .map(|option| option.id.as_ref())
                .collect::<Vec<_>>(),
            ["a-first", "a-second"]
        );
        assert_eq!(
            groups
                .iter()
                .flat_map(|group| group.options.iter())
                .map(|option| option.id.as_ref())
                .collect::<Vec<_>>(),
            ["a-first", "a-second", "z-current"]
        );

        let mut reordered = models;
        reordered.reverse();
        let (reordered_groups, _) = conversation_header::model_menu(&reordered, "z-current");
        assert_eq!(groups, reordered_groups);
    }

    #[test]
    fn model_menu_bounds_long_names_and_isolates_unavailable_current_model() {
        let long_name =
            "A deliberately very long model name used to prove bounded popup rows ".repeat(3);
        let models = vec![
            model_menu_fixture(
                "lost-auth-model",
                "Lost Authentication",
                "z-provider",
                false,
                true,
            ),
            model_menu_fixture(
                "configured-model-with-a-very-long-identifier-that-remains-typed",
                &long_name,
                "a-provider",
                true,
                true,
            ),
        ];

        let (groups, warning) = conversation_header::model_menu(&models, "lost-auth-model");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].options.len(), 1);
        assert_eq!(groups[0].options[0].name.as_ref(), long_name);
        assert_ne!(groups[0].options[0].display_name.as_ref(), long_name);
        assert!(groups[0].options[0].display_name.ends_with('…'));
        assert_eq!(
            warning,
            Some(ConversationHeaderModelWarning {
                id: Arc::from("lost-auth-model"),
                name: Arc::from("Lost Authentication"),
                reason: Arc::from("Authentication required"),
            })
        );

        let unavailable = models
            .into_iter()
            .map(|mut model| {
                model.configured = false;
                model
            })
            .collect::<Vec<_>>();
        let (empty_groups, warning) =
            conversation_header::model_menu(&unavailable, "lost-auth-model");
        assert!(empty_groups.is_empty());
        assert!(warning.is_some());
    }

    #[gpui::test]
    fn idle_profile_selector_uses_project_choices_and_submits_without_a_session(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        let view_model = shell.read_with(cx, |shell, _| {
            conversation_header::view_model(&shell.app, &shell.ui)
        });
        assert!(view_model.idle);
        assert_eq!(view_model.current_profile_id.as_ref(), "default");
        assert_eq!(
            view_model
                .profile_options
                .iter()
                .map(|option| option.id.as_ref())
                .collect::<Vec<_>>(),
            ["default", "exact-reviewer", "review-team"]
        );
        assert!(view_model.profile_options[0].selectable);
        assert!(view_model.profile_options[1].selectable);
        assert!(!view_model.profile_options[2].selectable);

        let selector = cx
            .debug_bounds("desktop-header-profile-selector")
            .expect("the idle header exposes the profile selector");
        cx.simulate_click(selector.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        choose_popup_item(cx, 1);

        assert_eq!(
            runtime_harness.drain_selections(),
            [(
                desktop::runtime::DesktopRuntimeCommandKind::SelectSessionProfile,
                DesktopRuntimeOwnerTarget::home(),
                "exact-reviewer".into(),
                None,
            )]
        );
    }

    #[test]
    fn input_render_latency_uses_latest_change_and_consumes_it_once() {
        let probe = InputRenderLatencyProbe::default();
        let started = Instant::now();
        probe.mark_changed_at(started);
        probe.mark_changed_at(started + Duration::from_millis(3));

        assert_eq!(
            probe.observe_render_at(started + Duration::from_millis(8)),
            Some(Duration::from_millis(5))
        );
        assert_eq!(probe.last_observed(), Some(Duration::from_millis(5)));
        assert_eq!(
            probe.observe_render_at(started + Duration::from_millis(9)),
            None
        );
    }

    #[gpui::test]
    fn composer_pane_render_consumes_pending_input_latency(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            visual_test_projection(),
        );
        cx.run_until_parked();

        let changed_at = Instant::now();
        shell.update(cx, |shell, cx| {
            shell.views.composer_pane.update(cx, |pane, cx| {
                pane.latency_probe().mark_changed_at(changed_at);
                cx.notify();
            });
        });
        cx.run_until_parked();

        assert!(shell.read_with(cx, |shell, cx| {
            let pane = shell.views.composer_pane.read(cx);
            pane.latency_probe().pending_is_empty()
                && pane
                    .latency_probe()
                    .last_observed()
                    .is_some_and(|latency| latency <= changed_at.elapsed())
        }));
    }

    #[gpui::test]
    fn composer_auto_grows_from_one_line_to_its_bounded_maximum(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            visual_test_projection(),
        );
        cx.simulate_resize(size(px(700.), px(800.)));
        cx.run_until_parked();

        shell.update(cx, |shell, cx| {
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .edit("one compact line");
            shell.app.workspaces.active_mut().composer_needs_sync = true;
            cx.notify();
        });
        settle_visual_measurements(cx);
        let one_line_height = f32::from(
            cx.debug_bounds("desktop-composer-panel")
                .expect("one-line Composer is laid out")
                .size
                .height,
        );
        let compact_content_height = f32::from(
            cx.debug_bounds("desktop-composer-content")
                .expect("compact Composer content is laid out")
                .size
                .height,
        );
        assert!(
            (48. ..=56.).contains(&compact_content_height),
            "empty and one-line Composer content stays compact: {compact_content_height}"
        );

        shell.update(cx, |shell, cx| {
            shell.app.workspaces.active_mut().composer.edit(
                (1..=20)
                    .map(|line| format!("composer line {line} 中文 🙂"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            shell.app.workspaces.active_mut().composer_needs_sync = true;
            cx.notify();
        });
        settle_visual_measurements(cx);
        let maximum_height = f32::from(
            cx.debug_bounds("desktop-composer-panel")
                .expect("maximum-height Composer is laid out")
                .size
                .height,
        );

        shell.update(cx, |shell, cx| {
            shell.app.workspaces.active_mut().composer.edit(
                (1..=40)
                    .map(|line| format!("saturation line {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            shell.app.workspaces.active_mut().composer_needs_sync = true;
            cx.notify();
        });
        settle_visual_measurements(cx);
        let saturated_height = f32::from(
            cx.debug_bounds("desktop-composer-panel")
                .expect("saturated Composer is laid out")
                .size
                .height,
        );

        assert!(
            maximum_height > one_line_height,
            "Composer must grow beyond its one-line geometry: one={one_line_height}, max={maximum_height}"
        );
        assert!(
            maximum_height <= COMPOSER_MAX_HEIGHT as f32 + 1.,
            "Composer auto-grow must remain bounded: {maximum_height}"
        );
        assert!(
            (saturated_height - maximum_height).abs() <= 1.,
            "content beyond the eight-row auto-grow maximum must not keep expanding the Composer: twenty={maximum_height}, forty={saturated_height}"
        );
    }

    #[gpui::test]
    fn project_directory_control_is_scoped_locked_pending_and_narrow_safe(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (idle_shell, cx) = add_idle_visual_shell(cx);

        let idle_directory = idle_shell.read_with(cx, |shell, _| {
            composer_pane::view_model(shell.app.workspaces.active()).project_directory
        });
        assert_eq!(idle_directory.value.as_ref(), "无项目");
        assert_eq!(
            idle_directory.state,
            desktop_controls::DesktopProjectDirectoryState::Editable
        );

        for width in [1_300., 700.] {
            cx.simulate_resize(size(px(width), px(800.)));
            settle_visual_measurements(cx);
            let attachment = cx
                .debug_bounds("desktop-hit-add-composer-attachments")
                .expect("attachment action remains in the Composer bottom-left");
            let project = cx
                .debug_bounds("desktop-project-directory-control")
                .expect("project directory control remains in the Composer bottom-left");
            let submit = cx
                .debug_bounds("desktop-hit-submit-composer")
                .expect("submit action remains in the Composer bottom-right");
            assert!(attachment.right() <= project.left());
            assert!(project.right() <= submit.left());
            assert!(f32::from(project.size.width) <= 280.);
            assert_eq!(f32::from(project.size.height), 36.);
            assert_minimum_hit_target(cx, "desktop-hit-add-composer-attachments");
            assert_minimum_hit_target(cx, "desktop-hit-project-directory");
            assert_minimum_hit_target(cx, "desktop-hit-submit-composer");
        }

        let long_path =
            PathBuf::from("/工作区/这是一个需要被压缩但必须保留完整辅助信息的项目目录/evo");
        let mut session_snapshot = visual_test_snapshot();
        session_snapshot.project.cwd = long_path.clone();
        let session_projection = DesktopProjection::new(session_snapshot)
            .expect("long-path session fixture is a valid product projection");
        let (session_shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            session_projection,
        );
        let session_directory = session_shell.read_with(cx, |shell, _| {
            composer_pane::view_model(shell.app.workspaces.active()).project_directory
        });
        assert_eq!(
            session_directory.value.as_ref(),
            long_path.display().to_string()
        );
        assert_eq!(
            session_directory.state,
            desktop_controls::DesktopProjectDirectoryState::Locked
        );
        cx.simulate_resize(size(px(700.), px(800.)));
        settle_visual_measurements(cx);
        assert!(
            f32::from(
                cx.debug_bounds("desktop-project-directory-control")
                    .expect("locked long-path pill remains visible")
                    .size
                    .width
            ) <= 280.
        );

        let (pending_shell, cx) = add_idle_visual_shell(cx);
        pending_shell.update(cx, |shell, cx| {
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .edit("submit against the frozen project target");
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .begin_submit(401, ComposerSubmissionKind::Prompt)
                .expect("Home draft enters pending admission");
            shell.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
        });
        assert_eq!(
            pending_shell.read_with(cx, |shell, _| {
                composer_pane::view_model(shell.app.workspaces.active())
                    .project_directory
                    .state
            }),
            desktop_controls::DesktopProjectDirectoryState::Pending
        );
    }

    #[gpui::test]
    fn composer_running_authorization_and_rejection_fit_at_narrow_width(cx: &mut TestAppContext) {
        initialize_visual_test(cx);

        let mut running_snapshot = visual_test_snapshot();
        running_snapshot.session.active_operation = Some("operation-running-composer".into());
        let running_projection = DesktopProjection::new(running_snapshot)
            .expect("running Composer fixture is a valid product projection");
        let (_, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            running_projection,
        );
        cx.simulate_resize(size(px(700.), px(800.)));
        settle_visual_measurements(cx);
        assert_composer_regions_do_not_overlap(cx, false);
        let abort = cx
            .debug_bounds("desktop-hit-abort-operation")
            .expect("running operation exposes the critical Abort action");
        assert_eq!(f32::from(abort.size.height), 40.);
        let selector = cx
            .debug_bounds("desktop-composer-running-mode-selector")
            .expect("running Composer exposes one mode selector");
        let submit = cx
            .debug_bounds("desktop-hit-submit-running-composer")
            .expect("running Composer exposes one primary submit action");
        assert!(selector.right() <= submit.left());
        assert!(
            (f32::from(selector.bottom() - submit.bottom())).abs() <= 2.1,
            "32 px selector and 36 px submit remain center-aligned: selector={selector:?}, submit={submit:?}"
        );
        assert!(cx.debug_bounds("desktop-hit-submit-composer").is_none());

        let mut authorization_snapshot = visual_test_snapshot();
        authorization_snapshot
            .session
            .pending_authorizations
            .push(ToolAuthorizationRequest {
                authorization_id: "authorization-composer-layout".into(),
                operation_id: "operation-composer-layout".into(),
                turn_id: "turn-composer-layout".into(),
                tool_call_id: "tool-composer-layout".into(),
                tool_name: "bash".into(),
                risk: ToolAuthorizationRisk::ShellExecution,
                scope: ToolAuthorizationScope::Shell {
                    cwd: "/desktop-visual-test".into(),
                    command_fingerprint: "composer-layout-fingerprint".into(),
                },
                preview: ToolAuthorizationPreview {
                    summary: "Authorize the pending shell command".into(),
                    path: None,
                    command: Some("cargo check".into()),
                    cwd: Some("/desktop-visual-test".into()),
                    content_preview: None,
                },
                capability_generation: 0,
                requested_at: "2026-07-27T00:00:00Z".into(),
            });
        let authorization_projection = DesktopProjection::new(authorization_snapshot)
            .expect("authorization Composer fixture is a valid product projection");
        let (_, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            authorization_projection,
        );
        cx.simulate_resize(size(px(700.), px(800.)));
        settle_visual_measurements(cx);
        assert_composer_regions_do_not_overlap(cx, true);

        let (rejection_shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            visual_test_projection(),
        );
        rejection_shell.update(cx, |shell, cx| {
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .edit("retry this exact draft");
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .begin_submit(91, ComposerSubmissionKind::Prompt)
                .expect("test draft starts a pending submission");
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .rejected(
                    91,
                    "The submitted draft was rejected and remains available for editing.",
                )
                .expect("matching rejection is applied");
            shell.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
        });
        cx.simulate_resize(size(px(700.), px(800.)));
        settle_visual_measurements(cx);
        assert_composer_regions_do_not_overlap(cx, true);
    }

    fn assert_composer_regions_do_not_overlap(
        cx: &mut gpui::VisualTestContext,
        notice_expected: bool,
    ) {
        let panel = cx
            .debug_bounds("desktop-composer-panel")
            .expect("Composer panel is laid out");
        let input = cx
            .debug_bounds("desktop-composer-input-region")
            .expect("Composer input region is laid out");
        let actions = cx
            .debug_bounds("desktop-composer-actions")
            .expect("Composer action region is laid out");
        assert!(input.bottom() <= actions.top());
        assert!(input.left() >= panel.left() && input.right() <= panel.right());
        assert!(actions.left() >= panel.left() && actions.right() <= panel.right());
        match cx.debug_bounds("desktop-composer-state-notice") {
            Some(notice) if notice_expected => {
                assert!(notice.bottom() <= input.top());
                assert!(notice.left() >= panel.left() && notice.right() <= panel.right());
            }
            None if !notice_expected => {}
            notice => panic!("unexpected Composer notice state: {notice:?}"),
        }
    }

    #[gpui::test]
    #[ignore = "release performance gate"]
    fn desktop_release_gpui_headless_frame_and_input_replay(cx: &mut TestAppContext) {
        let _performance_guard = crate::allocation_probe::serial_guard();
        const SAMPLE_COUNT: usize = 200;
        const CPU_FRAME_BUDGET_MICROS: u128 = 16_700;
        const WINDOW_RSS_GROWTH_BUDGET: u64 = 64 * 1024 * 1024;

        initialize_visual_test(cx);
        let projection =
            visual_performance_projection(desktop::conversation::MAX_TRANSCRIPT_BLOCKS);
        let window_rss_before = crate::allocation_probe::resident_bytes();
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection,
        );
        cx.simulate_resize(size(px(1_300.), px(900.)));
        cx.run_until_parked();
        let window_rss_after = crate::allocation_probe::resident_bytes();
        let window_rss_growth = match (window_rss_before, window_rss_after) {
            (Some(before), Some(after)) => Some(after.saturating_sub(before)),
            _ => None,
        };

        let mut frame_samples = Vec::with_capacity(SAMPLE_COUNT);
        for _ in 0..SAMPLE_COUNT {
            let started = Instant::now();
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            frame_samples.push(started.elapsed().as_micros());
        }

        let mut input_roundtrip_samples = Vec::with_capacity(SAMPLE_COUNT);
        let mut input_to_render_samples = Vec::with_capacity(SAMPLE_COUNT);
        let keystroke = gpui::Keystroke::parse("a")
            .expect("the headless composer input keystroke remains valid");
        for _ in 0..SAMPLE_COUNT {
            shell.read_with(cx, |shell, cx| {
                shell
                    .views
                    .composer_pane
                    .read(cx)
                    .latency_probe()
                    .clear_last_observed();
            });
            let started = Instant::now();
            let dispatched =
                cx.update(|window, cx| window.dispatch_keystroke(keystroke.clone(), cx));
            assert!(dispatched, "headless composer accepts keyboard input");
            // The headless platform deliberately has no on_request_frame
            // callback. Dispatch through the real keyboard/InputEvent::Change
            // path, drain it, then force the requested ComposerPane frame.
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            input_roundtrip_samples.push(started.elapsed().as_micros());
            let observed = shell
                .read_with(cx, |shell, cx| {
                    shell
                        .views
                        .composer_pane
                        .read(cx)
                        .latency_probe()
                        .last_observed()
                })
                .expect("InputEvent::Change reaches the next ComposerPane render");
            input_to_render_samples.push(observed.as_micros());
        }

        let frame_p95_micros = test_percentile_95(&mut frame_samples);
        let input_roundtrip_p95_micros = test_percentile_95(&mut input_roundtrip_samples);
        let input_to_render_p95_micros = test_percentile_95(&mut input_to_render_samples);
        println!(
            "desktop_perf\tplatform={}\theadless_blocks={}\theadless_cpu_frame_p95_us={frame_p95_micros}\t\
             headless_input_roundtrip_p95_us={input_roundtrip_p95_micros}\t\
             input_change_to_render_p95_us={input_to_render_p95_micros}\t\
             window_rss_supported={}\twindow_rss_before_bytes={}\twindow_rss_after_bytes={}\t\
             window_rss_growth_bytes={}",
            std::env::consts::OS,
            desktop::conversation::MAX_TRANSCRIPT_BLOCKS,
            window_rss_before.is_some() && window_rss_after.is_some(),
            window_rss_before.unwrap_or_default(),
            window_rss_after.unwrap_or_default(),
            window_rss_growth.unwrap_or_default()
        );
        assert!(
            frame_p95_micros <= CPU_FRAME_BUDGET_MICROS,
            "headless full-tree CPU frame P95 exceeded one frame: {frame_p95_micros} us"
        );
        assert!(
            input_roundtrip_p95_micros <= CPU_FRAME_BUDGET_MICROS,
            "headless Composer roundtrip P95 exceeded one frame: \
             {input_roundtrip_p95_micros} us"
        );
        assert!(
            input_to_render_p95_micros <= CPU_FRAME_BUDGET_MICROS,
            "Composer change-to-render P95 exceeded one frame: {input_to_render_p95_micros} us"
        );
        if let Some(window_rss_growth) = window_rss_growth {
            assert!(
                window_rss_growth <= WINDOW_RSS_GROWTH_BUDGET,
                "10k NativeShell window RSS growth exceeded 64 MiB: {window_rss_growth} bytes"
            );
        }
    }

    #[gpui::test]
    #[ignore = "release performance gate"]
    fn desktop_release_gpui_markdown_parser_matrix(cx: &mut TestAppContext) {
        let _performance_guard = crate::allocation_probe::serial_guard();
        const SAMPLE_COUNT: usize = 20;
        const MARKDOWN_PARSE_BUDGET_MICROS: u128 = 150_000;

        initialize_visual_test(cx);
        let table_row = format!(
            "| {} |\n",
            (0..32).map(|_| "cell").collect::<Vec<_>>().join(" | ")
        );
        let content_cases = [
            (
                "markdown_256k",
                format!(
                    "# heading\n\n{}",
                    "paragraph **bold** `code`\n".repeat(10_000)
                ),
            ),
            ("reasoning_512k", "reasoning step 中文 🧠\n".repeat(24_000)),
            (
                "bash_output",
                format!("```text\n{}\n```", "build output\n".repeat(80_000)),
            ),
            ("table", table_row.repeat(1_000)),
            (
                "code_cjk_emoji",
                format!(
                    "```rust\n{}\n```\n{}",
                    "fn main() {}\n".repeat(12_000),
                    "中文🙂🚀\n".repeat(12_000)
                ),
            ),
        ];

        for (name, payload) in content_cases {
            let preview = desktop::conversation::bounded_markdown_preview(&payload);
            let bounded_bytes = preview.text.len();
            let mut samples = Vec::with_capacity(SAMPLE_COUNT);
            for _ in 0..SAMPLE_COUNT {
                let source = preview.text.clone();
                let started = Instant::now();
                let state =
                    cx.update(|cx| cx.new(move |cx| TextViewState::markdown(source.as_str(), cx)));
                std::hint::black_box(state);
                samples.push(started.elapsed().as_micros());
            }
            let parse_p95_micros = test_percentile_95(&mut samples);
            println!(
                "desktop_perf\tcontent={name}\tinput_bytes={}\tbounded_bytes={bounded_bytes}\t\
                 markdown_parser_p95_us={parse_p95_micros}",
                payload.len()
            );
            assert!(
                parse_p95_micros <= MARKDOWN_PARSE_BUDGET_MICROS,
                "{name} GPUI Markdown parser P95 exceeded 150ms: {parse_p95_micros} us"
            );
        }
    }

    #[gpui::test]
    fn native_shell_markdown_code_action_copies_exact_block(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let mut snapshot = visual_test_snapshot();
        snapshot.transcript.items.push(
            coding_agent::api::view::CodingAgentSessionTranscriptItem::Assistant {
                id: "message-with-code".into(),
                text: "Before\n\n```rust\nfn main() { println!(\"exact\"); }\n```\n\nAfter".into(),
                thinking: String::new(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: None,
            },
        );
        let projection = DesktopProjection::new(snapshot)
            .expect("code-copy visual fixture is a valid product projection");
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection,
        );
        cx.run_until_parked();
        cx.refresh()
            .expect("final Markdown renders in the first refreshed frame");
        cx.run_until_parked();
        let notice_before_copy = shell.read_with(cx, |shell, _| {
            shell.app.workspaces.active().preference_notice.clone()
        });

        let bounds = cx
            .debug_bounds("desktop-copy-markdown-code")
            .expect("final Markdown code block exposes a copy action");
        assert_minimum_hit_target(cx, "desktop-copy-markdown-code");
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("fn main() { println!(\"exact\"); }".into())
        );
        assert!(
            cx.debug_bounds("desktop-conversation-copy-announcement")
                .is_some(),
            "Copy feedback is announced near the conversation instead of occupying the status bar"
        );
        assert_eq!(
            shell.read_with(cx, |shell, _| shell
                .app
                .workspaces
                .active()
                .preference_notice
                .clone()),
            notice_before_copy,
            "Copy feedback must not replace a persistent runtime or preference notice"
        );
        cx.executor()
            .advance_clock(Duration::from_secs(2) + Duration::from_millis(1));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("desktop-conversation-copy-announcement")
                .is_none(),
            "Copy announcement expires instead of becoming persistent chrome"
        );
    }

    #[gpui::test]
    fn native_shell_command_palette_smoke_uses_modal_focus_and_restores_it(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        cx.dispatch_action(OpenCommandPalette);
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.active_modal),
            Some(DesktopModalKind::CommandPalette)
        );
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.focus.active()),
            FocusTarget::Modal
        );
        cx.dispatch_action(EscapeHierarchy);
        cx.run_until_parked();
        assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_modal), None);
        assert!(cx.debug_bounds("desktop-authorization-actions").is_none());
        assert_ne!(
            shell.read_with(cx, |shell, _| shell.ui.focus.active()),
            FocusTarget::Modal
        );
    }

    #[gpui::test]
    fn authorization_modal_preempts_the_drawer_and_restores_its_root_focus_owner(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            visual_test_projection(),
        );
        cx.simulate_resize(size(px(1_000.), px(900.)));
        cx.run_until_parked();

        cx.dispatch_action(ToggleInspectorPanel);
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.active_drawer),
            Some(CenterDrawerKind::Inspector)
        );
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.focus.active()),
            FocusTarget::Composer,
            "drawer focus remains independent from the logical root focus owner"
        );

        let mut authorization_snapshot = visual_test_snapshot();
        authorization_snapshot
            .session
            .pending_authorizations
            .push(ToolAuthorizationRequest {
                authorization_id: "authorization-drawer-preemption".into(),
                operation_id: "operation-drawer-preemption".into(),
                turn_id: "turn-drawer-preemption".into(),
                tool_call_id: "tool-drawer-preemption".into(),
                tool_name: "bash".into(),
                risk: ToolAuthorizationRisk::ShellExecution,
                scope: ToolAuthorizationScope::Shell {
                    cwd: "/desktop-visual-test".into(),
                    command_fingerprint: "drawer-preemption-fingerprint".into(),
                },
                preview: ToolAuthorizationPreview {
                    summary: "Authorize after opening the Inspector drawer".into(),
                    path: None,
                    command: Some("true".into()),
                    cwd: Some("/desktop-visual-test".into()),
                    content_preview: None,
                },
                capability_generation: 0,
                requested_at: "2026-07-30T00:00:00Z".into(),
            });
        let authorization_projection = DesktopProjection::new(authorization_snapshot)
            .expect("authorization drawer fixture is a valid product projection");
        shell.update(cx, |shell, cx| {
            shell.app.workspaces.active_mut().projection = Some(authorization_projection);
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_drawer), None);
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.active_modal),
            Some(DesktopModalKind::Authorization)
        );
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.focus.active()),
            FocusTarget::Modal
        );
        assert!(
            cx.debug_bounds("desktop-authorization-actions").is_some(),
            "the authorization projection mounts the real root modal after closing the drawer"
        );

        shell.update(cx, |shell, cx| {
            shell.app.workspaces.active_mut().projection = Some(visual_test_projection());
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(shell.read_with(cx, |shell, _| shell.ui.active_modal), None);
        assert!(cx.debug_bounds("desktop-authorization-actions").is_none());
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.focus.active()),
            FocusTarget::Composer
        );
    }

    #[gpui::test]
    fn native_shell_authorization_smoke_traps_focus_and_submits_a_typed_decision(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let mut snapshot = visual_test_snapshot();
        snapshot
            .session
            .pending_authorizations
            .push(ToolAuthorizationRequest {
                authorization_id: "authorization-visual-test".into(),
                operation_id: "operation-visual-test".into(),
                turn_id: "turn-visual-test".into(),
                tool_call_id: "tool-call-visual-test".into(),
                tool_name: "bash".into(),
                risk: ToolAuthorizationRisk::ShellExecution,
                scope: ToolAuthorizationScope::Shell {
                    cwd: "/desktop-visual-test".into(),
                    command_fingerprint: "command-fingerprint".into(),
                },
                preview: ToolAuthorizationPreview {
                    summary: "Run a visual-test command".into(),
                    path: None,
                    command: Some("true".into()),
                    cwd: Some("/desktop-visual-test".into()),
                    content_preview: None,
                },
                capability_generation: 0,
                requested_at: "2026-07-27T00:00:00Z".into(),
            });
        let projection = DesktopProjection::new(snapshot)
            .expect("authorization visual fixture is a valid product projection");
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, projection);
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.active_modal),
            Some(DesktopModalKind::Authorization)
        );
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.ui.focus.active()),
            FocusTarget::Modal
        );
        let term_left = f32::from(
            cx.debug_bounds("desktop-authorization-term-operation")
                .expect("authorization operation term is visible")
                .left(),
        );
        let value_left = f32::from(
            cx.debug_bounds("desktop-authorization-value-operation")
                .expect("authorization operation value is visible")
                .left(),
        );
        for (term, term_selector, value_selector) in [
            (
                "tool",
                "desktop-authorization-term-tool",
                "desktop-authorization-value-tool",
            ),
            (
                "risk",
                "desktop-authorization-term-risk",
                "desktop-authorization-value-risk",
            ),
            (
                "scope",
                "desktop-authorization-term-scope",
                "desktop-authorization-value-scope",
            ),
            (
                "cwd",
                "desktop-authorization-term-cwd",
                "desktop-authorization-value-cwd",
            ),
            (
                "command",
                "desktop-authorization-term-command",
                "desktop-authorization-value-command",
            ),
        ] {
            let term_bounds = cx
                .debug_bounds(term_selector)
                .unwrap_or_else(|| panic!("authorization {term} term is visible"));
            let value_bounds = cx
                .debug_bounds(value_selector)
                .unwrap_or_else(|| panic!("authorization {term} value is visible"));
            assert_eq!(f32::from(term_bounds.left()), term_left);
            assert_eq!(f32::from(value_bounds.left()), value_left);
            assert!(term_bounds.right() <= value_bounds.left());
        }
        for selector in [
            "desktop-hit-deny-authorization",
            "desktop-hit-allow-authorization-once",
            "desktop-hit-allow-authorization-operation",
        ] {
            assert_minimum_hit_target(cx, selector);
            let bounds = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing critical action {selector}"));
            assert_eq!(f32::from(bounds.size.height), 40.);
        }

        cx.dispatch_action(AuthorizationDeny);
        cx.run_until_parked();
        assert!(
            runtime_harness
                .drain_command_kinds()
                .contains(&desktop::runtime::DesktopRuntimeCommandKind::DecideToolAuthorization)
        );
        assert!(shell.read_with(cx, |shell, _| {
            shell.active_command_contains_where(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::Authorization {
                        authorization_id,
                        ..
                    } if authorization_id == "authorization-visual-test"
                )
            })
        }));
    }

    #[gpui::test]
    fn native_shell_inspector_smoke_submits_recovery_and_file_review_commands(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let recovery = CodingAgentRecoveryPending {
            operation_id: "operation-recovery".into(),
            recovery_id: "recovery-visual-test".into(),
            operation_kind: Some("prompt".into()),
            record_version: 3,
            descriptor_revision: 2,
            capability_generation: Some(0),
            attempt_count: 1,
            last_attempt_at: Some("2026-07-27T00:00:00Z".into()),
            next_attempt_at: None,
        };
        let change = CodingAgentFileChangeSnapshot {
            path: "crates/desktop/src/app/native_shell.rs".into(),
            mutation_kind: "edit".into(),
            operation_id: "operation-file-review".into(),
            tool_call_id: Some("tool-call-file-review".into()),
            updated_sequence: 7,
            first_changed_line: Some(1),
            added_lines: Some(2),
            removed_lines: Some(1),
            diff: Some("@@ -1 +1 @@".into()),
        };
        let recovery_identity = DesktopRecoveryIdentity::from(&recovery);
        let review_request = CodingAgentFileReviewRequest::from(&change);
        let mut snapshot = visual_test_snapshot();
        snapshot.pending_recoveries.push(recovery);
        snapshot.session.context.changes.push(change);
        let projection = DesktopProjection::new(snapshot)
            .expect("inspector visual fixture is a valid product projection");
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell_with_preferences(
            cx,
            runtime,
            projection,
            visual_preferences_with_inspector(),
        );
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();
        let inspector = shell.read_with(cx, |shell, _| shell.views.inspector_pane.clone());

        inspector.update(cx, |_, cx| {
            cx.emit(InspectorPaneEvent::Recovery {
                identity: recovery_identity,
                action: DesktopRecoveryAction::Retry,
            });
        });
        cx.run_until_parked();
        assert!(
            runtime_harness
                .drain_command_kinds()
                .contains(&desktop::runtime::DesktopRuntimeCommandKind::RetryRecovery)
        );
        assert!(shell.read_with(cx, |shell, _| {
            shell.active_command_contains_where(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::Recovery {
                        recovery_id,
                        action: DesktopRecoveryAction::Retry,
                    } if recovery_id == "recovery-visual-test"
                )
            })
        }));

        let changed_file = cx
            .debug_bounds("desktop-changed-file-row-0")
            .expect("changed file is a full-row review action");
        assert!(
            f32::from(changed_file.size.height) >= 40.,
            "changed-file action row retains its stable height"
        );
        cx.simulate_click(changed_file.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(
            runtime_harness
                .drain_command_kinds()
                .contains(&desktop::runtime::DesktopRuntimeCommandKind::ReviewChangedFile)
        );
        assert!(shell.read_with(cx, |shell, _| {
            matches!(
                shell.app.workspaces.active().file_review.as_ref(),
                DesktopFileReviewState::Loading(request) if request == &review_request
            )
        }));

        shell.update(cx, |shell, cx| {
            shell.app.workspaces.active_mut().file_review =
                Arc::new(DesktopFileReviewState::Ready(
                    DesktopFileReviewDocument::from_product(CodingAgentFileReview {
                        change: review_request.change.clone(),
                        revision: review_request.revision,
                        display_path: review_request.change.path.clone(),
                        mutation_kind: "edit".into(),
                        content: "fn reviewed() {}\n".into(),
                        total_bytes: 17,
                        line_count: 1,
                        content_truncated: false,
                        diff: Some("@@ -0,0 +1 @@\n+fn reviewed() {}\n".into()),
                        diff_truncated: false,
                        first_changed_line: Some(1),
                        added_lines: Some(1),
                        removed_lines: Some(0),
                        external_editor_target: None,
                    }),
                ));
            shell.refresh_views(UiChangeSet::one(UiRegion::Inspector), cx);
        });
        cx.run_until_parked();
        for selector in [
            "desktop-hit-copy-review-path",
            "desktop-hit-copy-file-review",
            "desktop-hit-open-external-editor",
        ] {
            assert_minimum_hit_target(cx, selector);
        }
        inspector.update(cx, |_, cx| {
            cx.emit(InspectorPaneEvent::CopyFileReview);
        });
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell
                .app
                .workspaces
                .active()
                .preference_notice
                .clone()),
            Some("File review copied.".into())
        );
    }

    #[gpui::test]
    fn diagnostic_row_exposes_authoritative_recovery_action(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let recovery = CodingAgentRecoveryPending {
            operation_id: "operation-inline-recovery".into(),
            recovery_id: "recovery-inline-diagnostic".into(),
            operation_kind: Some("prompt".into()),
            record_version: 4,
            descriptor_revision: 2,
            capability_generation: Some(0),
            attempt_count: 1,
            last_attempt_at: Some("2026-07-27T00:00:00Z".into()),
            next_attempt_at: None,
        };
        let mut snapshot = visual_test_snapshot();
        snapshot.pending_recoveries.push(recovery);
        snapshot
            .transcript
            .items
            .push(CodingAgentSessionTranscriptItem::Diagnostic {
                message: "The operation requires recovery.".into(),
            });
        let projection = DesktopProjection::new(snapshot)
            .expect("inline recovery fixture is a valid product projection");
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, projection);
        settle_visual_measurements(cx);
        runtime_harness.drain_command_kinds();

        let recovery_actions = [
            "desktop-retry-diagnostic",
            "desktop-mark-failed-diagnostic",
            "desktop-abort-diagnostic",
        ]
        .map(|selector| {
            cx.debug_bounds(selector)
                .unwrap_or_else(|| panic!("Diagnostic exposes {selector} in place"))
        });
        let retry = recovery_actions[0];
        for bounds in recovery_actions {
            assert_eq!(f32::from(bounds.size.height), 40.);
        }
        cx.simulate_click(retry.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert!(
            runtime_harness
                .drain_command_kinds()
                .contains(&desktop::runtime::DesktopRuntimeCommandKind::RetryRecovery)
        );
        assert!(shell.read_with(cx, |shell, _| {
            shell.active_command_contains_where(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::Recovery {
                        recovery_id,
                        action: DesktopRecoveryAction::Retry,
                    } if recovery_id == "recovery-inline-diagnostic"
                )
            })
        }));
    }

    #[test]
    fn runtime_and_recovery_labels_are_exhaustive_and_typed() {
        assert_eq!(
            runtime_state_label(DesktopProjectionLifecycle::Running, false),
            "connected · idle"
        );
        assert_eq!(
            runtime_state_label(DesktopProjectionLifecycle::Running, true),
            "connected · active"
        );
        assert_eq!(
            runtime_state_label(DesktopProjectionLifecycle::NeedsResync, false),
            "resync required"
        );
        assert_eq!(
            runtime_state_label(DesktopProjectionLifecycle::Failed, false),
            "failed"
        );
        assert_eq!(
            runtime_state_label(DesktopProjectionLifecycle::Stopped, false),
            "stopped"
        );
        assert_eq!(
            recovery_status_label(DesktopRecoveryStatus::Pending),
            "pending"
        );
        assert_eq!(
            recovery_status_label(DesktopRecoveryStatus::Resolved),
            "resolved"
        );
        assert_eq!(
            recovery_status_label(DesktopRecoveryStatus::Recovered),
            "recovered"
        );
        assert_eq!(recovery_action_label(DesktopRecoveryAction::Retry), "retry");
        assert_eq!(
            recovery_action_label(DesktopRecoveryAction::MarkFailed),
            "mark-failed"
        );
        assert_eq!(recovery_action_label(DesktopRecoveryAction::Abort), "abort");
    }

    #[test]
    fn usage_cost_rejects_non_finite_or_negative_values() {
        assert_eq!(usage_cost_label(Some(1.25)), "$1.2500");
        assert_eq!(usage_cost_label(None), "—");
        assert_eq!(usage_cost_label(Some(f64::NAN)), "—");
        assert_eq!(usage_cost_label(Some(f64::INFINITY)), "—");
        assert_eq!(usage_cost_label(Some(-0.01)), "—");
    }

    #[test]
    fn thinking_menu_exactly_matches_the_product_capability() {
        let mut model = model_menu_fixture("reasoner", "Reasoner", "fixture", true, true);
        model.thinking_capability = CodingAgentThinkingCapability {
            supported: true,
            explicit_levels: vec![
                CodingAgentThinkingLevel::High,
                CodingAgentThinkingLevel::Low,
                CodingAgentThinkingLevel::High,
                CodingAgentThinkingLevel::Off,
            ],
            can_disable: false,
        };
        let options = conversation_header::thinking_menu(Some(&model));
        assert_eq!(
            options
                .iter()
                .map(|option| (option.selection, option.label))
                .collect::<Vec<_>>(),
            [
                (DesktopThinkingLevel::Default, "Auto"),
                (DesktopThinkingLevel::High, "High"),
                (DesktopThinkingLevel::Low, "Low"),
            ]
        );

        model.thinking_capability.can_disable = true;
        assert_eq!(
            conversation_header::thinking_menu(Some(&model))
                .iter()
                .map(|option| option.selection)
                .collect::<Vec<_>>(),
            [
                DesktopThinkingLevel::Default,
                DesktopThinkingLevel::Off,
                DesktopThinkingLevel::High,
                DesktopThinkingLevel::Low,
            ]
        );
        assert!(conversation_header::thinking_menu(None).is_empty());
        model.thinking_capability = CodingAgentThinkingCapability::default();
        assert!(conversation_header::thinking_menu(Some(&model)).is_empty());
    }

    #[gpui::test]
    fn unsupported_thinking_cannot_be_selected_outside_the_menu(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            visual_test_projection(),
        );
        shell.update(cx, |shell, cx| {
            let selected_model_id = shell
                .app
                .workspaces
                .active_mut()
                .project
                .selected_model_id
                .clone();
            let selected = shell
                .app
                .workspaces
                .active_mut()
                .project
                .models
                .iter_mut()
                .find(|model| model.id == selected_model_id)
                .expect("the fixture selected model exists");
            selected.thinking_capability = CodingAgentThinkingCapability {
                supported: true,
                explicit_levels: vec![CodingAgentThinkingLevel::Low],
                can_disable: false,
            };

            shell.select_thinking_level(DesktopThinkingLevel::High, cx);
            assert_eq!(
                shell.app.workspaces.active().thinking_selection,
                DesktopThinkingLevel::Default
            );
            shell.select_thinking_level(DesktopThinkingLevel::Off, cx);
            assert_eq!(
                shell.app.workspaces.active().thinking_selection,
                DesktopThinkingLevel::Default
            );
            shell.select_thinking_level(DesktopThinkingLevel::Low, cx);
            assert_eq!(
                shell.app.workspaces.active().thinking_selection,
                DesktopThinkingLevel::Low
            );
        });
    }

    #[gpui::test]
    fn model_switch_fallback_commits_auto_and_uses_a_header_local_hint(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        shell.update(cx, |shell, cx| {
            shell.select_thinking_level(DesktopThinkingLevel::High, cx);
            shell.submit_selection(
                DesktopRuntimeSelectionKind::Model,
                "adjacent-model".into(),
                cx,
            );
        });
        assert_eq!(
            runtime_harness.drain_selections(),
            [(
                desktop::runtime::DesktopRuntimeCommandKind::SelectModel,
                DesktopRuntimeOwnerTarget::session("desktop-visual-test"),
                "adjacent-model".into(),
                Some(CodingAgentThinkingLevel::High),
            )]
        );

        shell.update(cx, |shell, cx| {
            let mut snapshot = visual_test_snapshot();
            snapshot.project.selected_model_id = "adjacent-model".into();
            for model in &mut snapshot.project.models {
                model.selected = model.id == "adjacent-model";
            }
            shell.connection.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::SelectionChanged {
                    command_id: 1,
                    selection: DesktopRuntimeSelectionKind::Model,
                    thinking_level: None,
                    thinking_fallback: true,
                    metadata: desktop::runtime::DesktopRuntimeMetadataSnapshot {
                        project: snapshot.project,
                        session: Some(snapshot.session),
                    },
                },
            );
            shell.poll_runtime_for_test(cx);

            assert_eq!(
                shell.app.workspaces.active().thinking_selection,
                DesktopThinkingLevel::Default
            );
            assert_eq!(
                shell
                    .app
                    .preferences
                    .thinking_level_for_session("desktop-visual-test"),
                DesktopThinkingLevel::Default
            );
            assert_eq!(
                shell.app.workspaces.active().thinking_hint.as_deref(),
                Some("Thinking reset to Auto for the selected model.")
            );
            assert!(
                !shell
                    .app
                    .workspaces
                    .active()
                    .preference_notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("Thinking") || notice.contains("Auto"))
            );
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("desktop-header-thinking-selector")
                .is_none()
        );
        assert!(cx.debug_bounds("desktop-header-thinking-hint").is_some());
    }

    #[gpui::test]
    fn header_thinking_selector_submits_the_session_level_with_the_prompt(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        assert_eq!(
            shell.read_with(cx, |shell, _| shell
                .app
                .workspaces
                .active()
                .thinking_selection),
            DesktopThinkingLevel::Default
        );
        let selector = cx
            .debug_bounds("desktop-header-thinking-selector")
            .expect("the Header owns the session thinking selector");
        assert!(cx.debug_bounds("desktop-composer-thinking").is_none());

        cx.simulate_click(selector.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        choose_popup_item(cx, 5);

        assert_eq!(
            shell.read_with(cx, |shell, _| shell
                .app
                .workspaces
                .active()
                .thinking_selection),
            DesktopThinkingLevel::High
        );
        shell.update(cx, |shell, cx| {
            assert_eq!(
                shell
                    .app
                    .preferences
                    .thinking_level_for_session("desktop-visual-test"),
                DesktopThinkingLevel::High
            );
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .edit("use the session thinking level");
            shell.submit_composer(cx);
        });

        assert_eq!(
            runtime_harness.drain_prompts(),
            [(
                DesktopPromptTarget::existing("desktop-visual-test"),
                "use the session thinking level".into(),
                Some(CodingAgentThinkingLevel::High),
            )]
        );
    }

    #[gpui::test]
    fn composer_picker_attaches_bounded_paths_and_forwards_them_with_the_prompt(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        let add = cx
            .debug_bounds("desktop-hit-add-composer-attachments")
            .expect("composer bottom row exposes the attachment picker");
        cx.simulate_click(add.center(), gpui::Modifiers::default());
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|options| {
            assert!(options.files);
            assert!(!options.directories);
            assert!(options.multiple);
            Some(vec![
                PathBuf::from("/desktop-visual-test/screenshot.png"),
                PathBuf::from("/desktop-visual-test/notes.txt"),
            ])
        });
        cx.run_until_parked();

        shell.update(cx, |shell, cx| {
            assert_eq!(
                shell.app.workspaces.active_mut().composer_attachments.len(),
                2
            );
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .edit("inspect the selected files");
            shell.submit_composer(cx);
        });
        assert_eq!(
            runtime_harness.drain_prompt_attachments(),
            [(
                DesktopPromptTarget::existing("desktop-visual-test"),
                "inspect the selected files".into(),
                vec![
                    PathBuf::from("/desktop-visual-test/screenshot.png"),
                    PathBuf::from("/desktop-visual-test/notes.txt"),
                ],
            )]
        );
    }

    #[gpui::test]
    fn project_directory_menu_chooses_replaces_cancels_and_clears(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let first_root = tempfile::tempdir().expect("first picker fixture is created");
        let first = first_root.path().join("第一个项目");
        fs::create_dir(&first).expect("first project directory is created");
        let second_root = tempfile::tempdir().expect("second picker fixture is created");
        let second = second_root.path().join("替换后的项目");
        fs::create_dir(&second).expect("second project directory is created");
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        for selected in [&first, &second] {
            let selector = cx
                .debug_bounds("desktop-hit-project-directory")
                .expect("Home exposes the project directory selector");
            cx.simulate_click(selector.center(), gpui::Modifiers::default());
            cx.run_until_parked();
            choose_popup_item(cx, 0);
            assert!(cx.did_prompt_for_paths());
            let selected = selected.clone();
            cx.simulate_path_prompt_response(|options| {
                assert!(!options.files);
                assert!(options.directories);
                assert!(!options.multiple);
                assert_eq!(
                    options.prompt.as_deref(),
                    Some("Choose a project directory")
                );
                Some(vec![selected])
            });
            cx.run_until_parked();
        }

        assert_eq!(
            shell.read_with(cx, |shell, _| {
                shell
                    .app
                    .workspaces
                    .active()
                    .draft_workspace_selection
                    .clone()
            }),
            CodingAgentWorkspaceSelection::project(second.clone())
        );

        let selector = cx
            .debug_bounds("desktop-hit-project-directory")
            .expect("selected project remains replaceable");
        cx.simulate_click(selector.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        choose_popup_item(cx, 0);
        cx.simulate_path_prompt_response(|_| None);
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| {
                shell
                    .app
                    .workspaces
                    .active()
                    .draft_workspace_selection
                    .clone()
            }),
            CodingAgentWorkspaceSelection::project(second)
        );

        let selector = cx
            .debug_bounds("desktop-hit-project-directory")
            .expect("selected project exposes the clear option");
        cx.simulate_click(selector.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        choose_popup_item(cx, 1);
        assert!(matches!(
            shell.read_with(cx, |shell, _| {
                shell
                    .app
                    .workspaces
                    .active()
                    .draft_workspace_selection
                    .clone()
            }),
            CodingAgentWorkspaceSelection::Projectless { .. }
        ));
        assert_eq!(
            shell.read_with(cx, |shell, _| {
                composer_pane::view_model(shell.app.workspaces.active())
                    .project_directory
                    .value
            }),
            Arc::<str>::from("无项目")
        );
        assert_eq!(
            runtime_harness.drain_command_kinds(),
            [],
            "project selection remains client-local until prompt admission"
        );
    }

    #[gpui::test]
    fn project_directory_picker_failures_are_bounded_and_do_not_replace_selection(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, cx| {
            let original = PathBuf::from("/kept/project");
            assert!(set_project_directory_for_test(shell, original.clone()));
            apply_picker_result_for_test(
                shell,
                DesktopPickerKind::ProjectDirectory,
                PlatformOutcome::Completed(vec![
                    PathBuf::from("/unexpected/one"),
                    PathBuf::from("/unexpected/two"),
                ]),
                cx,
            );
            assert_eq!(
                shell.app.workspaces.active().draft_workspace_selection,
                CodingAgentWorkspaceSelection::project(original.clone())
            );
            assert!(
                shell
                    .app
                    .workspaces
                    .active()
                    .preference_notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("more than one"))
            );

            apply_picker_result_for_test(
                shell,
                DesktopPickerKind::ProjectDirectory,
                PlatformOutcome::Failed("The directory picker could not be opened.".into()),
                cx,
            );
            assert_eq!(
                shell.app.workspaces.active().draft_workspace_selection,
                CodingAgentWorkspaceSelection::project(original)
            );
            assert_eq!(
                shell.app.workspaces.active().preference_notice.as_deref(),
                Some("The directory picker could not be opened.")
            );
        });
    }

    #[gpui::test]
    fn prompt_admission_clones_project_selection_and_blocks_late_mutation(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let selected = tempfile::tempdir().expect("selected project fixture is created");
        let replacement = tempfile::tempdir().expect("replacement project fixture is created");
        let selected_path = selected.path().to_path_buf();
        let replacement_path = replacement.path().to_path_buf();
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        shell.update(cx, |shell, cx| {
            assert!(set_project_directory_for_test(shell, selected_path.clone()));
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .edit("freeze this project target");
            shell.submit_composer(cx);
            assert!(!set_project_directory_for_test(shell, replacement_path));
            assert!(!shell.clear_project_directory(cx));
        });

        assert_eq!(
            runtime_harness.drain_prompts(),
            [(
                DesktopPromptTarget::new(
                    CodingAgentWorkspaceSelection::project(selected_path.clone()),
                    "test-model",
                    "default",
                ),
                "freeze this project target".into(),
                None,
            )]
        );
        assert_eq!(
            shell.read_with(cx, |shell, _| {
                shell
                    .app
                    .workspaces
                    .active()
                    .draft_workspace_selection
                    .clone()
            }),
            CodingAgentWorkspaceSelection::project(selected_path)
        );
    }

    #[gpui::test]
    fn deleted_selected_project_rejects_submit_but_retains_draft_and_selection(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let selected = tempfile::tempdir().expect("selected project fixture is created");
        let selected_path = selected.path().to_path_buf();
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        shell.update(cx, |shell, _cx| {
            assert!(set_project_directory_for_test(shell, selected_path.clone()));
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .edit("retain after project deletion");
        });
        drop(selected);
        shell.update(cx, |shell, cx| shell.submit_composer(cx));

        assert!(runtime_harness.drain_prompts().is_empty());
        shell.read_with(cx, |shell, _| {
            assert_eq!(
                shell.app.workspaces.active().composer.draft(),
                "retain after project deletion"
            );
            assert!(matches!(
                shell.app.workspaces.active().composer.admission(),
                ComposerAdmission::Idle
            ));
            assert!(shell.app.workspaces.active().composer.rejection().is_some());
            assert_eq!(
                shell.app.workspaces.active().draft_workspace_selection,
                CodingAgentWorkspaceSelection::project(selected_path)
            );
        });
    }

    #[gpui::test]
    fn accepted_first_prompt_locks_scope_and_new_conversation_resets_projectless(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let selected = tempfile::tempdir().expect("selected project fixture is created");
        let selected_path = selected.path().to_path_buf();
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, cx| {
            assert!(set_project_directory_for_test(shell, selected_path.clone()));
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .edit("create the selected project session");
            let intent = DesktopCommandIntent::Prompt;
            let command_id = shell
                .reserve_command(intent)
                .expect("the Home prompt fits the command ledger");
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .begin_submit(command_id, ComposerSubmissionKind::Prompt)
                .expect("the Home draft enters admission");
            let mut snapshot = visual_test_snapshot_for("selected-project-session");
            snapshot.project.cwd = selected_path.clone();
            snapshot.project.workspace = Some(
                CodingAgentWorkspaceSelection::project(selected_path.clone())
                    .resolve(&snapshot.project.global_config_dir)
                    .expect("the selected project resolves for the session fixture"),
            );
            shell.connection.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::PromptAcceptedWithSession {
                    command_id,
                    snapshot,
                },
            );
            assert!(shell.poll_runtime_for_test(cx));
            assert!(shell.app.workspaces.active().composer.draft().is_empty());
            assert_eq!(
                shell
                    .app
                    .workspaces
                    .active()
                    .composer
                    .submitted()
                    .map(|submitted| submitted.command_id),
                Some(command_id),
                "the admission snapshot is not a completed durable transcript"
            );
            assert!(shell.app.workspaces.active().composer.rejection().is_none());
            let directory =
                composer_pane::view_model(shell.app.workspaces.active()).project_directory;
            assert_eq!(
                directory.value.as_ref(),
                selected_path.display().to_string()
            );
            assert_eq!(
                directory.state,
                desktop_controls::DesktopProjectDirectoryState::Locked
            );
            assert!(!shell.clear_project_directory(cx));
        });

        let new_conversation = cx
            .debug_bounds("desktop-hit-new-conversation")
            .expect("the Sidebar exposes New conversation");
        cx.simulate_click(new_conversation.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        shell.read_with(cx, |shell, _| {
            assert!(shell.app.workspaces.active().projection.is_none());
            assert!(matches!(
                shell.app.workspaces.active().draft_workspace_selection,
                CodingAgentWorkspaceSelection::Projectless { .. }
            ));
            assert_eq!(
                composer_pane::view_model(shell.app.workspaces.active())
                    .project_directory
                    .value,
                Arc::<str>::from("无项目")
            );
        });
    }

    #[gpui::test]
    fn temporarily_opening_a_session_preserves_the_home_project_draft(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        let selected = PathBuf::from("/home/draft/project");
        shell.update(cx, |shell, _cx| {
            assert!(set_project_directory_for_test(shell, selected.clone()));
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .edit("keep the scoped Home draft");
            shell
                .app
                .workspaces
                .active_mut()
                .composer_attachments
                .push(PathBuf::from("/tmp/home-owner.txt"));
            shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .set_scroll_top_for_tests(17.0);
            let snapshot = visual_test_snapshot_for("temporary-history-session");
            let projection = DesktopProjection::new(snapshot.clone())
                .expect("history session fixture is a valid projection");
            let history = make_session_workspace(snapshot.project, Some(projection), None);
            insert_session_workspace(shell, "temporary-history-session", history);
            assert!(activate_session(shell, "temporary-history-session"));
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .edit("history draft");
            shell
                .app
                .workspaces
                .active_mut()
                .composer_attachments
                .push(PathBuf::from("/tmp/history-owner.txt"));
            shell
                .app
                .workspaces
                .active()
                .presentation
                .conversation_controller
                .set_scroll_top_for_tests(42.0);
            assert!(shell.app.workspaces.activate(&WorkspaceKey::Home));
            assert_eq!(
                shell.app.workspaces.active().composer.draft(),
                "keep the scoped Home draft"
            );
            assert_eq!(
                shell.app.workspaces.active().draft_workspace_selection,
                CodingAgentWorkspaceSelection::project(selected)
            );
            assert_eq!(
                shell.app.workspaces.active().composer_attachments,
                [PathBuf::from("/tmp/home-owner.txt")]
            );
            assert_eq!(
                shell
                    .app
                    .workspaces
                    .active()
                    .presentation
                    .conversation_controller
                    .scroll_top_for_tests(),
                17.0
            );
            assert!(activate_session(shell, "temporary-history-session"));
            assert_eq!(
                shell.app.workspaces.active().composer.draft(),
                "history draft"
            );
            assert_eq!(
                shell.app.workspaces.active().composer_attachments,
                [PathBuf::from("/tmp/history-owner.txt")]
            );
            assert_eq!(
                shell
                    .app
                    .workspaces
                    .active()
                    .presentation
                    .conversation_controller
                    .scroll_top_for_tests(),
                42.0
            );
        });
    }

    #[gpui::test]
    fn composer_rejects_attachment_overflow_without_changing_the_draft(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, cx| {
            shell
                .app
                .workspaces
                .active_mut()
                .composer
                .edit("retain this exact draft");
            apply_picker_result_for_test(
                shell,
                DesktopPickerKind::Attachments,
                PlatformOutcome::Completed(
                    (0..=MAX_PROMPT_ATTACHMENTS)
                        .map(|index| PathBuf::from(format!("/tmp/attachment-{index}.png")))
                        .collect(),
                ),
                cx,
            );
            assert!(
                shell
                    .app
                    .workspaces
                    .active()
                    .composer_attachments
                    .is_empty()
            );
            assert_eq!(
                shell.app.workspaces.active().composer.draft(),
                "retain this exact draft"
            );
            assert!(
                shell
                    .app
                    .workspaces
                    .active()
                    .preference_notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("more than 16 attachments"))
            );
        });
    }

    #[gpui::test]
    fn composer_disables_attachment_picker_for_a_model_without_image_support(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, cx| {
            shell.app.workspaces.active_mut().project.selected_model_id = "adjacent-model".into();
            shell.refresh_views(UiChangeSet::one(UiRegion::Composer), cx);
        });
        cx.run_until_parked();

        assert_eq!(
            shell.read_with(cx, |shell, _| composer_pane::attachment_disabled_reason(
                shell.app.workspaces.active()
            )),
            Some("Selected model does not support image attachments.")
        );
        let add = cx
            .debug_bounds("desktop-hit-add-composer-attachments")
            .expect("disabled attachment action remains visible with its reason");
        cx.simulate_click(add.center(), gpui::Modifiers::default());
        assert!(!cx.did_prompt_for_paths());
    }

    #[gpui::test]
    fn switching_workspaces_restores_each_persisted_thinking_level(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let snapshot_a = visual_test_snapshot_for("thinking-session-a");
        let projection_a = DesktopProjection::new(snapshot_a)
            .expect("thinking session A fixture is a valid projection");
        let mut preferences = DesktopPreferences::default();
        assert!(
            preferences
                .set_thinking_level_for_session("thinking-session-a", DesktopThinkingLevel::High)
        );
        assert!(
            preferences
                .set_thinking_level_for_session("thinking-session-b", DesktopThinkingLevel::Low)
        );
        let (shell, cx) = add_visual_shell_with_preferences(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_a,
            preferences,
        );
        cx.run_until_parked();

        shell.update(cx, |shell, cx| {
            assert_eq!(
                shell.app.workspaces.active().thinking_selection,
                DesktopThinkingLevel::High
            );
            let snapshot_b = visual_test_snapshot_for("thinking-session-b");
            let projection_b = DesktopProjection::new(snapshot_b.clone())
                .expect("thinking session B fixture is a valid projection");
            let thinking_b = shell
                .app
                .preferences
                .thinking_level_for_session("thinking-session-b");
            insert_session_workspace(
                shell,
                "thinking-session-b",
                session_workspace_with_thinking(
                    snapshot_b.project,
                    Some(projection_b),
                    None,
                    thinking_b,
                ),
            );

            assert!(activate_session(shell, "thinking-session-b"));
            assert_eq!(
                shell.app.workspaces.active().thinking_selection,
                DesktopThinkingLevel::Low
            );
            shell.select_thinking_level(DesktopThinkingLevel::XHigh, cx);
            assert_eq!(
                shell
                    .app
                    .preferences
                    .thinking_level_for_session("thinking-session-b"),
                DesktopThinkingLevel::XHigh
            );

            assert!(activate_session(shell, "thinking-session-a"));
            assert_eq!(
                shell.app.workspaces.active().thinking_selection,
                DesktopThinkingLevel::High
            );
            assert!(activate_session(shell, "thinking-session-b"));
            assert_eq!(
                shell.app.workspaces.active().thinking_selection,
                DesktopThinkingLevel::XHigh
            );
        });
    }

    #[gpui::test]
    fn hydration_restores_existing_thinking_but_new_sessions_inherit_home(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);

        shell.update(cx, |shell, _| {
            assert!(shell.app.preferences.set_thinking_level_for_session(
                "existing-thinking-session",
                DesktopThinkingLevel::Low,
            ));
            shell.app.workspaces.active_mut().thinking_selection = DesktopThinkingLevel::XHigh;
            let existing = visual_test_snapshot_for("existing-thinking-session");

            assert!(shell.app.install_hydrated_workspace(&existing, false, true));
            assert_eq!(
                shell.app.workspaces.active().thinking_selection,
                DesktopThinkingLevel::Low
            );
            assert_eq!(
                shell
                    .app
                    .preferences
                    .thinking_level_for_session("existing-thinking-session"),
                DesktopThinkingLevel::Low
            );

            assert!(shell.app.workspaces.activate(&WorkspaceKey::Home));
            shell.app.workspaces.active_mut().thinking_selection = DesktopThinkingLevel::Medium;
            let created = visual_test_snapshot_for("created-thinking-session");

            assert!(shell.app.install_hydrated_workspace(&created, true, true));
            assert_eq!(
                shell.app.workspaces.active().thinking_selection,
                DesktopThinkingLevel::Medium
            );
            assert_eq!(
                shell
                    .app
                    .preferences
                    .thinking_level_for_session("created-thinking-session"),
                DesktopThinkingLevel::Medium
            );
        });
    }

    #[test]
    fn composer_mode_and_draft_are_scoped_to_the_active_session() {
        let projection = visual_test_projection();
        let project = projection.project().clone();
        let mut session_a = make_session_workspace(project.clone(), Some(projection), None);
        let mut session_b = make_session_workspace(project, None, None);
        session_a.composer.edit("draft a");
        session_a.presentation.composer_running_mode = ComposerRunningMode::QueueNext;
        session_b.composer.edit("draft b");

        assert_eq!(session_a.composer.draft(), "draft a");
        assert_eq!(
            session_a
                .presentation
                .composer_running_mode
                .submission_kind(),
            ComposerSubmissionKind::FollowUp
        );
        assert_eq!(session_b.composer.draft(), "draft b");
        assert_eq!(
            session_b
                .presentation
                .composer_running_mode
                .submission_kind(),
            ComposerSubmissionKind::Steer
        );
    }

    #[test]
    fn inspector_section_selection_is_scoped_to_the_session() {
        let projection = visual_test_projection();
        let project = projection.project().clone();
        let mut session_a = make_session_workspace(project.clone(), Some(projection), None);
        let mut session_b = make_session_workspace(project, None, None);
        session_a.presentation.inspector_section = InspectorSection::Runtime;
        session_b.presentation.inspector_section = InspectorSection::Task;

        assert_eq!(
            session_a.presentation.inspector_section,
            InspectorSection::Runtime
        );
        assert_eq!(
            session_b.presentation.inspector_section,
            InspectorSection::Task
        );
    }

    #[test]
    fn conversation_rows_adapt_to_kind_content_and_reasoning() {
        let diagnostic = conversation_block_height(
            ConversationBlockKind::Diagnostic,
            "invalid terminal tool-call name",
            "",
            900,
        );
        let short_assistant = conversation_block_height(
            ConversationBlockKind::Assistant,
            "A concise answer.",
            "",
            900,
        );
        let reasoning_assistant = conversation_block_height(
            ConversationBlockKind::Assistant,
            "A concise answer.",
            "First inspect the runtime.\nThen verify the provider stream.",
            900,
        );
        let long_assistant = conversation_block_height(
            ConversationBlockKind::Assistant,
            &"long response ".repeat(1_000),
            &"reasoning ".repeat(1_000),
            520,
        );

        assert!(diagnostic < short_assistant);
        assert!(short_assistant < reasoning_assistant);
        assert!(long_assistant > TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT);
    }

    #[test]
    fn secondary_message_details_are_collapsed_by_default_and_height_aware() {
        let mut cache = ConversationRowRenderCache::default();
        let reasoning = "reasoning ".repeat(45);
        cache.begin_frame();
        let assistant = cache.resolve(
            ConversationRowRenderSource {
                item_key: ConversationItemKey::new(
                    "reasoning-test-session",
                    ConversationItemKind::Durable(ConversationBlockKind::Assistant),
                    "assistant:reasoning",
                ),
                source_revision: 1,
                title: Cow::Borrowed("Assistant"),
                text: "Final answer",
                detail: &reasoning,
                kind: ConversationBlockKind::Assistant,
                done: true,
                is_error: false,
                image_count: 0,
                reasoning_duration_millis: Some(2_430),
                truncated: false,
                durable: true,
            },
            900,
        );
        let collapsed = conversation_row_target_height(&assistant, &HashSet::new(), 900);
        let expanded_ids = HashSet::from([assistant.item_key.row_id().to_owned()]);
        let expanded = conversation_row_target_height(&assistant, &expanded_ids, 900);
        let narrow_expanded = conversation_row_target_height(&assistant, &expanded_ids, 480);
        assert!(collapsed < expanded);
        assert_eq!(expanded, assistant.estimated_height);
        assert_eq!(
            narrow_expanded,
            conversation_block_height(
                ConversationBlockKind::Assistant,
                &assistant.text,
                &assistant.detail,
                480,
            )
        );
        assert!(narrow_expanded > expanded);
    }

    #[test]
    fn interleaved_live_rows_keep_event_order_instead_of_sinking_tools_to_the_tail() {
        // One agent loop alternates assistant message and tool: A1 → T1 → A2.
        // Both fold onto independent queues, so rendering one queue after the
        // other dropped every running tool below the newest message, and shifted
        // it down again on each new message. The tool cards visibly jumped to the
        // bottom mid-turn and only snapped back when the durable transcript
        // replaced the live tail at the end of the operation.
        let live_event = |sequence: u64, payload: serde_json::Value| {
            serde_json::from_value::<coding_agent::api::event::CodingAgentProductEvent>(
                serde_json::json!({
                    "stream_id": "desktop-visual-test-stream",
                    "sequence": sequence,
                    "event": payload,
                    "operation_id": "operation-1",
                    "session_id": "desktop-visual-test",
                    "terminal_status": null,
                    "terminal_operation": null,
                    "durability": {"state": "live_only"},
                    "delivery_class": "data",
                }),
            )
            .expect("the live overlay fixture must deserialize")
        };
        let message_started = |sequence: u64, turn_id: &str, message_id: &str| {
            live_event(
                sequence,
                serde_json::json!({
                    "family": "message",
                    "payload": {
                        "kind": "started",
                        "operation_id": "operation-1",
                        "turn_id": turn_id,
                        "message_id": message_id,
                    },
                }),
            )
        };
        let tool_started = |sequence: u64, tool_call_id: &str| {
            live_event(
                sequence,
                serde_json::json!({
                    "family": "tool",
                    "payload": {
                        "kind": "started",
                        "operation_id": "operation-1",
                        "turn_id": "turn-1",
                        "tool_call_id": tool_call_id,
                        "name": "read",
                        "arguments_json": "{}",
                    },
                }),
            )
        };

        let mut projection = visual_test_projection();
        for event in [
            message_started(1, "turn-1", "message-1"),
            tool_started(2, "tool-1"),
            message_started(3, "turn-2", "message-2"),
        ] {
            assert!(
                projection
                    .apply(ProjectionEvent::Product(event))
                    .is_applied()
            );
        }

        let mut controller = ConversationController::default();
        let source = ConversationSource::new(&projection, None);
        controller.apply_projection_delta(true, None, 3);
        controller.prepare_rows(&source, 900);
        let row_ids = |controller: &ConversationController| {
            (0..controller.row_count())
                .map(|index| {
                    controller
                        .row_at(index)
                        .expect("every counted row is resolvable")
                        .item_key
                        .row_id()
                        .to_owned()
                })
                .collect::<Vec<_>>()
        };
        let expected = [
            "assistant:message-1".to_owned(),
            "tool:tool-1".to_owned(),
            "assistant:message-2".to_owned(),
        ];
        assert_eq!(row_ids(&controller), expected);

        // The incremental sequence path resolves the same order as the rebuild,
        // so a streaming delta cannot reshuffle the tail behind the rebuild's back.
        let mut streamed = projection.clone();
        assert!(
            streamed
                .apply(ProjectionEvent::Product(live_event(
                    4,
                    serde_json::json!({
                        "family": "message",
                        "payload": {
                            "kind": "delta",
                            "operation_id": "operation-1",
                            "turn_id": "turn-2",
                            "message_id": "message-2",
                            "text": "streaming",
                        },
                    }),
                )))
                .is_applied()
        );
        let streamed_source = ConversationSource::new(&streamed, None);
        controller.apply_projection_delta(
            false,
            Some(&desktop::projection::DesktopProjectionDelta {
                conversation: true,
                ..Default::default()
            }),
            4,
        );
        controller.prepare_rows(&streamed_source, 900);
        assert_eq!(row_ids(&controller), expected);
    }

    #[test]
    fn a_metadata_reload_mid_turn_keeps_the_streaming_rows_mounted() {
        // A metadata reload carries no transcript, so wiping the live tail here
        // unmounted the streaming assistant and running tool rows with nothing to
        // take their place until the operation finished and rehydrated.
        let mut projection = visual_test_projection();
        let started = serde_json::from_value::<coding_agent::api::event::CodingAgentProductEvent>(
            serde_json::json!({
                "stream_id": "desktop-visual-test-stream",
                "sequence": 1,
                "event": {
                    "family": "tool",
                    "payload": {
                        "kind": "started",
                        "operation_id": "operation-1",
                        "turn_id": "turn-1",
                        "tool_call_id": "tool-1",
                        "name": "read",
                        "arguments_json": "{}",
                    },
                },
                "operation_id": "operation-1",
                "session_id": "desktop-visual-test",
                "terminal_status": null,
                "terminal_operation": null,
                "durability": {"state": "live_only"},
                "delivery_class": "data",
            }),
        )
        .expect("the live overlay fixture must deserialize");
        assert!(
            projection
                .apply(ProjectionEvent::Product(started))
                .is_applied()
        );
        assert_eq!(projection.tools().len(), 1);

        let fixture = visual_test_snapshot();
        let mut session = projection.snapshot().clone();
        session.cursor = projection.cursor().clone();
        assert!(
            projection
                .apply(ProjectionEvent::Metadata(
                    desktop::runtime::DesktopRuntimeMetadataSnapshot {
                        project: fixture.project,
                        session: Some(session),
                    },
                ))
                .is_replaced()
        );

        assert_eq!(
            projection.tools().len(),
            1,
            "a transcript-less reload must not unmount the running tool row"
        );
        let source = ConversationSource::new(&projection, None);
        let mut controller = ConversationController::default();
        controller.apply_projection_delta(true, None, 1);
        controller.prepare_rows(&source, 900);
        assert_eq!(
            controller
                .row_at(0)
                .expect("the running tool row stays mounted")
                .item_key
                .row_id(),
            "tool:tool-1"
        );
    }

    #[test]
    fn streaming_deltas_do_not_collapse_a_measured_row_to_its_estimate() {
        // End-to-end guard over the composed path: every delta bumps the row's
        // content revision, and the rendered card is far taller than the row
        // estimate. Discarding the measurement on each revision made the row
        // oscillate between the two, once per throttle window.
        const MEASURED_HEIGHT: f32 = 2_400.;
        let streaming_projection = |text: &str| {
            let mut snapshot = visual_test_snapshot();
            snapshot.transcript.items = vec![CodingAgentSessionTranscriptItem::Assistant {
                id: "streaming-answer".into(),
                text: text.to_owned(),
                thinking: String::new(),
                images: Vec::new(),
                done: false,
                reasoning_duration_millis: None,
            }];
            DesktopProjection::new(snapshot)
                .expect("streaming fixture is a valid product projection")
        };

        let mut controller = ConversationController::default();
        let mut body = String::new();
        let mut resolved_heights = Vec::new();
        let mut row_keys = Vec::new();

        for delta in 0..4 {
            body.push_str("Another sentence of the streamed answer. ");
            let projection = streaming_projection(&body);
            let source = ConversationSource::new(&projection, None);
            controller.apply_projection_delta(true, None, 0);
            controller.prepare_rows(&source, 900);

            let row = controller.row_at(0).expect("the streaming row exists");
            assert!(
                !row.done,
                "delta {delta} must still present the row as streaming"
            );
            resolved_heights.push(controller.render_heights_for_tests().borrow()[0]);
            row_keys.push(row.item_key.clone());

            controller.submit_row_measurement(
                &source,
                &ConversationRowMeasurement {
                    item_key: row.item_key,
                    source_revision: row.source_revision,
                    width_bucket: row.width_bucket,
                    text_phase: row.text_phase,
                    details_expanded: false,
                    height: MEASURED_HEIGHT,
                },
            );
            // Outlast the height throttle so the next revision is free to commit,
            // which is exactly when the collapse used to become visible.
            std::thread::sleep(
                crate::conversation::STREAMING_ROW_HEIGHT_INTERVAL + Duration::from_millis(5),
            );
        }

        assert!(
            row_keys.windows(2).all(|pair| pair[0] == pair[1]),
            "a streaming row must keep one identity across deltas: {row_keys:?}"
        );
        assert!(
            resolved_heights[1..]
                .iter()
                .all(|height| (*height - MEASURED_HEIGHT).abs() < 0.5),
            "streaming deltas fell back to the row estimate: {resolved_heights:?}"
        );
    }

    /// The two properties the streaming rows need from the append path.
    ///
    /// A synchronous first parse gives the first layout real geometry; upstream
    /// keeps `set_text` synchronous precisely because an async first parse would
    /// leave `parsed_content` empty and let a `measure_all` list latch a ~0
    /// height. Appends must then (1) retain the previous parse until the new one
    /// lands, so a frame arriving mid-parse measures stale-but-valid geometry
    /// rather than nothing, and (2) actually accumulate onto what came before.
    ///
    /// Property (2) only holds with `patches/gpui-component/0001-*.patch`
    /// applied: upstream's `increment_update` returns early on its synchronous
    /// branch and never seeds the background accumulator, so the first
    /// `push_str` appends to nothing and *replaces* the document. This drives
    /// the real `on_prepaint` hook the rows measure with, drawing frames without
    /// draining the background parse.
    #[gpui::test]
    fn async_markdown_append_retains_and_accumulates(cx: &mut TestAppContext) {
        struct ProbeRoot;
        impl Render for ProbeRoot {
            fn render(
                &mut self,
                _: &mut gpui::Window,
                _: &mut gpui::Context<Self>,
            ) -> impl IntoElement {
                div()
            }
        }

        use gpui_component::{ElementExt as _, text::TextView};

        initialize_visual_test(cx);
        let (_, visual_cx) = cx.add_window_view(|_, _| ProbeRoot);
        visual_cx.run_until_parked();

        let body = "paragraph **bold** `code`\n\n".repeat(20);
        let state = visual_cx.update(|_, cx| cx.new(|cx| TextViewState::markdown(&body, cx)));

        fn measure(
            visual_cx: &mut gpui::VisualTestContext,
            state: &gpui::Entity<TextViewState>,
        ) -> f32 {
            let observed = Rc::new(RefCell::new(0.0f32));
            let sink = Rc::clone(&observed);
            let state = state.clone();
            visual_cx.draw(
                gpui::point(px(0.), px(0.)),
                size(px(900.), px(4_000.)),
                move |_, _| {
                    div().w(px(900.)).child(
                        div()
                            .w_full()
                            .on_prepaint(move |bounds, _, _| {
                                *sink.borrow_mut() = f32::from(bounds.size.height);
                            })
                            .child(TextView::new(&state)),
                    )
                },
            );
            *observed.borrow()
        }

        // The constructor's full-replace parse is synchronous, so the very first
        // frame already has real geometry.
        let initial = measure(visual_cx, &state);
        assert!(
            initial > 200.,
            "a synchronous first parse must produce real geometry: {initial}"
        );

        // A streaming delta. The parse is queued to the background and has not
        // been drained, so this frame is exactly the one upstream warns about.
        let appended = "paragraph **bold** `code`\n\n".repeat(5);
        visual_cx.update(|_, cx| {
            state.update(cx, |state, cx| state.push_str(&appended, cx));
        });
        let mid_parse = measure(visual_cx, &state);

        visual_cx.run_until_parked();
        let settled = measure(visual_cx, &state);

        println!("desktop_probe\tinitial={initial}\tmid_parse={mid_parse}\tsettled={settled}");
        assert_eq!(
            mid_parse, initial,
            "an appended row keeps its previous parse until the new one lands, so a \
             frame arriving mid-parse measures stale-but-valid geometry"
        );
        // Accumulation: the appended chunk extends the seeded document instead
        // of replacing it. Without the seed patch this collapses to just the
        // appended chunk, silently dropping everything streamed before it.
        let appended_height = settled - initial;
        assert!(
            appended_height > 0.,
            "the append must extend the document, not replace it: \
             {initial} -> {settled} (a drop here means the vendored patch is missing)"
        );
        assert!(
            (appended_height - initial / 4.).abs() < initial / 8.,
            "appending a quarter of the body should grow the row by about a \
             quarter: {initial} -> {settled}"
        );
    }

    #[test]
    fn detail_toggle_holds_its_row_anchor_while_following_latest() {
        let mut snapshot = visual_test_snapshot();
        snapshot.transcript.items = vec![
            CodingAgentSessionTranscriptItem::User {
                text: "Earlier user context".repeat(8),
            },
            CodingAgentSessionTranscriptItem::Assistant {
                id: "anchored-reasoning".into(),
                text: "A compact final answer.".into(),
                thinking: "reasoning line\n".repeat(32),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: Some(1_200),
            },
            CodingAgentSessionTranscriptItem::User {
                text: "Later content keeps the toggled row above the viewport".repeat(16),
            },
        ];
        let projection = DesktopProjection::new(snapshot)
            .expect("toggle anchor fixture is a valid product projection");
        let source = ConversationSource::new(&projection, None);
        let mut controller = ConversationController::default();
        controller.prepare_rows(&source, 600);

        let heights = controller.render_heights_for_tests();
        let heights = heights.borrow();
        let target_top = heights[0];
        let scroll_top = target_top + heights[1] + 48.;
        drop(heights);
        controller.set_scroll_top_for_tests(scroll_top);
        assert!(controller.follow_latest_enabled());
        let target_id = controller
            .row_at(1)
            .expect("reasoning row exists")
            .item_key
            .row_id()
            .to_owned();

        controller.toggle_details(&target_id);
        controller.prepare_rows(&source, 600);
        assert_eq!(controller.scroll_top_for_tests(), scroll_top);
        assert!(controller.expanded_details().contains(&target_id));

        let expanded_row = controller.row_at(1).expect("expanded reasoning row exists");
        let estimated_height = controller.render_heights_for_tests().borrow()[1];
        let outcome = controller.submit_row_measurement(
            &source,
            &ConversationRowMeasurement {
                item_key: expanded_row.item_key.clone(),
                source_revision: expanded_row.source_revision,
                width_bucket: 600,
                text_phase: expanded_row.text_phase,
                details_expanded: true,
                height: estimated_height + 96.,
            },
        );
        assert!(outcome.pane_dirty);
        assert_eq!(controller.scroll_top_for_tests(), scroll_top);
        assert_eq!(
            controller.render_heights_for_tests().borrow()[0],
            target_top
        );

        let second_outcome = controller.submit_row_measurement(
            &source,
            &ConversationRowMeasurement {
                item_key: expanded_row.item_key,
                source_revision: expanded_row.source_revision,
                width_bucket: 600,
                text_phase: expanded_row.text_phase,
                details_expanded: true,
                height: estimated_height + 160.,
            },
        );
        assert!(second_outcome.pane_dirty);
        assert_eq!(
            controller.scroll_top_for_tests(),
            scroll_top,
            "later measurements from the same expansion must keep its top anchor"
        );
    }

    #[test]
    fn conversation_bottom_distance_matches_negative_gpui_offsets() {
        assert_eq!(conversation_distance_to_bottom(0.0, 640.0), 640.0);
        assert_eq!(conversation_distance_to_bottom(-400.0, 640.0), 240.0);
        assert_eq!(conversation_distance_to_bottom(-640.0, 640.0), 0.0);
        assert_eq!(conversation_distance_to_bottom(-641.0, 640.0), 0.0);
        assert_eq!(conversation_distance_to_bottom(4.0, 0.0), 0.0);
    }

    #[test]
    fn single_measurement_compensates_the_exact_paused_anchor() {
        let heights = [100., 100., 100.];
        assert_eq!(
            compensate_scroll_top_for_single_row_height(&heights, 0, 140., 150.),
            190.
        );
        assert_eq!(
            compensate_scroll_top_for_single_row_height(&heights, 1, 40., 150.),
            140.
        );
        assert_eq!(
            compensate_scroll_top_for_single_row_height(&heights, 2, 180., 150.),
            150.
        );
    }

    #[test]
    fn keyboard_conversation_selection_is_bounded_and_predictable() {
        assert_eq!(adjacent_conversation_index(0, None, false), None);
        assert_eq!(adjacent_conversation_index(4, None, false), Some(0));
        assert_eq!(adjacent_conversation_index(4, None, true), Some(3));
        assert_eq!(adjacent_conversation_index(4, Some(2), false), Some(3));
        assert_eq!(adjacent_conversation_index(4, Some(3), false), Some(3));
        assert_eq!(adjacent_conversation_index(4, Some(1), true), Some(0));
        assert_eq!(adjacent_conversation_index(4, Some(0), true), Some(0));
        assert_eq!(adjacent_conversation_index(4, Some(99), false), Some(0));
    }

    #[test]
    fn gpui_accessibility_metadata_writes_real_accesskit_nodes() {
        let element = div()
            .id("accessibility-contract-probe")
            .role(Role::ListItem)
            .aria_label("Assistant message, streaming")
            .aria_description("Conversation item")
            .aria_selected(true)
            .aria_position_in_set(2)
            .aria_size_of_set(4);

        assert_eq!(gpui::Element::a11y_role(&element), Some(Role::ListItem));
        let mut node = gpui::accesskit::Node::new(Role::ListItem);
        gpui::Element::write_a11y_info(&element, &mut node);
        assert_eq!(node.label(), Some("Assistant message, streaming"));
        assert_eq!(node.description(), Some("Conversation item"));
        assert_eq!(node.is_selected(), Some(true));
        assert_eq!(node.position_in_set(), Some(2));
        assert_eq!(node.size_of_set(), Some(4));
    }

    #[test]
    fn conversation_kinds_have_distinct_leading_markers() {
        let theme = SemanticTheme::GEEK_DARK;
        let user = conversation_block_visual(ConversationBlockKind::User, false, theme);
        let assistant = conversation_block_visual(ConversationBlockKind::Assistant, false, theme);
        let tool = conversation_block_visual(ConversationBlockKind::Tool, false, theme);
        let failed_tool = conversation_block_visual(ConversationBlockKind::Tool, true, theme);
        let delegation = conversation_block_visual(ConversationBlockKind::Delegation, false, theme);
        let diagnostic = conversation_block_visual(ConversationBlockKind::Diagnostic, true, theme);

        assert!(user.align_right);
        assert_eq!(user.glyph, "");
        assert!(!assistant.align_right);
        assert_ne!(tool.accent, failed_tool.accent);
        assert_eq!(tool.accent, theme.muted_text);
        assert_eq!(failed_tool.accent, theme.danger);
        assert_eq!(diagnostic.accent, theme.danger);
        assert_ne!(assistant.glyph, tool.glyph);
        assert_ne!(tool.glyph, diagnostic.glyph);
        assert_eq!(delegation.accent, theme.accent);
    }

    #[gpui::test]
    fn conversation_selection_rail_preserves_card_geometry(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(CodingAgentSessionTranscriptItem::User {
                text: "Selection rail must preserve this row.".into(),
            }),
        );
        cx.simulate_resize(size(px(1_300.), px(900.)));
        settle_visual_measurements(cx);
        let card_before = cx
            .debug_bounds("conversation-last-card")
            .expect("the final conversation card is visible");
        shell.update(cx, |shell, cx| shell.select_adjacent_conversation(true, cx));
        settle_visual_measurements(cx);
        let rail = cx
            .debug_bounds("conversation-selected-rail")
            .expect("keyboard selection paints a dedicated rail");
        assert_eq!(f32::from(rail.size.width), CONVERSATION_RAIL_WIDTH);
        assert!(f32::from(rail.size.height) > 0.);
        assert_eq!(
            cx.debug_bounds("conversation-last-card"),
            Some(card_before),
            "the selection rail must not participate in card layout"
        );
    }

    #[gpui::test]
    fn conversation_track_centers_without_inspector_and_keeps_ai_copy_at_bottom_left(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (_, cx) = add_visual_shell_with_preferences(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(CodingAgentSessionTranscriptItem::Assistant {
                id: "centered-track".into(),
                text: "A short answer inside the centered transcript track.".into(),
                thinking: String::new(),
                images: Vec::new(),
                done: true,
                reasoning_duration_millis: None,
            }),
            DesktopPreferences {
                sessions_panel_visible: false,
                context_panel_visible: false,
                ..DesktopPreferences::default()
            },
        );
        cx.simulate_resize(size(px(1_600.), px(900.)));
        settle_visual_measurements(cx);

        let panel = cx
            .debug_bounds("desktop-conversation-panel")
            .expect("conversation panel is laid out");
        let track = cx
            .debug_bounds("conversation-last-track")
            .expect("conversation row exposes its centered content track");
        let card = cx
            .debug_bounds("conversation-last-card")
            .expect("Assistant card is laid out");
        let copy = cx
            .debug_bounds("desktop-copy-conversation-row")
            .expect("Assistant copy action is laid out");
        let composer = cx
            .debug_bounds("desktop-composer-panel")
            .expect("Composer is laid out");
        let left_margin = f32::from(track.left() - panel.left());
        let right_margin = f32::from(panel.right() - track.right());

        assert!(
            (left_margin - right_margin).abs() <= 1.,
            "hidden Inspector must leave equal transcript margins: panel={panel:?}, track={track:?}"
        );
        assert!(
            (f32::from(track.size.width) - CONVERSATION_CONTENT_MAX_WIDTH as f32).abs() <= 1.,
            "wide viewports must cap the centered transcript track"
        );
        assert_eq!(
            composer.left(),
            track.left(),
            "Composer and transcript share the same centered left edge"
        );
        assert_eq!(
            composer.right(),
            track.right(),
            "Composer and transcript share the same centered right edge"
        );
        assert!(
            (f32::from(card.size.width) - desktop::shell::ASSISTANT_MESSAGE_MAX_WIDTH as f32).abs()
                <= 1.,
            "Assistant content fills the bounded track interior"
        );
        assert!(
            f32::from(copy.left() - card.left()) <= 17. && copy.top() > card.top() + px(32.),
            "Assistant copy action belongs at the card's bottom-left: card={card:?}, copy={copy:?}"
        );
    }

    #[gpui::test]
    fn short_user_message_wraps_content_and_keeps_copy_outside_bottom_right(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (_, cx) = add_visual_shell_with_preferences(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(CodingAgentSessionTranscriptItem::User {
                text: "Short prompt".into(),
            }),
            DesktopPreferences {
                sessions_panel_visible: false,
                context_panel_visible: false,
                ..DesktopPreferences::default()
            },
        );
        cx.simulate_resize(size(px(1_600.), px(900.)));
        settle_visual_measurements(cx);

        let track = cx
            .debug_bounds("conversation-last-track")
            .expect("User row exposes its centered content track");
        let card = cx
            .debug_bounds("conversation-last-card")
            .expect("User card is laid out");
        let copy = cx
            .debug_bounds("desktop-copy-conversation-row")
            .expect("User copy action is laid out");
        let bubble = cx
            .debug_bounds("desktop-user-message-bubble")
            .expect("User message exposes its rounded background independently");

        assert!(
            f32::from(card.size.width) < 320.,
            "short User content should determine the bubble width: card={card:?}"
        );
        assert!(
            (f32::from(track.right() - card.right())
                - desktop::shell::DESKTOP_DESIGN_TOKENS.spacing.lg as f32)
                .abs()
                <= 1.,
            "User bubble remains right-aligned inside the centered track: track={track:?}, card={card:?}"
        );
        assert!(
            (f32::from(card.left() - bubble.left())).abs() <= 1.
                && (f32::from(card.right() - bubble.right())).abs() <= 1.,
            "the rounded background should span the User card independently: card={card:?}, bubble={bubble:?}"
        );
        assert!(
            copy.top() >= bubble.bottom() && f32::from(card.right() - copy.right()) <= 17.,
            "User copy action belongs outside the bubble at bottom-right: card={card:?}, bubble={bubble:?}, copy={copy:?}"
        );
        assert!(
            cx.debug_bounds("desktop-last-conversation-row-header")
                .is_none(),
            "User messages should not render a YOU identity label"
        );
    }

    #[gpui::test]
    fn long_user_message_stops_at_max_width_and_grows_vertically(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (_, cx) = add_visual_shell_with_preferences(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(CodingAgentSessionTranscriptItem::User {
                text: "A long prompt with enough words to wrap naturally. ".repeat(120),
            }),
            DesktopPreferences {
                sessions_panel_visible: false,
                context_panel_visible: false,
                ..DesktopPreferences::default()
            },
        );
        cx.simulate_resize(size(px(1_600.), px(900.)));
        settle_visual_measurements(cx);

        let card = cx
            .debug_bounds("conversation-last-card")
            .expect("long User card is laid out");
        assert!(
            (f32::from(card.size.width) - desktop::shell::USER_MESSAGE_MAX_WIDTH as f32).abs()
                <= 1.,
            "long User content must stop at the configured maximum width: card={card:?}"
        );
        assert!(
            f32::from(card.size.height) > 160.,
            "content beyond the maximum width must wrap and grow vertically: card={card:?}"
        );
    }

    #[test]
    fn conversation_focus_uses_the_existing_header_divider_without_panel_geometry() {
        let theme = SemanticTheme::GEEK_DARK;
        assert_eq!(conversation_focus_accent(false, theme), theme.divider);
        assert_eq!(conversation_focus_accent(true, theme), theme.accent);
    }

    #[test]
    fn streaming_only_projection_delta_does_not_dirty_native_shell_root() {
        let streaming = desktop::projection::DesktopProjectionDelta {
            cursor: true,
            conversation: true,
            tools: true,
            ..Default::default()
        };
        assert!(!UiChangeSet::for_projection(false, Some(&streaming)).contains(UiRegion::Root));
        assert!(UiChangeSet::for_projection(true, Some(&streaming)).contains(UiRegion::Root));

        let authorization = desktop::projection::DesktopProjectionDelta {
            authorizations: true,
            ..Default::default()
        };
        assert!(UiChangeSet::for_projection(false, Some(&authorization)).contains(UiRegion::Root));
    }

    #[test]
    fn streaming_only_projection_delta_does_not_dirty_inspector() {
        let mut streaming = desktop::projection::DesktopProjectionDelta {
            cursor: true,
            conversation: true,
            tools: true,
            ..Default::default()
        };
        assert!(
            !UiChangeSet::for_projection(false, Some(&streaming)).contains(UiRegion::Inspector)
        );

        streaming.diagnostics = true;
        assert!(UiChangeSet::for_projection(false, Some(&streaming)).contains(UiRegion::Inspector));
    }

    #[test]
    fn inspector_defaults_to_changes() {
        assert_eq!(InspectorSection::default(), InspectorSection::Changes);
    }

    #[test]
    fn usage_only_projection_delta_is_throttled_for_inspector() {
        let usage = desktop::projection::DesktopProjectionDelta {
            context: desktop::projection::ContextDirtyFlags::USAGE,
            ..Default::default()
        };
        let changes = UiChangeSet::for_projection(false, Some(&usage));
        assert!(changes.contains(UiRegion::InspectorTelemetry));
        assert!(!changes.contains(UiRegion::Inspector));

        let start = Instant::now();
        assert_eq!(
            inspector_telemetry_refresh_delay(None, start),
            Duration::ZERO
        );
        assert_eq!(
            inspector_telemetry_refresh_delay(Some(start), start + Duration::from_millis(50)),
            Duration::from_millis(200)
        );
        assert_eq!(
            inspector_telemetry_refresh_delay(Some(start), start + Duration::from_millis(250)),
            Duration::ZERO
        );
    }

    #[test]
    fn streaming_only_projection_delta_does_not_dirty_conversation_header() {
        let mut streaming = desktop::projection::DesktopProjectionDelta {
            cursor: true,
            conversation: true,
            tools: true,
            ..Default::default()
        };
        assert!(
            !UiChangeSet::for_projection(false, Some(&streaming))
                .contains(UiRegion::ConversationHeader)
        );

        streaming.lifecycle = true;
        assert!(
            UiChangeSet::for_projection(false, Some(&streaming))
                .contains(UiRegion::ConversationHeader)
        );
    }

    #[test]
    fn streaming_only_projection_delta_does_not_dirty_root_modal_host() {
        let mut streaming = desktop::projection::DesktopProjectionDelta {
            cursor: true,
            conversation: true,
            tools: true,
            ..Default::default()
        };
        assert!(!UiChangeSet::for_projection(false, Some(&streaming)).contains(UiRegion::Modal));

        streaming.authorizations = true;
        assert!(UiChangeSet::for_projection(false, Some(&streaming)).contains(UiRegion::Modal));
    }

    #[test]
    fn indexed_row_update_accepts_non_clone_history_and_changes_one_slot() {
        #[derive(Debug, PartialEq, Eq)]
        struct NonClone(usize);

        let mut rows = (0..=10_000).map(NonClone).collect::<Vec<_>>();
        let capacity = rows.capacity();
        let index = upsert_indexed_item(&mut rows, Some(10_000), 10_000, NonClone(42));
        assert_eq!(index, 10_000);
        assert_eq!(rows.len(), 10_001);
        assert_eq!(rows[0], NonClone(0));
        assert_eq!(rows[9_999], NonClone(9_999));
        assert_eq!(rows[10_000], NonClone(42));
        assert_eq!(rows.capacity(), capacity);

        let append_index = rows.len();
        let appended = upsert_indexed_item(&mut rows, None, append_index, NonClone(43));
        assert_eq!(appended, 10_001);
        assert_eq!(rows[10_001], NonClone(43));
    }

    #[test]
    fn runtime_error_notices_do_not_expose_secret_bodies() {
        const SECRET: &str = "desktop-secret-canary";
        let notice = safe_runtime_rejection_notice(
            desktop::runtime::DesktopRuntimeCommandKind::DecideToolAuthorization,
            "authorization_not_pending",
        );
        assert!(!notice.contains(SECRET));
    }
}
pub(crate) mod center_drawer_host;
pub(crate) mod center_navigation;
mod command_adapter;
mod commands;
pub(crate) mod composer_pane;
mod conversation_adapter;
mod conversation_controller;
pub(crate) mod conversation_header;
mod conversation_layout_adapter;
pub(crate) mod conversation_pane;
mod desktop_controls;
mod desktop_style;
mod evo_brand;
pub(crate) mod home_pane;
pub(crate) mod inspector_pane;
mod intent;
mod layout_adapter;
mod overlay_adapter;
mod platform_update;
mod project_catalog_controller;
mod review_adapter;
mod root_actions;
pub(crate) mod root_modal_host;
mod root_view;
mod runtime_adapter;
pub(crate) mod sessions_pane;
pub(crate) mod skills_pane;
mod streaming_text;
pub(crate) mod toast_host;

use center_drawer_host::{CenterDrawerHost, CenterDrawerHostEvent, CenterDrawerKind};
use center_navigation::{CenterNavigationTarget, CenterSurface};
#[cfg(test)]
use composer_pane::InputRenderLatencyProbe;
use composer_pane::{ComposerPane, ComposerPaneEvent};
use conversation_controller::{
    ConversationController, ConversationRefresh, ConversationSource,
    RESIZE_DEBOUNCE as CONVERSATION_RESIZE_DEBOUNCE,
    message_block_id as message_conversation_block_id, tool_block_id as tool_conversation_block_id,
};
#[cfg(test)]
use conversation_controller::{
    compensate_scroll_top_for_single_row_height,
    distance_to_bottom as conversation_distance_to_bottom,
    row_target_height as conversation_row_target_height, upsert_indexed_item,
};
#[cfg(test)]
use conversation_header::ConversationHeaderModelWarning;
#[cfg(test)]
use conversation_header::header_runtime_status_slot_width;
use conversation_header::{ConversationHeader, ConversationHeaderEvent};
#[cfg(test)]
use conversation_pane::CONVERSATION_RAIL_WIDTH;
use conversation_pane::{ConversationPane, ConversationPaneEvent};
use home_pane::HomePane;
use inspector_pane::{InspectorPane, InspectorPaneEvent};
use intent::UiIntent;
use root_modal_host::{RootModalHost, RootModalHostEvent};
use sessions_pane::{SessionsPane, SessionsPaneEvent};
use skills_pane::SkillsPane;
use toast_host::{ToastHost, ToastNotice};
