use coding_agent::api::authorization::{ToolAuthorizationDecision, ToolAuthorizationIdentity};
#[cfg(test)]
use coding_agent::api::embedding::CodingAgentResourceCommandKind;
use coding_agent::api::embedding::{
    CodingAgentEmbeddingSnapshot, CodingAgentModelChoice, CodingAgentResourceCommand,
    CodingAgentThinkingLevel, CodingAgentWorkspaceScope, CodingAgentWorkspaceSelection,
};
use coding_agent::api::event::CodingAgentRecoveryResolution;
use coding_agent::api::review::CodingAgentFileReviewRequest;
use coding_agent::api::view::ProfileKind;
use desktop::conversation::{
    ComposerAdmission, ComposerState, ComposerSubmissionKind, ConversationBlockKind,
    ConversationRowMeasurement, MAX_COPY_BYTES, conversation_copy_text, conversation_width_bucket,
};
#[cfg(test)]
use desktop::conversation::{TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT, conversation_block_height};
use desktop::file_review::{DesktopFileReviewDocument, MAX_VISIBLE_FILE_CHANGES};
use desktop::preferences::{
    DesktopPreferences, DesktopThinkingLevel, PreferenceWriteResult, PreferenceWriter,
};
use desktop::projection::{
    DesktopProjection, DesktopProjectionLifecycle, DesktopRecoveryStatus, ProjectionEvent,
};
use desktop::runtime::{
    DesktopPromptTarget, DesktopRecoveryAction, DesktopRecoveryIdentity, DesktopRuntimeBridge,
    DesktopRuntimeOwnerTarget, DesktopRuntimeSelectionKind, MAX_PROMPT_ATTACHMENTS,
    validate_prompt_attachments,
};
use desktop::shell::{
    CONTEXT_PANEL_MAX_WIDTH, CONTEXT_PANEL_MIN_WIDTH, CONTEXT_PANEL_WIDTH,
    CONVERSATION_CONTENT_MAX_WIDTH, FocusTarget, MIN_CONVERSATION_WIDTH, PanelVisibility,
    SESSION_PANEL_MAX_WIDTH, SESSION_PANEL_MIN_WIDTH, SESSION_PANEL_WIDTH, SemanticColor,
    SemanticStatus, SemanticTheme, ShellLayout, UI_FONT_FAMILY, truncate_label,
};
use gpui::{
    ClipboardItem, Context, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, PathPromptOptions, Render, Role, ScrollStrategy, Styled as _,
    Window, WindowBounds, div, prelude::*, px, rgb,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use self::desktop_style::{DesignText, DesktopStyledExt as _};
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
use crate::application::reducer::safe_runtime_rejection_notice;
use crate::application::{
    change_set::{UiChangeSet, UiRegion},
    commands::{CommandCompletionError, CommandTracker, DesktopCommandIntent},
    effect::{
        ClipboardFeedback, DesktopEffect, DesktopPickerKind, DesktopTimer, DesktopTimerKind,
        PlatformOutcome, PlatformResult,
    },
    reducer::{
        DesktopController, DesktopEvent, PlatformUpdatePort, ProjectionUpdateResult,
        RuntimeUpdatePort, Transition, UiIntent,
    },
    state::DesktopState,
    workspace::{SessionId, WorkspaceKey, WorkspaceStore},
};
use crate::ui::shell::{ShellConnection, ShellUiState, ShellViews};

const MAX_RUNTIME_UPDATES_PER_FRAME: usize = 64;
const INSPECTOR_TELEMETRY_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
const MAX_SESSION_WORKSPACES: usize = 4;

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

impl DesktopThinkingLevel {
    const fn explicit(self) -> Option<CodingAgentThinkingLevel> {
        match self {
            Self::Default => None,
            Self::Off => Some(CodingAgentThinkingLevel::Off),
            Self::Minimal => Some(CodingAgentThinkingLevel::Minimal),
            Self::Low => Some(CodingAgentThinkingLevel::Low),
            Self::Medium => Some(CodingAgentThinkingLevel::Medium),
            Self::High => Some(CodingAgentThinkingLevel::High),
            Self::XHigh => Some(CodingAgentThinkingLevel::XHigh),
        }
    }

    const fn from_explicit(level: Option<CodingAgentThinkingLevel>) -> Self {
        match level {
            None => Self::Default,
            Some(CodingAgentThinkingLevel::Off) => Self::Off,
            Some(CodingAgentThinkingLevel::Minimal) => Self::Minimal,
            Some(CodingAgentThinkingLevel::Low) => Self::Low,
            Some(CodingAgentThinkingLevel::Medium) => Self::Medium,
            Some(CodingAgentThinkingLevel::High) => Self::High,
            Some(CodingAgentThinkingLevel::XHigh) => Self::XHigh,
        }
    }

    fn label(self, default: Option<&str>) -> String {
        match self {
            Self::Default => default
                .map(|level| format!("default:{}", truncate_label(level, 10)))
                .unwrap_or_else(|| "default".into()),
            Self::Off => "off".into(),
            Self::Minimal => "minimal".into(),
            Self::Low => "low".into(),
            Self::Medium => "medium".into(),
            Self::High => "high".into(),
            Self::XHigh => "xhigh".into(),
        }
    }
}

#[derive(Clone, Default)]
enum DesktopFileReviewState {
    #[default]
    Empty,
    Loading(CodingAgentFileReviewRequest),
    Ready(DesktopFileReviewDocument),
    Failed {
        request: CodingAgentFileReviewRequest,
        code: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DesktopModalKind {
    Authorization,
    CommandPalette,
    FullMessage,
}

#[derive(Debug, Clone)]
pub(crate) struct ConversationFullMessageView {
    pub(crate) block_id: String,
    pub(crate) title: Arc<str>,
    pub(crate) text: Arc<str>,
    pub(crate) source_truncated: bool,
}

pub(super) struct SessionWorkspace {
    project: CodingAgentEmbeddingSnapshot,
    projection: Option<DesktopProjection>,
    draft_workspace_selection: CodingAgentWorkspaceSelection,
    /// Workspace-local transient notice. DSK-710 audited every producer and
    /// made the active workspace owner explicit; the Shell has no parallel
    /// global notice slot. A future global notice must use a distinct field.
    preference_notice: Option<String>,
    preference_notice_revision: u64,
    conversation_controller: ConversationController,
    inspector_section: InspectorSection,
    composer: ComposerState,
    composer_needs_sync: bool,
    composer_running_mode: ComposerRunningMode,
    composer_attachments: Vec<PathBuf>,
    thinking_selection: DesktopThinkingLevel,
    thinking_hint: Option<Arc<str>>,
    file_review: Arc<DesktopFileReviewState>,
}

impl SessionWorkspace {
    #[cfg(test)]
    fn new(
        project: CodingAgentEmbeddingSnapshot,
        projection: Option<DesktopProjection>,
        preference_notice: Option<String>,
    ) -> Self {
        Self::new_with_thinking(
            project,
            projection,
            preference_notice,
            DesktopThinkingLevel::Default,
        )
    }

    fn new_with_thinking(
        project: CodingAgentEmbeddingSnapshot,
        projection: Option<DesktopProjection>,
        preference_notice: Option<String>,
        thinking_selection: DesktopThinkingLevel,
    ) -> Self {
        let draft_workspace_selection = workspace_selection_from_embedding(&project);
        Self::new_home_with_thinking(
            project,
            projection,
            preference_notice,
            thinking_selection,
            draft_workspace_selection,
        )
    }

    fn new_home_with_thinking(
        project: CodingAgentEmbeddingSnapshot,
        projection: Option<DesktopProjection>,
        preference_notice: Option<String>,
        thinking_selection: DesktopThinkingLevel,
        draft_workspace_selection: CodingAgentWorkspaceSelection,
    ) -> Self {
        let preference_notice_revision = u64::from(preference_notice.is_some());
        let (thinking_selection, thinking_fallback) =
            admitted_desktop_thinking_selection(&project, thinking_selection);
        Self {
            project,
            projection,
            draft_workspace_selection,
            preference_notice,
            preference_notice_revision,
            conversation_controller: ConversationController::default(),
            inspector_section: InspectorSection::default(),
            composer: ComposerState::default(),
            composer_needs_sync: false,
            composer_running_mode: ComposerRunningMode::default(),
            composer_attachments: Vec::new(),
            thinking_selection,
            thinking_hint: thinking_fallback
                .then(|| Arc::from("Thinking reset to Auto for the selected model.")),
            file_review: Arc::new(DesktopFileReviewState::default()),
        }
    }

    fn prompt_target(&self) -> DesktopPromptTarget {
        if let Some(projection) = self.projection.as_ref() {
            return DesktopPromptTarget::existing(projection.snapshot().session.session_id.clone());
        }
        DesktopPromptTarget::new(
            self.draft_workspace_selection.clone(),
            self.project.selected_model_id.clone(),
            self.project.default_agent_profile_id.as_str(),
        )
    }

    fn project_directory(&self) -> Option<&Path> {
        if self.projection.is_none() {
            return match &self.draft_workspace_selection {
                CodingAgentWorkspaceSelection::Project { cwd } => Some(cwd.as_path()),
                CodingAgentWorkspaceSelection::Projectless { .. } => None,
            };
        }
        match self
            .project
            .workspace
            .as_ref()
            .map(|workspace| &workspace.scope)
        {
            Some(CodingAgentWorkspaceScope::Project { cwd })
            | Some(CodingAgentWorkspaceScope::Legacy { cwd: Some(cwd) }) => Some(cwd.as_path()),
            Some(CodingAgentWorkspaceScope::Projectless { .. })
            | Some(CodingAgentWorkspaceScope::Legacy { cwd: None }) => None,
            None => Some(self.project.cwd.as_path()),
        }
    }

    fn project_directory_editable(&self) -> bool {
        self.projection.is_none()
            && matches!(self.composer.admission(), ComposerAdmission::Idle)
            && self.composer.submitted().is_none()
    }

    fn runtime_owner_target(&self) -> DesktopRuntimeOwnerTarget {
        self.projection
            .as_ref()
            .map_or_else(DesktopRuntimeOwnerTarget::home, |projection| {
                DesktopRuntimeOwnerTarget::session(projection.snapshot().session.session_id.clone())
            })
    }

    fn set_preference_notice(&mut self, message: String) {
        self.preference_notice = Some(message);
        self.preference_notice_revision = self.preference_notice_revision.wrapping_add(1).max(1);
    }
}

fn workspace_selection_from_embedding(
    project: &CodingAgentEmbeddingSnapshot,
) -> CodingAgentWorkspaceSelection {
    match project.workspace.as_ref().map(|workspace| &workspace.scope) {
        Some(CodingAgentWorkspaceScope::Project { cwd })
        | Some(CodingAgentWorkspaceScope::Legacy { cwd: Some(cwd) }) => {
            CodingAgentWorkspaceSelection::project(cwd.clone())
        }
        Some(CodingAgentWorkspaceScope::Projectless { workspace_id }) => {
            CodingAgentWorkspaceSelection::projectless(workspace_id.clone())
        }
        Some(CodingAgentWorkspaceScope::Legacy { cwd: None }) | None => {
            CodingAgentWorkspaceSelection::project(project.cwd.clone())
        }
    }
}

fn workspace_semantic_status(workspace: &SessionWorkspace) -> SemanticStatus {
    let Some(projection) = workspace.projection.as_ref() else {
        return SemanticStatus::Idle;
    };
    match projection.lifecycle() {
        DesktopProjectionLifecycle::Failed | DesktopProjectionLifecycle::NeedsResync => {
            SemanticStatus::Error
        }
        DesktopProjectionLifecycle::Stopped => SemanticStatus::Warning,
        DesktopProjectionLifecycle::Running
            if !projection.snapshot().pending_authorizations.is_empty() =>
        {
            SemanticStatus::Authorization
        }
        DesktopProjectionLifecycle::Running if projection.snapshot().active_operation.is_some() => {
            SemanticStatus::Running
        }
        DesktopProjectionLifecycle::Running => SemanticStatus::Idle,
    }
}

fn hydrated_session_id(snapshot: &desktop::runtime::DesktopRuntimeHydratedSnapshot) -> &str {
    &snapshot.session.session.session_id
}

pub(super) struct NativeShell {
    connection: ShellConnection,
    app: DesktopState<SessionWorkspace, ProjectCatalogController>,
    home_project: CodingAgentEmbeddingSnapshot,
    projectless_workspace_selection: CodingAgentWorkspaceSelection,
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
                |this, _, event: &ConversationPaneEvent, window, cx| match event {
                    ConversationPaneEvent::Select { block_id, durable } => {
                        this.record_focus(FocusTarget::CenterBody, window, cx);
                        let workspace = &mut this.app.workspaces.active_mut();
                        let Some(projection) = workspace.projection.as_ref() else {
                            return;
                        };
                        workspace.conversation_controller.select_row(
                            block_id.clone(),
                            *durable,
                            projection.conversation(),
                        );
                        this.notify_conversation_pane(cx);
                        this.notify_conversation_header(cx);
                    }
                    ConversationPaneEvent::Scrolled => {
                        cx.defer_in(window, |this, _, cx| {
                            this.reconcile_conversation_scroll(cx);
                        });
                    }
                    ConversationPaneEvent::Copy { block_id } => {
                        this.copy_conversation_row(block_id, cx);
                    }
                    ConversationPaneEvent::CopyToolDetails { block_id } => {
                        this.copy_tool_details(block_id, cx);
                    }
                    ConversationPaneEvent::CopyCodeCompleted => {
                        this.announce_conversation_copy("Code copied.", cx);
                    }
                    ConversationPaneEvent::ToggleDetails { block_id } => {
                        this.toggle_conversation_details(block_id, cx);
                    }
                    ConversationPaneEvent::OpenFull { block_id } => {
                        this.open_full_conversation_message(block_id, window, cx);
                    }
                    ConversationPaneEvent::Recovery { identity, action } => {
                        this.submit_recovery_action(identity.clone(), *action, cx);
                    }
                    ConversationPaneEvent::Measured(measurement) => {
                        this.submit_conversation_row_measurement(measurement, cx);
                    }
                    ConversationPaneEvent::FollowLatest => this.follow_latest(cx),
                },
            ),
            cx.subscribe_in(
                &conversation_header,
                window,
                |this, _, event: &ConversationHeaderEvent, window, cx| match event {
                    ConversationHeaderEvent::ToggleSessions => this.toggle_sessions(window, cx),
                    ConversationHeaderEvent::ToggleInspector => this.toggle_context(window, cx),
                    ConversationHeaderEvent::Reload => this.reload_local_resources(cx),
                    ConversationHeaderEvent::SelectModel(model_id) => this.submit_selection(
                        DesktopRuntimeSelectionKind::Model,
                        model_id.to_string(),
                        cx,
                    ),
                    ConversationHeaderEvent::SelectSessionProfile(profile_id) => this
                        .submit_selection(
                            DesktopRuntimeSelectionKind::SessionProfile,
                            profile_id.to_string(),
                            cx,
                        ),
                    ConversationHeaderEvent::SelectThinking(level) => {
                        this.select_thinking_level(*level, cx);
                    }
                    ConversationHeaderEvent::Abort => this.abort_active_operation(cx),
                },
            ),
            cx.subscribe_in(
                &sessions_pane,
                window,
                |this, _, event: &SessionsPaneEvent, window, cx| match event {
                    SessionsPaneEvent::Navigate(target) => {
                        this.navigate_center(target.clone(), window, cx);
                    }
                    SessionsPaneEvent::Refresh => this.request_session_catalog(cx),
                    SessionsPaneEvent::SetProjectCollapsed {
                        group_id,
                        collapsed,
                    } => {
                        let transition = this.connection.controller.reduce(
                            &mut this.app,
                            DesktopEvent::Ui(UiIntent::SetProjectCollapsed {
                                group_id: group_id.clone(),
                                collapsed: *collapsed,
                            }),
                            |state, event| {
                                let DesktopEvent::Ui(UiIntent::SetProjectCollapsed {
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
                            this.notify_sessions_pane(cx);
                            cx.notify();
                        }
                    }
                    SessionsPaneEvent::Rename(session_id, name) => {
                        this.rename_session(session_id.clone(), name.clone(), cx);
                    }
                    SessionsPaneEvent::CloseSession(session_id) => {
                        this.close_session(session_id, cx);
                    }
                    SessionsPaneEvent::Dismiss => {
                        this.dismiss_drawer(window, cx, true);
                    }
                },
            ),
            cx.subscribe_in(
                &composer_pane,
                window,
                |this, _, event: &ComposerPaneEvent, window, cx| match event {
                    ComposerPaneEvent::InputChanged(value) => {
                        this.app
                            .workspaces
                            .active_mut()
                            .composer
                            .edit(value.clone());
                        this.notify_composer_pane(cx);
                    }
                    ComposerPaneEvent::Focused => {
                        this.record_focus(FocusTarget::Composer, window, cx);
                    }
                    ComposerPaneEvent::AddAttachments => this.choose_composer_attachments(cx),
                    ComposerPaneEvent::RemoveAttachment(index) => {
                        this.remove_composer_attachment(*index, cx);
                    }
                    ComposerPaneEvent::ChooseProjectDirectory => {
                        this.choose_project_directory(cx);
                    }
                    ComposerPaneEvent::ClearProjectDirectory => {
                        this.clear_project_directory(cx);
                    }
                    ComposerPaneEvent::SubmitPrimary => {
                        if !this.root_action_blocked_by_modal(window, cx) {
                            this.submit_primary_composer(cx);
                        }
                    }
                    ComposerPaneEvent::Submit => this.submit_composer(cx),
                    ComposerPaneEvent::SubmitRunning => {
                        this.submit_active_control(
                            this.active_composer_running_mode().submission_kind(),
                            cx,
                        );
                    }
                    ComposerPaneEvent::SetRunningMode(mode) => {
                        this.set_active_composer_running_mode(*mode, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &inspector_pane,
                window,
                |this, _, event: &InspectorPaneEvent, window, cx| match event {
                    InspectorPaneEvent::Close => {
                        this.dismiss_drawer(window, cx, true);
                    }
                    InspectorPaneEvent::RequestFileReview(request) => {
                        this.request_file_review(request.clone(), cx);
                    }
                    InspectorPaneEvent::CopyReviewPath => this.copy_review_path(cx),
                    InspectorPaneEvent::CopyFileReview => this.copy_file_review(cx),
                    InspectorPaneEvent::OpenExternalEditor => {
                        this.open_review_in_external_editor(cx);
                    }
                    InspectorPaneEvent::Recovery { identity, action } => {
                        this.submit_recovery_action(identity.clone(), *action, cx);
                    }
                    InspectorPaneEvent::SelectSection(section) => {
                        this.app.workspaces.active_mut().inspector_section = *section;
                        this.notify_inspector_pane(cx);
                    }
                },
            ),
            cx.subscribe_in(
                &root_modal_host,
                window,
                |this, _, event: &RootModalHostEvent, window, cx| match event {
                    RootModalHostEvent::ExecutePalette(command) => {
                        this.ui.command_palette.close();
                        this.dismiss_modal(window, cx);
                        this.execute_palette_command(*command, window, cx);
                    }
                    RootModalHostEvent::DecideAuthorization { identity, decision } => {
                        this.decide_tool_authorization(identity.clone(), decision.clone(), cx);
                    }
                    RootModalHostEvent::CopyFullMessage => {
                        if let Some(message) = &this.ui.conversation_full_message {
                            let text = message.text.to_string();
                            this.write_clipboard(
                                Some(text),
                                ClipboardFeedback::ConversationAnnouncement(
                                    "Full message copied.".into(),
                                ),
                                cx,
                            );
                        }
                    }
                    RootModalHostEvent::CloseFullMessage => {
                        this.close_full_conversation_message(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &center_drawer_host,
                window,
                |this, _, event: &CenterDrawerHostEvent, window, cx| match event {
                    CenterDrawerHostEvent::Dismiss => this.dismiss_drawer(window, cx, true),
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
            SessionWorkspace::new_home_with_thinking(
                project,
                projection,
                preference_notice,
                thinking_selection,
                projectless_workspace_selection.clone(),
            )
        } else {
            SessionWorkspace::new_with_thinking(
                project,
                projection,
                preference_notice,
                thinking_selection,
            )
        };
        let workspace_store = match active_session_id {
            Some(session_id) => {
                let mut store = WorkspaceStore::new(SessionWorkspace::new_home_with_thinking(
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
        let app = DesktopState::new(
            workspace_store,
            command_tracker,
            ProjectCatalogController::default(),
            preferences,
        );
        let shell = Self {
            connection,
            app,
            home_project,
            projectless_workspace_selection,
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
        shell.notify_toast_host(cx);
        let conversation_header_view_model = shell.conversation_header_view_model();
        shell
            .views
            .conversation_header
            .update(cx, |conversation_header, _| {
                conversation_header.set_view_model(conversation_header_view_model);
            });
        let sessions_pane_view_model = shell.sessions_pane_view_model();
        shell.views.sessions_pane.update(cx, |sessions_pane, _| {
            sessions_pane.set_view_model(sessions_pane_view_model);
        });
        let composer_pane_view_model = shell.composer_pane_view_model();
        shell.views.composer_pane.update(cx, |composer_pane, _| {
            composer_pane.set_view_model(composer_pane_view_model);
        });
        let skills_pane_view_model = shell.skills_pane_view_model();
        shell.views.skills_pane.update(cx, |skills_pane, _| {
            skills_pane.set_view_model(skills_pane_view_model);
        });
        let conversation_pane_view_model = shell.conversation_pane_view_model();
        shell
            .views
            .conversation_pane
            .update(cx, |conversation_pane, _| {
                conversation_pane.set_view_model(conversation_pane_view_model);
            });
        let inspector_pane_view_model = shell.inspector_pane_view_model();
        shell.views.inspector_pane.update(cx, |inspector_pane, _| {
            inspector_pane.set_view_model(inspector_pane_view_model);
        });
        let root_modal_view_model = shell.root_modal_view_model();
        shell
            .views
            .root_modal_host
            .update(cx, |root_modal_host, _| {
                root_modal_host.set_view_model(root_modal_view_model);
            });
        let center_drawer_view_model = shell.center_drawer_view_model();
        shell
            .views
            .center_drawer_host
            .update(cx, |center_drawer_host, _| {
                center_drawer_host.set_view_model(center_drawer_view_model);
            });
        shell
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
        self.complete_command(command_id, &owner, intent)
    }

    pub(super) fn complete_command(
        &mut self,
        command_id: u64,
        observed_owner: &WorkspaceKey,
        intent: &DesktopCommandIntent,
    ) -> bool {
        let pending_owner = self.app.commands.owner(command_id).cloned();
        match self
            .app
            .commands
            .complete(command_id, observed_owner, intent)
        {
            Ok(_) => true,
            Err(CommandCompletionError::OwnerMismatch) => {
                if let Some(pending_owner) = pending_owner {
                    self.require_command_owner_resync(&pending_owner, observed_owner);
                }
                false
            }
            Err(
                CommandCompletionError::UnknownCommand | CommandCompletionError::IntentMismatch,
            ) => false,
        }
    }

    fn reject_command(
        &mut self,
        command_id: u64,
        observed_owner: &WorkspaceKey,
        command: desktop::runtime::DesktopRuntimeCommandKind,
    ) -> Option<DesktopCommandIntent> {
        let pending_owner = self.app.commands.owner(command_id).cloned();
        match self
            .app
            .commands
            .reject(command_id, observed_owner, command)
        {
            Ok(pending) => Some(pending.into_intent()),
            Err(CommandCompletionError::OwnerMismatch) => {
                if let Some(pending_owner) = pending_owner {
                    self.require_command_owner_resync(&pending_owner, observed_owner);
                }
                None
            }
            Err(
                CommandCompletionError::UnknownCommand | CommandCompletionError::IntentMismatch,
            ) => None,
        }
    }

    fn complete_matching_command(
        &mut self,
        owner: &WorkspaceKey,
        predicate: impl Fn(&DesktopCommandIntent) -> bool,
    ) -> Option<DesktopCommandIntent> {
        let (command_id, intent) = self.app.commands.find(owner, predicate)?;
        self.complete_command(command_id, owner, &intent)
            .then_some(intent)
    }

    fn require_command_owner_resync(
        &mut self,
        pending_owner: &WorkspaceKey,
        observed_owner: &WorkspaceKey,
    ) {
        let mut marked = false;
        for owner in [pending_owner, observed_owner] {
            let Some(workspace) = self.app.workspaces.get_mut(owner) else {
                continue;
            };
            marked = true;
            if let Some(projection) = workspace.projection.as_mut() {
                projection.require_command_resync(
                    "command_owner_mismatch",
                    "runtime command completion targeted a different workspace",
                );
            }
            workspace.set_preference_notice(
                "Runtime response targeted another session; resync is required.".into(),
            );
        }
        if !marked {
            let workspace = self.app.workspaces.active_mut();
            if let Some(projection) = workspace.projection.as_mut() {
                projection.require_command_resync(
                    "command_owner_mismatch",
                    "runtime command completion targeted a different workspace",
                );
            }
            workspace.set_preference_notice(
                "Runtime response targeted another session; resync is required.".into(),
            );
        }
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
                self.notify_sessions_pane(cx);
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
                    self.notify_sessions_pane(cx);
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
        self.notify_sessions_pane(cx);
        self.notify_composer_pane(cx);
        self.notify_conversation_pane(cx);
        self.notify_conversation_header(cx);
        self.notify_inspector_pane(cx);
        self.notify_root_modal_host(cx);
        cx.notify();
    }

    fn install_hydrated_workspace(
        &mut self,
        snapshot: &desktop::runtime::DesktopRuntimeHydratedSnapshot,
        inherit_home_thinking: bool,
        activate: bool,
    ) -> bool {
        let target_session_id = SessionId::from_dto(hydrated_session_id(snapshot));
        let target_key = WorkspaceKey::Session(target_session_id.clone());
        if self.app.workspaces.active_key() == &target_key {
            return true;
        }
        if self.app.workspaces.contains(&target_key) {
            return !activate || self.app.workspaces.activate(&target_key);
        }
        if self.open_workspace_count() >= MAX_SESSION_WORKSPACES {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice(format!(
                    "Up to {MAX_SESSION_WORKSPACES} sessions can stay open; close one first."
                ));
            return false;
        }
        let projection = match DesktopProjection::new(snapshot.clone()) {
            Ok(projection) => projection,
            Err(issue) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(format!(
                        "Session response failed projection validation ({}).",
                        truncate_label(&issue.code, 28)
                    ));
                return false;
            }
        };
        let promoting_home = activate && self.app.workspaces.active_key() == &WorkspaceKey::Home;
        let thinking_selection = if inherit_home_thinking && promoting_home {
            self.app.workspaces.active_mut().thinking_selection
        } else {
            self.app
                .preferences
                .thinking_level_for_session(target_session_id.as_str())
        };
        if promoting_home {
            self.app.workspaces.active_mut().project = snapshot.project.clone();
            self.app.workspaces.active_mut().projection = Some(projection);
            self.app.workspaces.active_mut().thinking_selection = thinking_selection;
            self.reconcile_thinking_selection_with_project();
            let admitted_selection = self.app.workspaces.active().thinking_selection;
            self.remember_thinking_selection(target_session_id.as_str(), admitted_selection);
            let fresh_home = SessionWorkspace::new_home_with_thinking(
                self.home_project.clone(),
                None,
                None,
                thinking_selection,
                self.projectless_workspace_selection.clone(),
            );
            self.app.commands.transfer_owner(
                &WorkspaceKey::Home,
                &WorkspaceKey::Session(target_session_id.clone()),
            );
            let promoted = self
                .app
                .workspaces
                .promote_home(target_session_id, fresh_home);
            debug_assert!(
                promoted.is_ok(),
                "new session must promote the active Home entry"
            );
            return true;
        }
        let target = SessionWorkspace::new_with_thinking(
            snapshot.project.clone(),
            Some(projection),
            None,
            thinking_selection,
        );
        self.app
            .workspaces
            .insert_session(target_session_id.clone(), target);
        !activate
            || self
                .app
                .workspaces
                .activate(&WorkspaceKey::Session(target_session_id))
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
                self.notify_sessions_pane(cx);
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
            let _ = self.complete_command(command_id, &owner, &intent);
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice(error);
        }
        self.notify_sessions_pane(cx);
    }

    fn remove_closed_workspace(&mut self, session_id: &str) -> usize {
        let owner = WorkspaceKey::session(session_id);
        let cancelled = self.app.commands.cancel_owner(&owner).len();
        self.app
            .workspaces
            .remove_session(&SessionId::from_dto(session_id));
        cancelled
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
        self.notify_sessions_pane(cx);
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
        self.notify_sessions_pane(cx);
        self.notify_inspector_pane(cx);
        self.notify_conversation_header(cx);
        self.notify_center_drawer_host(cx);
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
        self.notify_composer_pane(cx);
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
        self.notify_conversation_header(cx);
        self.notify_composer_pane(cx);
        cx.notify();
    }

    fn visibility(&self) -> PanelVisibility {
        PanelVisibility {
            sessions: self.app.preferences.sessions_panel_visible,
            context: self.app.preferences.context_panel_visible,
        }
    }

    fn layout(&self, window: &Window) -> ShellLayout {
        let viewport = window.viewport_size();
        self.resolve_layout(
            u32::from(viewport.width),
            u32::from(viewport.height),
            self.visibility(),
        )
    }

    fn resolve_layout(&self, width: u32, height: u32, visibility: PanelVisibility) -> ShellLayout {
        ShellLayout::resolve_with_panel_widths(
            width,
            height,
            visibility,
            self.app.preferences.sessions_panel_width,
            self.app.preferences.context_panel_width,
        )
    }

    fn begin_panel_resize(
        &mut self,
        panel: ResizablePanel,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        if event.click_count >= 2 {
            self.ui.panel_resize = None;
            match panel {
                ResizablePanel::Sessions => {
                    self.app.preferences.sessions_panel_width = SESSION_PANEL_WIDTH;
                    self.notify_sessions_pane(cx);
                    self.notify_conversation_header(cx);
                }
                ResizablePanel::Context => {
                    self.app.preferences.context_panel_width = CONTEXT_PANEL_WIDTH;
                    self.notify_inspector_pane(cx);
                    self.notify_conversation_header(cx);
                }
            }
            self.schedule_preferences();
            cx.notify();
            return;
        }

        self.ui.panel_resize = Some(PanelResizeState {
            panel,
            pointer_origin_x: f32::from(event.position.x),
            width_origin: match panel {
                ResizablePanel::Sessions => self.app.preferences.sessions_panel_width,
                ResizablePanel::Context => self.app.preferences.context_panel_width,
            },
        });
    }

    fn update_panel_resize(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(resize) = self.ui.panel_resize else {
            return;
        };
        let delta = f32::from(event.position.x) - resize.pointer_origin_x;
        let desired = match resize.panel {
            ResizablePanel::Sessions => resize.width_origin as f32 + delta,
            ResizablePanel::Context => resize.width_origin as f32 - delta,
        };
        let layout = self.layout(window);
        let (minimum, configured_maximum, other_width) = match resize.panel {
            ResizablePanel::Sessions => (
                SESSION_PANEL_MIN_WIDTH,
                SESSION_PANEL_MAX_WIDTH,
                layout.inspector.map_or(0, |bounds| bounds.width),
            ),
            ResizablePanel::Context => (
                CONTEXT_PANEL_MIN_WIDTH,
                CONTEXT_PANEL_MAX_WIDTH,
                layout.sidebar.map_or(0, |bounds| bounds.width),
            ),
        };
        let viewport_width = u32::from(window.viewport_size().width);
        let maximum = configured_maximum.min(
            viewport_width
                .saturating_sub(MIN_CONVERSATION_WIDTH)
                .saturating_sub(other_width)
                .max(minimum),
        );
        let width = (desired.round() as i64).clamp(i64::from(minimum), i64::from(maximum)) as u32;

        match resize.panel {
            ResizablePanel::Sessions if self.app.preferences.sessions_panel_width != width => {
                self.app.preferences.sessions_panel_width = width;
                self.notify_sessions_pane(cx);
                self.notify_conversation_header(cx);
                cx.notify();
            }
            ResizablePanel::Context if self.app.preferences.context_panel_width != width => {
                self.app.preferences.context_panel_width = width;
                self.notify_inspector_pane(cx);
                self.notify_conversation_header(cx);
                cx.notify();
            }
            _ => {}
        }
    }

    fn finish_panel_resize(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.ui.panel_resize.take().is_some() {
            self.schedule_preferences();
            self.flush_queued_effects(cx);
        }
    }

    fn set_focus_input_modality(&mut self, modality: FocusInputModality, cx: &mut Context<Self>) {
        if self.ui.focus_input_modality == modality {
            return;
        }
        self.ui.focus_input_modality = modality;
        self.notify_sessions_pane(cx);
        self.notify_conversation_header(cx);
        self.notify_composer_pane(cx);
        self.notify_inspector_pane(cx);
        self.notify_toast_host(cx);
        cx.notify();
    }

    fn note_pointer_input(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.set_focus_input_modality(FocusInputModality::Pointer, cx);
    }

    fn note_keyboard_input(&mut self, _: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.set_focus_input_modality(FocusInputModality::Keyboard, cx);
    }

    pub(super) fn keyboard_focus_visible(&self) -> bool {
        self.ui.focus_input_modality == FocusInputModality::Keyboard
    }

    fn record_focus(&mut self, target: FocusTarget, window: &mut Window, cx: &mut Context<Self>) {
        let layout = self.layout(window);
        let previous = self.ui.focus.active();
        if self.ui.focus.request(target, layout) {
            if self.ui.active_drawer.is_some() {
                self.ui.drawer_restore_focus = Some(target);
            }
            cx.notify();
        }
        if previous == FocusTarget::Sidebar || target == FocusTarget::Sidebar {
            self.notify_sessions_pane(cx);
        }
        if previous == FocusTarget::CenterHeader || target == FocusTarget::CenterHeader {
            self.notify_conversation_header(cx);
        }
        if previous == FocusTarget::Composer || target == FocusTarget::Composer {
            self.notify_composer_pane(cx);
        }
        if previous == FocusTarget::Inspector || target == FocusTarget::Inspector {
            self.notify_inspector_pane(cx);
        }
    }

    fn window_bounds_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = window.window_bounds();
        let restore = bounds.get_bounds();
        self.app.preferences.window.x = f32::from(restore.origin.x).round() as i32;
        self.app.preferences.window.y = f32::from(restore.origin.y).round() as i32;
        self.app.preferences.window.width = u32::from(restore.size.width);
        self.app.preferences.window.height = u32::from(restore.size.height);
        self.app.preferences.window.maximized = matches!(bounds, WindowBounds::Maximized(_));

        let viewport = window.viewport_size();
        let forced_layout = self.resolve_layout(
            u32::from(viewport.width),
            u32::from(viewport.height),
            PanelVisibility::default(),
        );
        let drawer_became_dockable = match self.ui.active_drawer {
            Some(CenterDrawerKind::Sessions) if forced_layout.sidebar.is_some() => {
                self.app.preferences.sessions_panel_visible = true;
                true
            }
            Some(CenterDrawerKind::Inspector) if forced_layout.inspector.is_some() => {
                self.app.preferences.context_panel_visible = true;
                true
            }
            _ => false,
        };
        if drawer_became_dockable {
            self.dismiss_drawer(window, cx, true);
        }
        let layout = self.layout(window);
        let previous_focus = self.ui.focus.active();
        self.ui.focus.reconcile_layout(layout);
        if self.ui.focus.active() != previous_focus {
            self.focus_composer_input(window, cx);
        }
        self.schedule_preferences();
        self.notify_inspector_pane(cx);
        self.notify_conversation_header(cx);
        self.notify_center_drawer_host(cx);
        cx.notify();
    }

    fn apply_projection_event_for(
        &mut self,
        owner: &WorkspaceKey,
        event: Option<ProjectionEvent>,
        creates_session_from_prompt: bool,
        completed_prompt_command: Option<u64>,
    ) -> ProjectionUpdateResult {
        let Some(event) = event else {
            return ProjectionUpdateResult::new(false, false, Default::default());
        };
        let composer_state_before = self.composer_pane_state_for(owner);
        let projection_was_none = self
            .app
            .workspaces
            .get(owner)
            .is_none_or(|workspace| workspace.projection.is_none());
        if projection_was_none {
            let hydrated = match &event {
                ProjectionEvent::Hydrated { snapshot, .. } => Some(snapshot),
                _ => None,
            };
            if let Some(hydrated) = hydrated {
                if let Some(workspace) = self.app.workspaces.get_mut(owner) {
                    workspace.project = hydrated.project.clone();
                    match DesktopProjection::new(hydrated.clone()) {
                        Ok(projection) => workspace.projection = Some(projection),
                        Err(issue) => workspace.set_preference_notice(format!(
                            "Session response failed projection validation ({}).",
                            truncate_label(&issue.code, 28)
                        )),
                    }
                }
                self.reconcile_thinking_selection_for(owner);
            } else if let Some(metadata) = match &event {
                ProjectionEvent::Metadata(metadata)
                | ProjectionEvent::PromptStarted { metadata, .. } => Some(metadata),
                _ => None,
            } {
                if let Some(workspace) = self.app.workspaces.get_mut(owner) {
                    workspace.project = metadata.project.clone();
                }
                if self.app.workspaces.active_key() == owner {
                    self.home_project = metadata.project.clone();
                }
                self.reconcile_thinking_selection_for(owner);
            }
        }

        if creates_session_from_prompt
            && self
                .app
                .workspaces
                .get(owner)
                .is_some_and(|workspace| workspace.projection.is_some())
            && self.app.workspaces.active_key() == owner
        {
            let _ = self.insert_active_session_into_catalog();
        }

        let Some(workspace) = self.app.workspaces.get(owner) else {
            return ProjectionUpdateResult::new(false, false, Default::default());
        };
        if workspace.projection.is_none() {
            // Metadata-only updates are valid application updates even when
            // the Home workspace has no session projection.
            return ProjectionUpdateResult::new(true, false, Default::default());
        }

        let completes_submitted_prompt = completed_prompt_command.is_some_and(|command_id| {
            self.app
                .workspaces
                .get(owner)
                .and_then(|workspace| workspace.composer.submitted())
                .is_some_and(|submitted| submitted.command_id == command_id)
        });
        let (had_active_operation, outcome, project_after, active_operation_after, sequence_after) = {
            let workspace = self
                .app
                .workspaces
                .get_mut(owner)
                .expect("runtime reducer target must exist");
            let projection = workspace
                .projection
                .as_mut()
                .expect("projection availability was checked");
            let had_active_operation = projection.snapshot().active_operation.is_some();
            let outcome = projection.apply(event);
            let project_after = projection.project().clone();
            let active_operation_after = projection.snapshot().active_operation.is_some();
            let sequence_after = projection.cursor().last_event_sequence;
            (
                had_active_operation,
                outcome,
                project_after,
                active_operation_after,
                sequence_after,
            )
        };
        self.app
            .workspaces
            .get_mut(owner)
            .expect("runtime reducer target must exist")
            .project = project_after;
        self.reconcile_thinking_selection_for(owner);

        let delta = outcome.delta();
        let conversation_dirty = delta.is_some_and(|delta| delta.conversation || delta.tools);
        let file_changes_dirty = delta.is_some_and(|delta| {
            delta
                .context
                .contains(desktop::projection::ContextDirtyFlags::CHANGES)
        });
        let mut changes = UiChangeSet::for_projection(outcome.is_replaced(), delta);
        if had_active_operation != active_operation_after {
            changes.insert(UiRegion::Sessions);
        }

        if let Some(workspace) = self.app.workspaces.get_mut(owner) {
            workspace.conversation_controller.apply_projection_delta(
                outcome.is_replaced(),
                delta,
                sequence_after,
            );
            if outcome.is_replaced() {
                if completes_submitted_prompt
                    && !active_operation_after
                    && workspace.composer.submitted().is_some()
                {
                    if let Some(projection) = workspace.projection.as_ref()
                        && let Some((live_id, durable_id)) = workspace
                            .composer
                            .reconcile_completed_submission(projection.conversation())
                    {
                        workspace
                            .conversation_controller
                            .reconcile_live_selection(&live_id, &durable_id);
                    }
                    workspace.composer_needs_sync = true;
                }
                if let Some(projection) = workspace.projection.as_ref() {
                    let source =
                        ConversationSource::new(projection, workspace.composer.submitted());
                    workspace
                        .conversation_controller
                        .reconcile_hydration(&source, sequence_after);
                }
            } else if conversation_dirty && let Some(projection) = workspace.projection.as_ref() {
                let source = ConversationSource::new(projection, workspace.composer.submitted());
                workspace
                    .conversation_controller
                    .reconcile_content(&source, sequence_after);
            }
        }

        let authorization = self.app.commands.authorization(owner).map(
            |(command_id, authorization_id, operation_id)| {
                (
                    command_id,
                    authorization_id.to_owned(),
                    operation_id.to_owned(),
                )
            },
        );
        if let Some((command_id, authorization_id, operation_id)) = authorization
            && !self
                .app
                .workspaces
                .get(owner)
                .and_then(|workspace| workspace.projection.as_ref())
                .is_some_and(|projection| {
                    projection
                        .snapshot()
                        .pending_authorizations
                        .iter()
                        .any(|request| request.authorization_id == authorization_id)
                })
        {
            let intent = DesktopCommandIntent::Authorization {
                authorization_id,
                operation_id,
            };
            let _ = self.complete_command(command_id, owner, &intent);
        }
        if outcome.is_replaced() || file_changes_dirty {
            self.reconcile_file_review_for(owner);
        }
        if composer_state_before != self.composer_pane_state_for(owner) {
            changes.insert(UiRegion::Composer);
            changes.insert(UiRegion::Inspector);
            changes.insert(UiRegion::Toast);
            changes.insert(UiRegion::ConversationHeader);
            changes.insert(UiRegion::Modal);
        }
        ProjectionUpdateResult::new(
            outcome.is_replaced(),
            matches!(
                outcome,
                crate::projection::DesktopProjectionApply::NeedsResync
            ),
            changes,
        )
    }

    fn composer_pane_state_for(&self, owner: &WorkspaceKey) -> (bool, bool, bool, bool) {
        let Some(workspace) = self.app.workspaces.get(owner) else {
            return (false, false, false, false);
        };
        (
            matches!(
                workspace.composer.admission(),
                ComposerAdmission::Pending { .. }
            ),
            workspace
                .projection
                .as_ref()
                .is_some_and(|projection| projection.snapshot().active_operation.is_some()),
            workspace.composer.submitted().is_some(),
            workspace.composer.rejection().is_some(),
        )
    }

    fn reconcile_thinking_selection_for(&mut self, owner: &WorkspaceKey) {
        let Some(workspace) = self.app.workspaces.get_mut(owner) else {
            return;
        };
        let (selection, fallback) =
            admitted_desktop_thinking_selection(&workspace.project, workspace.thinking_selection);
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

    fn reconcile_file_review_for(&mut self, owner: &WorkspaceKey) {
        let Some(workspace) = self.app.workspaces.get(owner) else {
            return;
        };
        let request = match workspace.file_review.as_ref() {
            DesktopFileReviewState::Empty => return,
            DesktopFileReviewState::Loading(request)
            | DesktopFileReviewState::Failed { request, .. } => request.clone(),
            DesktopFileReviewState::Ready(document) => document.request.clone(),
        };
        let remains_current = workspace.projection.as_ref().is_some_and(|projection| {
            projection.snapshot().context.changes.iter().any(|change| {
                change.operation_id == request.change.operation_id
                    && change.tool_call_id == request.change.tool_call_id
                    && change.path == request.change.path
                    && change.updated_sequence == request.revision.value()
            })
        });
        if remains_current {
            return;
        }
        self.complete_matching_command(owner, |intent| {
            matches!(
                intent,
                DesktopCommandIntent::FileReview {
                    request: pending,
                } if pending == &request
            )
        });
        if let Some(workspace) = self.app.workspaces.get_mut(owner) {
            workspace.file_review = Arc::new(DesktopFileReviewState::Empty);
        }
    }

    fn with_controller<T>(
        &mut self,
        reduce: impl FnOnce(&mut DesktopController, &mut Self) -> T,
    ) -> T {
        let mut controller = std::mem::take(&mut self.connection.controller);
        let result = reduce(&mut controller, self);
        self.connection.controller = controller;
        result
    }

    fn dispatch_platform_result(&mut self, result: PlatformResult, cx: &mut Context<Self>) {
        let transition = self.with_controller(|controller, this| {
            controller.reduce_async(this, DesktopEvent::Platform(result))
        });
        self.apply_transition(transition, cx);
    }

    fn dispatch_timer(&mut self, timer: DesktopTimer, cx: &mut Context<Self>) {
        let transition = self.with_controller(|controller, this| {
            controller.reduce_async(this, DesktopEvent::Timer(timer))
        });
        self.apply_transition(transition, cx);
    }

    fn queue_transition(&mut self, transition: Transition) {
        let (changes, effects) = transition.into_parts();
        assert!(
            changes.is_empty(),
            "queued transitions cannot hide UI changes"
        );
        self.connection.queued_effects.extend(effects);
    }

    fn apply_transition(&mut self, transition: Transition, cx: &mut Context<Self>) {
        let (changes, effects) = transition.into_parts();
        self.refresh_runtime_changes(changes, cx);
        self.connection.queued_effects.extend(effects);
        self.flush_queued_effects(cx);
    }

    fn flush_queued_effects(&mut self, cx: &mut Context<Self>) {
        while let Some(effect) = self.connection.queued_effects.pop_front() {
            self.execute_effect(effect, cx);
        }
    }

    fn execute_effect(&mut self, effect: DesktopEffect, cx: &mut Context<Self>) {
        match effect {
            DesktopEffect::PickPaths { identity, picker } => {
                let options = match picker {
                    DesktopPickerKind::Attachments => PathPromptOptions {
                        files: true,
                        directories: false,
                        multiple: true,
                        prompt: Some("Attach files or images".into()),
                    },
                    DesktopPickerKind::ProjectDirectory => PathPromptOptions {
                        files: false,
                        directories: true,
                        multiple: false,
                        prompt: Some("Choose a project directory".into()),
                    },
                };
                let selection = cx.prompt_for_paths(options);
                cx.spawn(async move |this, cx| {
                    let outcome = match selection.await {
                        Ok(Ok(Some(paths))) => PlatformOutcome::Completed(paths),
                        Ok(Ok(None)) => PlatformOutcome::Cancelled,
                        Ok(Err(_)) | Err(_) => PlatformOutcome::Failed(match picker {
                            DesktopPickerKind::Attachments => {
                                "The file picker could not be opened.".into()
                            }
                            DesktopPickerKind::ProjectDirectory => {
                                "The directory picker could not be opened.".into()
                            }
                        }),
                    };
                    let _ = this.update(cx, |this, cx| {
                        this.dispatch_platform_result(
                            PlatformResult::PathsPicked {
                                identity,
                                picker,
                                outcome,
                            },
                            cx,
                        );
                    });
                })
                .detach();
            }
            DesktopEffect::WriteClipboard { identity, text, .. } => {
                if let Some(text) = text {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                self.dispatch_platform_result(
                    PlatformResult::ClipboardWritten {
                        identity,
                        outcome: PlatformOutcome::Completed(()),
                    },
                    cx,
                );
            }
            DesktopEffect::WritePreferences {
                identity,
                preferences,
            } => {
                let Some(writer) = self.connection.preference_writer.as_ref() else {
                    self.dispatch_platform_result(
                        PlatformResult::PreferencesWritten {
                            identity,
                            outcome: PlatformOutcome::Failed(
                                "Desktop preference writer is unavailable.".into(),
                            ),
                        },
                        cx,
                    );
                    return;
                };
                let completion = writer.schedule(preferences);
                cx.spawn(async move |this, cx| {
                    let outcome = match completion.await {
                        Ok(PreferenceWriteResult::Written) => PlatformOutcome::Completed(()),
                        Ok(PreferenceWriteResult::Superseded) => PlatformOutcome::Cancelled,
                        Ok(PreferenceWriteResult::Failed(message)) => {
                            PlatformOutcome::Failed(message)
                        }
                        Err(_) => PlatformOutcome::Failed(
                            "Desktop preference writer stopped before completion.".into(),
                        ),
                    };
                    let _ = this.update(cx, |this, cx| {
                        this.dispatch_platform_result(
                            PlatformResult::PreferencesWritten { identity, outcome },
                            cx,
                        );
                    });
                })
                .detach();
            }
            DesktopEffect::RequestResync {
                identity,
                command_id,
            } => {
                let outcome = self.connection.runtime_client.as_ref().map_or_else(
                    || PlatformOutcome::Failed("desktop runtime is stopped".into()),
                    |runtime| match runtime.try_resync(command_id) {
                        Ok(()) => PlatformOutcome::Completed(()),
                        Err(error) => PlatformOutcome::Failed(error.to_string()),
                    },
                );
                self.dispatch_platform_result(
                    PlatformResult::ResyncRequested { identity, outcome },
                    cx,
                );
            }
            DesktopEffect::ScheduleTimer { timer, delay } => {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    let _ = this.update(cx, |this, cx| {
                        this.dispatch_timer(timer, cx);
                    });
                })
                .detach();
            }
        }
    }

    fn poll_runtime(&mut self) -> RuntimePoll {
        if self.connection.runtime_client.is_none() {
            return RuntimePoll {
                transition: Transition::default(),
                running: false,
            };
        }
        let mut transition = Transition::default();
        let mut applied = 0;
        while applied < MAX_RUNTIME_UPDATES_PER_FRAME {
            let Some(update) = self.connection.runtime_updates.pop_front() else {
                break;
            };
            let reduced =
                self.with_controller(|controller, this| controller.reduce_runtime(this, update));
            transition.merge(reduced);
            applied += 1;
        }
        RuntimePoll {
            transition,
            running: RuntimeUpdatePort::active_runtime_is_running(self),
        }
    }

    fn apply_runtime_poll(&mut self, mut poll: RuntimePoll, cx: &mut Context<Self>) -> bool {
        let conversation_needs_refresh = self
            .app
            .workspaces
            .active_mut()
            .conversation_controller
            .needs_row_refresh();
        if conversation_needs_refresh && !self.refresh_conversation_rows_at_current_width(cx) {
            poll.transition.merge(Transition::changed(UiRegion::Root));
        }
        self.apply_transition(poll.transition, cx);
        poll.running
    }

    #[cfg(test)]
    fn poll_runtime_for_test(&mut self, cx: &mut Context<Self>) -> bool {
        let poll = self.poll_runtime();
        self.apply_runtime_poll(poll, cx)
    }

    fn refresh_runtime_changes(
        &mut self,
        changes: crate::application::change_set::UiChangeSet,
        cx: &mut Context<Self>,
    ) {
        #[cfg(test)]
        if !changes.is_empty() {
            self.ui.runtime_ui_notification_count += 1;
        }
        if changes.contains(UiRegion::Root) {
            cx.notify();
        }
        if changes.contains(UiRegion::Sessions) {
            self.notify_sessions_pane(cx);
        }
        if changes.contains(UiRegion::Composer) {
            self.notify_composer_pane(cx);
        }
        if changes.contains(UiRegion::Conversation) {
            self.notify_conversation_pane(cx);
        }
        if changes.contains(UiRegion::Inspector) {
            self.notify_inspector_pane(cx);
        } else if changes.contains(UiRegion::InspectorTelemetry) {
            self.schedule_inspector_telemetry_refresh(cx);
        }
        if changes.contains(UiRegion::Toast) {
            self.notify_toast_host(cx);
        }
        if changes.contains(UiRegion::ConversationHeader) {
            self.notify_conversation_header(cx);
        }
        if changes.contains(UiRegion::Modal) {
            self.notify_root_modal_host(cx);
        }
    }

    fn notify_sessions_pane(&self, cx: &mut Context<Self>) {
        let view_model = self.sessions_pane_view_model();
        self.views.sessions_pane.update(cx, |pane, cx| {
            pane.set_view_model(view_model);
            cx.notify();
        });
        self.notify_toast_host(cx);
        self.notify_root_modal_host(cx);
    }

    fn active_composer_running_mode(&self) -> ComposerRunningMode {
        self.app.workspaces.active().composer_running_mode
    }

    fn set_active_composer_running_mode(
        &mut self,
        mode: ComposerRunningMode,
        cx: &mut Context<Self>,
    ) {
        self.app.workspaces.active_mut().composer_running_mode = mode;
        self.notify_composer_pane(cx);
    }

    fn notify_composer_pane(&self, cx: &mut Context<Self>) {
        let view_model = self.composer_pane_view_model();
        self.views.composer_pane.update(cx, |pane, cx| {
            pane.set_view_model(view_model);
            cx.notify();
        });
    }

    fn notify_inspector_pane(&mut self, cx: &mut Context<Self>) {
        self.ui.inspector_telemetry_last_refresh = Some(Instant::now());
        self.ui.inspector_telemetry_refresh_deadline = None;
        self.push_inspector_pane_view_model(cx);
        self.notify_toast_host(cx);
    }

    fn push_inspector_pane_view_model(&self, cx: &mut Context<Self>) {
        let view_model = self.inspector_pane_view_model();
        self.views.inspector_pane.update(cx, |pane, cx| {
            pane.set_view_model(view_model);
            cx.notify();
        });
    }

    fn schedule_inspector_telemetry_refresh(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let delay =
            inspector_telemetry_refresh_delay(self.ui.inspector_telemetry_last_refresh, now);
        if delay.is_zero() {
            self.ui.inspector_telemetry_last_refresh = Some(now);
            self.ui.inspector_telemetry_refresh_deadline = None;
            self.push_inspector_pane_view_model(cx);
            return;
        }

        let deadline = now + delay;
        if self
            .ui
            .inspector_telemetry_refresh_deadline
            .is_some_and(|scheduled| scheduled <= deadline)
        {
            return;
        }
        self.ui.inspector_telemetry_refresh_deadline = Some(deadline);
        let owner = self.app.workspaces.active_key().clone();
        match self.connection.controller.schedule_timer(
            owner,
            DesktopTimerKind::InspectorTelemetryRefresh,
            delay,
        ) {
            Ok(transition) => self.apply_transition(transition, cx),
            Err(error) => {
                self.ui.inspector_telemetry_refresh_deadline = None;
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.notify_toast_host(cx);
            }
        }
    }

    fn notify_toast_host(&self, cx: &mut Context<Self>) {
        let notice_owner: Arc<str> = match self.app.workspaces.active_key() {
            WorkspaceKey::Home => Arc::from("workspace:home"),
            WorkspaceKey::Session(session_id) => {
                Arc::from(format!("session:{}", session_id.as_str()))
            }
        };
        let notice = self
            .app
            .workspaces
            .active()
            .preference_notice
            .as_ref()
            .map(|message| ToastNotice {
                session_id: notice_owner,
                revision: self.app.workspaces.active().preference_notice_revision,
                message: Arc::from(message.as_str()),
            });
        self.views.toast_host.update(cx, |host, cx| {
            host.observe_notice(notice, cx);
        });
    }

    fn notify_conversation_header(&self, cx: &mut Context<Self>) {
        let view_model = self.conversation_header_view_model();
        self.views
            .conversation_header
            .update(cx, |conversation_header, cx| {
                conversation_header.set_view_model(view_model);
                cx.notify();
            });
    }

    fn notify_root_modal_host(&self, cx: &mut Context<Self>) {
        let view_model = self.root_modal_view_model();
        self.views.root_modal_host.update(cx, |host, cx| {
            host.set_view_model(view_model);
            cx.notify();
        });
    }

    fn notify_center_drawer_host(&self, cx: &mut Context<Self>) {
        let view_model = self.center_drawer_view_model();
        self.views.center_drawer_host.update(cx, |host, cx| {
            host.set_view_model(view_model);
            cx.notify();
        });
    }

    fn schedule_preferences(&mut self) {
        if self.connection.preference_writer.is_none() {
            return;
        }
        let owner = self.app.workspaces.active_key().clone();
        match self
            .connection
            .controller
            .write_preferences(owner, self.app.preferences.clone())
        {
            Ok(transition) => self.queue_transition(transition),
            Err(error) => self
                .app
                .workspaces
                .active_mut()
                .set_preference_notice(error.to_string()),
        }
    }

    fn remember_thinking_selection(&mut self, session_id: &str, selection: DesktopThinkingLevel) {
        if self
            .app
            .preferences
            .set_thinking_level_for_session(session_id, selection)
        {
            self.schedule_preferences();
        }
    }

    fn reconcile_thinking_selection_with_project(&mut self) {
        let owner = self.app.workspaces.active_key().clone();
        self.reconcile_thinking_selection_for(&owner);
    }

    fn toggle_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size();
        let dockable = self
            .resolve_layout(
                u32::from(viewport.width),
                u32::from(viewport.height),
                PanelVisibility {
                    sessions: true,
                    context: self.app.preferences.context_panel_visible,
                },
            )
            .sidebar
            .is_some();
        if !dockable {
            if self.ui.active_drawer == Some(CenterDrawerKind::Sessions) {
                self.dismiss_drawer(window, cx, true);
            } else {
                self.activate_drawer(CenterDrawerKind::Sessions, window, cx);
            }
            return;
        }
        self.app.preferences.sessions_panel_visible = !self.app.preferences.sessions_panel_visible;
        let layout = self.layout(window);
        self.ui.focus.reconcile_layout(layout);
        if self.ui.focus.active() == FocusTarget::Composer {
            self.focus_composer_input(window, cx);
        }
        self.schedule_preferences();
        self.notify_inspector_pane(cx);
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn visible_conversation_count(&self) -> usize {
        self.app
            .workspaces
            .active()
            .projection
            .as_ref()
            .map_or(0, |projection| {
                projection.conversation().blocks().len()
                    + usize::from(self.app.workspaces.active().composer.submitted().is_some())
                    + projection.messages().len()
                    + projection.tools().len()
            })
    }

    fn toggle_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size();
        let dockable = self
            .resolve_layout(
                u32::from(viewport.width),
                u32::from(viewport.height),
                PanelVisibility {
                    sessions: self.app.preferences.sessions_panel_visible,
                    context: true,
                },
            )
            .inspector
            .is_some();
        if !dockable {
            if self.ui.active_drawer == Some(CenterDrawerKind::Inspector) {
                self.dismiss_drawer(window, cx, true);
            } else {
                self.activate_drawer(CenterDrawerKind::Inspector, window, cx);
            }
            return;
        }
        self.app.preferences.context_panel_visible = !self.app.preferences.context_panel_visible;
        let layout = self.layout(window);
        self.ui.focus.reconcile_layout(layout);
        if self.ui.focus.active() == FocusTarget::Composer {
            self.focus_composer_input(window, cx);
        }
        self.schedule_preferences();
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn reserve_command(&mut self, intent: DesktopCommandIntent) -> Option<u64> {
        commands::reserve_command(self, intent)
    }

    fn submit_composer(&mut self, cx: &mut Context<Self>) {
        let selected_model_supports_images = {
            let workspace = self.app.workspaces.active();
            workspace
                .project
                .models
                .iter()
                .find(|model| model.id == workspace.project.selected_model_id)
                .is_some_and(|model| model.supports_images)
        };
        if !self.app.workspaces.active().composer_attachments.is_empty()
            && !selected_model_supports_images
        {
            self.app.workspaces.active_mut().set_preference_notice(
                "Selected model does not support image attachments; the draft was retained.".into(),
            );
            self.notify_composer_pane(cx);
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        let intent = DesktopCommandIntent::Prompt;
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let has_attachments = !self
            .app
            .workspaces
            .active_mut()
            .composer_attachments
            .is_empty();
        let payload = match self
            .app
            .workspaces
            .active_mut()
            .composer
            .begin_submit_with_attachments(
                command_id,
                ComposerSubmissionKind::Prompt,
                has_attachments,
            ) {
            Ok(payload) => payload.to_owned(),
            Err(error) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.notify_composer_pane(cx);
                self.notify_toast_host(cx);
                cx.notify();
                return;
            }
        };
        let thinking_level = self
            .app
            .workspaces
            .active_mut()
            .thinking_selection
            .explicit();
        let target = self.app.workspaces.active_mut().prompt_target();
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_submit_prompt_with_attachments(
                        command_id,
                        target,
                        &payload,
                        &self.app.workspaces.active_mut().composer_attachments,
                        thinking_level,
                    )
                    .map_err(|error| error.to_string())
            },
        );
        if let Err(message) = admission {
            self.complete_active_command(command_id, &intent);
            let _ = self
                .app
                .workspaces
                .active_mut()
                .composer
                .rejected(command_id, message);
        }
        self.notify_composer_pane(cx);
        self.notify_inspector_pane(cx);
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn submit_primary_composer(&mut self, cx: &mut Context<Self>) {
        if self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
        {
            self.submit_active_control(self.active_composer_running_mode().submission_kind(), cx);
        } else {
            self.submit_composer(cx);
        }
    }

    fn choose_composer_attachments(&mut self, cx: &mut Context<Self>) {
        if let Some(reason) = self.composer_attachment_disabled_reason() {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice(reason.to_string());
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        let owner = self.app.workspaces.active_key().clone();
        match self
            .connection
            .controller
            .pick_paths(owner, DesktopPickerKind::Attachments)
        {
            Ok(transition) => self.apply_transition(transition, cx),
            Err(error) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.notify_toast_host(cx);
            }
        }
    }

    fn choose_project_directory(&mut self, cx: &mut Context<Self>) {
        if !self
            .app
            .workspaces
            .active_mut()
            .project_directory_editable()
        {
            return;
        }
        let owner = self.app.workspaces.active_key().clone();
        match self
            .connection
            .controller
            .pick_paths(owner, DesktopPickerKind::ProjectDirectory)
        {
            Ok(transition) => self.apply_transition(transition, cx),
            Err(error) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.notify_toast_host(cx);
            }
        }
    }

    fn clear_project_directory(&mut self, cx: &mut Context<Self>) -> bool {
        if !self
            .app
            .workspaces
            .active_mut()
            .project_directory_editable()
        {
            return false;
        }
        self.app.workspaces.active_mut().draft_workspace_selection =
            self.projectless_workspace_selection.clone();
        self.notify_composer_pane(cx);
        cx.notify();
        true
    }

    fn remove_composer_attachment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.app.workspaces.active_mut().composer_attachments.len() {
            self.app
                .workspaces
                .active_mut()
                .composer_attachments
                .remove(index);
            self.notify_composer_pane(cx);
            cx.notify();
        }
    }

    fn composer_attachment_disabled_reason(&self) -> Option<&'static str> {
        let supports_images = self
            .app
            .workspaces
            .active()
            .project
            .models
            .iter()
            .find(|model| model.id == self.app.workspaces.active().project.selected_model_id)
            .is_some_and(|model| model.supports_images);
        if !supports_images {
            return Some("Selected model does not support image attachments.");
        }
        let snapshot = self
            .app
            .workspaces
            .active()
            .projection
            .as_ref()
            .map(DesktopProjection::snapshot);
        if snapshot.is_some_and(|snapshot| snapshot.active_operation.is_some()) {
            return Some("Attachments are unavailable while an operation is running.");
        }
        if matches!(
            self.app.workspaces.active().composer.admission(),
            ComposerAdmission::Pending { .. }
        ) || self.app.workspaces.active().composer.submitted().is_some()
        {
            return Some("Attachments are unavailable while a prompt is starting.");
        }
        None
    }

    fn submit_active_control(&mut self, kind: ComposerSubmissionKind, cx: &mut Context<Self>) {
        if !self
            .app
            .workspaces
            .active_mut()
            .composer_attachments
            .is_empty()
        {
            self.app.workspaces.active_mut().set_preference_notice(
                "Attachments cannot be added to a running operation; the draft was retained."
                    .into(),
            );
            self.notify_composer_pane(cx);
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        if kind == ComposerSubmissionKind::Prompt {
            self.app.workspaces.active_mut().set_preference_notice(
                "Prompt submissions must use the idle composer action.".into(),
            );
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        let intent = match kind {
            ComposerSubmissionKind::Steer => DesktopCommandIntent::Steer,
            ComposerSubmissionKind::FollowUp => DesktopCommandIntent::FollowUp,
            ComposerSubmissionKind::Prompt => {
                unreachable!("prompt submission was rejected before command reservation")
            }
        };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let payload = match self
            .app
            .workspaces
            .active_mut()
            .composer
            .begin_submit(command_id, kind)
        {
            Ok(payload) => payload.to_owned(),
            Err(error) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.notify_composer_pane(cx);
                self.notify_toast_host(cx);
                cx.notify();
                return;
            }
        };
        let session_id = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.clone());
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                let Some(session_id) = session_id.as_deref() else {
                    return Err("desktop session is unavailable".to_owned());
                };
                let result = match kind {
                    ComposerSubmissionKind::Steer => {
                        runtime.try_steer_for_session(command_id, session_id, &payload)
                    }
                    ComposerSubmissionKind::FollowUp => {
                        runtime.try_follow_up_for_session(command_id, session_id, &payload)
                    }
                    ComposerSubmissionKind::Prompt => {
                        unreachable!("prompt submission was rejected before runtime admission")
                    }
                };
                result.map_err(|error| error.to_string())
            },
        );
        if let Err(message) = admission {
            self.complete_active_command(command_id, &intent);
            let _ = self
                .app
                .workspaces
                .active_mut()
                .composer
                .rejected(command_id, message);
        }
        self.notify_composer_pane(cx);
        self.notify_inspector_pane(cx);
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn abort_active_operation(&mut self, cx: &mut Context<Self>) {
        if self.active_command_contains_where(|intent| {
            matches!(intent, DesktopCommandIntent::Abort { .. })
        }) {
            return;
        }
        let Some(operation_id) = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .and_then(|projection| projection.snapshot().active_operation.clone())
        else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("No active operation is available to abort.".into());
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let session_id = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .expect("an active operation always belongs to a session projection")
            .snapshot()
            .session
            .session_id
            .clone();
        let intent = DesktopCommandIntent::Abort { operation_id };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_abort_for_session(command_id, &session_id)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice("Abort requested…".into());
            }
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.notify_toast_host(cx);
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn reload_local_resources(&mut self, cx: &mut Context<Self>) {
        let intent = DesktopCommandIntent::Reload;
        if self.active_command_contains(&intent) {
            return;
        }
        if self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
            || self
                .app
                .workspaces
                .active_mut()
                .composer
                .submitted()
                .is_some()
        {
            self.app.workspaces.active_mut().set_preference_notice(
                "Reload is available only while the runtime is idle.".into(),
            );
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let target = self.app.workspaces.active_mut().runtime_owner_target();
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_reload(command_id, target)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice("Reloading local resources…".into());
            }
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.notify_toast_host(cx);
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn submit_recovery_action(
        &mut self,
        identity: DesktopRecoveryIdentity,
        action: DesktopRecoveryAction,
        cx: &mut Context<Self>,
    ) {
        if self.active_command_contains_where(|intent| {
            matches!(intent, DesktopCommandIntent::Recovery { .. })
        }) {
            return;
        }
        if self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
            || self
                .app
                .workspaces
                .active_mut()
                .composer
                .submitted()
                .is_some()
        {
            self.app.workspaces.active_mut().set_preference_notice(
                "Recovery actions are available only while the runtime is idle.".into(),
            );
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        let intent = DesktopCommandIntent::Recovery {
            recovery_id: identity.recovery_id.clone(),
            action,
        };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            cx.notify();
            return;
        };
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                let result = match action {
                    DesktopRecoveryAction::Retry => {
                        runtime.try_retry_recovery(command_id, &identity)
                    }
                    DesktopRecoveryAction::MarkFailed => runtime.try_resolve_recovery(
                        command_id,
                        &identity,
                        CodingAgentRecoveryResolution::Failed,
                    ),
                    DesktopRecoveryAction::Abort => runtime.try_resolve_recovery(
                        command_id,
                        &identity,
                        CodingAgentRecoveryResolution::Aborted,
                    ),
                };
                result.map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(format!(
                        "Submitting recovery {}…",
                        recovery_action_label(action)
                    ));
            }
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.notify_inspector_pane(cx);
        cx.notify();
    }

    fn submit_selection(
        &mut self,
        selection: DesktopRuntimeSelectionKind,
        id: String,
        cx: &mut Context<Self>,
    ) {
        let selected_profile_id = {
            let workspace = self.app.workspaces.active();
            workspace
                .projection
                .as_ref()
                .map(|projection| {
                    projection
                        .snapshot()
                        .session
                        .default_agent_profile_id
                        .as_str()
                })
                .unwrap_or(workspace.project.default_agent_profile_id.as_str())
                .to_owned()
        };
        let already_selected = match selection {
            DesktopRuntimeSelectionKind::Model => {
                id == self.app.workspaces.active_mut().project.selected_model_id
            }
            DesktopRuntimeSelectionKind::SessionProfile => id == selected_profile_id,
        };
        if already_selected {
            return;
        }
        if self.active_command_contains_where(|intent| {
            matches!(intent, DesktopCommandIntent::Selection(_))
        }) {
            return;
        }
        if self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
            || self
                .app
                .workspaces
                .active_mut()
                .composer
                .submitted()
                .is_some()
        {
            self.app.workspaces.active_mut().set_preference_notice(
                "Model and profile selection is available only while idle.".into(),
            );
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        let intent = DesktopCommandIntent::Selection(selection);
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let target = self.app.workspaces.active_mut().runtime_owner_target();
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                let result = match selection {
                    DesktopRuntimeSelectionKind::Model => runtime.try_select_model(
                        command_id,
                        target,
                        &id,
                        self.app
                            .workspaces
                            .active_mut()
                            .thinking_selection
                            .explicit(),
                    ),
                    DesktopRuntimeSelectionKind::SessionProfile => {
                        runtime.try_select_session_profile(command_id, target, &id)
                    }
                };
                result.map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice("Applying selection…".into());
            }
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.notify_toast_host(cx);
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn select_thinking_level(&mut self, selection: DesktopThinkingLevel, cx: &mut Context<Self>) {
        let options = {
            let workspace = self.app.workspaces.active();
            conversation_header_thinking_menu(
                workspace
                    .project
                    .models
                    .iter()
                    .find(|model| model.id == workspace.project.selected_model_id),
            )
        };
        if !options.iter().any(|option| option.selection == selection) {
            return;
        }
        if self.app.workspaces.active_mut().thinking_selection == selection {
            return;
        }
        self.app.workspaces.active_mut().thinking_selection = selection;
        self.app.workspaces.active_mut().thinking_hint = None;
        let session_id = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.clone());
        if let Some(session_id) = session_id.as_deref() {
            self.remember_thinking_selection(session_id, selection);
        }
        let label = self.app.workspaces.active_mut().thinking_selection.label(
            self.app
                .workspaces
                .active_mut()
                .project
                .settings
                .default_thinking_level
                .as_deref(),
        );
        self.app
            .workspaces
            .active_mut()
            .set_preference_notice(format!(
                "{} will use thinking {label}.",
                if session_id.is_some() {
                    "This session"
                } else {
                    "The next session"
                }
            ));
        self.notify_toast_host(cx);
        self.notify_conversation_header(cx);
        self.push_inspector_pane_view_model(cx);
        cx.notify();
    }

    fn cycle_thinking_selection(&mut self, cx: &mut Context<Self>) {
        let options = {
            let workspace = self.app.workspaces.active();
            conversation_header_thinking_menu(
                workspace
                    .project
                    .models
                    .iter()
                    .find(|model| model.id == workspace.project.selected_model_id),
            )
        };
        let Some(next) = options
            .iter()
            .position(|option| {
                option.selection == self.app.workspaces.active_mut().thinking_selection
            })
            .map(|index| options[(index + 1) % options.len()].selection)
            .or_else(|| options.first().map(|option| option.selection))
        else {
            return;
        };
        self.select_thinking_level(next, cx);
    }

    fn decide_tool_authorization(
        &mut self,
        identity: ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
        cx: &mut Context<Self>,
    ) {
        if self.active_command_contains_where(|intent| {
            matches!(intent, DesktopCommandIntent::Authorization { .. })
        }) {
            return;
        }
        let intent = DesktopCommandIntent::Authorization {
            authorization_id: identity.authorization_id.clone(),
            operation_id: identity.operation_id.clone(),
        };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let admission = self.connection.runtime_client.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_decide_tool_authorization(command_id, &identity, decision)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice("Authorization decision pending…".into());
            }
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.notify_toast_host(cx);
        self.notify_root_modal_host(cx);
        cx.notify();
    }

    fn copy_selected_conversation(&mut self, cx: &mut Context<Self>) {
        let workspace = self.app.workspaces.active_mut();
        let Some(projection) = workspace.projection.as_ref() else {
            return;
        };
        let Some(text) = workspace
            .conversation_controller
            .copy_selected(projection.conversation())
        else {
            self.app.workspaces.active_mut().set_preference_notice(
                "Select a committed conversation block before copying.".into(),
            );
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        self.write_clipboard(
            Some(text),
            ClipboardFeedback::ConversationAnnouncement("Selected message copied.".into()),
            cx,
        );
    }

    fn conversation_full_message_view(
        &self,
        block_id: &str,
    ) -> Option<ConversationFullMessageView> {
        let projection = self.app.workspaces.active().projection.as_ref()?;
        if let Some(block) = projection.conversation().block(block_id) {
            return Some(ConversationFullMessageView {
                block_id: block_id.to_owned(),
                title: Arc::from(block.title.as_str()),
                text: Arc::from(block.copy_text()),
                source_truncated: block.truncated
                    || block.text.len().saturating_add(block.detail.len()) > MAX_COPY_BYTES,
            });
        }
        if let Some(message) = self
            .app
            .workspaces
            .active()
            .projection
            .as_ref()?
            .messages()
            .iter()
            .find(|message| message_conversation_block_id(message) == block_id)
        {
            return Some(ConversationFullMessageView {
                block_id: block_id.to_owned(),
                title: Arc::from("Assistant · live"),
                text: Arc::from(conversation_copy_text(&message.text, &message.thinking)),
                source_truncated: message.truncated
                    || message.text.len().saturating_add(message.thinking.len()) > MAX_COPY_BYTES,
            });
        }
        if let Some(tool) = projection
            .tools()
            .iter()
            .find(|tool| tool_conversation_block_id(tool) == block_id)
        {
            return Some(ConversationFullMessageView {
                block_id: block_id.to_owned(),
                title: Arc::from(format!("Tool · {}", tool.name)),
                text: Arc::from(conversation_copy_text(&tool.detail, &tool.arguments)),
                source_truncated: tool.truncated
                    || tool.detail.len().saturating_add(tool.arguments.len()) > MAX_COPY_BYTES,
            });
        }
        self.app
            .workspaces
            .active()
            .conversation_controller
            .row_for_block(block_id)
            .map(|row| ConversationFullMessageView {
                block_id: block_id.to_owned(),
                title: Arc::clone(&row.title),
                text: Arc::from(conversation_copy_text(&row.text, &row.detail)),
                source_truncated: row.preview_truncated,
            })
    }

    fn copy_conversation_row(&mut self, block_id: &str, cx: &mut Context<Self>) {
        let Some(message) = self.conversation_full_message_view(block_id) else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Message is no longer available to copy.".into());
            self.notify_toast_host(cx);
            return;
        };
        self.write_clipboard(
            Some(message.text.to_string()),
            ClipboardFeedback::ConversationAnnouncement("Message copied.".into()),
            cx,
        );
    }

    fn copy_tool_details(&mut self, block_id: &str, cx: &mut Context<Self>) {
        let Some(row) = self
            .app
            .workspaces
            .active_mut()
            .conversation_controller
            .row_for_block(block_id)
        else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Tool details are no longer available to copy.".into());
            self.notify_toast_host(cx);
            return;
        };
        self.write_clipboard(
            Some(conversation_pane::tool_detail_copy_text(
                &row.title,
                &row.detail,
                &row.text,
            )),
            ClipboardFeedback::ConversationAnnouncement("Tool details copied.".into()),
            cx,
        );
    }

    fn announce_conversation_copy(&mut self, message: &str, cx: &mut Context<Self>) {
        self.write_clipboard(
            None,
            ClipboardFeedback::ConversationAnnouncement(message.into()),
            cx,
        );
    }

    fn write_clipboard(
        &mut self,
        text: Option<String>,
        feedback: ClipboardFeedback,
        cx: &mut Context<Self>,
    ) {
        let owner = self.app.workspaces.active_key().clone();
        match self
            .connection
            .controller
            .write_clipboard(owner, text, feedback)
        {
            Ok(transition) => self.apply_transition(transition, cx),
            Err(error) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.notify_toast_host(cx);
            }
        }
    }

    fn open_full_conversation_message(
        &mut self,
        block_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self.conversation_full_message_view(block_id) else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Message is no longer available to open.".into());
            self.notify_toast_host(cx);
            return;
        };
        tracing::trace!(
            target: "desktop",
            event = "message_full_view_open",
            block_id = message.block_id,
            bytes = message.text.len(),
        );
        self.ui.conversation_full_message = Some(message);
        self.activate_modal(DesktopModalKind::FullMessage, window, cx);
    }

    fn close_full_conversation_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ui.conversation_full_message = None;
        self.dismiss_modal(window, cx);
    }

    fn toggle_conversation_details(&mut self, block_id: &str, cx: &mut Context<Self>) {
        self.app
            .workspaces
            .active_mut()
            .conversation_controller
            .toggle_details(block_id);
        if !self.refresh_conversation_rows_at_current_width(cx) {
            cx.notify();
        }
    }

    pub(super) fn select_adjacent_conversation(&mut self, reverse: bool, cx: &mut Context<Self>) {
        let workspace = &mut self.app.workspaces.active_mut();
        let Some(projection) = workspace.projection.as_ref() else {
            return;
        };
        let row_count = workspace.conversation_controller.row_count();
        if row_count == 0 {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("The conversation is empty.".into());
            self.notify_toast_host(cx);
            return;
        }
        let current_index = workspace
            .conversation_controller
            .selected_block_id()
            .map(str::to_owned)
            .and_then(|selected| workspace.conversation_controller.row_index(&selected));
        let next_index = adjacent_conversation_index(row_count, current_index, reverse)
            .expect("non-empty conversation has an adjacent selection");
        let row = workspace
            .conversation_controller
            .row_at(next_index)
            .expect("adjacent index is inside the rendered rows");
        workspace.conversation_controller.select_row(
            row.item_key.row_id().to_owned(),
            row.durable,
            projection.conversation(),
        );
        workspace.conversation_controller.scroll_to_row(
            next_index,
            if reverse {
                ScrollStrategy::Top
            } else {
                ScrollStrategy::Bottom
            },
        );
        self.notify_conversation_pane(cx);
        self.notify_conversation_header(cx);
    }

    fn copy_keyboard_selected_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(block_id) = self
            .app
            .workspaces
            .active_mut()
            .conversation_controller
            .selected_block_id()
            .map(str::to_owned)
        else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Select a conversation message before copying.".into());
            self.notify_toast_host(cx);
            return;
        };
        self.copy_conversation_row(&block_id, cx);
    }

    fn toggle_keyboard_selected_conversation_details(&mut self, cx: &mut Context<Self>) {
        let Some(block_id) = self
            .app
            .workspaces
            .active_mut()
            .conversation_controller
            .selected_block_id()
            .map(str::to_owned)
        else {
            return;
        };
        let has_details = self
            .app
            .workspaces
            .active_mut()
            .conversation_controller
            .row_for_block(&block_id)
            .is_some_and(|row| !row.detail.is_empty());
        if has_details {
            self.toggle_conversation_details(&block_id, cx);
        }
    }

    fn request_file_review(
        &mut self,
        request: CodingAgentFileReviewRequest,
        cx: &mut Context<Self>,
    ) {
        let intent = DesktopCommandIntent::FileReview {
            request: request.clone(),
        };
        if self.active_command_contains_where(|pending| {
            matches!(pending, DesktopCommandIntent::FileReview { .. })
        }) {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Another file review is already pending.".into());
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let Some(session_id) = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.clone())
        else {
            self.complete_active_command(command_id, &intent);
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("File review requires an open session.".into());
            self.notify_inspector_pane(cx);
            cx.notify();
            return;
        };
        let admission = self
            .connection
            .runtime_client
            .as_ref()
            .ok_or_else(|| "desktop runtime is unavailable".to_owned())
            .and_then(|runtime| {
                runtime
                    .try_review_changed_file(command_id, &session_id, &request)
                    .map_err(|error| error.to_string())
            });
        match admission {
            Ok(()) => {
                self.app.workspaces.active_mut().file_review =
                    Arc::new(DesktopFileReviewState::Loading(request));
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice("Loading changed-file review…".into());
            }
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.notify_inspector_pane(cx);
        cx.notify();
    }

    fn copy_review_path(&mut self, cx: &mut Context<Self>) {
        let DesktopFileReviewState::Ready(document) =
            self.app.workspaces.active_mut().file_review.as_ref()
        else {
            self.app.workspaces.active_mut().set_preference_notice(
                "Load a changed-file review before copying its path.".into(),
            );
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let export = document.path_clipboard_export();
        let notice = if export.truncated {
            "Bounded changed-file path copied (truncated).".into()
        } else {
            "Changed-file path copied.".into()
        };
        self.write_clipboard(Some(export.text), ClipboardFeedback::Notice(notice), cx);
    }

    fn copy_file_review(&mut self, cx: &mut Context<Self>) {
        let DesktopFileReviewState::Ready(document) =
            self.app.workspaces.active_mut().file_review.as_ref()
        else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Load a changed-file review before copying it.".into());
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let export = document.clipboard_export();
        let notice = if export.truncated {
            "Bounded file review copied (truncated at the clipboard limit).".into()
        } else {
            "File review copied.".into()
        };
        self.write_clipboard(Some(export.text), ClipboardFeedback::Notice(notice), cx);
    }

    fn open_review_in_external_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.app.preferences.external_editor.clone() else {
            self.app.workspaces.active_mut().set_preference_notice(
                "Configure desktop.external_editor with a program and literal argv first.".into(),
            );
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let DesktopFileReviewState::Ready(document) =
            self.app.workspaces.active_mut().file_review.as_ref()
        else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("Load a changed-file review before opening it.".into());
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let Some(target) = document.external_editor_target.clone() else {
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("This review has no external-editor target.".into());
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let project_relative_path = target.project_relative_path().to_owned();
        let intent = DesktopCommandIntent::ExternalEditor {
            project_relative_path: project_relative_path.clone(),
        };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let Some(session_id) = self
            .app
            .workspaces
            .active_mut()
            .projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.clone())
        else {
            self.complete_active_command(command_id, &intent);
            self.app
                .workspaces
                .active_mut()
                .set_preference_notice("External editor requires an open session.".into());
            self.notify_inspector_pane(cx);
            cx.notify();
            return;
        };
        let admission = self
            .connection
            .runtime_client
            .as_ref()
            .ok_or_else(|| "desktop runtime is unavailable".to_owned())
            .and_then(|runtime| {
                runtime
                    .try_open_external_editor(command_id, &session_id, &target, &editor)
                    .map_err(|error| error.to_string())
            });
        match admission {
            Ok(()) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(format!(
                        "Validating {} before editor launch…",
                        truncate_label(&project_relative_path, 48)
                    ));
            }
            Err(message) => {
                self.complete_active_command(command_id, &intent);
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(message);
            }
        }
        self.notify_inspector_pane(cx);
        cx.notify();
    }

    fn activate_modal(
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
        }
        self.notify_conversation_header(cx);
        self.notify_root_modal_host(cx);
        cx.notify();
    }

    fn dismiss_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ui.active_modal = None;
        self.ui.focus.close_modal(self.layout(window));
        self.focus_active_target(window, cx);
        self.notify_conversation_header(cx);
        self.notify_root_modal_host(cx);
        cx.notify();
    }

    fn activate_drawer(
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
        self.notify_sessions_pane(cx);
        self.notify_inspector_pane(cx);
        self.notify_conversation_header(cx);
        self.notify_center_drawer_host(cx);
        cx.notify();
    }

    fn dismiss_drawer(&mut self, window: &mut Window, cx: &mut Context<Self>, restore_focus: bool) {
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
        self.notify_sessions_pane(cx);
        self.notify_inspector_pane(cx);
        self.notify_conversation_header(cx);
        self.notify_center_drawer_host(cx);
        cx.notify();
    }

    fn reconcile_authorization_modal(
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

    fn focus_target(&mut self, target: FocusTarget, window: &mut Window, cx: &mut Context<Self>) {
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

    fn cycle_focus(&mut self, reverse: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.ui.active_modal.is_some() {
            self.focus_active_target(window, cx);
            return;
        }
        self.ui.focus.cycle(self.layout(window), reverse);
        self.focus_active_target(window, cx);
        cx.notify();
    }

    fn root_action_blocked_by_modal(
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
            }
            .into(),
        );
        self.focus_active_target(window, cx);
        cx.notify();
        true
    }

    fn follow_latest(&mut self, cx: &mut Context<Self>) {
        let visible_count = self.visible_conversation_count();
        self.app
            .workspaces
            .active_mut()
            .conversation_controller
            .follow_latest(visible_count);
        self.notify_conversation_pane(cx);
        self.notify_conversation_header(cx);
    }

    fn reconcile_conversation_scroll(&mut self, cx: &mut Context<Self>) {
        if self
            .app
            .workspaces
            .active_mut()
            .conversation_controller
            .reconcile_scroll()
        {
            self.notify_conversation_pane(cx);
            self.notify_conversation_header(cx);
        }
    }

    fn review_next_file(&mut self, cx: &mut Context<Self>) {
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

    fn submit_latest_recovery(&mut self, action: DesktopRecoveryAction, cx: &mut Context<Self>) {
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
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        self.submit_recovery_action(identity, action, cx);
    }

    fn execute_palette_command(
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
                if self
                    .app
                    .workspaces
                    .active_mut()
                    .projection
                    .as_ref()
                    .is_some_and(|projection| projection.snapshot().active_operation.is_some())
                {
                    self.submit_active_control(ComposerSubmissionKind::Steer, cx);
                } else {
                    self.submit_composer(cx);
                }
            }
            DesktopPaletteCommand::SteerOperation => {
                self.submit_active_control(ComposerSubmissionKind::Steer, cx);
            }
            DesktopPaletteCommand::FollowUpOperation => {
                self.submit_active_control(ComposerSubmissionKind::FollowUp, cx);
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
                self.notify_toast_host(cx);
                cx.notify();
            }
        }
    }

    fn on_open_command_palette(
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
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        self.ui.command_palette.open();
        self.activate_modal(DesktopModalKind::CommandPalette, window, cx);
    }

    fn on_open_file_surface(
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

    fn on_new_session(&mut self, _: &NewSession, window: &mut Window, cx: &mut Context<Self>) {
        if self.root_action_blocked_by_modal(window, cx) {
            return;
        }
        self.create_session(cx);
    }

    fn on_focus_composer(
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

    fn on_submit_composer(
        &mut self,
        _: &SubmitComposer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.root_action_blocked_by_modal(window, cx) {
            return;
        }
        self.submit_primary_composer(cx);
    }

    fn on_abort_active_operation(
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

    fn on_escape_hierarchy(
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

    fn on_follow_latest_output(
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

    fn on_toggle_inspector_panel(
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

    fn on_focus_next_region(
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

    fn on_focus_previous_region(
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

    fn on_select_previous_conversation(
        &mut self,
        _: &SelectPreviousConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.root_action_blocked_by_modal(window, cx) {
            self.select_adjacent_conversation(true, cx);
        }
    }

    fn on_select_next_conversation(
        &mut self,
        _: &SelectNextConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.root_action_blocked_by_modal(window, cx) {
            self.select_adjacent_conversation(false, cx);
        }
    }

    fn on_copy_selected_conversation(
        &mut self,
        _: &CopySelectedConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.root_action_blocked_by_modal(window, cx) {
            self.copy_keyboard_selected_conversation(cx);
        }
    }

    fn on_toggle_selected_conversation_details(
        &mut self,
        _: &ToggleSelectedConversationDetails,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.root_action_blocked_by_modal(window, cx) {
            self.toggle_keyboard_selected_conversation_details(cx);
        }
    }

    fn on_palette_previous(&mut self, _: &PalettePrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.ui.command_palette.move_selection(true);
        self.notify_root_modal_host(cx);
        cx.notify();
    }

    fn on_palette_next(&mut self, _: &PaletteNext, _: &mut Window, cx: &mut Context<Self>) {
        self.ui.command_palette.move_selection(false);
        self.notify_root_modal_host(cx);
        cx.notify();
    }

    fn on_palette_confirm(
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

    fn decide_current_authorization(
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

    fn on_authorization_deny(
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

    fn on_authorization_allow_once(
        &mut self,
        _: &AuthorizationAllowOnce,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.decide_current_authorization(ToolAuthorizationDecision::AllowOnce, cx);
    }

    fn on_authorization_allow_for_operation(
        &mut self,
        _: &AuthorizationAllowForOperation,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.decide_current_authorization(ToolAuthorizationDecision::AllowForOperation, cx);
    }

    fn on_trap_overlay_focus(
        &mut self,
        _: &TrapOverlayFocus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_active_target(window, cx);
    }

    fn submit_conversation_row_measurement(
        &mut self,
        measurement: &ConversationRowMeasurement,
        cx: &mut Context<Self>,
    ) {
        let workspace = &mut self.app.workspaces.active_mut();
        let Some(projection) = workspace.projection.as_ref() else {
            return;
        };
        let source = ConversationSource::new(projection, workspace.composer.submitted());
        let outcome = workspace
            .conversation_controller
            .submit_row_measurement(&source, measurement);
        self.schedule_conversation_height_refresh(outcome.refresh, cx);
        if outcome.pane_dirty {
            self.notify_conversation_pane(cx);
        }
    }

    fn refresh_conversation_rows_at_width(&mut self, layout_width: u32, cx: &mut Context<Self>) {
        let workspace = &mut self.app.workspaces.active_mut();
        let Some(projection) = workspace.projection.as_ref() else {
            return;
        };
        let pane_dirty = workspace.conversation_controller.needs_row_refresh()
            || workspace.conversation_controller.active_width_bucket() != Some(layout_width);
        let source = ConversationSource::new(projection, workspace.composer.submitted());
        let refresh = workspace
            .conversation_controller
            .prepare_rows(&source, layout_width);
        if pane_dirty {
            self.notify_conversation_pane(cx);
        }
        self.schedule_conversation_height_refresh(refresh, cx);
    }

    fn refresh_conversation_rows_at_current_width(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(layout_width) = self
            .app
            .workspaces
            .active_mut()
            .conversation_controller
            .active_width_bucket()
        else {
            return false;
        };
        self.refresh_conversation_rows_at_width(layout_width, cx);
        true
    }

    fn schedule_conversation_height_refresh(
        &mut self,
        refresh: ConversationRefresh,
        cx: &mut Context<Self>,
    ) {
        let Some((delay, _deadline)) = self
            .app
            .workspaces
            .active_mut()
            .conversation_controller
            .arm_height_refresh(refresh)
        else {
            return;
        };
        let owner = self.app.workspaces.active_key().clone();
        match self.connection.controller.schedule_timer(
            owner,
            DesktopTimerKind::ConversationHeightRefresh,
            delay,
        ) {
            Ok(transition) => self.apply_transition(transition, cx),
            Err(error) => {
                self.app
                    .workspaces
                    .active_mut()
                    .set_preference_notice(error.to_string());
                self.notify_toast_host(cx);
            }
        }
    }

    pub(super) fn focus_composer_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.views.composer_pane.read(cx).focus_handle().clone();
        focus.focus(window, cx);
    }

    fn focus_active_target(&self, window: &mut Window, cx: &mut Context<Self>) {
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
                None => self.focus_composer_input(window, cx),
            },
        }
    }

    fn sessions_pane_view_model(&self) -> SessionsPaneViewModel {
        let snapshot = self
            .app
            .workspaces
            .active()
            .projection
            .as_ref()
            .map(DesktopProjection::snapshot);
        let composer_running = snapshot.is_some_and(|snapshot| snapshot.active_operation.is_some());
        let mut runtime_states = self
            .app
            .workspaces
            .iter()
            .filter_map(|(key, workspace)| {
                let WorkspaceKey::Session(session_id) = key else {
                    return None;
                };
                workspace.projection.as_ref()?;
                Some(SessionRuntimeState {
                    session_id: Arc::from(session_id.as_str()),
                    status: workspace_semantic_status(workspace),
                })
            })
            .collect::<Vec<_>>();
        runtime_states.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        SessionsPaneViewModel {
            panel_width: self.app.preferences.sessions_panel_width,
            project_groups: Arc::from(self.app.catalog.project_groups()),
            omitted_sessions: self.app.catalog.omitted(),
            catalog_state: self.app.catalog.state().clone(),
            active_session_id: Arc::from(
                snapshot
                    .map(|snapshot| snapshot.session.session_id.as_str())
                    .unwrap_or_default(),
            ),
            skills_active: self.ui.center_surface == CenterSurface::Skills,
            runtime_states: Arc::from(runtime_states),
            composer_running,
            awaiting_prompt_start: self.app.workspaces.active().composer.submitted().is_some()
                && !composer_running,
            session_pending: self.app.commands.contains_anywhere(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
                )
            }),
            active_status: self.semantic_status(),
            keyboard_focus_visible: self.keyboard_focus_visible(),
            presented_as_drawer: self.ui.active_drawer == Some(CenterDrawerKind::Sessions),
            reduced_motion: self.app.preferences.reduced_motion,
        }
    }

    fn composer_pane_view_model(&self) -> ComposerPaneViewModel {
        let snapshot = self
            .app
            .workspaces
            .active()
            .projection
            .as_ref()
            .map(DesktopProjection::snapshot);
        let composer_running = snapshot.is_some_and(|snapshot| snapshot.active_operation.is_some());
        let composer_pending = matches!(
            self.app.workspaces.active().composer.admission(),
            ComposerAdmission::Pending { .. }
        );
        let awaiting_prompt_start =
            self.app.workspaces.active().composer.submitted().is_some() && !composer_running;
        let attachment_disabled_reason = self.composer_attachment_disabled_reason();
        let project_directory_state = if self.app.workspaces.active().projection.is_some() {
            desktop_controls::DesktopProjectDirectoryState::Locked
        } else if !self.app.workspaces.active().project_directory_editable()
            || composer_pending
            || awaiting_prompt_start
        {
            desktop_controls::DesktopProjectDirectoryState::Pending
        } else {
            desktop_controls::DesktopProjectDirectoryState::Editable
        };
        let project_directory_path = self.app.workspaces.active().project_directory();
        ComposerPaneViewModel {
            composer_pending,
            composer_running,
            awaiting_prompt_start,
            authorization_pending: snapshot
                .is_some_and(|snapshot| !snapshot.pending_authorizations.is_empty()),
            running_mode: self.active_composer_running_mode(),
            project_directory: composer_pane::ComposerProjectDirectoryViewModel {
                value: Arc::from(
                    project_directory_path
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "无项目".into()),
                ),
                state: project_directory_state,
                is_projectless: project_directory_path.is_none(),
            },
            attachments: self
                .app
                .workspaces
                .active()
                .composer_attachments
                .iter()
                .map(|path| composer_pane::ComposerAttachmentViewModel {
                    label: Arc::from(
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("attachment"),
                    ),
                    path: Arc::from(path.display().to_string()),
                })
                .collect::<Vec<_>>()
                .into(),
            attachments_enabled: attachment_disabled_reason.is_none()
                && self.app.workspaces.active().composer_attachments.len() < MAX_PROMPT_ATTACHMENTS,
            attachment_disabled_reason: attachment_disabled_reason.map(Arc::from),
            rejection: self
                .app
                .workspaces
                .active()
                .composer
                .rejection()
                .map(Arc::from),
        }
    }

    fn skills_pane_view_model(&self) -> SkillsPaneViewModel {
        SkillsPaneViewModel {
            skills: Arc::clone(&self.global_skills),
        }
    }

    fn inspector_pane_view_model(&self) -> InspectorPaneViewModel {
        let Some(projection) = self.app.workspaces.active().projection.as_ref() else {
            return InspectorPaneViewModel {
                panel_width: self.app.preferences.context_panel_width,
                presented_as_drawer: self.ui.active_drawer == Some(CenterDrawerKind::Inspector),
                keyboard_focus_visible: self.keyboard_focus_visible(),
                selected_section: self.app.workspaces.active().inspector_section,
                composer_running: false,
                awaiting_prompt_start: self.app.workspaces.active().composer.submitted().is_some(),
                recovery_pending: false,
                file_review_pending: false,
                external_editor_pending: false,
                external_editor_configured: self.app.preferences.external_editor.is_some(),
                changed_files: Vec::new(),
                change_count: 0,
                file_review: Arc::clone(&self.app.workspaces.active().file_review),
                runtime_attention_count: self.app.workspaces.active().project.diagnostics.len(),
                task_state: "ready".into(),
                active_operation: "—".into(),
                operation_count: 0,
                delegation_count: 0,
                selected_model: truncate_label(
                    &self.app.workspaces.active().project.selected_model_id,
                    28,
                ),
                profile: truncate_label(
                    self.app
                        .workspaces
                        .active()
                        .project
                        .default_agent_profile_id
                        .as_str(),
                    28,
                ),
                thinking: self.app.workspaces.active().thinking_selection.label(
                    self.app
                        .workspaces
                        .active()
                        .project
                        .settings
                        .default_thinking_level
                        .as_deref(),
                ),
                usage_input: "0".into(),
                usage_output: "0".into(),
                usage_cache_read: "0".into(),
                usage_cache_write: "0".into(),
                usage_tokens: "0".into(),
                usage_context: "—".into(),
                usage_cost: "—".into(),
                reduced_motion: self.app.preferences.reduced_motion,
                stream_id: "—".into(),
                sequence: "0".into(),
                generation: "0".into(),
                model_count: self.app.workspaces.active().project.models.len(),
                profile_count: self.app.workspaces.active().project.profiles.len(),
                skill_count: self.global_skills.len(),
                prompt_count: 0,
                context_count: 0,
                latest_recovery: None,
                latest_diagnostic: None,
                latest_config_diagnostic: self
                    .app
                    .workspaces
                    .active()
                    .project
                    .diagnostics
                    .last()
                    .map(|diagnostic| {
                        (
                            truncate_label(&diagnostic.code, 28),
                            truncate_label(&diagnostic.summary, 120),
                        )
                    }),
                latest_issue: None,
                cwd: truncate_label(
                    &self
                        .app
                        .workspaces
                        .active()
                        .project
                        .cwd
                        .display()
                        .to_string(),
                    54,
                ),
            };
        };
        let snapshot = projection.snapshot();
        let project = &self.app.workspaces.active().project;
        let composer_running = snapshot.active_operation.is_some();
        let awaiting_prompt_start =
            self.app.workspaces.active().composer.submitted().is_some() && !composer_running;
        let changed_files = snapshot
            .context
            .changes
            .iter()
            .take(MAX_VISIBLE_FILE_CHANGES)
            .map(|change| InspectorChangedFileView {
                request: CodingAgentFileReviewRequest::from(change),
                mutation_kind: truncate_label(&change.mutation_kind, 10),
                file_name: truncate_label(
                    change
                        .path
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(change.path.as_str()),
                    22,
                ),
                path: truncate_label(&change.path, 34),
            })
            .collect();
        let latest_recovery =
            projection
                .recoveries()
                .front()
                .map(|recovery| InspectorRecoveryView {
                    status: recovery_status_label(recovery.status).to_owned(),
                    recovery_id: truncate_label(&recovery.recovery_id, 22),
                    operation_id: truncate_label(&recovery.operation_id, 22),
                    detail: truncate_label(&recovery.reason, 120),
                    attempt_count: recovery.attempt_count.to_string(),
                    identity: recovery.identity.clone().filter(|_| {
                        recovery.status == DesktopRecoveryStatus::Pending && recovery.authoritative
                    }),
                });
        let latest_diagnostic =
            projection
                .diagnostics()
                .back()
                .map(|diagnostic| InspectorDiagnosticView {
                    sequence: diagnostic.sequence.to_string(),
                    operation: diagnostic
                        .operation_id
                        .as_deref()
                        .map(|id| truncate_label(id, 22))
                        .unwrap_or_else(|| "global".into()),
                    detail: truncate_label(&diagnostic.message, 120),
                    truncated: diagnostic.truncated,
                });
        let latest_config_diagnostic = project.diagnostics.last().map(|diagnostic| {
            (
                truncate_label(&diagnostic.code, 28),
                truncate_label(&diagnostic.summary, 120),
            )
        });
        let latest_issue = projection
            .issues()
            .back()
            .map(|issue| truncate_label(&issue.code, 28));
        let runtime_attention_count = projection
            .diagnostics()
            .len()
            .saturating_add(projection.recoveries().len())
            .saturating_add(project.diagnostics.len())
            .saturating_add(projection.issues().len());
        let usage = &snapshot.context.usage;
        InspectorPaneViewModel {
            panel_width: self.app.preferences.context_panel_width,
            presented_as_drawer: self.ui.active_drawer == Some(CenterDrawerKind::Inspector),
            keyboard_focus_visible: self.keyboard_focus_visible(),
            selected_section: self.app.workspaces.active().inspector_section,
            composer_running,
            awaiting_prompt_start,
            recovery_pending: self.active_command_contains_where(|intent| {
                matches!(intent, DesktopCommandIntent::Recovery { .. })
            }),
            file_review_pending: self.active_command_contains_where(|intent| {
                matches!(intent, DesktopCommandIntent::FileReview { .. })
            }),
            external_editor_pending: self.active_command_contains_where(|intent| {
                matches!(intent, DesktopCommandIntent::ExternalEditor { .. })
            }),
            external_editor_configured: self.app.preferences.external_editor.is_some(),
            changed_files,
            change_count: snapshot.context.changes.len(),
            file_review: Arc::clone(&self.app.workspaces.active().file_review),
            runtime_attention_count,
            task_state: runtime_state_label(projection.lifecycle(), composer_running).to_owned(),
            active_operation: snapshot
                .active_operation
                .as_deref()
                .map(|id| truncate_label(id, 24))
                .unwrap_or_else(|| "—".into()),
            operation_count: snapshot.context.operations.len(),
            delegation_count: snapshot.context.delegations.len(),
            selected_model: truncate_label(&project.selected_model_id, 28),
            profile: truncate_label(snapshot.session.default_agent_profile_id.as_str(), 28),
            thinking: self
                .app
                .workspaces
                .active()
                .thinking_selection
                .label(project.settings.default_thinking_level.as_deref()),
            usage_input: usage.input.to_string(),
            usage_output: usage.output.to_string(),
            usage_cache_read: usage.cache_read.to_string(),
            usage_cache_write: usage.cache_write.to_string(),
            usage_tokens: usage.input.saturating_add(usage.output).to_string(),
            usage_context: usage
                .context_window
                .map(|value| value.to_string())
                .unwrap_or_else(|| "—".into()),
            usage_cost: usage_cost_label(usage.cost),
            reduced_motion: self.app.preferences.reduced_motion,
            stream_id: truncate_label(&snapshot.cursor.stream_id, 18),
            sequence: snapshot.cursor.last_event_sequence.to_string(),
            generation: snapshot.cursor.capability_generation.to_string(),
            model_count: project.models.len(),
            profile_count: project.profiles.len(),
            skill_count: project.resources.skill_names.len(),
            prompt_count: project.resources.prompt_template_names.len(),
            context_count: project.resources.context_files.len(),
            latest_recovery,
            latest_diagnostic,
            latest_config_diagnostic,
            latest_issue,
            cwd: truncate_label(&project.cwd.display().to_string(), 54),
        }
    }

    fn root_modal_view_model(&self) -> RootModalViewModel {
        let authorization = self
            .app
            .workspaces
            .active()
            .projection
            .as_ref()
            .and_then(|projection| projection.snapshot().pending_authorizations.first())
            .cloned()
            .map(|request| {
                let decision_pending = self
                    .app
                    .commands
                    .authorization(self.app.workspaces.active_key())
                    .is_some_and(|(_, authorization_id, operation_id)| {
                        authorization_id == request.authorization_id
                            && operation_id == request.operation_id
                    });
                RootModalAuthorizationView {
                    request,
                    decision_pending,
                }
            });
        RootModalViewModel {
            palette_open: self.ui.command_palette.is_open(),
            palette_selected: self.ui.command_palette.selected(),
            authorization,
            full_message: self.ui.conversation_full_message.clone(),
        }
    }

    fn center_drawer_view_model(&self) -> CenterDrawerViewModel {
        CenterDrawerViewModel {
            active: self.ui.active_drawer,
            sessions_width: self.app.preferences.sessions_panel_width,
            inspector_width: self.app.preferences.context_panel_width,
        }
    }

    fn conversation_pane_view_model(&self) -> ConversationPaneViewModel {
        let diagnostic_recovery =
            self.app
                .workspaces
                .active()
                .projection
                .as_ref()
                .and_then(|projection| {
                    projection.recoveries().iter().find_map(|recovery| {
                        (recovery.status == DesktopRecoveryStatus::Pending
                            && recovery.authoritative)
                            .then(|| recovery.identity.clone())
                            .flatten()
                    })
                });
        ConversationPaneViewModel {
            render: self
                .app
                .workspaces
                .active()
                .conversation_controller
                .render_reader(),
            scroll: self
                .app
                .workspaces
                .active()
                .conversation_controller
                .scroll
                .clone(),
            visible_count: self.visible_conversation_count(),
            event_count: self
                .app
                .workspaces
                .active()
                .projection
                .as_ref()
                .map(|projection| projection.recent_events().len())
                .unwrap_or_default(),
            message_count: self
                .app
                .workspaces
                .active()
                .projection
                .as_ref()
                .map(|projection| projection.messages().len())
                .unwrap_or_default(),
            tool_count: self
                .app
                .workspaces
                .active()
                .projection
                .as_ref()
                .map(|projection| projection.tools().len())
                .unwrap_or_default(),
            omitted_count: self
                .app
                .workspaces
                .active()
                .projection
                .as_ref()
                .map(|projection| projection.conversation().omitted_blocks())
                .unwrap_or_default(),
            follow_latest: self
                .app
                .workspaces
                .active()
                .conversation_controller
                .follow_latest_enabled(),
            unseen_updates: self
                .app
                .workspaces
                .active()
                .conversation_controller
                .unseen_updates(),
            selected_block_id: self
                .app
                .workspaces
                .active()
                .conversation_controller
                .selected_block_id()
                .map(str::to_owned),
            expanded_details: Rc::new(
                self.app
                    .workspaces
                    .active()
                    .conversation_controller
                    .expanded_details()
                    .clone(),
            ),
            full_view_block_id: self
                .ui
                .conversation_full_message
                .as_ref()
                .map(|message| message.block_id.clone()),
            diagnostic_recovery,
        }
    }

    fn notify_conversation_pane(&self, cx: &mut Context<Self>) {
        let view_model = self.conversation_pane_view_model();
        self.views.conversation_pane.update(cx, |pane, cx| {
            pane.set_view_model(view_model);
            cx.notify();
        });
    }

    fn conversation_header_view_model(&self) -> ConversationHeaderViewModel {
        let snapshot = self
            .app
            .workspaces
            .active()
            .projection
            .as_ref()
            .map(DesktopProjection::snapshot);
        let project = &self.app.workspaces.active().project;
        let composer_running = snapshot.is_some_and(|snapshot| snapshot.active_operation.is_some());
        let awaiting_prompt_start =
            self.app.workspaces.active().composer.submitted().is_some() && !composer_running;
        let reload_pending = self.active_command_contains(&DesktopCommandIntent::Reload);
        let selection_pending = self.active_command_contains_where(|intent| {
            matches!(intent, DesktopCommandIntent::Selection(_))
        });
        let current_model_id = project.selected_model_id.as_str();
        let current_profile_id = snapshot
            .map(|snapshot| snapshot.session.default_agent_profile_id.as_str())
            .unwrap_or_else(|| project.default_agent_profile_id.as_str());
        let model = project
            .models
            .iter()
            .find(|model| model.id == current_model_id)
            .map(|model| model.name.as_str())
            .unwrap_or(current_model_id);
        let current_model = project
            .models
            .iter()
            .find(|model| model.id == current_model_id);
        let profile = project
            .profiles
            .iter()
            .find(|profile| profile.id.as_str() == current_profile_id)
            .map(|profile| profile.display_name.as_str())
            .unwrap_or(current_profile_id);
        let (model_groups, unavailable_current_model) =
            conversation_header_model_menu(&project.models, current_model_id);
        let profile_options = project
            .profiles
            .iter()
            .map(|profile| ConversationHeaderSelectorOption {
                id: Arc::from(profile.id.as_str()),
                label: Arc::from(format!(
                    "{} · {}{}",
                    profile.display_name,
                    profile.id.as_str(),
                    if profile.kind == ProfileKind::Team {
                        " · team profile"
                    } else {
                        ""
                    }
                )),
                selectable: profile.kind == ProfileKind::Agent,
            })
            .collect::<Vec<_>>();
        let project_name = project
            .cwd
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| truncate_label(name, 18))
            .unwrap_or_else(|| "Project".into());

        ConversationHeaderViewModel {
            idle: self.app.workspaces.active().projection.is_none(),
            status: self.semantic_status(),
            composer_running,
            abort_pending: self.active_command_contains_where(|intent| {
                matches!(intent, DesktopCommandIntent::Abort { .. })
            }),
            reload_pending,
            selector_disabled: composer_running
                || awaiting_prompt_start
                || reload_pending
                || selection_pending,
            model: Arc::from(truncate_label(model, 10)),
            profile: Arc::from(truncate_label(profile, 9)),
            thinking: Arc::from(
                if self.app.workspaces.active().thinking_selection == DesktopThinkingLevel::Default
                {
                    "Auto".to_owned()
                } else {
                    truncate_label(
                        &self.app.workspaces.active().thinking_selection.label(None),
                        12,
                    )
                },
            ),
            thinking_selection: self.app.workspaces.active().thinking_selection,
            thinking_options: conversation_header_thinking_menu(current_model).into(),
            thinking_hint: self.app.workspaces.active().thinking_hint.clone(),
            current_model_id: Arc::from(current_model_id),
            current_profile_id: Arc::from(current_profile_id),
            model_groups: model_groups.into(),
            unavailable_current_model,
            profile_options: profile_options.into(),
            project_name: Arc::from(project_name),
            keyboard_focus_visible: self.keyboard_focus_visible(),
            panel_visibility: self.visibility(),
            sessions_drawer_open: self.ui.active_drawer == Some(CenterDrawerKind::Sessions),
            inspector_drawer_open: self.ui.active_drawer == Some(CenterDrawerKind::Inspector),
            sessions_panel_width: self.app.preferences.sessions_panel_width,
            context_panel_width: self.app.preferences.context_panel_width,
        }
    }

    fn semantic_status(&self) -> SemanticStatus {
        workspace_semantic_status(self.app.workspaces.active())
    }
}

fn conversation_header_thinking_menu(
    model: Option<&CodingAgentModelChoice>,
) -> Vec<ConversationHeaderThinkingOption> {
    let Some(capability) = model.map(|model| &model.thinking_capability) else {
        return Vec::new();
    };
    if !capability.supported {
        return Vec::new();
    }
    let mut options = vec![ConversationHeaderThinkingOption {
        selection: DesktopThinkingLevel::Default,
        label: "Auto",
    }];
    if capability.can_disable {
        options.push(ConversationHeaderThinkingOption {
            selection: DesktopThinkingLevel::Off,
            label: "Off",
        });
    }
    for level in &capability.explicit_levels {
        if *level == CodingAgentThinkingLevel::Off {
            continue;
        }
        let selection = DesktopThinkingLevel::from_explicit(Some(*level));
        if options.iter().any(|option| option.selection == selection) {
            continue;
        }
        options.push(ConversationHeaderThinkingOption {
            selection,
            label: match level {
                CodingAgentThinkingLevel::Off => "Off",
                CodingAgentThinkingLevel::Minimal => "Minimal",
                CodingAgentThinkingLevel::Low => "Low",
                CodingAgentThinkingLevel::Medium => "Medium",
                CodingAgentThinkingLevel::High => "High",
                CodingAgentThinkingLevel::XHigh => "XHigh",
            },
        });
    }
    options
}

fn admitted_desktop_thinking_selection(
    project: &CodingAgentEmbeddingSnapshot,
    requested: DesktopThinkingLevel,
) -> (DesktopThinkingLevel, bool) {
    if requested == DesktopThinkingLevel::Default {
        return (requested, false);
    }
    let model = project
        .models
        .iter()
        .find(|model| model.id == project.selected_model_id);
    if conversation_header_thinking_menu(model)
        .iter()
        .any(|option| option.selection == requested)
    {
        (requested, false)
    } else {
        (DesktopThinkingLevel::Default, true)
    }
}

fn conversation_header_model_menu(
    models: &[CodingAgentModelChoice],
    current_model_id: &str,
) -> (
    Vec<ConversationHeaderModelGroup>,
    Option<ConversationHeaderModelWarning>,
) {
    let mut grouped = BTreeMap::<&str, Vec<ConversationHeaderModelOption>>::new();
    for model in models
        .iter()
        .filter(|model| model.configured && model.supports_text)
    {
        grouped
            .entry(model.provider.as_str())
            .or_default()
            .push(ConversationHeaderModelOption {
                id: Arc::from(model.id.as_str()),
                name: Arc::from(model.name.as_str()),
                display_name: Arc::from(truncate_label(&model.name, 44)),
            });
    }

    let groups = grouped
        .into_iter()
        .map(|(provider, mut options)| {
            options.sort_by(|left, right| {
                left.name
                    .cmp(&right.name)
                    .then_with(|| left.id.cmp(&right.id))
            });
            ConversationHeaderModelGroup {
                provider: Arc::from(provider),
                options: options.into(),
            }
        })
        .collect();
    let unavailable_current_model = models
        .iter()
        .find(|model| model.id == current_model_id)
        .filter(|model| !(model.configured && model.supports_text))
        .map(|model| ConversationHeaderModelWarning {
            id: Arc::from(model.id.as_str()),
            name: Arc::from(model.name.as_str()),
            reason: Arc::from(if !model.supports_text {
                "No text input"
            } else {
                "Authentication required"
            }),
        })
        .or_else(|| {
            (!models.iter().any(|model| model.id == current_model_id)).then(|| {
                ConversationHeaderModelWarning {
                    id: Arc::from(current_model_id),
                    name: Arc::from(current_model_id),
                    reason: Arc::from("Not in model catalog"),
                }
            })
        });

    (groups, unavailable_current_model)
}

impl PlatformUpdatePort for NativeShell {
    fn active_workspace_key(&self) -> WorkspaceKey {
        self.app.workspaces.active_key().clone()
    }

    fn workspace_exists(&self, owner: &WorkspaceKey) -> bool {
        self.app.workspaces.contains(owner)
    }

    fn project_directory_editable(&self, owner: &WorkspaceKey) -> bool {
        self.app
            .workspaces
            .get(owner)
            .is_some_and(SessionWorkspace::project_directory_editable)
    }

    fn set_project_directory(&mut self, owner: &WorkspaceKey, path: PathBuf) -> bool {
        let Some(workspace) = self.app.workspaces.get_mut(owner) else {
            return false;
        };
        if !workspace.project_directory_editable() {
            return false;
        }
        workspace.draft_workspace_selection = CodingAgentWorkspaceSelection::project(path);
        true
    }

    fn add_composer_attachments(
        &mut self,
        owner: &WorkspaceKey,
        paths: Vec<PathBuf>,
    ) -> Result<bool, String> {
        let Some(workspace) = self.app.workspaces.get_mut(owner) else {
            return Ok(false);
        };
        let mut candidate = workspace.composer_attachments.clone();
        for path in paths {
            if !candidate.contains(&path) {
                candidate.push(path);
            }
        }
        validate_prompt_attachments(&candidate).map_err(|error| error.to_string())?;
        if candidate == workspace.composer_attachments {
            return Ok(false);
        }
        workspace.composer_attachments = candidate;
        Ok(true)
    }

    fn set_notice(&mut self, owner: &WorkspaceKey, notice: String) {
        if let Some(workspace) = self.app.workspaces.get_mut(owner) {
            workspace.set_preference_notice(notice);
        }
    }

    fn show_conversation_announcement(&mut self, owner: &WorkspaceKey, message: String) {
        self.ui.announce_conversation(owner.clone(), message);
    }

    fn clear_conversation_announcement(&mut self, owner: &WorkspaceKey) -> bool {
        self.ui.clear_conversation_announcement(owner)
    }

    fn fire_conversation_height_refresh(&mut self, owner: &WorkspaceKey) -> bool {
        self.app.workspaces.get_mut(owner).is_some_and(|workspace| {
            workspace
                .conversation_controller
                .fire_current_height_refresh()
        })
    }

    fn commit_conversation_width(&mut self, owner: &WorkspaceKey) -> bool {
        self.app.workspaces.get_mut(owner).is_some_and(|workspace| {
            workspace
                .conversation_controller
                .commit_current_pending_width()
        })
    }

    fn refresh_inspector_telemetry(&mut self, owner: &WorkspaceKey) -> bool {
        if self.app.workspaces.active_key() != owner
            || self.ui.inspector_telemetry_refresh_deadline.is_none()
        {
            return false;
        }
        self.ui.inspector_telemetry_refresh_deadline = None;
        self.ui.inspector_telemetry_last_refresh = Some(Instant::now());
        true
    }

    fn complete_resync_admission(
        &mut self,
        owner: &WorkspaceKey,
        command_id: u64,
        failure: Option<String>,
    ) {
        let Some(message) = failure else {
            return;
        };
        let intent = DesktopCommandIntent::Resync;
        let _ = self.app.commands.complete(command_id, owner, &intent);
        if let Some(workspace) = self.app.workspaces.get_mut(owner) {
            workspace.set_preference_notice(message);
        }
    }
}

impl RuntimeUpdatePort for NativeShell {
    fn active_workspace_key(&self) -> WorkspaceKey {
        self.app.workspaces.active_key().clone()
    }

    fn workspace_exists(&self, owner: &WorkspaceKey) -> bool {
        self.app.workspaces.contains(owner)
    }

    fn command_owner(&self, command_id: u64) -> Option<WorkspaceKey> {
        self.app.commands.owner(command_id).cloned()
    }

    fn command_intent(&self, command_id: u64) -> Option<DesktopCommandIntent> {
        self.app.commands.intent(command_id).cloned()
    }

    fn command_matches(
        &self,
        command_id: u64,
        owner: &WorkspaceKey,
        intent: &DesktopCommandIntent,
    ) -> bool {
        self.app.commands.matches(command_id, owner, intent)
    }

    fn transfer_command(&mut self, command_id: u64, owner: WorkspaceKey) -> bool {
        self.app
            .commands
            .transfer_command(command_id, owner)
            .is_ok()
    }

    fn require_command_owner_resync(
        &mut self,
        pending_owner: &WorkspaceKey,
        observed_owner: &WorkspaceKey,
    ) {
        self.require_command_owner_resync(pending_owner, observed_owner);
    }

    fn complete_command(
        &mut self,
        command_id: u64,
        owner: &WorkspaceKey,
        intent: &DesktopCommandIntent,
    ) -> bool {
        self.complete_command(command_id, owner, intent)
    }

    fn reject_command(
        &mut self,
        command_id: u64,
        owner: &WorkspaceKey,
        command: desktop::runtime::DesktopRuntimeCommandKind,
    ) -> Option<DesktopCommandIntent> {
        self.reject_command(command_id, owner, command)
    }

    fn complete_operation_commands(&mut self, owner: &WorkspaceKey, operation_id: &str) {
        self.complete_matching_command(owner, |intent| {
            matches!(
                intent,
                DesktopCommandIntent::Abort {
                    operation_id: pending,
                } if pending == operation_id
            )
        });
        self.complete_matching_command(owner, |intent| {
            matches!(
                intent,
                DesktopCommandIntent::Authorization {
                    operation_id: pending,
                    ..
                } if pending == operation_id
            )
        });
    }

    fn install_hydrated_workspace(
        &mut self,
        snapshot: &desktop::runtime::DesktopRuntimeHydratedSnapshot,
        inherit_home_thinking: bool,
        activate: bool,
    ) -> bool {
        self.install_hydrated_workspace(snapshot, inherit_home_thinking, activate)
    }

    fn remove_closed_workspace(&mut self, session_id: &str) -> usize {
        self.remove_closed_workspace(session_id)
    }

    fn remove_catalog_session(&mut self, session_id: &str) {
        self.app.catalog.remove_session(session_id);
    }

    fn replace_catalog(
        &mut self,
        sessions: Vec<desktop::runtime::DesktopSessionCatalogEntry>,
        omitted: usize,
    ) {
        self.app.catalog.replace_catalog(sessions, omitted);
    }

    fn rename_catalog_session(
        &mut self,
        session_id: &str,
        name: Option<String>,
        updated_at: String,
    ) -> bool {
        self.app
            .catalog
            .rename_session(session_id, name, updated_at)
    }

    fn insert_session_into_catalog(&mut self, owner: &WorkspaceKey) -> bool {
        debug_assert_eq!(owner, self.app.workspaces.active_key());
        self.insert_active_session_into_catalog()
    }

    fn catalog_is_loading(&self) -> bool {
        self.app.catalog.state().is_loading()
    }

    fn fail_catalog(&mut self, message: String) {
        self.app.catalog.fail_refresh(message);
    }

    fn cancel_all_commands(&mut self) {
        self.app.commands.cancel_all();
    }

    fn set_notice(&mut self, owner: &WorkspaceKey, notice: String) {
        if let Some(workspace) = self.app.workspaces.get_mut(owner) {
            workspace.set_preference_notice(notice);
        }
    }

    fn accept_composer(&mut self, owner: &WorkspaceKey, command_id: u64) -> bool {
        let Some(workspace) = self.app.workspaces.get_mut(owner) else {
            return false;
        };
        if workspace.composer.accepted(command_id).is_err() {
            return false;
        }
        workspace.composer_attachments.clear();
        workspace.composer_needs_sync = true;
        workspace.conversation_controller.mark_live_dirty();
        true
    }

    fn reject_composer(&mut self, owner: &WorkspaceKey, command_id: u64, notice: String) -> bool {
        let Some(workspace) = self.app.workspaces.get_mut(owner) else {
            return false;
        };
        workspace.composer.rejected(command_id, notice).is_ok()
    }

    fn submitted_composer_command(&self, owner: &WorkspaceKey) -> Option<u64> {
        self.app
            .workspaces
            .get(owner)
            .and_then(|workspace| workspace.composer.submitted())
            .map(|submitted| submitted.command_id)
    }

    fn reject_pending_composer(&mut self, owner: &WorkspaceKey, message: String) {
        let command_id = self.app.workspaces.get(owner).and_then(|workspace| {
            match workspace.composer.admission() {
                ComposerAdmission::Pending { command_id, .. } => Some(*command_id),
                ComposerAdmission::Idle => None,
            }
        });
        if let Some(command_id) = command_id
            && RuntimeUpdatePort::reject_composer(self, owner, command_id, message)
            && let Some(workspace) = self.app.workspaces.get_mut(owner)
        {
            workspace.composer_needs_sync = true;
        }
    }

    fn set_file_review_ready(
        &mut self,
        owner: &WorkspaceKey,
        review: coding_agent::api::review::CodingAgentFileReview,
    ) {
        if let Some(workspace) = self.app.workspaces.get_mut(owner) {
            workspace.file_review = Arc::new(DesktopFileReviewState::Ready(
                DesktopFileReviewDocument::from_product(review),
            ));
        }
    }

    fn set_file_review_failed(
        &mut self,
        owner: &WorkspaceKey,
        request: CodingAgentFileReviewRequest,
        code: String,
    ) {
        if let Some(workspace) = self.app.workspaces.get_mut(owner) {
            workspace.file_review = Arc::new(DesktopFileReviewState::Failed { request, code });
        }
    }

    fn apply_model_thinking_selection(
        &mut self,
        owner: &WorkspaceKey,
        thinking_level: Option<CodingAgentThinkingLevel>,
        thinking_fallback: bool,
    ) {
        let selection = DesktopThinkingLevel::from_explicit(thinking_level);
        let session_id = self.app.workspaces.get_mut(owner).and_then(|workspace| {
            workspace.thinking_selection = selection;
            workspace.thinking_hint = thinking_fallback
                .then(|| Arc::from("Thinking reset to Auto for the selected model."));
            workspace
                .projection
                .as_ref()
                .map(|projection| projection.snapshot().session.session_id.clone())
        });
        if let Some(session_id) = session_id.as_deref() {
            self.remember_thinking_selection(session_id, selection);
        }
    }

    fn selected_model_label(&self, owner: &WorkspaceKey) -> String {
        self.app
            .workspaces
            .get(owner)
            .map(|workspace| workspace.project.selected_model_id.clone())
            .unwrap_or_default()
    }

    fn selected_profile_label(&self, owner: &WorkspaceKey) -> String {
        self.app
            .workspaces
            .get(owner)
            .map(|workspace| {
                workspace
                    .projection
                    .as_ref()
                    .map(|projection| {
                        projection
                            .snapshot()
                            .session
                            .default_agent_profile_id
                            .as_str()
                            .to_owned()
                    })
                    .unwrap_or_else(|| {
                        workspace
                            .project
                            .default_agent_profile_id
                            .as_str()
                            .to_owned()
                    })
            })
            .unwrap_or_default()
    }

    fn apply_projection_event(
        &mut self,
        owner: &WorkspaceKey,
        event: Option<ProjectionEvent>,
        creates_session_from_prompt: bool,
        completed_prompt_command: Option<u64>,
    ) -> ProjectionUpdateResult {
        self.apply_projection_event_for(
            owner,
            event,
            creates_session_from_prompt,
            completed_prompt_command,
        )
    }

    fn reserve_resync_command(&mut self, owner: &WorkspaceKey) -> Option<u64> {
        if !self
            .app
            .workspaces
            .get(owner)
            .and_then(|workspace| workspace.projection.as_ref())
            .is_some_and(|projection| {
                projection.lifecycle() == DesktopProjectionLifecycle::NeedsResync
            })
            || self
                .app
                .commands
                .contains(owner, &DesktopCommandIntent::Resync)
        {
            return None;
        }
        match self
            .app
            .commands
            .reserve(owner.clone(), DesktopCommandIntent::Resync)
        {
            Ok(command_id) => Some(command_id),
            Err(error) => {
                RuntimeUpdatePort::set_notice(self, owner, error.to_string());
                None
            }
        }
    }

    fn abandon_resync_command(&mut self, owner: &WorkspaceKey, command_id: u64, message: String) {
        let _ = self
            .app
            .commands
            .complete(command_id, owner, &DesktopCommandIntent::Resync);
        RuntimeUpdatePort::set_notice(self, owner, message);
    }

    fn active_runtime_is_running(&self) -> bool {
        !self
            .app
            .workspaces
            .active()
            .projection
            .as_ref()
            .is_some_and(|projection| projection.lifecycle() == DesktopProjectionLifecycle::Stopped)
    }
}

impl Render for NativeShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _span = tracing::trace_span!("desktop.render").entered();
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
                        .rounded_token(desktop_style::DesignRadius::Md)
                        .border_1()
                        .border_color(rgb(theme.success.value()))
                        .bg(rgb(theme.elevated.value()))
                        .px_token(desktop_style::DesignSpace::Md)
                        .py_token(desktop_style::DesignSpace::Sm)
                        .text_color(rgb(theme.text.value()))
                        .child(message),
                )
            })
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

fn runtime_state_label(
    lifecycle: DesktopProjectionLifecycle,
    operation_active: bool,
) -> &'static str {
    match (lifecycle, operation_active) {
        (DesktopProjectionLifecycle::Running, true) => "connected · active",
        (DesktopProjectionLifecycle::Running, false) => "connected · idle",
        (DesktopProjectionLifecycle::NeedsResync, _) => "resync required",
        (DesktopProjectionLifecycle::Failed, _) => "failed",
        (DesktopProjectionLifecycle::Stopped, _) => "stopped",
    }
}

fn recovery_status_label(status: DesktopRecoveryStatus) -> &'static str {
    match status {
        DesktopRecoveryStatus::Pending => "pending",
        DesktopRecoveryStatus::Resolved => "resolved",
        DesktopRecoveryStatus::Recovered => "recovered",
    }
}

fn recovery_action_label(action: DesktopRecoveryAction) -> &'static str {
    match action {
        DesktopRecoveryAction::Retry => "retry",
        DesktopRecoveryAction::MarkFailed => "mark-failed",
        DesktopRecoveryAction::Abort => "abort",
    }
}

fn usage_cost_label(cost: Option<f64>) -> String {
    cost.filter(|cost| cost.is_finite() && *cost >= 0.0)
        .map(|cost| format!("${cost:.4}"))
        .unwrap_or_else(|| "—".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{borrow::Cow, cell::RefCell, collections::HashSet, fs};

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

    fn session_workspace<'a>(
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
    fn idle_session_catalog_is_loaded_only_by_explicit_refresh(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);

        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.app.catalog.state().clone()),
            project_catalog_controller::ProjectCatalogState::NotLoaded
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
            project_catalog_controller::ProjectCatalogState::Loading
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
            assert_eq!(
                shell.app.catalog.state(),
                &project_catalog_controller::ProjectCatalogState::Ready
            );
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
                &project_catalog_controller::ProjectCatalogState::Error {
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
                project_catalog_controller::ProjectCatalogState::Error { .. }
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
                &project_catalog_controller::ProjectCatalogState::Error {
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
            shell.notify_sessions_pane(cx);
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
            shell.notify_sessions_pane(cx);
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
            shell.notify_sessions_pane(cx);
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
                shell
                    .sessions_pane_view_model()
                    .active_session_id
                    .is_empty()
            );
            assert!(!shell.composer_pane_view_model().composer_running);
            let inspector = shell.inspector_pane_view_model();
            assert_eq!(inspector.active_operation, "—");
            assert_eq!(inspector.stream_id, "—");
            assert!(shell.root_modal_view_model().authorization.is_none());
            assert!(shell.views.toast_host.read(cx).messages().len() <= 3);
            assert_eq!(shell.conversation_pane_view_model().visible_count, 0);
            let header = shell.conversation_header_view_model();
            assert_eq!(header.profile.as_ref(), "Default");
            assert_eq!(header.current_profile_id.as_ref(), "default");
            assert_eq!(shell.skills_pane_view_model().skills.len(), 1);
            assert!(!shell.sessions_pane_view_model().skills_active);
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
            shell.notify_sessions_pane(cx);
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
            shell.notify_sessions_pane(cx);
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
            shell.notify_sessions_pane(cx);
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
            shell.notify_sessions_pane(cx);
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
                && shell.sessions_pane_view_model().skills_active
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
            session_workspace(shell, "desktop-visual-test").is_some()
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
            shell.notify_toast_host(cx);
            shell
                .app
                .workspaces
                .active_mut()
                .set_preference_notice("Repeated notice".into());
            shell.notify_toast_host(cx);

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
            shell.notify_toast_host(cx);
            shell
                .app
                .workspaces
                .active_mut()
                .set_preference_notice("Fourth notice".into());
            shell.notify_toast_host(cx);

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
            assert!(shell.install_hydrated_workspace(&visual_test_snapshot(), true, true));
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
                SessionWorkspace::new(
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
            assert!(session_workspace(shell, "session-background").is_some());
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
            let mut session_b = SessionWorkspace::new(
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
            session_b.inspector_section = InspectorSection::Task;
            session_b.file_review =
                Arc::new(DesktopFileReviewState::Loading(review_request.clone()));

            shell.app.workspaces.active_mut().composer.edit("draft a");
            shell.app.workspaces.active_mut().inspector_section = InspectorSection::Runtime;
            insert_session_workspace(shell, "session-b", session_b);
            let review_intent = DesktopCommandIntent::FileReview {
                request: review_request.clone(),
            };
            let review_command_id = shell
                .app
                .commands
                .reserve(WorkspaceKey::session("session-b"), review_intent.clone())
                .expect("session B test command fits the global tracker");
            let sessions = shell.sessions_pane_view_model();
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
                shell.app.workspaces.active().inspector_section,
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

            let background = session_workspace(shell, "session-b")
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
            assert_eq!(background.inspector_section, InspectorSection::Task);
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
                shell.app.workspaces.active().inspector_section,
                InspectorSection::Task
            );
            assert!(activate_session(shell, "session-a"));
            assert_eq!(shell.app.workspaces.active().composer.draft(), "draft a");
            assert_eq!(
                shell.app.workspaces.active().inspector_section,
                InspectorSection::Runtime
            );

            for session_id in ["session-c", "session-d"] {
                let snapshot = visual_test_snapshot_for(session_id);
                let projection = DesktopProjection::new(snapshot.clone())
                    .expect("workspace-cap fixture is a valid projection");
                insert_session_workspace(
                    shell,
                    session_id,
                    SessionWorkspace::new(snapshot.project, Some(projection), None),
                );
            }
            assert_eq!(shell.app.workspaces.session_count(), MAX_SESSION_WORKSPACES);
            let session_e = visual_test_snapshot_for("session-e");
            assert!(!shell.install_hydrated_workspace(&session_e, false, true));
            assert!(session_workspace(shell, "session-e").is_none());
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
                SessionWorkspace::new(snapshot_b.project, Some(projection_b), None),
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
            assert!(session_workspace(shell, "close-session-b").is_none());
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
            assert!(session_workspace(shell, "close-active-session").is_none());
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
                let mut view_model = shell.conversation_header_view_model();
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
                    let mut view_model = shell.conversation_header_view_model();
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
                shell.app.workspaces.active_mut().inspector_section = InspectorSection::Runtime;
                shell.notify_inspector_pane(cx);
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
                        .conversation_controller
                        .row_count(),
                    shell
                        .app
                        .workspaces
                        .active()
                        .conversation_controller
                        .render_heights_for_tests(),
                    shell
                        .app
                        .workspaces
                        .active()
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
            shell.notify_sessions_pane(cx);
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
            shell.notify_sessions_pane(cx);
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

        let view_model = shell.read_with(cx, |shell, _| shell.conversation_header_view_model());
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

        let (groups, warning) = conversation_header_model_menu(&models, "z-current");
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
        let (reordered_groups, _) = conversation_header_model_menu(&reordered, "z-current");
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

        let (groups, warning) = conversation_header_model_menu(&models, "lost-auth-model");
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
            conversation_header_model_menu(&unavailable, "lost-auth-model");
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

        let view_model = shell.read_with(cx, |shell, _| shell.conversation_header_view_model());
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
            shell.composer_pane_view_model().project_directory
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
            shell.composer_pane_view_model().project_directory
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
            shell.notify_composer_pane(cx);
        });
        assert_eq!(
            pending_shell.read_with(cx, |shell, _| {
                shell.composer_pane_view_model().project_directory.state
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
            shell.notify_composer_pane(cx);
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
            shell.notify_inspector_pane(cx);
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
        let options = conversation_header_thinking_menu(Some(&model));
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
            conversation_header_thinking_menu(Some(&model))
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
        assert!(conversation_header_thinking_menu(None).is_empty());
        model.thinking_capability = CodingAgentThinkingCapability::default();
        assert!(conversation_header_thinking_menu(Some(&model)).is_empty());
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
                shell.composer_pane_view_model().project_directory.value
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
            let directory = shell.composer_pane_view_model().project_directory;
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
                shell.composer_pane_view_model().project_directory.value,
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
                .conversation_controller
                .set_scroll_top_for_tests(17.0);
            let snapshot = visual_test_snapshot_for("temporary-history-session");
            let projection = DesktopProjection::new(snapshot.clone())
                .expect("history session fixture is a valid projection");
            let history = SessionWorkspace::new(snapshot.project, Some(projection), None);
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
            shell.notify_composer_pane(cx);
        });
        cx.run_until_parked();

        assert_eq!(
            shell.read_with(cx, |shell, _| shell.composer_attachment_disabled_reason()),
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
                SessionWorkspace::new_with_thinking(
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

            assert!(shell.install_hydrated_workspace(&existing, false, true));
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

            assert!(shell.install_hydrated_workspace(&created, true, true));
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
        let mut session_a = SessionWorkspace::new(project.clone(), Some(projection), None);
        let mut session_b = SessionWorkspace::new(project, None, None);
        session_a.composer.edit("draft a");
        session_a.composer_running_mode = ComposerRunningMode::QueueNext;
        session_b.composer.edit("draft b");

        assert_eq!(session_a.composer.draft(), "draft a");
        assert_eq!(
            session_a.composer_running_mode.submission_kind(),
            ComposerSubmissionKind::FollowUp
        );
        assert_eq!(session_b.composer.draft(), "draft b");
        assert_eq!(
            session_b.composer_running_mode.submission_kind(),
            ComposerSubmissionKind::Steer
        );
    }

    #[test]
    fn inspector_section_selection_is_scoped_to_the_session() {
        let projection = visual_test_projection();
        let project = projection.project().clone();
        let mut session_a = SessionWorkspace::new(project.clone(), Some(projection), None);
        let mut session_b = SessionWorkspace::new(project, None, None);
        session_a.inspector_section = InspectorSection::Runtime;
        session_b.inspector_section = InspectorSection::Task;

        assert_eq!(session_a.inspector_section, InspectorSection::Runtime);
        assert_eq!(session_b.inspector_section, InspectorSection::Task);
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
mod commands;
pub(crate) mod composer_pane;
mod conversation_controller;
pub(crate) mod conversation_header;
pub(crate) mod conversation_pane;
mod desktop_controls;
mod desktop_style;
mod evo_brand;
pub(crate) mod home_pane;
pub(crate) mod inspector_pane;
mod project_catalog_controller;
pub(crate) mod root_modal_host;
pub(crate) mod sessions_pane;
pub(crate) mod skills_pane;
mod streaming_text;
pub(crate) mod toast_host;

use center_drawer_host::{
    CenterDrawerHost, CenterDrawerHostEvent, CenterDrawerKind, CenterDrawerViewModel,
};
use center_navigation::{CenterNavigationTarget, CenterSurface};
#[cfg(test)]
use composer_pane::InputRenderLatencyProbe;
use composer_pane::{ComposerPane, ComposerPaneEvent, ComposerPaneViewModel};
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
use conversation_header::header_runtime_status_slot_width;
use conversation_header::{
    ConversationHeader, ConversationHeaderEvent, ConversationHeaderModelGroup,
    ConversationHeaderModelOption, ConversationHeaderModelWarning,
    ConversationHeaderSelectorOption, ConversationHeaderThinkingOption,
    ConversationHeaderViewModel,
};
#[cfg(test)]
use conversation_pane::CONVERSATION_RAIL_WIDTH;
use conversation_pane::{ConversationPane, ConversationPaneEvent, ConversationPaneViewModel};
use home_pane::HomePane;
use inspector_pane::{
    InspectorChangedFileView, InspectorDiagnosticView, InspectorPane, InspectorPaneEvent,
    InspectorPaneViewModel, InspectorRecoveryView,
};
use project_catalog_controller::ProjectCatalogController;
use root_modal_host::{
    RootModalAuthorizationView, RootModalHost, RootModalHostEvent, RootModalViewModel,
};
use sessions_pane::{SessionRuntimeState, SessionsPane, SessionsPaneEvent, SessionsPaneViewModel};
use skills_pane::{SkillsPane, SkillsPaneViewModel};
use toast_host::{ToastHost, ToastNotice};
