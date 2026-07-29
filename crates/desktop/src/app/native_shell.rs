use coding_agent::api::authorization::{ToolAuthorizationDecision, ToolAuthorizationIdentity};
#[cfg(test)]
use coding_agent::api::embedding::CodingAgentResourceCommandKind;
use coding_agent::api::embedding::{
    CodingAgentEmbeddingSnapshot, CodingAgentResourceCommand, CodingAgentThinkingLevel,
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
use desktop::preferences::{DesktopPreferences, DesktopThinkingLevel, PreferenceWriter};
use desktop::projection::{DesktopProjection, DesktopProjectionLifecycle, DesktopRecoveryStatus};
use desktop::runtime::{
    DesktopRecoveryAction, DesktopRecoveryIdentity, DesktopRuntimeBridge,
    DesktopRuntimeCommandHandle, DesktopRuntimeSelectionKind, MAX_PROMPT_ATTACHMENTS,
    validate_prompt_attachments,
};
use desktop::shell::{
    CONTEXT_PANEL_MAX_WIDTH, CONTEXT_PANEL_MIN_WIDTH, CONTEXT_PANEL_WIDTH, FocusState, FocusTarget,
    MIN_CONVERSATION_WIDTH, PanelVisibility, SESSION_PANEL_MAX_WIDTH, SESSION_PANEL_MIN_WIDTH,
    SESSION_PANEL_WIDTH, SemanticColor, SemanticStatus, SemanticTheme, ShellLayout, UI_FONT_FAMILY,
    truncate_label,
};
use gpui::{
    ClipboardItem, Context, FocusHandle, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, PathPromptOptions, Render, Role,
    ScrollStrategy, Styled as _, Subscription, Window, WindowBounds, div, prelude::*, px, rgb,
};
use std::collections::{HashMap, VecDeque};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use self::desktop_style::{DesignText, DesktopStyledExt as _};
use crate::actions::{
    self, AbortActiveOperation, AuthorizationAllowForOperation, AuthorizationAllowOnce,
    AuthorizationDeny, CopySelectedConversation, DesktopCommandPalette, DesktopPaletteCommand,
    EscapeHierarchy, FocusComposer, FocusNextRegion, FocusPreviousRegion, FollowLatestOutput,
    NewSession, OpenCommandPalette, OpenFileSurface, PALETTE_ENTRIES, PaletteConfirm, PaletteNext,
    PalettePrevious, SelectNextConversation, SelectPreviousConversation, SubmitComposer,
    ToggleInspectorPanel, ToggleSelectedConversationDetails, TrapOverlayFocus,
};
use crate::command_ledger::{DesktopCommandIntent, DesktopCommandLedger};

const MAX_RUNTIME_UPDATES_PER_FRAME: usize = 64;
const INSPECTOR_TELEMETRY_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
const MAX_SESSION_WORKSPACES: usize = 4;
const CONVERSATION_ANNOUNCEMENT_DURATION: Duration = Duration::from_secs(2);
/// Draft slot for the idle Home surface. It is a Composer state key only; the
/// runtime never sees it, and no projection is ever constructed for it.
const HOME_COMPOSER_SESSION_KEY: &str = "home";

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
            glyph: "YOU",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizablePanel {
    Sessions,
    Context,
}

#[derive(Debug, Clone, Copy)]
struct PanelResizeState {
    panel: ResizablePanel,
    pointer_origin_x: f32,
    width_origin: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum FocusInputModality {
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
    const fn next(self) -> Self {
        match self {
            Self::Default => Self::Off,
            Self::Off => Self::Minimal,
            Self::Minimal => Self::Low,
            Self::Low => Self::Medium,
            Self::Medium => Self::High,
            Self::High => Self::XHigh,
            Self::XHigh => Self::Default,
        }
    }

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
enum DesktopOverlayKind {
    Authorization,
    CommandPalette,
    NarrowSessions,
    NarrowContext,
    FullMessage,
}

#[derive(Debug, Clone)]
struct ConversationFullMessageView {
    block_id: String,
    title: Arc<str>,
    text: Arc<str>,
    source_truncated: bool,
}

pub(super) struct SessionWorkspace {
    project: CodingAgentEmbeddingSnapshot,
    projection: Option<DesktopProjection>,
    preference_notice: Option<String>,
    preference_notice_revision: u64,
    conversation_controller: ConversationController,
    inspector_section: InspectorSection,
    composer: ComposerState,
    composer_needs_sync: bool,
    composer_running_mode: ComposerRunningMode,
    composer_attachments: Vec<PathBuf>,
    command_ledger: DesktopCommandLedger,
    thinking_selection: DesktopThinkingLevel,
    file_review: Arc<DesktopFileReviewState>,
}

impl SessionWorkspace {
    fn new(
        project: CodingAgentEmbeddingSnapshot,
        projection: Option<DesktopProjection>,
        preference_notice: Option<String>,
        command_ledger: DesktopCommandLedger,
    ) -> Self {
        Self::new_with_thinking(
            project,
            projection,
            preference_notice,
            command_ledger,
            DesktopThinkingLevel::Default,
        )
    }

    fn new_with_thinking(
        project: CodingAgentEmbeddingSnapshot,
        projection: Option<DesktopProjection>,
        preference_notice: Option<String>,
        command_ledger: DesktopCommandLedger,
        thinking_selection: DesktopThinkingLevel,
    ) -> Self {
        let preference_notice_revision = u64::from(preference_notice.is_some());
        Self {
            project,
            projection,
            preference_notice,
            preference_notice_revision,
            conversation_controller: ConversationController::default(),
            inspector_section: InspectorSection::default(),
            composer: ComposerState::default(),
            composer_needs_sync: false,
            composer_running_mode: ComposerRunningMode::default(),
            composer_attachments: Vec::new(),
            command_ledger,
            thinking_selection,
            file_review: Arc::new(DesktopFileReviewState::default()),
        }
    }

    fn session_id(&self) -> &str {
        self.projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.as_str())
            .unwrap_or(HOME_COMPOSER_SESSION_KEY)
    }

    fn set_preference_notice(&mut self, message: String) {
        self.preference_notice = Some(message);
        self.preference_notice_revision = self.preference_notice_revision.wrapping_add(1).max(1);
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

fn runtime_update_hydrated_snapshot(
    update: &desktop::runtime::DesktopRuntimeUpdate,
) -> Option<&desktop::runtime::DesktopRuntimeHydratedSnapshot> {
    match update {
        desktop::runtime::DesktopRuntimeUpdate::SessionChanged { snapshot, .. }
        | desktop::runtime::DesktopRuntimeUpdate::PromptAcceptedWithSession { snapshot, .. }
        | desktop::runtime::DesktopRuntimeUpdate::PromptFinished { snapshot, .. } => Some(snapshot),
        desktop::runtime::DesktopRuntimeUpdate::PromptRejectedWithSession {
            snapshot: Some(snapshot),
            ..
        } => Some(snapshot),
        desktop::runtime::DesktopRuntimeUpdate::Resynced {
            replacement: desktop::runtime::DesktopRuntimeResyncSnapshot::Hydrated(snapshot),
            ..
        } => Some(snapshot),
        _ => None,
    }
}

fn runtime_update_command_id(update: &desktop::runtime::DesktopRuntimeUpdate) -> Option<u64> {
    use desktop::runtime::DesktopRuntimeUpdate;
    match update {
        DesktopRuntimeUpdate::Reloaded { command_id, .. }
        | DesktopRuntimeUpdate::Resynced { command_id, .. }
        | DesktopRuntimeUpdate::SessionChanged { command_id, .. }
        | DesktopRuntimeUpdate::SessionClosed { command_id, .. }
        | DesktopRuntimeUpdate::SessionsListed { command_id, .. }
        | DesktopRuntimeUpdate::SessionRenamed { command_id, .. }
        | DesktopRuntimeUpdate::SelectionChanged { command_id, .. }
        | DesktopRuntimeUpdate::PromptAccepted { command_id }
        | DesktopRuntimeUpdate::PromptAcceptedWithSession { command_id, .. }
        | DesktopRuntimeUpdate::PromptRejectedWithSession { command_id, .. }
        | DesktopRuntimeUpdate::PromptStarted { command_id, .. }
        | DesktopRuntimeUpdate::ControlAccepted { command_id, .. }
        | DesktopRuntimeUpdate::AuthorizationDecisionAccepted { command_id, .. }
        | DesktopRuntimeUpdate::RecoveryChanged { command_id, .. }
        | DesktopRuntimeUpdate::FileReviewed { command_id, .. }
        | DesktopRuntimeUpdate::ExternalEditorOpened { command_id, .. }
        | DesktopRuntimeUpdate::PromptFinished { command_id, .. }
        | DesktopRuntimeUpdate::CommandRejected { command_id, .. } => Some(*command_id),
        DesktopRuntimeUpdate::ProductEvent { .. }
        | DesktopRuntimeUpdate::ResyncRequired { .. }
        | DesktopRuntimeUpdate::RuntimeFailed { .. }
        | DesktopRuntimeUpdate::Stopped => None,
    }
}

pub(super) struct NativeShell {
    runtime: Option<DesktopRuntimeCommandHandle>,
    runtime_updates: VecDeque<desktop::runtime::DesktopRuntimeUpdate>,
    next_command_id: u64,
    active_workspace: SessionWorkspace,
    workspaces: HashMap<String, SessionWorkspace>,
    global_skills: Arc<[CodingAgentResourceCommand]>,
    preferences: DesktopPreferences,
    preference_writer: Option<PreferenceWriter>,
    inspector_telemetry_last_refresh: Option<Instant>,
    inspector_telemetry_refresh_deadline: Option<Instant>,
    conversation_pane: gpui::Entity<ConversationPane>,
    conversation_header: gpui::Entity<ConversationHeader>,
    sessions_pane: gpui::Entity<SessionsPane>,
    composer_pane: gpui::Entity<ComposerPane>,
    home_pane: gpui::Entity<HomePane>,
    inspector_pane: gpui::Entity<InspectorPane>,
    toast_host: gpui::Entity<ToastHost>,
    overlay_host: gpui::Entity<OverlayHost>,
    focus: FocusState,
    sessions_focus: FocusHandle,
    conversation_focus: FocusHandle,
    context_focus: FocusHandle,
    authorization_focus: FocusHandle,
    command_palette_focus: FocusHandle,
    narrow_sessions_focus: FocusHandle,
    full_message_focus: FocusHandle,
    command_palette: DesktopCommandPalette,
    active_overlay: Option<DesktopOverlayKind>,
    conversation_full_message: Option<ConversationFullMessageView>,
    conversation_announcement: Option<(u64, String)>,
    conversation_announcement_sequence: u64,
    narrow_sessions_open: bool,
    narrow_context_open: bool,
    session_controller: SessionController,
    panel_resize: Option<PanelResizeState>,
    focus_input_modality: FocusInputModality,
    #[cfg(test)]
    runtime_ui_notification_count: usize,
    _subscriptions: Vec<Subscription>,
}

impl Deref for NativeShell {
    type Target = SessionWorkspace;

    fn deref(&self) -> &Self::Target {
        &self.active_workspace
    }
}

impl DerefMut for NativeShell {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.active_workspace
    }
}

pub(super) struct NativeShellInit {
    pub(super) runtime: DesktopRuntimeBridge,
    pub(super) project: CodingAgentEmbeddingSnapshot,
    pub(super) projection: Option<DesktopProjection>,
    pub(super) global_skills: Arc<[CodingAgentResourceCommand]>,
    pub(super) preferences: DesktopPreferences,
    pub(super) preference_writer: Option<PreferenceWriter>,
    pub(super) preference_notice: Option<String>,
    pub(super) initial_session_id: Option<String>,
}

impl NativeShell {
    pub(super) fn new(init: NativeShellInit, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let NativeShellInit {
            runtime,
            project,
            projection,
            global_skills,
            preferences,
            preference_writer,
            mut preference_notice,
            initial_session_id,
        } = init;
        let (runtime_commands, mut runtime_events, runtime_shutdown) = runtime.into_parts();
        let mut command_ledger = DesktopCommandLedger::default();
        if let Some(session_id) = initial_session_id {
            let intent = DesktopCommandIntent::OpenSession {
                session_id: session_id.clone(),
            };
            match command_ledger.reserve(intent.clone()) {
                Ok(command_id) => {
                    if let Err(error) = runtime_commands.try_open_session(command_id, &session_id) {
                        command_ledger.complete(command_id, &intent);
                        preference_notice = Some(error.to_string());
                    }
                }
                Err(error) => preference_notice = Some(error.to_string()),
            }
        }
        let sessions_focus = cx.focus_handle().tab_stop(true).tab_index(1);
        let conversation_focus = cx.focus_handle().tab_stop(true).tab_index(2);
        let context_focus = cx.focus_handle().tab_stop(true).tab_index(4);
        let authorization_focus = cx.focus_handle().tab_stop(true).tab_index(3);
        let command_palette_focus = cx.focus_handle().tab_stop(true).tab_index(3);
        let narrow_sessions_focus = cx.focus_handle().tab_stop(true).tab_index(3);
        let full_message_focus = cx.focus_handle().tab_stop(true).tab_index(3);
        let conversation_pane = cx.new(|_| ConversationPane::new());
        let conversation_header = cx.new(|_| ConversationHeader::new(conversation_focus.clone()));
        let sessions_pane = cx.new(|cx| SessionsPane::new(sessions_focus.clone(), window, cx));
        let composer_pane = cx.new(|cx| ComposerPane::new(window, cx));
        let home_pane = cx.new(|_| HomePane::new());
        let inspector_pane = cx.new(|cx| InspectorPane::new(context_focus.clone(), cx));
        let toast_host = cx.new(|cx| ToastHost::new(window, cx));
        let overlay_host = cx.new(|_| {
            OverlayHost::new(
                inspector_pane.clone(),
                sessions_pane.clone(),
                authorization_focus.clone(),
                command_palette_focus.clone(),
                narrow_sessions_focus.clone(),
                full_message_focus.clone(),
            )
        });

        let subscriptions = vec![
            cx.on_focus(&sessions_focus, window, |this, window, cx| {
                this.record_focus(FocusTarget::Sessions, window, cx);
            }),
            cx.on_focus(&conversation_focus, window, |this, window, cx| {
                this.record_focus(FocusTarget::Conversation, window, cx);
            }),
            cx.on_focus(&context_focus, window, |this, window, cx| {
                this.record_focus(FocusTarget::Context, window, cx);
            }),
            cx.subscribe_in(
                &conversation_pane,
                window,
                |this, _, event: &ConversationPaneEvent, window, cx| match event {
                    ConversationPaneEvent::Select { block_id, durable } => {
                        this.record_focus(FocusTarget::Conversation, window, cx);
                        let workspace = &mut this.active_workspace;
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
                    ConversationPaneEvent::CopyToolCommand { block_id } => {
                        this.copy_tool_command(block_id, cx);
                    }
                    ConversationPaneEvent::CopyToolOutput { block_id } => {
                        this.copy_tool_output(block_id, cx);
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
                    ConversationPaneEvent::OpenToolOutput { block_id } => {
                        this.open_full_tool_output(block_id, window, cx);
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
                    SessionsPaneEvent::NewConversation => {
                        this.show_home_workspace(window, cx);
                    }
                    SessionsPaneEvent::Refresh => this.request_session_catalog(cx),
                    SessionsPaneEvent::Open(session_id) => {
                        this.open_session(session_id.clone(), cx);
                    }
                    SessionsPaneEvent::Rename(session_id, name) => {
                        this.rename_session(session_id.clone(), name.clone(), cx);
                    }
                    SessionsPaneEvent::CloseSession(session_id) => {
                        this.close_session(session_id, cx);
                    }
                    SessionsPaneEvent::Dismiss => {
                        this.narrow_sessions_open = false;
                        this.dismiss_overlay(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &composer_pane,
                window,
                |this, _, event: &ComposerPaneEvent, window, cx| match event {
                    ComposerPaneEvent::InputChanged(value) => {
                        this.composer.edit(value.clone());
                        this.notify_composer_pane(cx);
                    }
                    ComposerPaneEvent::Focused => {
                        this.record_focus(FocusTarget::Composer, window, cx);
                    }
                    ComposerPaneEvent::AddAttachments => this.choose_composer_attachments(cx),
                    ComposerPaneEvent::RemoveAttachment(index) => {
                        this.remove_composer_attachment(*index, cx);
                    }
                    ComposerPaneEvent::SubmitPrimary => {
                        if !this.root_action_blocked_by_overlay(window, cx) {
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
                &home_pane,
                window,
                |this, _, event: &HomePaneEvent, _, cx| match event {
                    HomePaneEvent::OpenSession(session_id) => {
                        this.open_session(session_id.clone(), cx);
                    }
                },
            ),
            cx.subscribe_in(
                &inspector_pane,
                window,
                |this, _, event: &InspectorPaneEvent, window, cx| match event {
                    InspectorPaneEvent::Close => {
                        this.narrow_context_open = false;
                        this.dismiss_overlay(window, cx);
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
                        this.inspector_section = *section;
                        this.notify_inspector_pane(cx);
                    }
                },
            ),
            cx.subscribe_in(
                &overlay_host,
                window,
                |this, _, event: &OverlayHostEvent, window, cx| match event {
                    OverlayHostEvent::ExecutePalette(command) => {
                        this.command_palette.close();
                        this.dismiss_overlay(window, cx);
                        this.execute_palette_command(*command, window, cx);
                    }
                    OverlayHostEvent::DecideAuthorization { identity, decision } => {
                        this.decide_tool_authorization(identity.clone(), decision.clone(), cx);
                    }
                    OverlayHostEvent::CopyFullMessage => {
                        if let Some(message) = &this.conversation_full_message {
                            cx.write_to_clipboard(ClipboardItem::new_string(
                                message.text.to_string(),
                            ));
                            this.announce_conversation_copy("Full message copied.", cx);
                        }
                    }
                    OverlayHostEvent::CloseFullMessage => {
                        this.close_full_conversation_message(window, cx);
                    }
                },
            ),
            cx.observe_window_bounds(window, Self::window_bounds_changed),
        ];

        let composer_focus = composer_pane.read(cx).focus_handle().clone();
        composer_focus.focus(window, cx);
        cx.spawn(async move |this, cx| {
            let runtime_shutdown = runtime_shutdown;
            while let Some(updates) = runtime_events.next_update_batch().await {
                let Some(this) = this.upgrade() else {
                    break;
                };
                this.update(cx, |this, cx| {
                    this.runtime_updates.extend(updates);
                    this.poll_runtime(cx)
                });
            }
            let _ = runtime_shutdown.shutdown(&mut runtime_events).await;
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |this, cx| this.request_session_catalog(cx));
        })
        .detach();

        let next_command_id = command_ledger.next_command_id();
        let thinking_selection = projection
            .as_ref()
            .map(|projection| {
                preferences.thinking_level_for_session(&projection.snapshot().session.session_id)
            })
            .unwrap_or_default();
        let active_workspace = SessionWorkspace::new_with_thinking(
            project,
            projection,
            preference_notice,
            command_ledger,
            thinking_selection,
        );
        let shell = Self {
            runtime: Some(runtime_commands),
            runtime_updates: VecDeque::new(),
            next_command_id,
            active_workspace,
            workspaces: HashMap::with_capacity(MAX_SESSION_WORKSPACES.saturating_sub(1)),
            global_skills,
            preferences,
            preference_writer,
            inspector_telemetry_last_refresh: None,
            inspector_telemetry_refresh_deadline: None,
            conversation_pane,
            conversation_header,
            sessions_pane,
            composer_pane,
            home_pane,
            inspector_pane,
            toast_host,
            overlay_host,
            focus: FocusState::default(),
            sessions_focus,
            conversation_focus,
            context_focus,
            authorization_focus,
            command_palette_focus,
            narrow_sessions_focus,
            full_message_focus,
            command_palette: DesktopCommandPalette::default(),
            active_overlay: None,
            conversation_full_message: None,
            conversation_announcement: None,
            conversation_announcement_sequence: 0,
            narrow_sessions_open: false,
            narrow_context_open: false,
            session_controller: SessionController::default(),
            panel_resize: None,
            focus_input_modality: FocusInputModality::default(),
            #[cfg(test)]
            runtime_ui_notification_count: 0,
            _subscriptions: subscriptions,
        };
        shell.notify_toast_host(cx);
        let conversation_header_view_model = shell.conversation_header_view_model();
        shell
            .conversation_header
            .update(cx, |conversation_header, _| {
                conversation_header.set_view_model(conversation_header_view_model);
            });
        let sessions_pane_view_model = shell.sessions_pane_view_model();
        shell.sessions_pane.update(cx, |sessions_pane, _| {
            sessions_pane.set_view_model(sessions_pane_view_model);
        });
        let composer_pane_view_model = shell.composer_pane_view_model();
        shell.composer_pane.update(cx, |composer_pane, _| {
            composer_pane.set_view_model(composer_pane_view_model);
        });
        let home_pane_view_model = shell.home_pane_view_model();
        shell.home_pane.update(cx, |home_pane, _| {
            home_pane.set_view_model(home_pane_view_model);
        });
        let conversation_pane_view_model = shell.conversation_pane_view_model();
        shell.conversation_pane.update(cx, |conversation_pane, _| {
            conversation_pane.set_view_model(conversation_pane_view_model);
        });
        let inspector_pane_view_model = shell.inspector_pane_view_model();
        shell.inspector_pane.update(cx, |inspector_pane, _| {
            inspector_pane.set_view_model(inspector_pane_view_model);
        });
        let overlay_view_model = shell.overlay_view_model();
        shell.overlay_host.update(cx, |overlay_host, _| {
            overlay_host.set_view_model(overlay_view_model);
        });
        shell
    }

    fn command_owner_session_id(&self, command_id: u64) -> Option<String> {
        if self.command_ledger.intent(command_id).is_some() {
            return Some(self.active_workspace.session_id().to_owned());
        }
        self.workspaces.iter().find_map(|(session_id, workspace)| {
            workspace
                .command_ledger
                .intent(command_id)
                .is_some()
                .then(|| session_id.clone())
        })
    }

    fn complete_workspace_command(
        &mut self,
        session_id: &str,
        command_id: u64,
        intent: &DesktopCommandIntent,
    ) -> bool {
        if self.active_workspace.session_id() == session_id
            || (session_id == HOME_COMPOSER_SESSION_KEY
                && !self.workspaces.contains_key(HOME_COMPOSER_SESSION_KEY)
                && self.command_ledger.matches(command_id, intent))
        {
            return self.command_ledger.complete(command_id, intent);
        }
        self.workspaces
            .get_mut(session_id)
            .is_some_and(|workspace| workspace.command_ledger.complete(command_id, intent))
    }

    fn runtime_update_session_id(
        &self,
        update: &desktop::runtime::DesktopRuntimeUpdate,
    ) -> Option<String> {
        use desktop::runtime::{DesktopRuntimeResyncSnapshot, DesktopRuntimeUpdate};
        match update {
            DesktopRuntimeUpdate::ProductEvent { session_id, .. } => Some(session_id.clone()),
            update if runtime_update_hydrated_snapshot(update).is_some() => {
                runtime_update_hydrated_snapshot(update)
                    .map(|snapshot| hydrated_session_id(snapshot).to_owned())
            }
            DesktopRuntimeUpdate::Reloaded { metadata, .. }
            | DesktopRuntimeUpdate::SelectionChanged { metadata, .. }
            | DesktopRuntimeUpdate::PromptStarted { metadata, .. }
            | DesktopRuntimeUpdate::PromptRejectedWithSession { metadata, .. } => metadata
                .session
                .as_ref()
                .map(|snapshot| snapshot.session.session_id.clone()),
            DesktopRuntimeUpdate::Resynced {
                replacement: DesktopRuntimeResyncSnapshot::Metadata(metadata),
                ..
            } => metadata
                .session
                .as_ref()
                .map(|snapshot| snapshot.session.session_id.clone()),
            DesktopRuntimeUpdate::RecoveryChanged { recovery, .. } => {
                Some(recovery.session.session.session_id.clone())
            }
            DesktopRuntimeUpdate::ResyncRequired { snapshot, .. } => {
                Some(snapshot.session.session_id.clone())
            }
            DesktopRuntimeUpdate::SessionsListed { .. }
            | DesktopRuntimeUpdate::SessionRenamed { .. }
            | DesktopRuntimeUpdate::SessionClosed { .. } => None,
            _ => runtime_update_command_id(update)
                .and_then(|command_id| self.command_owner_session_id(command_id)),
        }
    }

    fn swap_active_workspace(&mut self, target_session_id: &str) -> bool {
        if self.active_workspace.session_id() == target_session_id {
            return true;
        }
        let Some(target) = self.workspaces.remove(target_session_id) else {
            return false;
        };
        let previous = std::mem::replace(&mut self.active_workspace, target);
        self.workspaces
            .insert(previous.session_id().to_owned(), previous);
        true
    }

    fn show_home_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_workspace.session_id() != HOME_COMPOSER_SESSION_KEY
            && !self.swap_active_workspace(HOME_COMPOSER_SESSION_KEY)
        {
            let home = SessionWorkspace::new_with_thinking(
                self.project.clone(),
                None,
                None,
                DesktopCommandLedger::default(),
                self.thinking_selection,
            );
            let previous = std::mem::replace(&mut self.active_workspace, home);
            self.workspaces
                .insert(previous.session_id().to_owned(), previous);
        }

        self.narrow_sessions_open = false;
        if self.active_overlay == Some(DesktopOverlayKind::NarrowSessions) {
            self.dismiss_overlay(window, cx);
        }
        self.record_focus(FocusTarget::Composer, window, cx);
        self.notify_sessions_pane(cx);
        self.notify_home_pane(cx);
        self.notify_composer_pane(cx);
        self.notify_conversation_pane(cx);
        self.notify_conversation_header(cx);
        self.notify_inspector_pane(cx);
        self.notify_overlay_host(cx);
        cx.notify();
    }

    fn install_hydrated_workspace(
        &mut self,
        snapshot: &desktop::runtime::DesktopRuntimeHydratedSnapshot,
        inherit_home_thinking: bool,
    ) -> bool {
        let target_session_id = hydrated_session_id(snapshot);
        if self.active_workspace.session_id() == target_session_id {
            return true;
        }
        if self.workspaces.contains_key(target_session_id) {
            return self.swap_active_workspace(target_session_id);
        }
        if self.open_workspace_count() >= MAX_SESSION_WORKSPACES {
            self.set_preference_notice(format!(
                "Up to {MAX_SESSION_WORKSPACES} sessions can stay open; close one first."
            ));
            return false;
        }
        let projection = match DesktopProjection::new(snapshot.clone()) {
            Ok(projection) => projection,
            Err(issue) => {
                self.set_preference_notice(format!(
                    "Session response failed projection validation ({}).",
                    truncate_label(&issue.code, 28)
                ));
                return false;
            }
        };
        let thinking_selection = if inherit_home_thinking
            && self.active_workspace.session_id() == HOME_COMPOSER_SESSION_KEY
        {
            self.thinking_selection
        } else {
            self.preferences
                .thinking_level_for_session(target_session_id)
        };
        if self.active_workspace.session_id() == HOME_COMPOSER_SESSION_KEY
            && self.workspaces.is_empty()
        {
            self.project = snapshot.project.clone();
            self.projection = Some(projection);
            self.thinking_selection = thinking_selection;
            self.remember_thinking_selection(target_session_id, thinking_selection);
            return true;
        }
        let target = SessionWorkspace::new_with_thinking(
            snapshot.project.clone(),
            Some(projection),
            None,
            DesktopCommandLedger::default(),
            thinking_selection,
        );
        let previous = std::mem::replace(&mut self.active_workspace, target);
        self.workspaces
            .insert(previous.session_id().to_owned(), previous);
        true
    }

    fn open_workspace_count(&self) -> usize {
        self.workspaces.len() + usize::from(self.projection.is_some())
    }

    fn reserve_session_command(
        &mut self,
        session_id: &str,
        intent: DesktopCommandIntent,
    ) -> Result<u64, String> {
        let command_id = self.next_command_id;
        let next_command_id = command_id
            .checked_add(1)
            .ok_or_else(|| "Desktop command sequence is exhausted; restart the app.".to_owned())?;
        let ledger = if self.active_workspace.session_id() == session_id {
            &mut self.active_workspace.command_ledger
        } else {
            &mut self
                .workspaces
                .get_mut(session_id)
                .ok_or_else(|| "Cannot close an unavailable session.".to_owned())?
                .command_ledger
        };
        ledger
            .reserve_with_id(command_id, intent)
            .map_err(|error| error.to_string())?;
        self.next_command_id = next_command_id;
        Ok(command_id)
    }

    fn close_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let intent = DesktopCommandIntent::CloseSession {
            session_id: session_id.to_owned(),
        };
        let command_id = match self.reserve_session_command(session_id, intent.clone()) {
            Ok(command_id) => command_id,
            Err(error) => {
                self.set_preference_notice(error);
                self.notify_sessions_pane(cx);
                return;
            }
        };
        let admission = self
            .runtime
            .as_ref()
            .ok_or_else(|| "desktop runtime is unavailable".to_owned())
            .and_then(|runtime| {
                runtime
                    .try_close_session(command_id, session_id)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = admission {
            let _ = self.complete_workspace_command(session_id, command_id, &intent);
            self.set_preference_notice(error);
        }
        self.notify_sessions_pane(cx);
    }

    fn remove_closed_workspace(&mut self, session_id: &str) {
        if self.active_workspace.session_id() != session_id {
            self.workspaces.remove(session_id);
            return;
        }
        if let Some(next_session_id) = self.workspaces.keys().min().cloned() {
            let _ = self.swap_active_workspace(&next_session_id);
            self.workspaces.remove(session_id);
        } else {
            let project = self.project.clone();
            let command_ledger = std::mem::take(&mut self.active_workspace.command_ledger);
            self.active_workspace = SessionWorkspace::new(
                project,
                None,
                Some("Closed the last open session.".into()),
                command_ledger,
            );
        }
    }

    pub(super) fn install_native_visual_session_fixture(&mut self) {
        let Some(projection) = self.projection.as_ref() else {
            return;
        };
        let session_id = projection.snapshot().session.session_id.clone();
        self.session_controller.replace_catalog(
            vec![desktop::runtime::DesktopSessionCatalogEntry {
                session_id,
                name: Some("Current desktop task".into()),
                cwd: Some(self.project.cwd.display().to_string()),
                // A future timestamp is clamped to zero elapsed time by the
                // presentation helper, keeping the replay's `now` label
                // deterministic across calendar dates.
                created_at: "9999-12-31T23:59:59Z".into(),
                updated_at: "9999-12-31T23:59:59Z".into(),
                active_leaf_id: None,
            }],
            0,
        );
        self.narrow_sessions_open = true;
    }

    fn visibility(&self) -> PanelVisibility {
        PanelVisibility {
            sessions: self.preferences.sessions_panel_visible,
            context: self.preferences.context_panel_visible,
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
        if self.projection.is_none() {
            return ShellLayout::resolve_idle(width, height);
        }
        ShellLayout::resolve_with_panel_widths(
            width,
            height,
            visibility,
            self.preferences.sessions_panel_width,
            self.preferences.context_panel_width,
        )
    }

    fn begin_panel_resize(
        &mut self,
        panel: ResizablePanel,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        if event.click_count >= 2 {
            self.panel_resize = None;
            match panel {
                ResizablePanel::Sessions => {
                    self.preferences.sessions_panel_width = SESSION_PANEL_WIDTH;
                    self.notify_sessions_pane(cx);
                    self.notify_conversation_header(cx);
                }
                ResizablePanel::Context => {
                    self.preferences.context_panel_width = CONTEXT_PANEL_WIDTH;
                    self.notify_inspector_pane(cx);
                    self.notify_conversation_header(cx);
                }
            }
            self.schedule_preferences();
            cx.notify();
            return;
        }

        self.panel_resize = Some(PanelResizeState {
            panel,
            pointer_origin_x: f32::from(event.position.x),
            width_origin: match panel {
                ResizablePanel::Sessions => self.preferences.sessions_panel_width,
                ResizablePanel::Context => self.preferences.context_panel_width,
            },
        });
    }

    fn update_panel_resize(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(resize) = self.panel_resize else {
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
                layout.context.map_or(0, |bounds| bounds.width),
            ),
            ResizablePanel::Context => (
                CONTEXT_PANEL_MIN_WIDTH,
                CONTEXT_PANEL_MAX_WIDTH,
                layout.sessions.map_or(0, |bounds| bounds.width),
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
            ResizablePanel::Sessions if self.preferences.sessions_panel_width != width => {
                self.preferences.sessions_panel_width = width;
                self.notify_sessions_pane(cx);
                self.notify_conversation_header(cx);
                cx.notify();
            }
            ResizablePanel::Context if self.preferences.context_panel_width != width => {
                self.preferences.context_panel_width = width;
                self.notify_inspector_pane(cx);
                self.notify_conversation_header(cx);
                cx.notify();
            }
            _ => {}
        }
    }

    fn finish_panel_resize(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if self.panel_resize.take().is_some() {
            self.schedule_preferences();
        }
    }

    fn set_focus_input_modality(&mut self, modality: FocusInputModality, cx: &mut Context<Self>) {
        if self.focus_input_modality == modality {
            return;
        }
        self.focus_input_modality = modality;
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
        self.focus_input_modality == FocusInputModality::Keyboard
    }

    fn record_focus(&mut self, target: FocusTarget, window: &mut Window, cx: &mut Context<Self>) {
        let layout = self.layout(window);
        let previous = self.focus.active();
        if self.focus.request(target, layout) {
            cx.notify();
        }
        if previous == FocusTarget::Sessions || target == FocusTarget::Sessions {
            self.notify_sessions_pane(cx);
        }
        if previous == FocusTarget::Conversation || target == FocusTarget::Conversation {
            self.notify_conversation_header(cx);
        }
        if previous == FocusTarget::Composer || target == FocusTarget::Composer {
            self.notify_composer_pane(cx);
        }
        if previous == FocusTarget::Context || target == FocusTarget::Context {
            self.notify_inspector_pane(cx);
        }
    }

    fn window_bounds_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = window.window_bounds();
        let restore = bounds.get_bounds();
        self.preferences.window.x = f32::from(restore.origin.x).round() as i32;
        self.preferences.window.y = f32::from(restore.origin.y).round() as i32;
        self.preferences.window.width = u32::from(restore.size.width);
        self.preferences.window.height = u32::from(restore.size.height);
        self.preferences.window.maximized = matches!(bounds, WindowBounds::Maximized(_));

        let viewport = window.viewport_size();
        let forced_layout = self.resolve_layout(
            u32::from(viewport.width),
            u32::from(viewport.height),
            PanelVisibility::default(),
        );
        if self.narrow_sessions_open && forced_layout.sessions.is_some() {
            self.narrow_sessions_open = false;
            self.preferences.sessions_panel_visible = true;
            if self.active_overlay == Some(DesktopOverlayKind::NarrowSessions) {
                self.active_overlay = None;
                self.focus.close_overlay(self.layout(window));
            }
        }
        if self.narrow_context_open && forced_layout.context.is_some() {
            self.narrow_context_open = false;
            self.preferences.context_panel_visible = true;
            if self.active_overlay == Some(DesktopOverlayKind::NarrowContext) {
                self.active_overlay = None;
                self.focus.close_overlay(self.layout(window));
            }
        }
        let layout = self.layout(window);
        let previous_focus = self.focus.active();
        self.focus.reconcile_layout(layout);
        if self.focus.active() != previous_focus {
            self.focus_composer_input(window, cx);
        }
        self.schedule_preferences();
        self.notify_inspector_pane(cx);
        self.notify_conversation_header(cx);
        self.notify_overlay_host(cx);
        cx.notify();
    }

    fn poll_runtime(&mut self, cx: &mut Context<Self>) -> bool {
        if self.runtime.is_none() {
            return false;
        }
        let mut applied = 0;
        let mut sessions_pane_dirty = false;
        let mut composer_pane_dirty = false;
        let mut inspector_pane_dirty = false;
        let mut inspector_telemetry_dirty = false;
        let mut toast_host_dirty = false;
        let mut conversation_header_dirty = false;
        let mut overlay_host_dirty = false;
        let mut root_dirty = false;
        while applied < MAX_RUNTIME_UPDATES_PER_FRAME {
            let Some(update) = self.runtime_updates.pop_front() else {
                break;
            };
            let foreground_session_id = self.active_workspace.session_id().to_owned();
            let dirty_before = (
                sessions_pane_dirty,
                composer_pane_dirty,
                inspector_pane_dirty,
                inspector_telemetry_dirty,
                toast_host_dirty,
                conversation_header_dirty,
                overlay_host_dirty,
                root_dirty,
            );
            let is_session_change = matches!(
                &update,
                desktop::runtime::DesktopRuntimeUpdate::SessionChanged { .. }
            );
            let inherit_home_thinking = match &update {
                desktop::runtime::DesktopRuntimeUpdate::PromptAcceptedWithSession { .. }
                | desktop::runtime::DesktopRuntimeUpdate::PromptRejectedWithSession {
                    snapshot: Some(_),
                    ..
                } => true,
                desktop::runtime::DesktopRuntimeUpdate::SessionChanged { command_id, .. } => self
                    .command_ledger
                    .matches(*command_id, &DesktopCommandIntent::CreateSession),
                _ => false,
            };
            let mut background_update = false;
            if !is_session_change
                && let Some(target_session_id) = self.runtime_update_session_id(&update)
                && target_session_id != foreground_session_id
            {
                if self.swap_active_workspace(&target_session_id) {
                    background_update = true;
                } else if let Some(snapshot) = runtime_update_hydrated_snapshot(&update)
                    && self.install_hydrated_workspace(snapshot, inherit_home_thinking)
                {
                    background_update = foreground_session_id != HOME_COMPOSER_SESSION_KEY;
                }
            }
            if !matches!(
                &update,
                desktop::runtime::DesktopRuntimeUpdate::ProductEvent { .. }
            ) {
                toast_host_dirty = true;
                conversation_header_dirty = true;
                overlay_host_dirty = true;
                root_dirty = true;
            }
            let update = match commands::reconcile_direct_update(self, update, cx) {
                DirectCommandUpdate::Continue(update) => *update,
                DirectCommandUpdate::Consumed {
                    sessions_dirty,
                    inspector_dirty,
                } => {
                    sessions_pane_dirty |= sessions_dirty;
                    inspector_pane_dirty |= inspector_dirty;
                    if background_update {
                        let _ = self.swap_active_workspace(&foreground_session_id);
                        (
                            sessions_pane_dirty,
                            composer_pane_dirty,
                            inspector_pane_dirty,
                            inspector_telemetry_dirty,
                            toast_host_dirty,
                            conversation_header_dirty,
                            overlay_host_dirty,
                            root_dirty,
                        ) = dirty_before;
                    }
                    applied += 1;
                    continue;
                }
            };
            let composer_pane_state_before = self.composer_pane_state();
            let projection_completions = ProjectionCommandCompletions::capture(self, &update);
            if let desktop::runtime::DesktopRuntimeUpdate::SessionChanged { snapshot, .. } = &update
                && self.active_workspace.session_id() != hydrated_session_id(snapshot)
                && !self.install_hydrated_workspace(snapshot, inherit_home_thinking)
            {
                let _ = projection_completions.reconcile(self, false, cx);
                applied += 1;
                continue;
            }
            match &update {
                desktop::runtime::DesktopRuntimeUpdate::PromptAccepted { command_id }
                | desktop::runtime::DesktopRuntimeUpdate::PromptAcceptedWithSession {
                    command_id,
                    ..
                } => {
                    if self
                        .command_ledger
                        .complete(*command_id, &DesktopCommandIntent::Prompt)
                        && self.composer.accepted(*command_id).is_ok()
                    {
                        self.composer_attachments.clear();
                        self.composer_needs_sync = true;
                        self.conversation_controller.mark_live_dirty();
                        sessions_pane_dirty = true;
                    }
                }
                desktop::runtime::DesktopRuntimeUpdate::PromptRejectedWithSession {
                    command_id,
                    error,
                    ..
                } => {
                    if self
                        .command_ledger
                        .complete_rejection(
                            *command_id,
                            desktop::runtime::DesktopRuntimeCommandKind::SubmitPrompt,
                        )
                        .is_some()
                    {
                        let _ = self.composer.rejected(
                            *command_id,
                            safe_runtime_rejection_notice(
                                desktop::runtime::DesktopRuntimeCommandKind::SubmitPrompt,
                                &error.code,
                            ),
                        );
                        sessions_pane_dirty = true;
                    }
                }
                desktop::runtime::DesktopRuntimeUpdate::PromptStarted { command_id, .. } => {
                    if self
                        .composer
                        .submitted()
                        .is_some_and(|submitted| submitted.command_id != *command_id)
                    {
                        self.set_preference_notice(
                            "Prompt start did not match the submitted command.".into(),
                        );
                    }
                }
                desktop::runtime::DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command: desktop::runtime::DesktopRuntimeCommandKind::SubmitPrompt,
                    code,
                    ..
                } => {
                    if self
                        .command_ledger
                        .complete_rejection(
                            *command_id,
                            desktop::runtime::DesktopRuntimeCommandKind::SubmitPrompt,
                        )
                        .is_some()
                    {
                        let _ = self.composer.rejected(
                            *command_id,
                            safe_runtime_rejection_notice(
                                desktop::runtime::DesktopRuntimeCommandKind::SubmitPrompt,
                                code,
                            ),
                        );
                        sessions_pane_dirty = true;
                    }
                }
                desktop::runtime::DesktopRuntimeUpdate::ControlAccepted {
                    command_id,
                    command: desktop::runtime::DesktopRuntimeCommandKind::Abort,
                    receipt,
                } if self.command_ledger.complete(
                    *command_id,
                    &DesktopCommandIntent::Abort {
                        operation_id: receipt.operation_id.clone(),
                    },
                ) =>
                {
                    self.set_preference_notice(format!(
                        "Abort accepted for {}.",
                        receipt.operation_id
                    ));
                }
                desktop::runtime::DesktopRuntimeUpdate::ControlAccepted {
                    command_id,
                    command:
                        command @ (desktop::runtime::DesktopRuntimeCommandKind::Steer
                        | desktop::runtime::DesktopRuntimeCommandKind::FollowUp),
                    receipt,
                } => {
                    let intent = match command {
                        desktop::runtime::DesktopRuntimeCommandKind::Steer => {
                            DesktopCommandIntent::Steer
                        }
                        desktop::runtime::DesktopRuntimeCommandKind::FollowUp => {
                            DesktopCommandIntent::FollowUp
                        }
                        _ => unreachable!("match pattern admits only active controls"),
                    };
                    if self.command_ledger.complete(*command_id, &intent)
                        && self.composer.accepted(*command_id).is_ok()
                    {
                        self.composer_needs_sync = true;
                        self.set_preference_notice(format!(
                            "{command:?} accepted for {}.",
                            receipt.operation_id
                        ));
                    }
                }
                desktop::runtime::DesktopRuntimeUpdate::AuthorizationDecisionAccepted {
                    command_id,
                    authorization_id,
                    decision,
                } if self
                    .command_ledger
                    .complete_authorization(*command_id, authorization_id) =>
                {
                    let decision = match decision {
                        ToolAuthorizationDecision::AllowOnce => "allow once",
                        ToolAuthorizationDecision::AllowForOperation => "allow for operation",
                        ToolAuthorizationDecision::Deny { .. } => "deny",
                    };
                    self.set_preference_notice(format!(
                        "Authorization decision accepted: {decision}."
                    ));
                }
                desktop::runtime::DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command: desktop::runtime::DesktopRuntimeCommandKind::Abort,
                    code,
                    ..
                } if self
                    .command_ledger
                    .complete_rejection(
                        *command_id,
                        desktop::runtime::DesktopRuntimeCommandKind::Abort,
                    )
                    .is_some() =>
                {
                    self.set_preference_notice(safe_runtime_rejection_notice(
                        desktop::runtime::DesktopRuntimeCommandKind::Abort,
                        code,
                    ));
                }
                desktop::runtime::DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command: desktop::runtime::DesktopRuntimeCommandKind::Reload,
                    code,
                    ..
                } if self
                    .command_ledger
                    .complete_rejection(
                        *command_id,
                        desktop::runtime::DesktopRuntimeCommandKind::Reload,
                    )
                    .is_some() =>
                {
                    self.set_preference_notice(format!(
                        "Reload failed ({}); previous context retained.",
                        truncate_label(code, 28)
                    ));
                }
                desktop::runtime::DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command:
                        command @ (desktop::runtime::DesktopRuntimeCommandKind::SelectModel
                        | desktop::runtime::DesktopRuntimeCommandKind::SelectSessionProfile),
                    code,
                    ..
                } if self
                    .command_ledger
                    .complete_rejection(*command_id, *command)
                    .is_some() =>
                {
                    self.set_preference_notice(format!(
                        "{command:?} failed ({}); previous selection retained.",
                        truncate_label(code, 28)
                    ));
                }
                desktop::runtime::DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command:
                        command @ (desktop::runtime::DesktopRuntimeCommandKind::Steer
                        | desktop::runtime::DesktopRuntimeCommandKind::FollowUp),
                    code,
                    ..
                } => {
                    let notice = safe_runtime_rejection_notice(*command, code);
                    if self
                        .command_ledger
                        .complete_rejection(*command_id, *command)
                        .is_some()
                        && self.composer.rejected(*command_id, notice.clone()).is_ok()
                    {
                        self.set_preference_notice(notice);
                    }
                }
                desktop::runtime::DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command: desktop::runtime::DesktopRuntimeCommandKind::DecideToolAuthorization,
                    code,
                    ..
                } if self
                    .command_ledger
                    .complete_rejection(
                        *command_id,
                        desktop::runtime::DesktopRuntimeCommandKind::DecideToolAuthorization,
                    )
                    .is_some() =>
                {
                    self.set_preference_notice(safe_runtime_rejection_notice(
                        desktop::runtime::DesktopRuntimeCommandKind::DecideToolAuthorization,
                        code,
                    ));
                }
                desktop::runtime::DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command:
                        command @ (desktop::runtime::DesktopRuntimeCommandKind::RetryRecovery
                        | desktop::runtime::DesktopRuntimeCommandKind::ResolveRecovery),
                    code,
                    ..
                } if self
                    .command_ledger
                    .complete_rejection(*command_id, *command)
                    .is_some() =>
                {
                    self.set_preference_notice(safe_runtime_rejection_notice(*command, code));
                    inspector_pane_dirty = true;
                }
                desktop::runtime::DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command:
                        command @ (desktop::runtime::DesktopRuntimeCommandKind::Resync
                        | desktop::runtime::DesktopRuntimeCommandKind::CreateSession
                        | desktop::runtime::DesktopRuntimeCommandKind::OpenSession
                        | desktop::runtime::DesktopRuntimeCommandKind::CloseSession
                        | desktop::runtime::DesktopRuntimeCommandKind::ListSessions),
                    code,
                    ..
                } if self
                    .command_ledger
                    .complete_rejection(*command_id, *command)
                    .is_some() =>
                {
                    self.set_preference_notice(safe_runtime_rejection_notice(*command, code));
                    sessions_pane_dirty = true;
                }
                desktop::runtime::DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command: desktop::runtime::DesktopRuntimeCommandKind::ReviewChangedFile,
                    code,
                    ..
                } => {
                    if let Some(DesktopCommandIntent::FileReview { request }) =
                        self.command_ledger.complete_rejection(
                            *command_id,
                            desktop::runtime::DesktopRuntimeCommandKind::ReviewChangedFile,
                        )
                    {
                        self.file_review = Arc::new(DesktopFileReviewState::Failed {
                            request,
                            code: code.clone(),
                        });
                        self.set_preference_notice(format!(
                            "File review unavailable ({}).",
                            truncate_label(code, 32)
                        ));
                        inspector_pane_dirty = true;
                    }
                }
                desktop::runtime::DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command: desktop::runtime::DesktopRuntimeCommandKind::OpenExternalEditor,
                    code,
                    ..
                } if self
                    .command_ledger
                    .complete_rejection(
                        *command_id,
                        desktop::runtime::DesktopRuntimeCommandKind::OpenExternalEditor,
                    )
                    .is_some() =>
                {
                    self.set_preference_notice(format!(
                        "External editor unavailable ({}).",
                        truncate_label(code, 32)
                    ));
                    inspector_pane_dirty = true;
                }
                desktop::runtime::DesktopRuntimeUpdate::PromptFinished {
                    command_id,
                    operation_id,
                    error,
                    ..
                } => {
                    sessions_pane_dirty = true;
                    self.command_ledger
                        .complete(*command_id, &DesktopCommandIntent::Prompt);
                    self.command_ledger.complete_where(|intent| {
                        matches!(
                            intent,
                            DesktopCommandIntent::Abort {
                                operation_id: pending_operation_id,
                            } if pending_operation_id == operation_id
                        )
                    });
                    self.command_ledger.complete_where(|intent| {
                        matches!(
                            intent,
                            DesktopCommandIntent::Authorization {
                                operation_id: pending_operation_id,
                                ..
                            } if pending_operation_id == operation_id
                        )
                    });
                    if let Some(error) = error {
                        self.set_preference_notice(format!(
                            "Prompt finished with runtime error ({}).",
                            truncate_label(&error.code, 28)
                        ));
                    }
                }
                desktop::runtime::DesktopRuntimeUpdate::RuntimeFailed { error } => {
                    sessions_pane_dirty = true;
                    self.command_ledger.clear();
                    self.reject_pending_composer(format!(
                        "desktop runtime failed ({})",
                        truncate_label(&error.code, 28)
                    ));
                }
                desktop::runtime::DesktopRuntimeUpdate::Stopped => {
                    sessions_pane_dirty = true;
                    self.command_ledger.clear();
                    self.reject_pending_composer("desktop runtime stopped".into());
                }
                _ => {}
            }
            let projection_was_none = self.projection.is_none();
            if projection_was_none {
                let hydrated = match &update {
                    desktop::runtime::DesktopRuntimeUpdate::SessionChanged { snapshot, .. }
                    | desktop::runtime::DesktopRuntimeUpdate::PromptAcceptedWithSession {
                        snapshot,
                        ..
                    }
                    | desktop::runtime::DesktopRuntimeUpdate::PromptFinished { snapshot, .. } => {
                        Some(snapshot)
                    }
                    desktop::runtime::DesktopRuntimeUpdate::PromptRejectedWithSession {
                        snapshot: Some(snapshot),
                        ..
                    } => Some(snapshot.as_ref()),
                    desktop::runtime::DesktopRuntimeUpdate::Resynced {
                        replacement:
                            desktop::runtime::DesktopRuntimeResyncSnapshot::Hydrated(snapshot),
                        ..
                    } => Some(snapshot),
                    _ => None,
                };
                if let Some(hydrated) = hydrated {
                    self.project = hydrated.project.clone();
                    match DesktopProjection::new(hydrated.clone()) {
                        Ok(projection) => self.projection = Some(projection),
                        Err(issue) => {
                            self.set_preference_notice(format!(
                                "Session response failed projection validation ({}).",
                                truncate_label(&issue.code, 28)
                            ));
                        }
                    }
                } else if let Some(metadata) = match &update {
                    desktop::runtime::DesktopRuntimeUpdate::Reloaded { metadata, .. }
                    | desktop::runtime::DesktopRuntimeUpdate::SelectionChanged {
                        metadata, ..
                    }
                    | desktop::runtime::DesktopRuntimeUpdate::PromptStarted { metadata, .. } => {
                        Some(metadata)
                    }
                    desktop::runtime::DesktopRuntimeUpdate::PromptRejectedWithSession {
                        metadata,
                        ..
                    } => Some(metadata),
                    _ => None,
                } {
                    self.project = metadata.project.clone();
                }
            }
            if self.projection.is_none() {
                let _ = projection_completions.reconcile(self, true, cx);
                if background_update {
                    let _ = self.swap_active_workspace(&foreground_session_id);
                    (
                        sessions_pane_dirty,
                        composer_pane_dirty,
                        inspector_pane_dirty,
                        inspector_telemetry_dirty,
                        toast_host_dirty,
                        conversation_header_dirty,
                        overlay_host_dirty,
                        root_dirty,
                    ) = dirty_before;
                }
                applied += 1;
                continue;
            }
            // The idle branch above already consumed this update, so the
            // remainder of the iteration owns a session projection. Every fact
            // it needs is read here so the borrow ends before the `&mut self`
            // reconciliation calls below.
            let Some(projection) = self.projection.as_mut() else {
                applied += 1;
                continue;
            };
            let had_active_operation = projection.snapshot().active_operation.is_some();
            let outcome = projection.apply(update);
            let project_after = projection.project().clone();
            let active_operation_after = projection.snapshot().active_operation.is_some();
            let event_sequence_after = projection.cursor().last_event_sequence;
            self.project = project_after;
            let dirty =
                ProjectionDirtyRouting::for_projection(outcome.is_replaced(), outcome.delta());
            if dirty.root {
                root_dirty = true;
            }
            if dirty.composer {
                composer_pane_dirty = true;
            }
            if had_active_operation != active_operation_after {
                sessions_pane_dirty = true;
            }
            let conversation_dirty = dirty.conversation;
            if dirty.inspector_immediate {
                inspector_pane_dirty = true;
            } else if dirty.inspector_telemetry {
                inspector_telemetry_dirty = true;
            }
            if dirty.conversation_header {
                conversation_header_dirty = true;
            }
            if dirty.overlay {
                overlay_host_dirty = true;
            }
            if dirty.sessions {
                sessions_pane_dirty = true;
                self.conversation_controller.apply_delta(true, 0);
            } else if conversation_dirty {
                self.conversation_controller
                    .apply_delta(false, event_sequence_after);
            }
            let file_changes_dirty = dirty.file_changes;
            if projection_completions.reconcile(self, outcome.is_replaced(), cx) {
                sessions_pane_dirty = true;
            }
            if outcome.is_replaced() {
                let workspace = &mut self.active_workspace;
                if !active_operation_after && workspace.composer.submitted().is_some() {
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
                        .reconcile_hydration(&source, event_sequence_after);
                }
            } else if conversation_dirty
                && let workspace = &mut self.active_workspace
                && let Some(projection) = workspace.projection.as_ref()
            {
                let source = ConversationSource::new(projection, workspace.composer.submitted());
                workspace
                    .conversation_controller
                    .reconcile_content(&source, event_sequence_after);
            }
            if let Some((command_id, authorization_id, _)) = self
                .command_ledger
                .authorization()
                .map(|(command_id, authorization_id, operation_id)| {
                    (
                        command_id,
                        authorization_id.to_owned(),
                        operation_id.to_owned(),
                    )
                })
                && !self.projection.as_ref().is_some_and(|projection| {
                    projection
                        .snapshot()
                        .pending_authorizations
                        .iter()
                        .any(|request| request.authorization_id == authorization_id)
                })
            {
                self.command_ledger
                    .complete_authorization(command_id, &authorization_id);
            }
            if outcome.is_replaced() || file_changes_dirty {
                self.reconcile_file_review();
            }
            self.request_resync_if_needed();
            if composer_pane_state_before != self.composer_pane_state() {
                composer_pane_dirty = true;
                inspector_pane_dirty = true;
                toast_host_dirty = true;
                conversation_header_dirty = true;
                overlay_host_dirty = true;
            }
            if background_update {
                let _ = self.swap_active_workspace(&foreground_session_id);
                (
                    sessions_pane_dirty,
                    composer_pane_dirty,
                    inspector_pane_dirty,
                    inspector_telemetry_dirty,
                    toast_host_dirty,
                    conversation_header_dirty,
                    overlay_host_dirty,
                    root_dirty,
                ) = dirty_before;
            }
            applied += 1;
        }
        if let Some(writer) = &self.preference_writer
            && let Some(error) = writer.take_error()
        {
            self.set_preference_notice(error);
            toast_host_dirty = true;
        }
        let conversation_needs_refresh = self.conversation_controller.needs_row_refresh();
        if conversation_needs_refresh && !self.refresh_conversation_rows_at_current_width(cx) {
            root_dirty = true;
        }
        #[cfg(test)]
        if root_dirty
            || sessions_pane_dirty
            || composer_pane_dirty
            || inspector_pane_dirty
            || inspector_telemetry_dirty
            || toast_host_dirty
            || conversation_header_dirty
            || overlay_host_dirty
        {
            self.runtime_ui_notification_count += 1;
        }
        if root_dirty {
            cx.notify();
        }
        if sessions_pane_dirty {
            self.notify_sessions_pane(cx);
            self.notify_home_pane(cx);
        }
        if composer_pane_dirty {
            self.notify_composer_pane(cx);
        }
        if inspector_pane_dirty {
            self.notify_inspector_pane(cx);
        } else if inspector_telemetry_dirty {
            self.schedule_inspector_telemetry_refresh(cx);
        }
        if toast_host_dirty {
            self.notify_toast_host(cx);
        }
        if conversation_header_dirty {
            self.notify_conversation_header(cx);
            self.notify_home_pane(cx);
        }
        if overlay_host_dirty {
            self.notify_overlay_host(cx);
        }
        !self
            .projection
            .as_ref()
            .is_some_and(|projection| projection.lifecycle() == DesktopProjectionLifecycle::Stopped)
    }

    fn notify_sessions_pane(&self, cx: &mut Context<Self>) {
        let view_model = self.sessions_pane_view_model();
        self.sessions_pane.update(cx, |pane, cx| {
            pane.set_view_model(view_model);
            cx.notify();
        });
        self.notify_toast_host(cx);
        self.notify_overlay_host(cx);
    }

    fn composer_pane_state(&self) -> (bool, bool, bool, bool) {
        (
            matches!(self.composer.admission(), ComposerAdmission::Pending { .. }),
            self.projection
                .as_ref()
                .is_some_and(|projection| projection.snapshot().active_operation.is_some()),
            self.composer.submitted().is_some(),
            self.composer.rejection().is_some(),
        )
    }

    fn active_composer_running_mode(&self) -> ComposerRunningMode {
        self.composer_running_mode
    }

    fn set_active_composer_running_mode(
        &mut self,
        mode: ComposerRunningMode,
        cx: &mut Context<Self>,
    ) {
        self.composer_running_mode = mode;
        self.notify_composer_pane(cx);
    }

    fn notify_composer_pane(&self, cx: &mut Context<Self>) {
        let view_model = self.composer_pane_view_model();
        self.composer_pane.update(cx, |pane, cx| {
            pane.set_view_model(view_model);
            cx.notify();
        });
    }

    fn notify_inspector_pane(&mut self, cx: &mut Context<Self>) {
        self.inspector_telemetry_last_refresh = Some(Instant::now());
        self.inspector_telemetry_refresh_deadline = None;
        self.push_inspector_pane_view_model(cx);
        self.notify_toast_host(cx);
    }

    fn push_inspector_pane_view_model(&self, cx: &mut Context<Self>) {
        let view_model = self.inspector_pane_view_model();
        self.inspector_pane.update(cx, |pane, cx| {
            pane.set_view_model(view_model);
            cx.notify();
        });
    }

    fn schedule_inspector_telemetry_refresh(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let delay = inspector_telemetry_refresh_delay(self.inspector_telemetry_last_refresh, now);
        if delay.is_zero() {
            self.inspector_telemetry_last_refresh = Some(now);
            self.inspector_telemetry_refresh_deadline = None;
            self.push_inspector_pane_view_model(cx);
            return;
        }

        let deadline = now + delay;
        if self
            .inspector_telemetry_refresh_deadline
            .is_some_and(|scheduled| scheduled <= deadline)
        {
            return;
        }
        self.inspector_telemetry_refresh_deadline = Some(deadline);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                if this.inspector_telemetry_refresh_deadline == Some(deadline) {
                    this.inspector_telemetry_refresh_deadline = None;
                    this.inspector_telemetry_last_refresh = Some(Instant::now());
                    this.push_inspector_pane_view_model(cx);
                }
            });
        })
        .detach();
    }

    fn notify_toast_host(&self, cx: &mut Context<Self>) {
        let notice = self.preference_notice.as_ref().map(|message| ToastNotice {
            session_id: Arc::from(self.active_workspace.session_id()),
            revision: self.active_workspace.preference_notice_revision,
            message: Arc::from(message.as_str()),
        });
        self.toast_host.update(cx, |host, cx| {
            host.observe_notice(notice, cx);
        });
    }

    fn notify_conversation_header(&self, cx: &mut Context<Self>) {
        let view_model = self.conversation_header_view_model();
        self.conversation_header
            .update(cx, |conversation_header, cx| {
                conversation_header.set_view_model(view_model);
                cx.notify();
            });
    }

    fn notify_overlay_host(&self, cx: &mut Context<Self>) {
        let view_model = self.overlay_view_model();
        self.overlay_host.update(cx, |host, cx| {
            host.set_view_model(view_model);
            cx.notify();
        });
    }

    fn schedule_preferences(&mut self) {
        if let Some(writer) = &self.preference_writer {
            writer.schedule(self.preferences.clone());
        }
    }

    fn remember_thinking_selection(&mut self, session_id: &str, selection: DesktopThinkingLevel) {
        if self
            .preferences
            .set_thinking_level_for_session(session_id, selection)
        {
            self.schedule_preferences();
        }
    }

    fn reconcile_file_review(&mut self) {
        let request = match self.file_review.as_ref() {
            DesktopFileReviewState::Empty => return,
            DesktopFileReviewState::Loading(request)
            | DesktopFileReviewState::Failed { request, .. } => request.clone(),
            DesktopFileReviewState::Ready(document) => document.request.clone(),
        };
        let remains_current = self.projection.as_ref().is_some_and(|projection| {
            projection.snapshot().context.changes.iter().any(|change| {
                change.operation_id == request.change.operation_id
                    && change.tool_call_id == request.change.tool_call_id
                    && change.path == request.change.path
                    && change.updated_sequence == request.revision.value()
            })
        });
        if !remains_current {
            self.command_ledger.complete_where(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::FileReview {
                        request: pending_request,
                    } if pending_request == &request
                )
            });
            self.file_review = Arc::new(DesktopFileReviewState::Empty);
        }
    }

    fn toggle_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size();
        let dockable = self
            .resolve_layout(
                u32::from(viewport.width),
                u32::from(viewport.height),
                PanelVisibility {
                    sessions: true,
                    context: self.preferences.context_panel_visible,
                },
            )
            .sessions
            .is_some();
        if !dockable {
            self.command_palette.close();
            self.narrow_sessions_open = !self.narrow_sessions_open;
            if self.narrow_sessions_open {
                self.activate_overlay(DesktopOverlayKind::NarrowSessions, window, cx);
                self.request_session_catalog(cx);
            } else {
                self.dismiss_overlay(window, cx);
            }
            self.notify_inspector_pane(cx);
            return;
        }
        self.preferences.sessions_panel_visible = !self.preferences.sessions_panel_visible;
        let layout = self.layout(window);
        self.focus.reconcile_layout(layout);
        if self.focus.active() == FocusTarget::Composer {
            self.focus_composer_input(window, cx);
        }
        self.schedule_preferences();
        self.notify_inspector_pane(cx);
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn visible_conversation_count(&self) -> usize {
        self.projection.as_ref().map_or(0, |projection| {
            projection.conversation().blocks().len()
                + usize::from(self.composer.submitted().is_some())
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
                    sessions: self.preferences.sessions_panel_visible,
                    context: true,
                },
            )
            .context
            .is_some();
        if !dockable {
            self.command_palette.close();
            self.narrow_sessions_open = false;
            self.narrow_context_open = !self.narrow_context_open;
            if self.narrow_context_open {
                self.activate_overlay(DesktopOverlayKind::NarrowContext, window, cx);
            } else {
                self.dismiss_overlay(window, cx);
            }
            return;
        }
        self.preferences.context_panel_visible = !self.preferences.context_panel_visible;
        let layout = self.layout(window);
        self.focus.reconcile_layout(layout);
        if self.focus.active() == FocusTarget::Composer {
            self.focus_composer_input(window, cx);
        }
        self.schedule_preferences();
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn reserve_command(&mut self, intent: DesktopCommandIntent) -> Option<u64> {
        commands::reserve_command(self, intent)
    }

    fn request_resync_if_needed(&mut self) {
        if !self.projection.as_ref().is_some_and(|projection| {
            projection.lifecycle() == DesktopProjectionLifecycle::NeedsResync
        }) || self.command_ledger.contains(&DesktopCommandIntent::Resync)
        {
            return;
        }
        let intent = DesktopCommandIntent::Resync;
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            return;
        };
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_resync(command_id)
                    .map_err(|error| error.to_string())
            },
        );
        if let Err(message) = admission {
            self.command_ledger.complete(command_id, &intent);
            self.set_preference_notice(message);
        }
    }

    fn submit_composer(&mut self, cx: &mut Context<Self>) {
        if !self.composer_attachments.is_empty()
            && !self
                .project
                .models
                .iter()
                .find(|model| model.id == self.project.selected_model_id)
                .is_some_and(|model| model.supports_images)
        {
            self.set_preference_notice(
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
        let has_attachments = !self.composer_attachments.is_empty();
        let payload = match self.composer.begin_submit_with_attachments(
            command_id,
            ComposerSubmissionKind::Prompt,
            has_attachments,
        ) {
            Ok(payload) => payload.to_owned(),
            Err(error) => {
                self.command_ledger.complete(command_id, &intent);
                self.set_preference_notice(error.to_string());
                self.notify_composer_pane(cx);
                self.notify_toast_host(cx);
                cx.notify();
                return;
            }
        };
        let thinking_level = self.thinking_selection.explicit();
        let session_id = self
            .projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.clone());
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                session_id
                    .as_deref()
                    .map_or_else(
                        || {
                            runtime.try_submit_prompt_with_attachments(
                                command_id,
                                &payload,
                                &self.composer_attachments,
                                thinking_level,
                            )
                        },
                        |session_id| {
                            runtime.try_submit_prompt_with_attachments_for_session(
                                command_id,
                                session_id,
                                &payload,
                                &self.composer_attachments,
                                thinking_level,
                            )
                        },
                    )
                    .map_err(|error| error.to_string())
            },
        );
        if let Err(message) = admission {
            self.command_ledger.complete(command_id, &intent);
            let _ = self.composer.rejected(command_id, message);
        }
        self.notify_composer_pane(cx);
        self.notify_inspector_pane(cx);
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn submit_primary_composer(&mut self, cx: &mut Context<Self>) {
        if self
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
            self.set_preference_notice(reason.to_string());
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach files or images".into()),
        });
        cx.spawn(async move |this, cx| match selection.await {
            Ok(Ok(Some(paths))) => {
                let _ = this.update(cx, |this, cx| {
                    this.add_composer_attachments(paths, cx);
                });
            }
            Ok(Ok(None)) => {}
            Ok(Err(_)) | Err(_) => {
                let _ = this.update(cx, |this, cx| {
                    this.set_preference_notice("The file picker could not be opened.".into());
                    this.notify_toast_host(cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn add_composer_attachments(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let mut candidate = self.composer_attachments.clone();
        for path in paths {
            if !candidate.contains(&path) {
                candidate.push(path);
            }
        }
        if let Err(error) = validate_prompt_attachments(&candidate) {
            self.set_preference_notice(error.to_string());
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        self.composer_attachments = candidate;
        self.notify_composer_pane(cx);
        cx.notify();
    }

    fn remove_composer_attachment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.composer_attachments.len() {
            self.composer_attachments.remove(index);
            self.notify_composer_pane(cx);
            cx.notify();
        }
    }

    fn composer_attachment_disabled_reason(&self) -> Option<&'static str> {
        let supports_images = self
            .project
            .models
            .iter()
            .find(|model| model.id == self.project.selected_model_id)
            .is_some_and(|model| model.supports_images);
        if !supports_images {
            return Some("Selected model does not support image attachments.");
        }
        let snapshot = self.projection.as_ref().map(DesktopProjection::snapshot);
        if snapshot.is_some_and(|snapshot| snapshot.active_operation.is_some()) {
            return Some("Attachments are unavailable while an operation is running.");
        }
        if matches!(self.composer.admission(), ComposerAdmission::Pending { .. })
            || self.composer.submitted().is_some()
        {
            return Some("Attachments are unavailable while a prompt is starting.");
        }
        None
    }

    fn reject_pending_composer(&mut self, message: String) {
        let command_id = match self.composer.admission() {
            ComposerAdmission::Pending { command_id, .. } => *command_id,
            ComposerAdmission::Idle => return,
        };
        if self.composer.rejected(command_id, message).is_ok() {
            self.composer_needs_sync = true;
        }
    }

    fn submit_active_control(&mut self, kind: ComposerSubmissionKind, cx: &mut Context<Self>) {
        if !self.composer_attachments.is_empty() {
            self.set_preference_notice(
                "Attachments cannot be added to a running operation; the draft was retained."
                    .into(),
            );
            self.notify_composer_pane(cx);
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        if kind == ComposerSubmissionKind::Prompt {
            self.set_preference_notice(
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
        let payload = match self.composer.begin_submit(command_id, kind) {
            Ok(payload) => payload.to_owned(),
            Err(error) => {
                self.command_ledger.complete(command_id, &intent);
                self.set_preference_notice(error.to_string());
                self.notify_composer_pane(cx);
                self.notify_toast_host(cx);
                cx.notify();
                return;
            }
        };
        let session_id = self
            .projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.clone());
        let admission = self.runtime.as_ref().map_or_else(
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
            self.command_ledger.complete(command_id, &intent);
            let _ = self.composer.rejected(command_id, message);
        }
        self.notify_composer_pane(cx);
        self.notify_inspector_pane(cx);
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn abort_active_operation(&mut self, cx: &mut Context<Self>) {
        if self
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Abort { .. }))
        {
            return;
        }
        let Some(operation_id) = self
            .projection
            .as_ref()
            .and_then(|projection| projection.snapshot().active_operation.clone())
        else {
            self.set_preference_notice("No active operation is available to abort.".into());
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let session_id = self
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
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_abort_for_session(command_id, &session_id)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.set_preference_notice("Abort requested…".into());
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.set_preference_notice(message);
            }
        }
        self.notify_toast_host(cx);
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn reload_local_resources(&mut self, cx: &mut Context<Self>) {
        let intent = DesktopCommandIntent::Reload;
        if self.command_ledger.contains(&intent) {
            return;
        }
        if self
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
            || self.composer.submitted().is_some()
        {
            self.set_preference_notice(
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
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_reload(command_id)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.set_preference_notice("Reloading local resources…".into());
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.set_preference_notice(message);
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
        if self
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Recovery { .. }))
        {
            return;
        }
        if self
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
            || self.composer.submitted().is_some()
        {
            self.set_preference_notice(
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
        let admission = self.runtime.as_ref().map_or_else(
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
                self.set_preference_notice(format!(
                    "Submitting recovery {}…",
                    recovery_action_label(action)
                ));
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.set_preference_notice(message);
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
        let selected_profile_id = self
            .projection
            .as_ref()
            .map(|projection| {
                projection
                    .snapshot()
                    .session
                    .default_agent_profile_id
                    .as_str()
            })
            .unwrap_or_else(|| self.project.default_agent_profile_id.as_str());
        let already_selected = match selection {
            DesktopRuntimeSelectionKind::Model => id == self.project.selected_model_id,
            DesktopRuntimeSelectionKind::SessionProfile => id == selected_profile_id,
        };
        if already_selected {
            return;
        }
        if self
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Selection(_)))
        {
            return;
        }
        if self
            .projection
            .as_ref()
            .is_some_and(|projection| projection.snapshot().active_operation.is_some())
            || self.composer.submitted().is_some()
        {
            self.set_preference_notice(
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
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                let result = match selection {
                    DesktopRuntimeSelectionKind::Model => runtime.try_select_model(command_id, &id),
                    DesktopRuntimeSelectionKind::SessionProfile => {
                        runtime.try_select_session_profile(command_id, &id)
                    }
                };
                result.map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.set_preference_notice("Applying selection…".into());
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.set_preference_notice(message);
            }
        }
        self.notify_toast_host(cx);
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn select_thinking_level(&mut self, selection: DesktopThinkingLevel, cx: &mut Context<Self>) {
        if self.thinking_selection == selection {
            return;
        }
        self.thinking_selection = selection;
        let session_id = self
            .projection
            .as_ref()
            .map(|projection| projection.snapshot().session.session_id.clone());
        if let Some(session_id) = session_id.as_deref() {
            self.remember_thinking_selection(session_id, selection);
        }
        let label = self
            .thinking_selection
            .label(self.project.settings.default_thinking_level.as_deref());
        self.set_preference_notice(format!(
            "{} will use thinking {label}.",
            if session_id.is_some() {
                "This session"
            } else {
                "The next session"
            }
        ));
        self.notify_toast_host(cx);
        self.notify_conversation_header(cx);
        self.notify_home_pane(cx);
        self.push_inspector_pane_view_model(cx);
        cx.notify();
    }

    fn cycle_thinking_selection(&mut self, cx: &mut Context<Self>) {
        self.select_thinking_level(self.thinking_selection.next(), cx);
    }

    fn decide_tool_authorization(
        &mut self,
        identity: ToolAuthorizationIdentity,
        decision: ToolAuthorizationDecision,
        cx: &mut Context<Self>,
    ) {
        if self
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Authorization { .. }))
        {
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
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_decide_tool_authorization(command_id, &identity, decision)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.set_preference_notice("Authorization decision pending…".into());
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.set_preference_notice(message);
            }
        }
        self.notify_toast_host(cx);
        self.notify_overlay_host(cx);
        cx.notify();
    }

    fn copy_selected_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(projection) = self.projection.as_ref() else {
            return;
        };
        let Some(text) = self
            .conversation_controller
            .copy_selected(projection.conversation())
        else {
            self.set_preference_notice(
                "Select a committed conversation block before copying.".into(),
            );
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.announce_conversation_copy("Selected message copied.", cx);
    }

    fn conversation_full_message_view(
        &self,
        block_id: &str,
    ) -> Option<ConversationFullMessageView> {
        let projection = self.projection.as_ref()?;
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
        self.conversation_controller
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
            self.set_preference_notice("Message is no longer available to copy.".into());
            self.notify_toast_host(cx);
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(message.text.to_string()));
        self.announce_conversation_copy("Message copied.", cx);
    }

    fn tool_command(&self, block_id: &str) -> Option<String> {
        let projection = self.projection.as_ref()?;
        let arguments = projection
            .conversation()
            .block(block_id)
            .filter(|block| block.kind == ConversationBlockKind::Tool)
            .map(|block| block.detail.as_str())
            .or_else(|| {
                projection
                    .tools()
                    .iter()
                    .find(|tool| tool_conversation_block_id(tool) == block_id)
                    .map(|tool| tool.arguments.as_str())
            })?;
        serde_json::from_str::<serde_json::Value>(arguments)
            .ok()?
            .get("command")?
            .as_str()
            .map(str::to_owned)
    }

    fn tool_output(&self, block_id: &str) -> Option<(Arc<str>, Arc<str>, bool)> {
        let projection = self.projection.as_ref()?;
        if let Some(block) = projection
            .conversation()
            .block(block_id)
            .filter(|block| block.kind == ConversationBlockKind::Tool)
        {
            return Some((
                Arc::from(block.title.as_str()),
                Arc::from(block.text.as_str()),
                block.truncated || block.text.len() > MAX_COPY_BYTES,
            ));
        }
        projection
            .tools()
            .iter()
            .find(|tool| tool_conversation_block_id(tool) == block_id)
            .map(|tool| {
                (
                    Arc::from(format!("Tool · {} · output", tool.name)),
                    Arc::from(tool.detail.as_str()),
                    tool.truncated || tool.detail.len() > MAX_COPY_BYTES,
                )
            })
    }

    fn copy_tool_command(&mut self, block_id: &str, cx: &mut Context<Self>) {
        let Some(command) = self.tool_command(block_id) else {
            self.set_preference_notice("This tool does not expose a structured command.".into());
            self.notify_toast_host(cx);
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(command));
        self.announce_conversation_copy("Tool command copied.", cx);
    }

    fn copy_tool_output(&mut self, block_id: &str, cx: &mut Context<Self>) {
        let Some((_, output, _)) = self.tool_output(block_id) else {
            self.set_preference_notice("Tool output is no longer available to copy.".into());
            self.notify_toast_host(cx);
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(conversation_copy_text(
            &output, "",
        )));
        self.announce_conversation_copy("Tool output copied.", cx);
    }

    fn announce_conversation_copy(&mut self, message: &str, cx: &mut Context<Self>) {
        self.conversation_announcement_sequence =
            self.conversation_announcement_sequence.wrapping_add(1);
        let sequence = self.conversation_announcement_sequence;
        self.conversation_announcement = Some((sequence, message.to_owned()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(CONVERSATION_ANNOUNCEMENT_DURATION)
                .await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .conversation_announcement
                    .as_ref()
                    .is_some_and(|(current, _)| *current == sequence)
                {
                    this.conversation_announcement = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn open_full_tool_output(
        &mut self,
        block_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((title, text, source_truncated)) = self.tool_output(block_id) else {
            self.set_preference_notice("Tool output is no longer available to open.".into());
            self.notify_toast_host(cx);
            return;
        };
        tracing::trace!(
            target: "desktop",
            event = "message_full_view_open",
            block_id,
            content = "tool_output",
            bytes = text.len(),
        );
        self.conversation_full_message = Some(ConversationFullMessageView {
            block_id: block_id.to_owned(),
            title,
            text,
            source_truncated,
        });
        self.activate_overlay(DesktopOverlayKind::FullMessage, window, cx);
    }

    fn open_full_conversation_message(
        &mut self,
        block_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self.conversation_full_message_view(block_id) else {
            self.set_preference_notice("Message is no longer available to open.".into());
            self.notify_toast_host(cx);
            return;
        };
        tracing::trace!(
            target: "desktop",
            event = "message_full_view_open",
            block_id = message.block_id,
            bytes = message.text.len(),
        );
        self.conversation_full_message = Some(message);
        self.activate_overlay(DesktopOverlayKind::FullMessage, window, cx);
    }

    fn close_full_conversation_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.conversation_full_message = None;
        self.dismiss_overlay(window, cx);
    }

    fn toggle_conversation_details(&mut self, block_id: &str, cx: &mut Context<Self>) {
        self.conversation_controller.toggle_details(block_id);
        if !self.refresh_conversation_rows_at_current_width(cx) {
            cx.notify();
        }
    }

    pub(super) fn select_adjacent_conversation(&mut self, reverse: bool, cx: &mut Context<Self>) {
        let workspace = &mut self.active_workspace;
        let Some(projection) = workspace.projection.as_ref() else {
            return;
        };
        let row_count = workspace.conversation_controller.row_count();
        if row_count == 0 {
            self.set_preference_notice("The conversation is empty.".into());
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
            .conversation_controller
            .selected_block_id()
            .map(str::to_owned)
        else {
            self.set_preference_notice("Select a conversation message before copying.".into());
            self.notify_toast_host(cx);
            return;
        };
        self.copy_conversation_row(&block_id, cx);
    }

    fn toggle_keyboard_selected_conversation_details(&mut self, cx: &mut Context<Self>) {
        let Some(block_id) = self
            .conversation_controller
            .selected_block_id()
            .map(str::to_owned)
        else {
            return;
        };
        let has_details = self
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
        if self
            .command_ledger
            .contains_where(|pending| matches!(pending, DesktopCommandIntent::FileReview { .. }))
        {
            self.set_preference_notice("Another file review is already pending.".into());
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let admission = self
            .runtime
            .as_ref()
            .ok_or_else(|| "desktop runtime is unavailable".to_owned())
            .and_then(|runtime| {
                runtime
                    .try_review_changed_file(command_id, &request)
                    .map_err(|error| error.to_string())
            });
        match admission {
            Ok(()) => {
                self.file_review = Arc::new(DesktopFileReviewState::Loading(request));
                self.set_preference_notice("Loading changed-file review…".into());
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.set_preference_notice(message);
            }
        }
        self.notify_inspector_pane(cx);
        cx.notify();
    }

    fn copy_review_path(&mut self, cx: &mut Context<Self>) {
        let DesktopFileReviewState::Ready(document) = self.file_review.as_ref() else {
            self.set_preference_notice(
                "Load a changed-file review before copying its path.".into(),
            );
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let export = document.path_clipboard_export();
        cx.write_to_clipboard(ClipboardItem::new_string(export.text));
        self.set_preference_notice(if export.truncated {
            "Bounded changed-file path copied (truncated).".into()
        } else {
            "Changed-file path copied.".into()
        });
        self.notify_toast_host(cx);
        cx.notify();
    }

    fn copy_file_review(&mut self, cx: &mut Context<Self>) {
        let DesktopFileReviewState::Ready(document) = self.file_review.as_ref() else {
            self.set_preference_notice("Load a changed-file review before copying it.".into());
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let export = document.clipboard_export();
        cx.write_to_clipboard(ClipboardItem::new_string(export.text));
        self.set_preference_notice(if export.truncated {
            "Bounded file review copied (truncated at the clipboard limit).".into()
        } else {
            "File review copied.".into()
        });
        self.notify_toast_host(cx);
        cx.notify();
    }

    fn open_review_in_external_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.preferences.external_editor.clone() else {
            self.set_preference_notice(
                "Configure desktop.external_editor with a program and literal argv first.".into(),
            );
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let DesktopFileReviewState::Ready(document) = self.file_review.as_ref() else {
            self.set_preference_notice("Load a changed-file review before opening it.".into());
            self.notify_toast_host(cx);
            cx.notify();
            return;
        };
        let Some(target) = document.external_editor_target.clone() else {
            self.set_preference_notice("This review has no external-editor target.".into());
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
        let admission = self
            .runtime
            .as_ref()
            .ok_or_else(|| "desktop runtime is unavailable".to_owned())
            .and_then(|runtime| {
                runtime
                    .try_open_external_editor(command_id, &target, &editor)
                    .map_err(|error| error.to_string())
            });
        match admission {
            Ok(()) => {
                self.set_preference_notice(format!(
                    "Validating {} before editor launch…",
                    truncate_label(&project_relative_path, 48)
                ));
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.set_preference_notice(message);
            }
        }
        self.notify_inspector_pane(cx);
        cx.notify();
    }

    fn activate_overlay(
        &mut self,
        overlay: DesktopOverlayKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let inspector_overlay_changed = self.active_overlay
            == Some(DesktopOverlayKind::NarrowContext)
            || overlay == DesktopOverlayKind::NarrowContext;
        if self.active_overlay.is_none() {
            self.focus.open_overlay();
        }
        self.active_overlay = Some(overlay);
        match overlay {
            DesktopOverlayKind::Authorization => self.authorization_focus.focus(window, cx),
            DesktopOverlayKind::CommandPalette => self.command_palette_focus.focus(window, cx),
            DesktopOverlayKind::NarrowSessions => self.narrow_sessions_focus.focus(window, cx),
            DesktopOverlayKind::NarrowContext => self.context_focus.focus(window, cx),
            DesktopOverlayKind::FullMessage => self.full_message_focus.focus(window, cx),
        }
        if inspector_overlay_changed {
            self.notify_inspector_pane(cx);
        }
        self.notify_conversation_header(cx);
        self.notify_overlay_host(cx);
        cx.notify();
    }

    fn dismiss_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let inspector_overlay_changed =
            self.active_overlay == Some(DesktopOverlayKind::NarrowContext);
        self.active_overlay = None;
        self.focus.close_overlay(self.layout(window));
        self.focus_active_target(window, cx);
        if inspector_overlay_changed {
            self.notify_inspector_pane(cx);
        }
        self.notify_conversation_header(cx);
        self.notify_overlay_host(cx);
        cx.notify();
    }

    fn reconcile_authorization_overlay(
        &mut self,
        authorization_present: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if authorization_present {
            self.command_palette.close();
            self.narrow_sessions_open = false;
            self.narrow_context_open = false;
            self.conversation_full_message = None;
            if self.active_overlay != Some(DesktopOverlayKind::Authorization) {
                self.activate_overlay(DesktopOverlayKind::Authorization, window, cx);
            }
        } else if self.active_overlay == Some(DesktopOverlayKind::Authorization) {
            self.dismiss_overlay(window, cx);
        }
    }

    fn focus_target(&mut self, target: FocusTarget, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_overlay.is_some() {
            return;
        }
        let layout = self.layout(window);
        if target == FocusTarget::Sessions && !layout.is_visible(target) {
            self.narrow_sessions_open = true;
            self.activate_overlay(DesktopOverlayKind::NarrowSessions, window, cx);
            self.request_session_catalog(cx);
            return;
        }
        if target == FocusTarget::Context && !layout.is_visible(target) {
            self.narrow_context_open = true;
            self.activate_overlay(DesktopOverlayKind::NarrowContext, window, cx);
            return;
        }
        if !self.focus.request(target, layout) {
            self.set_preference_notice(format!(
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
        if self.active_overlay.is_some() {
            self.focus_active_target(window, cx);
            return;
        }
        self.focus.cycle(self.layout(window), reverse);
        self.focus_active_target(window, cx);
        cx.notify();
    }

    fn root_action_blocked_by_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(overlay) = self.active_overlay else {
            return false;
        };
        self.set_preference_notice(
            match overlay {
                DesktopOverlayKind::Authorization => {
                    "Resolve the authorization dialog before using workspace shortcuts."
                }
                DesktopOverlayKind::CommandPalette => {
                    "Choose a typed command or close the command palette first."
                }
                DesktopOverlayKind::NarrowSessions => {
                    "Choose a session or close the sessions dialog first."
                }
                DesktopOverlayKind::NarrowContext => {
                    "Use the Inspector surface or close it before workspace shortcuts."
                }
                DesktopOverlayKind::FullMessage => {
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
        self.conversation_controller.follow_latest(visible_count);
        self.notify_conversation_pane(cx);
        self.notify_conversation_header(cx);
    }

    fn reconcile_conversation_scroll(&mut self, cx: &mut Context<Self>) {
        if self.conversation_controller.reconcile_scroll() {
            self.notify_conversation_pane(cx);
            self.notify_conversation_header(cx);
        }
    }

    fn review_next_file(&mut self, cx: &mut Context<Self>) {
        let Some(projection) = self.projection.as_ref() else {
            self.set_preference_notice("No session is open for file review.".into());
            cx.notify();
            return;
        };
        let changes = &projection.snapshot().context.changes;
        if changes.is_empty() {
            self.set_preference_notice("No changed file is available for review.".into());
            cx.notify();
            return;
        }
        let current = match self.file_review.as_ref() {
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
        self.request_file_review(CodingAgentFileReviewRequest::from(&changes[next]), cx);
    }

    fn submit_latest_recovery(&mut self, action: DesktopRecoveryAction, cx: &mut Context<Self>) {
        let identity = self
            .projection
            .as_ref()
            .and_then(|projection| {
                projection.recoveries().iter().find(|recovery| {
                    recovery.status == DesktopRecoveryStatus::Pending && recovery.authoritative
                })
            })
            .and_then(|recovery| recovery.identity.clone());
        let Some(identity) = identity else {
            self.set_preference_notice("No authoritative pending recovery is available.".into());
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
                self.focus_target(FocusTarget::Sessions, window, cx);
            }
            DesktopPaletteCommand::FocusConversation => {
                self.focus_target(FocusTarget::Conversation, window, cx);
            }
            DesktopPaletteCommand::FocusComposer => {
                self.focus_target(FocusTarget::Composer, window, cx);
            }
            DesktopPaletteCommand::FocusInspector => {
                self.focus_target(FocusTarget::Context, window, cx);
            }
            DesktopPaletteCommand::SubmitPrompt => {
                if self
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
                self.preferences.reduced_motion = !self.preferences.reduced_motion;
                self.schedule_preferences();
                let notice = if self.preferences.reduced_motion {
                    "Reduced motion enabled; desktop transitions remain static.".into()
                } else {
                    "Reduced motion disabled; idle presentation remains static.".into()
                };
                self.set_preference_notice(notice);
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
            .projection
            .as_ref()
            .is_some_and(|projection| !projection.snapshot().pending_authorizations.is_empty())
        {
            self.set_preference_notice("Resolve authorization before opening commands.".into());
            self.authorization_focus.focus(window, cx);
            self.notify_toast_host(cx);
            cx.notify();
            return;
        }
        self.narrow_sessions_open = false;
        self.narrow_context_open = false;
        self.command_palette.open();
        self.activate_overlay(DesktopOverlayKind::CommandPalette, window, cx);
    }

    fn on_open_file_surface(
        &mut self,
        _: &OpenFileSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.root_action_blocked_by_overlay(window, cx) {
            return;
        }
        self.review_next_file(cx);
        self.focus_target(FocusTarget::Context, window, cx);
    }

    fn on_new_session(&mut self, _: &NewSession, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_overlay != Some(DesktopOverlayKind::NarrowSessions)
            && self.root_action_blocked_by_overlay(window, cx)
        {
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
        if self.root_action_blocked_by_overlay(window, cx) {
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
        if self.root_action_blocked_by_overlay(window, cx) {
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
        if self.root_action_blocked_by_overlay(window, cx) {
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
        match self.active_overlay {
            Some(DesktopOverlayKind::Authorization) => {
                self.set_preference_notice(
                    "Authorization requires Deny, Allow once, or Allow for operation.".into(),
                );
                self.authorization_focus.focus(window, cx);
                cx.notify();
            }
            Some(DesktopOverlayKind::CommandPalette) => {
                self.command_palette.close();
                self.dismiss_overlay(window, cx);
            }
            Some(DesktopOverlayKind::NarrowSessions) => {
                self.narrow_sessions_open = false;
                self.dismiss_overlay(window, cx);
            }
            Some(DesktopOverlayKind::NarrowContext) => {
                self.narrow_context_open = false;
                self.dismiss_overlay(window, cx);
            }
            Some(DesktopOverlayKind::FullMessage) => {
                self.close_full_conversation_message(window, cx);
            }
            None if !matches!(self.file_review.as_ref(), DesktopFileReviewState::Empty) => {
                self.file_review = Arc::new(DesktopFileReviewState::Empty);
                self.set_preference_notice("Closed the changed-file review.".into());
                cx.notify();
            }
            None => self.focus_target(FocusTarget::Composer, window, cx),
        }
    }

    fn on_follow_latest_output(
        &mut self,
        _: &FollowLatestOutput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.root_action_blocked_by_overlay(window, cx) {
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
        if self.root_action_blocked_by_overlay(window, cx) {
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
        if self.root_action_blocked_by_overlay(window, cx) {
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
        if self.root_action_blocked_by_overlay(window, cx) {
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
        if !self.root_action_blocked_by_overlay(window, cx) {
            self.select_adjacent_conversation(true, cx);
        }
    }

    fn on_select_next_conversation(
        &mut self,
        _: &SelectNextConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.root_action_blocked_by_overlay(window, cx) {
            self.select_adjacent_conversation(false, cx);
        }
    }

    fn on_copy_selected_conversation(
        &mut self,
        _: &CopySelectedConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.root_action_blocked_by_overlay(window, cx) {
            self.copy_keyboard_selected_conversation(cx);
        }
    }

    fn on_toggle_selected_conversation_details(
        &mut self,
        _: &ToggleSelectedConversationDetails,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.root_action_blocked_by_overlay(window, cx) {
            self.toggle_keyboard_selected_conversation_details(cx);
        }
    }

    fn on_palette_previous(&mut self, _: &PalettePrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.command_palette.move_selection(true);
        self.notify_overlay_host(cx);
        cx.notify();
    }

    fn on_palette_next(&mut self, _: &PaletteNext, _: &mut Window, cx: &mut Context<Self>) {
        self.command_palette.move_selection(false);
        self.notify_overlay_host(cx);
        cx.notify();
    }

    fn on_palette_confirm(
        &mut self,
        _: &PaletteConfirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(command) = self.command_palette.selected_command() else {
            return;
        };
        self.command_palette.close();
        self.dismiss_overlay(window, cx);
        self.execute_palette_command(command, window, cx);
    }

    fn decide_current_authorization(
        &mut self,
        decision: ToolAuthorizationDecision,
        cx: &mut Context<Self>,
    ) {
        let Some(request) = self
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
        let workspace = &mut self.active_workspace;
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
        let workspace = &mut self.active_workspace;
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
        let Some(layout_width) = self.conversation_controller.active_width_bucket() else {
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
        let Some((delay, deadline)) = self.conversation_controller.arm_height_refresh(refresh)
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                if this.conversation_controller.fire_height_refresh(deadline) {
                    let _ = this.refresh_conversation_rows_at_current_width(cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn focus_composer_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.composer_pane.read(cx).focus_handle().clone();
        focus.focus(window, cx);
    }

    fn focus_active_target(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.focus.active() {
            FocusTarget::Sessions => self.sessions_focus.focus(window, cx),
            FocusTarget::Conversation => self.conversation_focus.focus(window, cx),
            FocusTarget::Composer => self.focus_composer_input(window, cx),
            FocusTarget::Context => self.context_focus.focus(window, cx),
            FocusTarget::Overlay => match self.active_overlay {
                Some(DesktopOverlayKind::Authorization) => {
                    self.authorization_focus.focus(window, cx)
                }
                Some(DesktopOverlayKind::CommandPalette) => {
                    self.command_palette_focus.focus(window, cx);
                }
                Some(DesktopOverlayKind::NarrowSessions) => {
                    self.narrow_sessions_focus.focus(window, cx);
                }
                Some(DesktopOverlayKind::NarrowContext) => self.context_focus.focus(window, cx),
                Some(DesktopOverlayKind::FullMessage) => self.full_message_focus.focus(window, cx),
                None => self.focus_composer_input(window, cx),
            },
        }
    }

    fn sessions_pane_view_model(&self) -> SessionsPaneViewModel {
        let snapshot = self.projection.as_ref().map(DesktopProjection::snapshot);
        let composer_running = snapshot.is_some_and(|snapshot| snapshot.active_operation.is_some());
        let mut runtime_states = self
            .workspaces
            .values()
            .chain(std::iter::once(&self.active_workspace))
            .filter(|workspace| workspace.projection.is_some())
            .map(|workspace| SessionRuntimeState {
                session_id: Arc::from(workspace.session_id()),
                status: workspace_semantic_status(workspace),
            })
            .collect::<Vec<_>>();
        runtime_states.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        SessionsPaneViewModel {
            panel_width: self.preferences.sessions_panel_width,
            catalog: Arc::from(self.session_controller.catalog().to_vec()),
            omitted_sessions: self.session_controller.omitted(),
            global_skills: Arc::clone(&self.global_skills),
            active_session_id: Arc::from(
                snapshot
                    .map(|snapshot| snapshot.session.session_id.as_str())
                    .unwrap_or_default(),
            ),
            runtime_states: Arc::from(runtime_states),
            composer_running,
            awaiting_prompt_start: self.composer.submitted().is_some() && !composer_running,
            session_pending: self.command_ledger.contains_where(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
                )
            }),
            session_catalog_pending: self
                .command_ledger
                .contains(&DesktopCommandIntent::ListSessions),
            active_status: self.semantic_status(),
            keyboard_focus_visible: self.keyboard_focus_visible(),
            context_is_overlay: self.narrow_sessions_open,
        }
    }

    fn composer_pane_view_model(&self) -> ComposerPaneViewModel {
        let snapshot = self.projection.as_ref().map(DesktopProjection::snapshot);
        let composer_running = snapshot.is_some_and(|snapshot| snapshot.active_operation.is_some());
        let attachment_disabled_reason = self.composer_attachment_disabled_reason();
        ComposerPaneViewModel {
            composer_pending: matches!(
                self.composer.admission(),
                ComposerAdmission::Pending { .. }
            ),
            composer_running,
            awaiting_prompt_start: self.composer.submitted().is_some() && !composer_running,
            authorization_pending: snapshot
                .is_some_and(|snapshot| !snapshot.pending_authorizations.is_empty()),
            running_mode: self.active_composer_running_mode(),
            attachments: self
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
                && self.composer_attachments.len() < MAX_PROMPT_ATTACHMENTS,
            attachment_disabled_reason: attachment_disabled_reason.map(Arc::from),
            rejection: self.composer.rejection().map(Arc::from),
            keyboard_focus_visible: self.keyboard_focus_visible(),
        }
    }

    fn home_pane_view_model(&self) -> HomePaneViewModel {
        let thinking = self
            .thinking_selection
            .label(self.project.settings.default_thinking_level.as_deref());
        let scratch_root = self.project.global_config_dir.join("scratch");
        let scratch_workspace = self
            .project
            .cwd
            .parent()
            .is_some_and(|parent| parent == scratch_root);
        HomePaneViewModel {
            model: Arc::from(truncate_label(&self.project.selected_model_id, 28)),
            thinking: Arc::from(truncate_label(&thinking, 18)),
            workspace: Arc::from(truncate_label(&self.project.cwd.display().to_string(), 42)),
            scratch_workspace,
            recent_sessions: Arc::from(self.session_controller.catalog().to_vec()),
            omitted_sessions: self.session_controller.omitted(),
            global_skills: Arc::clone(&self.global_skills),
            session_pending: self.command_ledger.contains_where(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
                )
            }),
            catalog_pending: self
                .command_ledger
                .contains(&DesktopCommandIntent::ListSessions),
        }
    }

    fn notify_home_pane(&self, cx: &mut Context<Self>) {
        let view_model = self.home_pane_view_model();
        self.home_pane.update(cx, |pane, cx| {
            pane.set_view_model(view_model);
            cx.notify();
        });
    }

    fn inspector_pane_view_model(&self) -> InspectorPaneViewModel {
        let Some(projection) = self.projection.as_ref() else {
            return InspectorPaneViewModel {
                panel_width: self.preferences.context_panel_width,
                context_is_overlay: self.narrow_context_open,
                keyboard_focus_visible: self.keyboard_focus_visible(),
                selected_section: self.inspector_section,
                composer_running: false,
                awaiting_prompt_start: self.composer.submitted().is_some(),
                recovery_pending: false,
                file_review_pending: false,
                external_editor_pending: false,
                external_editor_configured: self.preferences.external_editor.is_some(),
                changed_files: Vec::new(),
                change_count: 0,
                file_review: Arc::clone(&self.file_review),
                runtime_attention_count: self.project.diagnostics.len(),
                task_state: "ready".into(),
                active_operation: "—".into(),
                operation_count: 0,
                delegation_count: 0,
                selected_model: truncate_label(&self.project.selected_model_id, 28),
                profile: truncate_label(self.project.default_agent_profile_id.as_str(), 28),
                thinking: self
                    .thinking_selection
                    .label(self.project.settings.default_thinking_level.as_deref()),
                usage_input: "0".into(),
                usage_output: "0".into(),
                usage_cache_read: "0".into(),
                usage_cache_write: "0".into(),
                usage_tokens: "0".into(),
                usage_context: "—".into(),
                usage_cost: "—".into(),
                reduced_motion: self.preferences.reduced_motion,
                stream_id: "—".into(),
                sequence: "0".into(),
                generation: "0".into(),
                model_count: self.project.models.len(),
                profile_count: self.project.profiles.len(),
                skill_count: self.global_skills.len(),
                prompt_count: 0,
                context_count: 0,
                latest_recovery: None,
                latest_diagnostic: None,
                latest_config_diagnostic: self.project.diagnostics.last().map(|diagnostic| {
                    (
                        truncate_label(&diagnostic.code, 28),
                        truncate_label(&diagnostic.summary, 120),
                    )
                }),
                latest_issue: None,
                cwd: truncate_label(&self.project.cwd.display().to_string(), 54),
            };
        };
        let snapshot = projection.snapshot();
        let project = &self.project;
        let composer_running = snapshot.active_operation.is_some();
        let awaiting_prompt_start = self.composer.submitted().is_some() && !composer_running;
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
            panel_width: self.preferences.context_panel_width,
            context_is_overlay: self.narrow_context_open,
            keyboard_focus_visible: self.keyboard_focus_visible(),
            selected_section: self.inspector_section,
            composer_running,
            awaiting_prompt_start,
            recovery_pending: self
                .command_ledger
                .contains_where(|intent| matches!(intent, DesktopCommandIntent::Recovery { .. })),
            file_review_pending: self
                .command_ledger
                .contains_where(|intent| matches!(intent, DesktopCommandIntent::FileReview { .. })),
            external_editor_pending: self.command_ledger.contains_where(|intent| {
                matches!(intent, DesktopCommandIntent::ExternalEditor { .. })
            }),
            external_editor_configured: self.preferences.external_editor.is_some(),
            changed_files,
            change_count: snapshot.context.changes.len(),
            file_review: Arc::clone(&self.file_review),
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
            reduced_motion: self.preferences.reduced_motion,
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

    fn overlay_view_model(&self) -> OverlayViewModel {
        let authorization = self
            .projection
            .as_ref()
            .and_then(|projection| projection.snapshot().pending_authorizations.first())
            .cloned()
            .map(|request| {
                let decision_pending = self.command_ledger.authorization().is_some_and(
                    |(_, authorization_id, operation_id)| {
                        authorization_id == request.authorization_id
                            && operation_id == request.operation_id
                    },
                );
                OverlayAuthorizationView {
                    request,
                    decision_pending,
                }
            });
        OverlayViewModel {
            palette_open: self.command_palette.is_open(),
            palette_selected: self.command_palette.selected(),
            narrow_context_open: self.narrow_context_open,
            narrow_sessions_open: self.narrow_sessions_open,
            authorization,
            full_message: self.conversation_full_message.clone(),
        }
    }

    fn conversation_pane_view_model(&self) -> ConversationPaneViewModel {
        let diagnostic_recovery = self.projection.as_ref().and_then(|projection| {
            projection.recoveries().iter().find_map(|recovery| {
                (recovery.status == DesktopRecoveryStatus::Pending && recovery.authoritative)
                    .then(|| recovery.identity.clone())
                    .flatten()
            })
        });
        ConversationPaneViewModel {
            render: self.conversation_controller.render_reader(),
            scroll: self.conversation_controller.scroll.clone(),
            visible_count: self.visible_conversation_count(),
            event_count: self
                .projection
                .as_ref()
                .map(|projection| projection.recent_events().len())
                .unwrap_or_default(),
            message_count: self
                .projection
                .as_ref()
                .map(|projection| projection.messages().len())
                .unwrap_or_default(),
            tool_count: self
                .projection
                .as_ref()
                .map(|projection| projection.tools().len())
                .unwrap_or_default(),
            omitted_count: self
                .projection
                .as_ref()
                .map(|projection| projection.conversation().omitted_blocks())
                .unwrap_or_default(),
            follow_latest: self.conversation_controller.follow_latest_enabled(),
            unseen_updates: self.conversation_controller.unseen_updates(),
            selected_block_id: self
                .conversation_controller
                .selected_block_id()
                .map(str::to_owned),
            expanded_details: Rc::new(self.conversation_controller.expanded_details().clone()),
            full_view_block_id: self
                .conversation_full_message
                .as_ref()
                .map(|message| message.block_id.clone()),
            diagnostic_recovery,
        }
    }

    fn notify_conversation_pane(&self, cx: &mut Context<Self>) {
        let view_model = self.conversation_pane_view_model();
        self.conversation_pane.update(cx, |pane, cx| {
            pane.set_view_model(view_model);
            cx.notify();
        });
    }

    fn conversation_header_view_model(&self) -> ConversationHeaderViewModel {
        let snapshot = self.projection.as_ref().map(DesktopProjection::snapshot);
        let project = &self.project;
        let composer_running = snapshot.is_some_and(|snapshot| snapshot.active_operation.is_some());
        let awaiting_prompt_start = self.composer.submitted().is_some() && !composer_running;
        let reload_pending = self.command_ledger.contains(&DesktopCommandIntent::Reload);
        let selection_pending = self
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Selection(_)));
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
        let profile = project
            .profiles
            .iter()
            .find(|profile| profile.id.as_str() == current_profile_id)
            .map(|profile| profile.display_name.as_str())
            .unwrap_or(current_profile_id);
        let model_options = project
            .models
            .iter()
            .map(|model| {
                let selectable =
                    model.supports_text && (model.configured || model.id == current_model_id);
                let availability = if !model.supports_text {
                    " · no text input"
                } else if !selectable {
                    " · not configured"
                } else {
                    ""
                };
                ConversationHeaderSelectorOption {
                    id: Arc::from(model.id.as_str()),
                    label: Arc::from(format!(
                        "{} · {} · {}{}",
                        model.name, model.provider, model.id, availability
                    )),
                    selectable,
                }
            })
            .collect::<Vec<_>>();
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
            idle: self.projection.is_none(),
            status: self.semantic_status(),
            composer_running,
            abort_pending: self
                .command_ledger
                .contains_where(|intent| matches!(intent, DesktopCommandIntent::Abort { .. })),
            reload_pending,
            selector_disabled: composer_running
                || awaiting_prompt_start
                || reload_pending
                || selection_pending,
            model: Arc::from(truncate_label(model, 10)),
            profile: Arc::from(truncate_label(profile, 9)),
            thinking: Arc::from(truncate_label(
                &self
                    .thinking_selection
                    .label(project.settings.default_thinking_level.as_deref()),
                12,
            )),
            thinking_selection: self.thinking_selection,
            current_model_id: Arc::from(current_model_id),
            current_profile_id: Arc::from(current_profile_id),
            model_options: model_options.into(),
            profile_options: profile_options.into(),
            project_name: Arc::from(project_name),
            keyboard_focus_visible: self.keyboard_focus_visible(),
            panel_visibility: self.visibility(),
            narrow_sessions_open: self.narrow_sessions_open,
            narrow_context_open: self.narrow_context_open,
            sessions_panel_width: self.preferences.sessions_panel_width,
            context_panel_width: self.preferences.context_panel_width,
        }
    }

    fn semantic_status(&self) -> SemanticStatus {
        workspace_semantic_status(&self.active_workspace)
    }
}

impl Render for NativeShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _span = tracing::trace_span!("desktop.render").entered();
        if self.composer_needs_sync {
            let draft = self.composer.draft().to_owned();
            self.composer_pane.update(cx, |pane, cx| {
                pane.set_input_value(draft, window, cx);
            });
            self.composer_needs_sync = false;
        }
        let theme = SemanticTheme::GEEK_DARK;
        let layout = self.layout(window);
        self.focus.reconcile_layout(layout);
        if self.projection.is_some() {
            let requested_layout_width = conversation_width_bucket(layout.workspace.width);
            let (layout_width, width_refresh) = self
                .conversation_controller
                .width_for_render(requested_layout_width);
            if let Some((requested, deadline)) = width_refresh {
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(CONVERSATION_RESIZE_DEBOUNCE)
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if this
                            .conversation_controller
                            .commit_pending_width(requested, deadline)
                        {
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            self.refresh_conversation_rows_at_width(layout_width, cx);
        }
        let authorization_present = self
            .projection
            .as_ref()
            .is_some_and(|projection| !projection.snapshot().pending_authorizations.is_empty());
        self.reconcile_authorization_overlay(authorization_present, window, cx);
        let sessions_panel = layout.sessions.map(|bounds| {
            div()
                .relative()
                .flex_none()
                .w(px(bounds.width as f32))
                .h_full()
                .child(self.sessions_pane.clone())
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

        let context_panel = layout.context.map(|bounds| {
            div()
                .relative()
                .flex_none()
                .w(px(bounds.width as f32))
                .h_full()
                .child(self.inspector_pane.clone())
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

        let workspace = if self.projection.is_some() {
            div()
                .id("conversation-panel")
                .role(Role::Main)
                .aria_label("Conversation workspace")
                .aria_description(
                    "Conversation history and message composer. Use Up and Down to select messages.",
                )
                .debug_selector(|| "desktop-conversation-panel".into())
                .key_context(actions::CONVERSATION_KEY_CONTEXT)
                .track_focus(&self.conversation_focus)
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .bg(rgb(theme.canvas.value()))
                .child(self.conversation_header.clone())
                .child(self.conversation_pane.clone())
                .child(self.composer_pane.clone())
        } else {
            div()
                .id("home-workspace")
                .debug_selector(|| "desktop-home-workspace".into())
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .bg(rgb(theme.canvas.value()))
                .child(self.conversation_header.clone())
                .child(self.home_pane.clone())
                .child(
                    div()
                        .w_full()
                        .max_w(px(900.))
                        .mx_auto()
                        .px_6()
                        .pb_8()
                        .child(self.composer_pane.clone()),
                )
        };

        let overlay_host = self.overlay_host.clone();
        let toast_host = self.toast_host.clone();
        let conversation_announcement = self
            .conversation_announcement
            .as_ref()
            .map(|(_, message)| message.clone());

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
                    .children(sessions_panel)
                    .child(workspace)
                    .children(context_panel),
            )
            .child(overlay_host)
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
        FocusTarget::Sessions => "Sessions",
        FocusTarget::Conversation => "Conversation",
        FocusTarget::Composer => "Composer",
        FocusTarget::Context => "Inspector",
        FocusTarget::Overlay => "Overlay",
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

fn safe_runtime_rejection_notice(
    command: desktop::runtime::DesktopRuntimeCommandKind,
    code: &str,
) -> String {
    format!("{command:?} rejected ({})", truncate_label(code, 28))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::cell::RefCell;
    use std::collections::HashSet;

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
        CodingAgentResourceSummary, CodingAgentSettingsSummary,
    };
    use coding_agent::api::review::CodingAgentFileReview;
    use coding_agent::api::view::{
        CodingAgentCapabilities, CodingAgentSessionTranscriptItem, CodingAgentSessionView,
        CodingAgentTranscriptSnapshot, ProfileId, ProfileKind, ProfileSource,
    };
    use gpui::TestAppContext;
    use gpui_component::{Theme, ThemeMode, text::TextViewState};

    use desktop::shell::{COMPOSER_MAX_HEIGHT, CONVERSATION_ROW_VERTICAL_PADDING_PX};

    fn visual_test_snapshot() -> desktop::runtime::DesktopRuntimeHydratedSnapshot {
        visual_test_snapshot_for("desktop-visual-test")
    }

    fn visual_test_snapshot_for(
        session_id: &str,
    ) -> desktop::runtime::DesktopRuntimeHydratedSnapshot {
        let session_id = session_id.to_owned();
        let stream_id = format!("{session_id}-stream");
        desktop::runtime::DesktopRuntimeHydratedSnapshot {
            project: CodingAgentEmbeddingSnapshot {
                cwd: std::path::PathBuf::from("/desktop-visual-test"),
                global_config_dir: std::path::PathBuf::from("/desktop-visual-test/config"),
                selected_model_id: "test-model".into(),
                default_agent_profile_id: ProfileId::from("default"),
                models: vec![
                    CodingAgentModelChoice {
                        id: "test-model".into(),
                        name: "Test Model".into(),
                        provider: "fixture".into(),
                        reasoning: true,
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
                        project: projection.project().clone(),
                        projection: Some(projection),
                        global_skills: visual_global_skills(),
                        preferences,
                        preference_writer: None,
                        preference_notice: None,
                        initial_session_id: None,
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
                        project,
                        projection: None,
                        global_skills: visual_global_skills(),
                        preferences: DesktopPreferences::default(),
                        preference_writer: None,
                        preference_notice: None,
                        initial_session_id: None,
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
    fn idle_shell_constructs_all_bounded_view_models_without_session_facts(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, cx| {
            assert!(shell.projection.is_none());
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
            assert!(shell.overlay_view_model().authorization.is_none());
            assert!(shell.toast_host.read(cx).messages().len() <= 3);
            assert_eq!(shell.conversation_pane_view_model().visible_count, 0);
            let header = shell.conversation_header_view_model();
            assert_eq!(header.profile.as_ref(), "Default");
            assert_eq!(header.current_profile_id.as_ref(), "default");
            assert_eq!(shell.home_pane_view_model().global_skills.len(), 1);
            assert!(shell.home_pane_view_model().scratch_workspace);
            assert!(shell.home_pane_view_model().workspace.contains("scratch"));
        });

        for (width, height) in [(1_300., 900.), (900., 800.), (700., 800.)] {
            cx.simulate_resize(size(px(width), px(height)));
            cx.run_until_parked();
            let home = cx
                .debug_bounds("desktop-home-workspace")
                .expect("idle workspace is visible");
            assert_eq!(f32::from(home.size.width), width);
            assert!(cx.debug_bounds("desktop-conversation-panel").is_none());
            assert!(cx.debug_bounds("desktop-inspector-panel").is_none());
            assert!(cx.debug_bounds("desktop-composer-panel").is_some());
        }
    }

    #[gpui::test]
    fn idle_sessions_overlay_renders_new_conversation_skills_and_history(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, cx| {
            shell.session_controller.replace_catalog(
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
            .expect("idle Header exposes the Sessions overlay toggle");
        cx.simulate_click(toggle.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert!(
            cx.debug_bounds("desktop-new-conversation-section")
                .is_some()
        );
        assert!(cx.debug_bounds("desktop-global-skills-section").is_some());
        assert!(cx.debug_bounds("desktop-sessions-skill-0").is_some());
        assert!(cx.debug_bounds("desktop-session-history-section").is_some());
        assert!(cx.debug_bounds("desktop-session-row-0").is_some());
        assert!(cx.debug_bounds("sessions-search").is_some());
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.active_overlay),
            Some(DesktopOverlayKind::NarrowSessions)
        );
    }

    #[gpui::test]
    fn session_panel_renders_all_sections_and_new_conversation_returns_home(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();
        shell.update(cx, |shell, cx| {
            shell.session_controller.replace_catalog(
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
        assert!(cx.debug_bounds("desktop-global-skills-section").is_some());
        assert!(cx.debug_bounds("desktop-sessions-skill-0").is_some());
        assert!(cx.debug_bounds("desktop-session-history-section").is_some());
        assert!(cx.debug_bounds("desktop-session-row-0").is_some());

        let new_conversation = cx
            .debug_bounds("desktop-hit-new-conversation")
            .expect("the panel exposes the new-conversation row");
        cx.simulate_click(new_conversation.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert!(shell.read_with(cx, |shell, _| shell.projection.is_none()));
        assert!(shell.read_with(cx, |shell, _| {
            shell.workspaces.contains_key("desktop-visual-test")
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
            shell.set_preference_notice("Repeated notice".into());
            shell.notify_toast_host(cx);
            shell.set_preference_notice("Repeated notice".into());
            shell.notify_toast_host(cx);

            let repeated = shell.toast_host.read(cx).messages();
            assert_eq!(
                repeated
                    .iter()
                    .rev()
                    .take(2)
                    .map(AsRef::as_ref)
                    .collect::<Vec<_>>(),
                ["Repeated notice", "Repeated notice"]
            );

            shell.set_preference_notice("Third notice".into());
            shell.notify_toast_host(cx);
            shell.set_preference_notice("Fourth notice".into());
            shell.notify_toast_host(cx);

            let bounded = shell.toast_host.read(cx).messages();
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
            shell.composer.edit("keep this home draft");
            shell.projection = Some(visual_test_projection());
            assert_eq!(shell.composer.draft(), "keep this home draft");
            assert_ne!(
                shell.active_workspace.session_id(),
                HOME_COMPOSER_SESSION_KEY
            );
        });
    }

    #[gpui::test]
    fn first_session_change_rekeys_the_home_workspace_and_completes_its_command(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, cx| {
            shell.composer.edit("home draft");
            let intent = DesktopCommandIntent::OpenSession {
                session_id: "session-first".into(),
            };
            let command_id = shell
                .reserve_command(intent.clone())
                .expect("the first open command fits the home ledger");
            shell.runtime_ui_notification_count = 0;
            shell.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::SessionChanged {
                    command_id,
                    snapshot: visual_test_snapshot_for("session-first"),
                },
            );

            assert!(shell.poll_runtime(cx));
            assert_eq!(shell.active_workspace.session_id(), "session-first");
            assert_eq!(shell.composer.draft(), "home draft");
            assert!(!shell.command_ledger.matches(command_id, &intent));
            assert!(shell.runtime_ui_notification_count > 0);
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
                DesktopCommandLedger::default(),
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
            session_b
                .command_ledger
                .reserve_with_id(
                    9_001,
                    DesktopCommandIntent::FileReview {
                        request: review_request.clone(),
                    },
                )
                .expect("session B test command fits its bounded ledger");

            shell.composer.edit("draft a");
            shell.inspector_section = InspectorSection::Runtime;
            shell.workspaces.insert("session-b".into(), session_b);
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
            shell.runtime_ui_notification_count = 0;

            let mut finished_snapshot = visual_test_snapshot_for("session-b");
            finished_snapshot.session.cursor.last_event_sequence = 7;
            finished_snapshot.session.cursor.last_session_sequence = 7;
            finished_snapshot.session.context.changes.push(change);
            shell.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::PromptFinished {
                    command_id: 9_002,
                    operation_id: "operation-session-b".into(),
                    snapshot: finished_snapshot,
                    error: None,
                },
            );
            assert!(shell.poll_runtime(cx));

            assert_eq!(shell.active_workspace.session_id(), "session-a");
            assert_eq!(shell.runtime_ui_notification_count, 0);
            assert_eq!(shell.composer.draft(), "draft a");
            assert_eq!(shell.inspector_section, InspectorSection::Runtime);
            assert!(matches!(
                shell.file_review.as_ref(),
                DesktopFileReviewState::Empty
            ));
            assert!(shell.command_ledger.intent(9_001).is_none());

            let background = shell
                .workspaces
                .get("session-b")
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
                DesktopFileReviewState::Loading(request) if request == &review_request
            ));
            assert!(background.command_ledger.intent(9_001).is_some());

            assert!(shell.swap_active_workspace("session-b"));
            assert_eq!(shell.composer.draft(), "draft b");
            assert_eq!(shell.inspector_section, InspectorSection::Task);
            assert!(shell.swap_active_workspace("session-a"));
            assert_eq!(shell.composer.draft(), "draft a");
            assert_eq!(shell.inspector_section, InspectorSection::Runtime);

            for session_id in ["session-c", "session-d"] {
                let snapshot = visual_test_snapshot_for(session_id);
                let projection = DesktopProjection::new(snapshot.clone())
                    .expect("workspace-cap fixture is a valid projection");
                shell.workspaces.insert(
                    session_id.into(),
                    SessionWorkspace::new(
                        snapshot.project,
                        Some(projection),
                        None,
                        DesktopCommandLedger::default(),
                    ),
                );
            }
            assert_eq!(shell.workspaces.len() + 1, MAX_SESSION_WORKSPACES);
            let session_e = visual_test_snapshot_for("session-e");
            assert!(!shell.install_hydrated_workspace(&session_e, false));
            assert!(!shell.workspaces.contains_key("session-e"));
            let workspace_ids_before = shell.workspaces.keys().cloned().collect::<HashSet<_>>();
            shell.open_session("session-e".into(), cx);
            assert_eq!(
                shell.workspaces.keys().cloned().collect::<HashSet<_>>(),
                workspace_ids_before
            );
            assert!(
                shell
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
            shell.workspaces.insert(
                "close-session-b".into(),
                SessionWorkspace::new(
                    snapshot_b.project,
                    Some(projection_b),
                    None,
                    DesktopCommandLedger::default(),
                ),
            );
            let intent = DesktopCommandIntent::CloseSession {
                session_id: "close-session-b".into(),
            };
            shell.close_session("close-session-b", cx);
            let command_id = shell
                .workspaces
                .get("close-session-b")
                .and_then(|workspace| workspace.command_ledger.command_id_for(&intent))
                .expect("close command is owned by the target workspace");
            shell.runtime_updates.push_back(
                desktop::runtime::DesktopRuntimeUpdate::SessionClosed {
                    command_id,
                    session_id: "close-session-b".into(),
                },
            );
            assert!(shell.poll_runtime(cx));
            assert_eq!(shell.active_workspace.session_id(), "close-session-a");
            assert!(!shell.workspaces.contains_key("close-session-b"));
        });
        assert!(
            runtime_harness
                .drain_command_kinds()
                .contains(&desktop::runtime::DesktopRuntimeCommandKind::CloseSession)
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
        let (_, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            visual_test_projection(),
        );

        cx.simulate_resize(size(px(1_300.), px(900.)));
        cx.run_until_parked();
        let wide_before_focus = desktop_region_bounds(cx);
        assert!(wide_before_focus.iter().all(Option::is_some));
        cx.dispatch_action(FocusNextRegion);
        cx.run_until_parked();
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
        assert!(medium_layout.sessions.is_some());
        assert!(medium_layout.context.is_none());
        let narrow_layout = ShellLayout::resolve(700, 900, PanelVisibility::default());
        assert!(narrow_layout.sessions.is_none());
        assert!(narrow_layout.context.is_none());
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
                shell.conversation_header.update(cx, |header, cx| {
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
                    shell.conversation_header.update(cx, |header, cx| {
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
    fn inspector_tabs_stay_on_one_line_in_docked_and_overlay_layouts(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            visual_test_projection(),
        );

        for (width, open_overlay) in [(1_300., false), (700., true)] {
            cx.simulate_resize(size(px(width), px(900.)));
            cx.run_until_parked();
            if open_overlay {
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
                shell.inspector_section = InspectorSection::Runtime;
                shell.notify_inspector_pane(cx);
            });
            cx.run_until_parked();
            let runtime = cx
                .debug_bounds("desktop-inspector-tab-runtime")
                .expect("selected Runtime tab remains mounted");
            assert!(runtime.left() >= tabs.left() && runtime.right() <= tabs.right());
            assert!(shell.read_with(cx, |shell, cx| {
                shell.inspector_pane.read(cx).tab_scroll_offset().x <= px(0.)
            }));

            cx.update(|window, app| {
                shell.update(app, |shell, app| {
                    shell.inspector_pane.update(app, |pane, app| {
                        pane.focus_tab(InspectorSection::Runtime, window, app)
                    });
                });
            });
            let left = gpui::Keystroke::parse("left").expect("left is a valid keystroke");
            assert!(cx.update(|window, app| window.dispatch_keystroke(left, app)));
            cx.run_until_parked();
            assert_eq!(
                shell.read_with(cx, |shell, _| shell.inspector_section),
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
        let medium_scroll =
            shell.read_with(cx, |shell, _| shell.conversation_controller.scroll.offset());

        cx.dispatch_action(ToggleInspectorPanel);
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.active_overlay),
            Some(DesktopOverlayKind::NarrowContext)
        );
        assert!(cx.debug_bounds("desktop-inspector-panel").is_some());
        assert_minimum_hit_target(cx, "desktop-hit-close-inspector");
        assert_eq!(
            cx.debug_bounds("desktop-conversation-panel"),
            Some(medium_conversation)
        );
        assert_eq!(cx.debug_bounds("conversation-last-row"), Some(medium_row));
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.conversation_controller.scroll.offset()),
            medium_scroll
        );

        cx.dispatch_action(EscapeHierarchy);
        cx.run_until_parked();
        assert_eq!(shell.read_with(cx, |shell, _| shell.active_overlay), None);
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.focus.active()),
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
        let narrow_scroll =
            shell.read_with(cx, |shell, _| shell.conversation_controller.scroll.offset());
        let sessions_toggle = cx
            .debug_bounds("desktop-hit-toggle-sessions")
            .expect("narrow layout retains the Sessions drawer toggle");
        cx.simulate_click(sessions_toggle.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert_eq!(
            shell.read_with(cx, |shell, _| shell.active_overlay),
            Some(DesktopOverlayKind::NarrowSessions)
        );
        assert_minimum_hit_target(cx, "desktop-hit-sessions-overflow");
        assert_minimum_hit_target(cx, "desktop-hit-close-narrow-sessions");
        assert!(
            cx.debug_bounds("sessions-search").is_some(),
            "narrow overlay reuses the searchable SessionsPane"
        );
        assert_eq!(
            cx.debug_bounds("desktop-conversation-panel"),
            Some(narrow_conversation)
        );
        assert_eq!(cx.debug_bounds("conversation-last-row"), Some(narrow_row));
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.conversation_controller.scroll.offset()),
            narrow_scroll
        );

        cx.dispatch_action(EscapeHierarchy);
        cx.run_until_parked();
        assert_eq!(shell.read_with(cx, |shell, _| shell.active_overlay), None);
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.focus.active()),
            FocusTarget::Composer
        );
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
                    shell.conversation_controller.row_count(),
                    shell.conversation_controller.render_heights_for_tests(),
                    shell.conversation_controller.scroll.offset(),
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
    }

    #[gpui::test]
    fn expanded_tool_actions_copy_structured_sources_and_open_output(cx: &mut TestAppContext) {
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
                    .conversation_controller
                    .selected_block_id()
                    .map(str::to_owned)
            }),
            Some(block_id.clone())
        );

        assert_minimum_hit_target(cx, "desktop-toggle-tool-details");
        assert_minimum_hit_target(cx, "desktop-copy-tool-command");
        let copy_command = cx
            .debug_bounds("desktop-copy-tool-command")
            .expect("structured shell command exposes a copy action");
        cx.simulate_click(copy_command.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(command.into())
        );

        assert_minimum_hit_target(cx, "desktop-copy-tool-output");
        let copy_output = cx
            .debug_bounds("desktop-copy-tool-output")
            .expect("tool output exposes a copy action");
        cx.simulate_click(copy_output.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(output.into())
        );

        assert_minimum_hit_target(cx, "desktop-open-tool-output");
        let open_output = cx
            .debug_bounds("desktop-open-tool-output")
            .expect("tool output exposes a full-output action");
        cx.simulate_click(open_output.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.active_overlay),
            Some(DesktopOverlayKind::FullMessage)
        );
        assert!(shell.read_with(cx, |shell, _| {
            shell
                .conversation_full_message
                .as_ref()
                .is_some_and(|message| message.text.as_ref() == output)
        }));
    }

    #[gpui::test]
    fn assistant_reasoning_expands_without_losing_the_answer_tail(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (_, cx) = add_visual_shell(
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

        assert_minimum_hit_target(cx, "desktop-toggle-reasoning-details");
        let reasoning_header = cx
            .debug_bounds("desktop-reasoning-toggle-header")
            .expect("the complete reasoning header is a disclosure action");
        cx.simulate_click(reasoning_header.center(), gpui::Modifiers::default());
        settle_visual_measurements(cx);

        assert_last_row_matches_card_and_tail(cx, "expanded Reasoning");
        let expanded_height = f32::from(
            cx.debug_bounds("conversation-last-card")
                .expect("expanded reasoning card is laid out")
                .size
                .height,
        );
        assert!(
            expanded_height > collapsed_height + 100.,
            "expanded reasoning must contribute its real content height: collapsed={collapsed_height}, expanded={expanded_height}"
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
            .debug_bounds("desktop-conversation-row-header")
            .expect("conversation row header exposes its typed selection path");
        cx.simulate_click(row_header.center(), gpui::Modifiers::default());
        settle_visual_measurements(cx);
        assert_eq!(
            shell.read_with(cx, |shell, _| {
                shell
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
            shell.read_with(cx, |shell, _| shell.conversation_controller.scroll.offset())
        );
        cx.simulate_click(open.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.active_overlay),
            Some(DesktopOverlayKind::FullMessage)
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
        assert_eq!(shell.read_with(cx, |shell, _| shell.active_overlay), None);
        assert!(shell.read_with(cx, |shell, _| shell.conversation_full_message.is_none()));
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
                .projection
                .as_ref()
                .expect("the visual shell owns a session projection")
                .snapshot()
                .session
                .session_id
                .clone();
            shell.session_controller.replace_catalog(
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
        assert_minimum_hit_target(cx, "desktop-session-row-1");
    }

    #[gpui::test]
    fn sessions_show_names_search_name_and_id_and_offer_manual_rename(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();
        shell.update(cx, |shell, cx| {
            shell.session_controller.replace_catalog(
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
                shell.sessions_pane.update(app, |pane, app| {
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
                    .sessions_pane
                    .update(app, |pane, app| pane.set_search_value("", window, app));
            });
        });
        cx.run_until_parked();
        let rename = cx
            .debug_bounds("desktop-hit-rename-session-1")
            .expect("unnamed session exposes rename fallback");
        cx.simulate_click(rename.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        choose_popup_item(cx, 0);
        assert!(cx.debug_bounds("desktop-session-rename-1").is_some());
        cx.update(|window, app| {
            shell.update(app, |shell, app| {
                shell.sessions_pane.update(app, |pane, app| {
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
    fn idle_model_selector_lists_the_complete_catalog_and_submits_the_exact_id(
        cx: &mut TestAppContext,
    ) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_idle_visual_shell_with_runtime(cx, runtime);
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        let view_model = shell.read_with(cx, |shell, _| shell.conversation_header_view_model());
        assert!(view_model.idle);
        assert!(shell.read_with(cx, |shell, _| shell.projection.is_none()));
        assert_eq!(
            view_model
                .model_options
                .iter()
                .map(|option| option.id.as_ref())
                .collect::<Vec<_>>(),
            [
                "test-model",
                "adjacent-model",
                "exact-target-model",
                "image-only-model"
            ]
        );
        assert!(view_model.model_options[0].selectable);
        assert!(view_model.model_options[1].selectable);
        assert!(view_model.model_options[2].selectable);
        assert!(!view_model.model_options[3].selectable);

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
                None,
                "exact-target-model".into(),
            )]
        );
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
                None,
                "exact-reviewer".into(),
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
            shell.composer_pane.update(cx, |pane, cx| {
                pane.latency_probe().mark_changed_at(changed_at);
                cx.notify();
            });
        });
        cx.run_until_parked();

        assert!(shell.read_with(cx, |shell, cx| {
            let pane = shell.composer_pane.read(cx);
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
            shell.composer.edit("one compact line");
            shell.composer_needs_sync = true;
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
            shell.composer.edit(
                (1..=20)
                    .map(|line| format!("composer line {line} 中文 🙂"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            shell.composer_needs_sync = true;
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
            shell.composer.edit(
                (1..=40)
                    .map(|line| format!("saturation line {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            shell.composer_needs_sync = true;
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
            shell.composer.edit("retry this exact draft");
            shell
                .composer
                .begin_submit(91, ComposerSubmissionKind::Prompt)
                .expect("test draft starts a pending submission");
            shell
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
                    shell.composer_pane.read(cx).latency_probe().last_observed()
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
        let notice_before_copy = shell.read_with(cx, |shell, _| shell.preference_notice.clone());

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
            shell.read_with(cx, |shell, _| shell.preference_notice.clone()),
            notice_before_copy,
            "Copy feedback must not replace a persistent runtime or preference notice"
        );
        cx.executor()
            .advance_clock(CONVERSATION_ANNOUNCEMENT_DURATION + Duration::from_millis(1));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("desktop-conversation-copy-announcement")
                .is_none(),
            "Copy announcement expires instead of becoming persistent chrome"
        );
    }

    #[gpui::test]
    fn native_shell_command_palette_smoke_uses_overlay_focus_and_restores_it(
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
            shell.read_with(cx, |shell, _| shell.active_overlay),
            Some(DesktopOverlayKind::CommandPalette)
        );
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.focus.active()),
            FocusTarget::Overlay
        );

        cx.dispatch_action(EscapeHierarchy);
        cx.run_until_parked();
        assert_eq!(shell.read_with(cx, |shell, _| shell.active_overlay), None);
        assert_ne!(
            shell.read_with(cx, |shell, _| shell.focus.active()),
            FocusTarget::Overlay
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
            shell.read_with(cx, |shell, _| shell.active_overlay),
            Some(DesktopOverlayKind::Authorization)
        );
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.focus.active()),
            FocusTarget::Overlay
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
            shell.command_ledger.contains_where(|intent| {
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
        let (shell, cx) = add_visual_shell(cx, runtime, projection);
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();
        let inspector = shell.read_with(cx, |shell, _| shell.inspector_pane.clone());

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
            shell.command_ledger.contains_where(|intent| {
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
                shell.file_review.as_ref(),
                DesktopFileReviewState::Loading(request) if request == &review_request
            )
        }));

        shell.update(cx, |shell, cx| {
            shell.file_review = Arc::new(DesktopFileReviewState::Ready(
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
            shell.read_with(cx, |shell, _| shell.preference_notice.clone()),
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
            shell.command_ledger.contains_where(|intent| {
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
    fn thinking_selection_cycles_and_maps_only_explicit_overrides() {
        let mut selection = DesktopThinkingLevel::Default;
        assert_eq!(selection.explicit(), None);
        assert_eq!(selection.label(Some("xhigh")), "default:xhigh");

        selection = selection.next();
        assert_eq!(selection.explicit(), Some(CodingAgentThinkingLevel::Off));
        selection = selection.next();
        assert_eq!(
            selection.explicit(),
            Some(CodingAgentThinkingLevel::Minimal)
        );
        selection = selection.next();
        assert_eq!(selection.explicit(), Some(CodingAgentThinkingLevel::Low));
        selection = selection.next();
        assert_eq!(selection.explicit(), Some(CodingAgentThinkingLevel::Medium));
        selection = selection.next();
        assert_eq!(selection.explicit(), Some(CodingAgentThinkingLevel::High));
        selection = selection.next();
        assert_eq!(selection.explicit(), Some(CodingAgentThinkingLevel::XHigh));
        assert_eq!(selection.next(), DesktopThinkingLevel::Default);
    }

    #[gpui::test]
    fn header_thinking_selector_submits_the_session_level_with_the_prompt(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (runtime, mut runtime_harness) = DesktopRuntimeBridge::instrumented_for_test();
        let (shell, cx) = add_visual_shell(cx, runtime, visual_test_projection());
        cx.run_until_parked();
        runtime_harness.drain_command_kinds();

        assert_eq!(
            shell.read_with(cx, |shell, _| shell.thinking_selection),
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
            shell.read_with(cx, |shell, _| shell.thinking_selection),
            DesktopThinkingLevel::High
        );
        shell.update(cx, |shell, cx| {
            assert_eq!(
                shell
                    .preferences
                    .thinking_level_for_session("desktop-visual-test"),
                DesktopThinkingLevel::High
            );
            shell.composer.edit("use the session thinking level");
            shell.submit_composer(cx);
        });

        assert_eq!(
            runtime_harness.drain_prompts(),
            [(
                Some("desktop-visual-test".into()),
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
            assert_eq!(shell.composer_attachments.len(), 2);
            shell.composer.edit("inspect the selected files");
            shell.submit_composer(cx);
        });
        assert_eq!(
            runtime_harness.drain_prompt_attachments(),
            [(
                Some("desktop-visual-test".into()),
                "inspect the selected files".into(),
                vec![
                    PathBuf::from("/desktop-visual-test/screenshot.png"),
                    PathBuf::from("/desktop-visual-test/notes.txt"),
                ],
            )]
        );
    }

    #[gpui::test]
    fn composer_rejects_attachment_overflow_without_changing_the_draft(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);
        shell.update(cx, |shell, cx| {
            shell.composer.edit("retain this exact draft");
            shell.add_composer_attachments(
                (0..=MAX_PROMPT_ATTACHMENTS)
                    .map(|index| PathBuf::from(format!("/tmp/attachment-{index}.png")))
                    .collect(),
                cx,
            );
            assert!(shell.composer_attachments.is_empty());
            assert_eq!(shell.composer.draft(), "retain this exact draft");
            assert!(
                shell
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
            shell.project.selected_model_id = "adjacent-model".into();
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
            assert_eq!(shell.thinking_selection, DesktopThinkingLevel::High);
            let snapshot_b = visual_test_snapshot_for("thinking-session-b");
            let projection_b = DesktopProjection::new(snapshot_b.clone())
                .expect("thinking session B fixture is a valid projection");
            let thinking_b = shell
                .preferences
                .thinking_level_for_session("thinking-session-b");
            shell.workspaces.insert(
                "thinking-session-b".into(),
                SessionWorkspace::new_with_thinking(
                    snapshot_b.project,
                    Some(projection_b),
                    None,
                    DesktopCommandLedger::default(),
                    thinking_b,
                ),
            );

            assert!(shell.swap_active_workspace("thinking-session-b"));
            assert_eq!(shell.thinking_selection, DesktopThinkingLevel::Low);
            shell.select_thinking_level(DesktopThinkingLevel::XHigh, cx);
            assert_eq!(
                shell
                    .preferences
                    .thinking_level_for_session("thinking-session-b"),
                DesktopThinkingLevel::XHigh
            );

            assert!(shell.swap_active_workspace("thinking-session-a"));
            assert_eq!(shell.thinking_selection, DesktopThinkingLevel::High);
            assert!(shell.swap_active_workspace("thinking-session-b"));
            assert_eq!(shell.thinking_selection, DesktopThinkingLevel::XHigh);
        });
    }

    #[gpui::test]
    fn hydration_restores_existing_thinking_but_new_sessions_inherit_home(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_idle_visual_shell(cx);

        shell.update(cx, |shell, _| {
            assert!(shell.preferences.set_thinking_level_for_session(
                "existing-thinking-session",
                DesktopThinkingLevel::Low,
            ));
            shell.thinking_selection = DesktopThinkingLevel::XHigh;
            let existing = visual_test_snapshot_for("existing-thinking-session");

            assert!(shell.install_hydrated_workspace(&existing, false));
            assert_eq!(shell.thinking_selection, DesktopThinkingLevel::Low);
            assert_eq!(
                shell
                    .preferences
                    .thinking_level_for_session("existing-thinking-session"),
                DesktopThinkingLevel::Low
            );

            let home_project = shell.project.clone();
            shell.active_workspace =
                SessionWorkspace::new(home_project, None, None, DesktopCommandLedger::default());
            shell.workspaces.clear();
            shell.thinking_selection = DesktopThinkingLevel::Medium;
            let created = visual_test_snapshot_for("created-thinking-session");

            assert!(shell.install_hydrated_workspace(&created, true));
            assert_eq!(shell.thinking_selection, DesktopThinkingLevel::Medium);
            assert_eq!(
                shell
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
        let mut session_a = SessionWorkspace::new(
            project.clone(),
            Some(projection),
            None,
            DesktopCommandLedger::default(),
        );
        let mut session_b =
            SessionWorkspace::new(project, None, None, DesktopCommandLedger::default());
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
        let mut session_a = SessionWorkspace::new(
            project.clone(),
            Some(projection),
            None,
            DesktopCommandLedger::default(),
        );
        let mut session_b =
            SessionWorkspace::new(project, None, None, DesktopCommandLedger::default());
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

        let pane = include_str!("native_shell/conversation_pane.rs");
        assert!(pane.contains("Reasoning · collapsed"));
        assert!(pane.contains("\"OUTPUT\""));
        assert!(pane.contains("\"ARGUMENTS\""));
        assert!(pane.contains("DesktopIconButton::new("));
        assert!(pane.contains("DesktopIcon::ChevronDown"));
        assert!(pane.contains("DesktopIcon::ChevronUp"));
        assert!(pane.contains("DesktopIcon::Copy"));
        assert!(pane.contains("DesktopIcon::Expand"));
        assert!(pane.contains("desktop-tool-toggle-header"));
        assert!(pane.contains("desktop-reasoning-toggle-header"));
        assert!(pane.contains("conversation_hover_tool"));
        assert!(pane.contains(".opacity(0.)"));
        assert!(pane.contains(".focus(|style| style.opacity(1.))"));
        assert!(!pane.contains(".invisible()"));
        assert!(pane.contains("ConversationPaneEvent::ToggleDetails"));
        assert!(pane.contains("group_hover(hover_group"));
        assert!(!pane.contains(".label(\"Show\")"));
        assert!(!pane.contains(".label(\"Hide\")"));
        assert!(!pane.contains(".label(\"Copy command\")"));
        assert!(!pane.contains(".label(\"Copy output\")"));
        assert!(!pane.contains(".label(\"Open full output\")"));
        assert!(!pane.contains(".label(\"Open full message\")"));
        assert!(pane.contains(".absolute()"));
        assert!(pane.contains(
            ".id((\n                                    ElementId::from(\"conversation-block\")"
        ));
        assert!(!pane.contains("row_click_block_id"));
        assert!(pane.contains("USER_MESSAGE_WIDTH_PERCENT as f32 / 100."));
        assert!(pane.contains(".max_w(px(USER_MESSAGE_MAX_WIDTH as f32))"));
        assert!(pane.contains("card.max_w(px(ASSISTANT_MESSAGE_MAX_WIDTH as f32))"));
        assert!(pane.contains(".h(px(row_height))\n                                .w_full()\n                                .min_w_0()"));
        assert!(
            !pane.contains(".w_full()\n                                        .flex_shrink_0()")
        );
        let streaming = include_str!("native_shell/streaming_text.rs");
        assert!(streaming.contains(".selectable(true)"));
        assert!(streaming.contains(
            "TextView::markdown(self.id.clone(), self.text.clone())\n            .w_full()\n            .min_w_0()"
        ));
        assert!(!streaming.contains(".label(\"Copy code\")"));
        assert!(pane.contains(".child(visual.glyph)"));
        assert!(!pane.contains(
            "block.kind\n                                                                != ConversationBlockKind::User"
        ));
        assert!(pane.contains("!is_assistant"));
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
                item_key: expanded_row.item_key,
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
    fn focus_ring_visibility_tracks_keyboard_instead_of_pointer_input() {
        assert!(!matches!(
            FocusInputModality::default(),
            FocusInputModality::Keyboard
        ));
        let shell = include_str!("native_shell.rs");
        let pointer_capture = ["capture_any_mouse_", "down"].concat();
        let keyboard_capture = ["capture_key_", "down"].concat();
        assert!(shell.contains(&pointer_capture));
        assert!(shell.contains(&keyboard_capture));
        assert!(shell.contains("keyboard_focus_visible"));
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
    fn desktop_accessibility_contract_covers_regions_items_and_modal_focus() {
        let shell = include_str!("native_shell.rs");
        let controls = include_str!("native_shell/desktop_controls.rs");
        let sessions = include_str!("native_shell/sessions_pane.rs");
        let conversation = include_str!("native_shell/conversation_pane.rs");
        let composer = include_str!("native_shell/composer_pane.rs");
        let inspector = include_str!("native_shell/inspector_pane.rs");
        let toast_host = include_str!("native_shell/toast_host.rs");
        let overlays = include_str!("native_shell/overlay_host.rs");

        assert!(shell.contains(".role(Role::Application)"));
        assert!(shell.contains(".role(Role::Main)"));
        assert!(sessions.contains(".role(Role::Navigation)"));
        assert!(sessions.contains(".role(Role::SearchInput)"));
        assert!(sessions.contains("DesktopActionRow::new("));
        assert!(controls.contains("pub(super) struct DesktopActionRow"));
        assert!(controls.contains("Button::new(self.id)"));
        assert!(controls.contains(".role(Role::Button)"));
        assert!(controls.contains(".aria_label(accessible_label)"));
        assert!(controls.contains(".aria_selected(selected)"));
        assert!(conversation.contains(".role(Role::Log)"));
        assert!(conversation.contains(".role(Role::ListItem)"));
        assert!(conversation.contains("row.aria_active_descendant()"));
        assert!(composer.contains(".role(Role::Form)"));
        assert!(inspector.contains(".role(Role::Complementary)"));
        assert!(inspector.contains(".role(Role::TabList)"));
        assert!(inspector.contains(".role(Role::TabPanel)"));
        assert!(toast_host.contains(".role(Role::Status)"));
        assert!(overlays.contains(".role(Role::Dialog)"));
        assert!(overlays.contains(".role(Role::AlertDialog)"));

        // Every modal role is attached to the same element that owns focus,
        // so assistive technology receives focus inside the active dialog.
        assert!(
            overlays.contains(
                "overlay_surface(\"command-palette-overlay\", &self.command_palette_focus)"
            )
        );
        assert!(
            overlays
                .contains("overlay_surface(\"authorization-overlay\", &self.authorization_focus)")
        );
        assert!(inspector.contains(".track_focus(&self.focus)"));
    }

    #[test]
    fn accessibility_dependencies_are_reproducibly_locked() {
        let manifest = include_str!("../../Cargo.toml");
        let lock = include_str!("../../../../Cargo.lock");
        assert!(manifest.contains("rev = \"bc174a7ec4534b2a4174fddde314b38d30d69093\""));
        assert!(manifest.contains("https://github.com/zed-industries/zed.git"));
        assert!(lock.contains(
            "git+https://github.com/zed-industries/zed.git#30730a305ae235f3be44643d5895e142048ef701"
        ));
        assert!(lock.contains(
            "git+https://github.com/longbridge/gpui-component.git?rev=bc174a7ec4534b2a4174fddde314b38d30d69093#bc174a7ec4534b2a4174fddde314b38d30d69093"
        ));
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
        assert!(!assistant.align_right);
        assert_ne!(tool.accent, failed_tool.accent);
        assert_eq!(tool.accent, theme.muted_text);
        assert_eq!(failed_tool.accent, theme.danger);
        assert_eq!(diagnostic.accent, theme.danger);
        assert_ne!(user.glyph, assistant.glyph);
        assert_ne!(assistant.glyph, tool.glyph);
        assert_ne!(tool.glyph, diagnostic.glyph);
        assert_eq!(delegation.accent, theme.accent);
    }

    #[test]
    fn conversation_blocks_use_geometry_neutral_selection_and_hover_rails() {
        let conversation = include_str!("native_shell/conversation_pane.rs");
        assert!(!conversation.contains("bg(rgb(visual.surface.value()))"));
        assert!(!conversation.contains("style.bg(rgb(theme.hover.value()))"));
        assert!(!conversation.contains("card.bg(rgb(theme.selection.value()))"));
        assert!(!conversation.contains(".rounded_token(DesignRadius::Sm)\n                                                                .bg(rgb(theme.elevated.value()))"));
        assert!(conversation.contains("conversation-selected-rail"));
        assert!(conversation.contains("conversation-hover-rail"));
        assert!(conversation.contains("gpui::transparent_black()"));
        assert!(conversation.contains("group_hover(hover_group.clone()"));
    }

    #[test]
    fn conversation_selection_outranks_hover_without_relying_on_hue() {
        let theme = SemanticTheme::GEEK_DARK;
        // The palette deliberately shares one hue between `focus_ring` and
        // `accent`, and the review pipeline also renders a grayscale
        // derivative of the wide fixture. Neither state may therefore depend
        // on colour alone; rail length is the carrier that survives both.
        assert_eq!(theme.focus_ring, theme.accent);
        assert_ne!(theme.focus_ring, theme.muted_text);
    }

    #[gpui::test]
    fn conversation_selection_and_hover_rails_preserve_card_geometry(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (shell, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection_with_last_item(CodingAgentSessionTranscriptItem::User {
                text: "Selection and hover rails must preserve this row.".into(),
            }),
        );
        cx.simulate_resize(size(px(1_300.), px(900.)));
        settle_visual_measurements(cx);
        let card_before = cx
            .debug_bounds("conversation-last-card")
            .expect("the final conversation card is visible");
        let hover_rail = cx
            .debug_bounds("conversation-hover-rail")
            .expect("unselected rows reserve the hover rail slot");
        assert_eq!(f32::from(hover_rail.size.width), CONVERSATION_RAIL_WIDTH);
        assert!(
            (f32::from(hover_rail.center().y) - f32::from(card_before.center().y)).abs() <= 1.,
            "the hover stub must stay vertically centred on its card"
        );
        cx.simulate_mouse_move(card_before.center(), None, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            cx.debug_bounds("conversation-last-card"),
            Some(card_before),
            "the hover rail must not participate in card layout"
        );

        shell.update(cx, |shell, cx| shell.select_adjacent_conversation(true, cx));
        settle_visual_measurements(cx);
        let rail = cx
            .debug_bounds("conversation-selected-rail")
            .expect("keyboard selection paints a dedicated rail");
        assert_eq!(f32::from(rail.size.width), CONVERSATION_RAIL_WIDTH);
        assert!(f32::from(rail.size.height) > 0.);
        assert!(
            f32::from(rail.size.height) > f32::from(hover_rail.size.height) * 2.,
            "selection must stay distinguishable from hover without colour: \
             selected {} vs hover {}",
            f32::from(rail.size.height),
            f32::from(hover_rail.size.height)
        );
        assert_eq!(
            cx.debug_bounds("conversation-last-card"),
            Some(card_before),
            "the selection rail must not participate in card layout"
        );
    }

    #[test]
    fn desktop_typography_uses_system_ui_with_local_monospace_data_regions() {
        let shell = include_str!("native_shell.rs");
        let controls = include_str!("native_shell/desktop_controls.rs");
        let conversation = include_str!("native_shell/conversation_pane.rs");
        let sessions = include_str!("native_shell/sessions_pane.rs");
        let inspector = include_str!("native_shell/inspector_pane.rs");
        let overlays = include_str!("native_shell/overlay_host.rs");

        assert!(shell.contains(".font_family(UI_FONT_FAMILY)"));
        for local_data_surface in [conversation, inspector, overlays] {
            assert!(local_data_surface.contains("MONOSPACE_FONT_FAMILY"));
        }
        assert!(sessions.contains("DesktopActionRow::new("));
        assert!(controls.contains(".text_token(DesignText::Body)"));
        assert!(controls.contains(".text_token(DesignText::Metadata)"));
        assert!(conversation.contains("theme.reasoning.value()"));
        assert!(!conversation.contains("border_color(rgb(visual.accent.value()))"));
    }

    #[test]
    fn desktop_panes_consume_spacing_radius_and_typography_tokens() {
        let panes = [
            include_str!("native_shell/conversation_header.rs"),
            include_str!("native_shell/conversation_pane.rs"),
            include_str!("native_shell/composer_pane.rs"),
            include_str!("native_shell/sessions_pane.rs"),
            include_str!("native_shell/inspector_pane.rs"),
            include_str!("native_shell/toast_host.rs"),
            include_str!("native_shell/overlay_host.rs"),
        ];
        let legacy_utility_tokens = [
            ".p_1()",
            ".p_2()",
            ".p_3()",
            ".p_4()",
            ".p_5()",
            ".px_2()",
            ".px_3()",
            ".px_4()",
            ".py_1()",
            ".py_2()",
            ".py_3()",
            ".gap_1()",
            ".gap_2()",
            ".gap_3()",
            ".mt_1()",
            ".mt_2()",
            ".mt_3()",
            ".rounded_md()",
            ".rounded_lg()",
            ".text_xs()",
            ".text_sm()",
        ];

        for pane in panes {
            assert!(pane.contains("DesktopStyledExt"));
            for legacy in legacy_utility_tokens {
                assert!(
                    !pane.contains(legacy),
                    "desktop pane bypasses design tokens with {legacy}"
                );
            }
        }
        assert!(!include_str!("native_shell/overlay_host.rs").contains("rgba(0x"));
    }

    #[test]
    fn conversation_focus_uses_the_existing_header_divider_without_panel_geometry() {
        let theme = SemanticTheme::GEEK_DARK;
        assert_eq!(conversation_focus_accent(false, theme), theme.divider);
        assert_eq!(conversation_focus_accent(true, theme), theme.accent);

        let header = include_str!("native_shell/conversation_header.rs");
        assert!(header.contains("focus_accent.value()"));
        assert!(header.contains(".border_b_1()"));
        assert!(!header.contains(".border_1()"));
    }

    #[test]
    fn conversation_streaming_text_uses_revision_phase_and_stable_markdown_identity() {
        let source = include_str!("native_shell.rs");
        let pane = include_str!("native_shell/conversation_pane.rs");
        let streaming = include_str!("native_shell/streaming_text.rs");
        assert!(pane.contains("block.markdown_state_key.clone()"));
        assert!(pane.contains("block.detail_markdown_state_key.clone()"));
        assert!(pane.contains("let text_phase = block.text_phase"));
        assert!(pane.contains("block.item_key.stable_id_arc()"));
        assert!(streaming.contains("StreamingTextPhase::StreamingPlainText"));
        assert!(streaming.contains("StreamingTextPhase::SettlingMarkdown"));
        assert!(streaming.contains("StreamingTextPhase::FinalMarkdown"));
        assert!(streaming.contains("TextView::markdown"));
        assert!(streaming.contains("EVO_DESKTOP_MARKDOWN_TRACE"));
        assert!(streaming.contains("element.request_layout(window, cx)"));
        assert!(streaming.contains("desktop.markdown.parse_complete"));
        assert!(streaming.contains("markdown_parse_to_layout_us"));
        assert!(!pane.contains(".id((\"conversation-block\", index))"));
        assert!(!source.contains("(\"transcript-markdown\", index)"));
        assert!(!source.contains("(\"transcript-detail-markdown\", index)"));
        let legacy_per_render_sanitizer =
            ["bounded_markdown_preview(", "&block.text", ")"].concat();
        assert!(!source.contains(&legacy_per_render_sanitizer));
    }

    #[test]
    fn composer_and_transcript_source_do_not_restore_fixed_heights() {
        let shell = include_str!("native_shell.rs");
        let composer = include_str!("native_shell/composer_pane.rs");
        assert!(composer.contains(".auto_grow(1, 8)"));
        assert!(shell.contains("row.estimated_height"));
        let fixed_row_height = [".h(px(", "220.))"].concat();
        let fixed_composer_height = [".h(px(", "COMPOSER_HEIGHT"].concat();
        assert!(!shell.contains(&fixed_row_height));
        assert!(!composer.contains(&fixed_composer_height));
    }

    #[test]
    fn conversation_list_sizes_persist_and_full_history_work_is_dirty_gated() {
        let shell = include_str!("native_shell.rs");
        let controller = include_str!("native_shell/conversation_controller.rs");

        // Row sizes, dirty gating and the resize debounce belong to the
        // conversation controller; the composition root must not re-own them.
        let row_sizes_field = [
            "row_sizes: Rc<RefCell<Rc<Vec<gpui::",
            "Size<gpui::Pixels>>>>>",
        ]
        .concat();
        let live_dirty_branch = ["else if self.render_", "live_dirty"].concat();
        let live_truncate = ["render_rows.truncate(", "durable_count)"].concat();
        let row_sizes_make_mut = ["Rc::make_mut(&mut ", "row_sizes)"].concat();
        let debounce_const = ["RESIZE_DEBOUNCE: Duration = ", "Duration::from_millis(67)"].concat();
        let sequence_update = ["fn update_rows_by_", "sequence("].concat();
        let message_sequence = ["message.updated_sequence == ", "sequence"].concat();
        let tool_sequence = ["tool.updated_sequence == ", "sequence"].concat();
        for owned in [
            &row_sizes_field,
            &live_dirty_branch,
            &live_truncate,
            &row_sizes_make_mut,
            &debounce_const,
            &sequence_update,
            &message_sequence,
            &tool_sequence,
        ] {
            assert!(
                controller.contains(owned.as_str()),
                "conversation controller must own {owned}"
            );
            assert!(
                !shell.contains(owned.as_str()),
                "native shell composition must not own {owned}"
            );
        }

        // The root prepares rows exactly once per frame, outside Render, and
        // never reaches for the legacy full-history rebuild.
        let render = shell
            .split_once("impl Render for NativeShell")
            .expect("native render implementation remains present")
            .1;
        let prepare_call = ["prepare_", "rows(&source, layout_width)"].concat();
        let refresh_call = ["refresh_conversation_rows_", "at_width(layout_width, cx)"].concat();
        let legacy_rebuild_call = ["rebuild_conversation_", "render_rows("].concat();
        assert_eq!(shell.matches(&prepare_call).count(), 1);
        assert_eq!(render.matches(&prepare_call).count(), 0);
        assert_eq!(render.matches(&refresh_call).count(), 1);
        assert_eq!(shell.matches(&legacy_rebuild_call).count(), 0);
    }

    #[test]
    fn conversation_transcript_rendering_is_owned_by_a_child_entity() {
        let shell = include_str!("native_shell.rs");
        let pane = include_str!("native_shell/conversation_pane.rs");
        let virtual_list_call = ["v_virtual_", "list("].concat();
        let scroll_region_id = ["conversation-scroll-", "region"].concat();
        let follow_latest_button = ["Button::new(\"follow-", "latest\")"].concat();
        assert!(!shell.contains(&virtual_list_call));
        assert!(pane.contains(&virtual_list_call));
        assert!(shell.contains("conversation_pane: gpui::Entity<ConversationPane>"));
        assert!(shell.contains("let conversation_pane = cx.new("));
        assert!(shell.contains(".child(self.conversation_pane.clone())"));
        assert!(pane.contains("impl EventEmitter<ConversationPaneEvent>"));
        assert!(pane.contains("struct ConversationPaneViewModel"));
        assert!(pane.contains("view_model: Option<ConversationPaneViewModel>"));
        assert!(!pane.contains("WeakEntity<NativeShell>"));
        assert!(!pane.contains("owner.read(cx)"));
        assert!(!pane.contains("DesktopProjection"));
        assert!(shell.contains("fn notify_conversation_pane("));
        assert!(shell.contains("fn conversation_pane_view_model("));
        assert!(shell.contains("if pane_dirty"));
        assert!(shell.contains("self.conversation_pane.update(cx, |_, cx| cx.notify())"));
        assert!(!shell.contains(&scroll_region_id));
        assert!(!shell.contains(&follow_latest_button));
        assert!(pane.contains(&scroll_region_id));
        assert!(pane.contains(&follow_latest_button));
        assert!(pane.contains("ConversationPaneEvent::Scrolled"));
        assert!(pane.contains("ConversationPaneEvent::FollowLatest"));
    }

    #[test]
    fn streaming_only_projection_delta_does_not_dirty_native_shell_root() {
        let streaming = desktop::projection::DesktopProjectionDelta {
            cursor: true,
            conversation: true,
            tools: true,
            ..Default::default()
        };
        assert!(!root_projection_dirty(false, Some(&streaming)));
        assert!(root_projection_dirty(true, Some(&streaming)));

        let authorization = desktop::projection::DesktopProjectionDelta {
            authorizations: true,
            ..Default::default()
        };
        assert!(root_projection_dirty(false, Some(&authorization)));

        let shell = include_str!("native_shell.rs");
        assert!(!shell.contains("if applied > 0 {\n            cx.notify();"));
        assert!(shell.contains("if root_dirty {\n            cx.notify();"));
        assert!(shell.contains("refresh_conversation_rows_at_current_width(cx)"));
    }

    #[test]
    fn sessions_rendering_is_owned_by_a_non_streaming_child_entity() {
        let shell = include_str!("native_shell.rs");
        let pane = include_str!("native_shell/sessions_pane.rs");
        let sessions_panel_id = [".id(\"sessions-", "panel\")"].concat();
        let root_search_field = ["sessions_search_", "input:"].concat();

        assert!(!shell.contains(&sessions_panel_id));
        assert!(pane.contains(&sessions_panel_id));
        assert!(shell.contains("sessions_pane: gpui::Entity<SessionsPane>"));
        assert!(shell.contains("let sessions_pane = cx.new("));
        assert!(shell.contains(".child(self.sessions_pane.clone())"));
        assert!(pane.contains("impl EventEmitter<SessionsPaneEvent>"));
        assert!(pane.contains("struct SessionsPaneViewModel"));
        assert!(pane.contains("view_model: Option<SessionsPaneViewModel>"));
        assert!(pane.contains("search_input: gpui::Entity<InputState>"));
        assert!(pane.contains("InputState::new(window, cx).placeholder(\"Search sessions…\")"));
        assert!(!pane.contains("WeakEntity<NativeShell>"));
        assert!(!pane.contains("owner.read(cx)"));
        assert!(!pane.contains("DesktopProjection"));
        assert!(shell.contains("fn notify_sessions_pane("));
        assert!(shell.contains("fn sessions_pane_view_model("));
        assert!(!shell.contains(&root_search_field));
        assert!(!pane.contains("conversation_controller.render_rows"));
        assert!(!pane.contains("conversation_controller.render_dirty_sequences"));
    }

    #[test]
    fn composer_rendering_and_input_changes_are_isolated_in_a_child_entity() {
        let shell = include_str!("native_shell.rs");
        let pane = include_str!("native_shell/composer_pane.rs");
        let composer_panel_id = [".id(\"composer-", "panel\")"].concat();
        let input_constructor = ["Input", "::new(&input)"].concat();
        let root_input_field = ["composer_", "input:"].concat();
        let root_latency_field = ["composer_", "input_latency:"].concat();
        let weak_root_owner = ["WeakEntity", "<NativeShell>"].concat();

        assert!(!shell.contains(&composer_panel_id));
        assert!(pane.contains(&composer_panel_id));
        assert!(shell.contains("composer_pane: gpui::Entity<ComposerPane>"));
        assert!(shell.contains(".child(self.composer_pane.clone())"));
        assert!(pane.contains("struct ComposerPaneViewModel"));
        assert!(pane.contains("input: gpui::Entity<InputState>"));
        assert!(pane.contains("focus: FocusHandle"));
        assert!(pane.contains("latency: InputRenderLatencyProbe"));
        assert!(pane.contains("impl EventEmitter<ComposerPaneEvent>"));
        assert!(pane.contains(&input_constructor));
        assert!(pane.contains("InputEvent::Change =>"));
        assert!(pane.contains("ComposerPaneEvent::InputChanged"));
        assert!(pane.contains("ComposerPaneEvent::Focused"));
        assert!(pane.contains("ComposerPaneEvent::SubmitPrimary"));
        assert!(!pane.contains(&weak_root_owner));
        assert!(!pane.contains("owner.read(cx)"));
        assert!(!pane.contains("DesktopProjection"));
        assert!(!shell.contains(&root_input_field));
        assert!(!shell.contains(&root_latency_field));
        assert!(shell.contains("fn composer_pane_view_model(&self) -> ComposerPaneViewModel"));
        assert!(shell.contains("composer: ComposerState"));
        assert!(shell.contains("this.notify_composer_pane(cx)"));
        assert!(shell.contains("ComposerPaneEvent::SubmitRunning"));
        assert!(shell.contains("ComposerSubmissionKind::Steer"));
        assert!(shell.contains("ComposerPaneEvent::SetRunningMode"));
        assert!(!pane.contains("ComposerPaneEvent::CycleThinking"));
        assert!(shell.contains("ComposerSubmissionKind::FollowUp"));
        assert!(pane.contains("DesktopSelector::new("));
        assert!(pane.contains("\"composer-running-mode-selector\""));
        assert!(pane.contains("PopupMenuItem::new(\"Steer now\")"));
        assert!(pane.contains("PopupMenuItem::new(\"Queue next\")"));
        assert!(pane.contains("DesktopIconButton::new("));
        assert!(pane.contains("DesktopIcon::Submit"));
        assert!(!pane.contains("desktop-composer-thinking"));
        assert!(pane.contains("desktop-composer-surface"));
        assert!(pane.contains(".min_h(px(48.))"));
        assert!(!pane.contains(".w(px(176.))"));
        assert!(!pane.contains("composer-mode-steer"));
        assert!(!pane.contains("composer-mode-follow-up"));
        assert!(pane.contains("submit-running-composer"));
        assert!(pane.contains("desktop-composer-state-notice"));
        let legacy_steer_button = ["Button::new(\"steer-", "operation\")"].concat();
        let legacy_follow_up_button = ["Button::new(\"follow-up-", "operation\")"].concat();
        let legacy_draft_map = ["composer_session_", "drafts: HashMap"].concat();
        let legacy_mode_map = ["composer_running_", "modes: HashMap"].concat();
        assert!(!pane.contains(&legacy_steer_button));
        assert!(!pane.contains(&legacy_follow_up_button));
        assert!(!shell.contains(&legacy_draft_map));
        assert!(!shell.contains(&legacy_mode_map));
        assert!(shell.contains("struct SessionWorkspace"));
        assert!(shell.contains("composer_running_mode: ComposerRunningMode"));
        assert!(shell.contains("workspaces: HashMap<String, SessionWorkspace>"));
        assert!(!pane.contains("conversation_controller.render_dirty_sequences"));
    }

    #[test]
    fn inspector_rendering_is_owned_by_a_non_streaming_child_entity() {
        let shell = include_str!("native_shell.rs");
        let pane = include_str!("native_shell/inspector_pane.rs");
        let inspector_panel_id = [".id(\"inspector-", "panel\")"].concat();
        let legacy_inspector_map = ["inspector_session_", "sections: HashMap"].concat();

        assert!(!shell.contains(&inspector_panel_id));
        assert!(pane.contains(&inspector_panel_id));
        assert!(shell.contains("inspector_pane: gpui::Entity<InspectorPane>"));
        assert!(shell.contains(".child(self.inspector_pane.clone())"));
        assert!(pane.contains("struct InspectorPaneViewModel"));
        assert!(pane.contains("view_model: Option<InspectorPaneViewModel>"));
        assert!(pane.contains("focus: FocusHandle"));
        assert!(pane.contains("file_review: Arc<DesktopFileReviewState>"));
        assert!(pane.contains("impl EventEmitter<InspectorPaneEvent>"));
        assert!(pane.contains("RequestFileReview(CodingAgentFileReviewRequest)"));
        assert!(pane.contains("identity: DesktopRecoveryIdentity"));
        assert!(!pane.contains("WeakEntity<NativeShell>"));
        assert!(!pane.contains("owner.read(cx)"));
        assert!(!pane.contains("DesktopProjection"));
        assert!(!pane.contains("command_ledger"));
        assert!(!pane.contains("preferences."));
        assert!(shell.contains("fn inspector_pane_view_model(&self) -> InspectorPaneViewModel"));
        assert!(shell.contains("file_review: Arc<DesktopFileReviewState>"));
        assert!(shell.contains("inspector_telemetry_refresh_deadline: Option<Instant>"));
        assert!(!shell.contains(&legacy_inspector_map));
        assert!(shell.contains("inspector_section: InspectorSection"));
        assert!(shell.contains("command_ledger: DesktopCommandLedger"));
        assert!(shell.contains("this.submit_recovery_action(identity.clone(), *action, cx)"));
        assert!(shell.contains("fn notify_inspector_pane("));
        assert!(!pane.contains("conversation_controller.render_dirty_sequences"));
    }

    #[test]
    fn streaming_only_projection_delta_does_not_dirty_inspector() {
        let mut streaming = desktop::projection::DesktopProjectionDelta {
            cursor: true,
            conversation: true,
            tools: true,
            ..Default::default()
        };
        assert!(!inspector_projection_dirty(&streaming));

        streaming.diagnostics = true;
        assert!(inspector_projection_dirty(&streaming));
        assert!(inspector_projection_immediate_dirty(&streaming));
    }

    #[test]
    fn sessions_navigation_is_searchable_recent_and_automatically_refreshed() {
        let shell = include_str!("native_shell.rs");
        let pane = include_str!("native_shell/sessions_pane.rs");
        let controller = include_str!("native_shell/session_controller.rs");
        let search_placeholder = ["placeholder(\"Search ", "sessions…\")"].concat();
        let refresh_interval = ["Duration::from_secs(", "15)"].concat();
        let active_duplicate = ["current_session_", "label"].concat();
        let root_refresh_deadline = ["session_catalog_refresh_", "deadline"].concat();

        assert!(pane.contains(&search_placeholder));
        assert!(controller.contains("schedule_session_catalog_refresh"));
        assert!(controller.contains(&refresh_interval));
        assert!(controller.contains("struct SessionController"));
        assert!(controller.contains("refresh_deadline: Option<Instant>"));
        assert!(!shell.contains(&root_refresh_deadline));
        assert!(pane.contains("relative_session_time"));
        assert!(pane.contains("Untitled"));
        assert!(pane.contains("Rename session"));
        assert!(pane.contains("SessionsPaneEvent::Rename"));
        assert!(pane.contains("view_model.catalog"));
        assert!(pane.contains("sessions-overflow"));
        assert!(pane.contains("PopupMenuItem::new(if session_catalog_pending"));
        assert!(pane.contains("DesktopActionRow::new("));
        assert!(pane.contains("DesktopIcon::Plus"));
        assert!(pane.contains("DesktopIcon::Search"));
        assert!(pane.contains("DesktopIcon::Clear"));
        assert!(pane.contains("DesktopIcon::Overflow"));
        assert!(pane.contains("No recent sessions yet."));
        assert!(pane.contains("No sessions match"));
        assert!(pane.contains("Loading sessions"));
        assert!(pane.contains("context_is_overlay"));
        assert!(!pane.contains(".label(\"Open\")"));
        assert!(!pane.contains("refresh-session-catalog"));
        assert!(!pane.contains(&active_duplicate));
    }

    #[test]
    fn inspector_defaults_to_task_relevant_sections_and_hides_empty_telemetry() {
        let pane = include_str!("native_shell/inspector_pane.rs");
        let permanent_diagnostics = ["diagnostics ", "{:>4}"].concat();
        let permanent_recoveries = ["recoveries ", "{:>4}"].concat();

        assert_eq!(InspectorSection::default(), InspectorSection::Changes);
        for section in ["Changes", "Task", "Usage", "Runtime"] {
            assert!(pane.contains(section));
        }
        assert!(pane.contains("InspectorPaneEvent::SelectSection(section)"));
        assert!(pane.contains("Badge::new()"));
        assert!(pane.contains(".count(runtime_attention_count)"));
        assert!(pane.contains("DesktopActionRow::new("));
        assert!(pane.contains("DesktopIcon::Copy"));
        assert!(pane.contains("DesktopIcon::OpenExternal"));
        assert!(pane.contains("DesktopIcon::Close"));
        assert!(pane.contains("desktop-inspector-tabs"));
        assert!(!pane.contains("\"●\""));
        assert!(!pane.contains("\"○\""));
        assert!(!pane.contains(".label(\"Copy path\")"));
        assert!(!pane.contains(".label(\"Copy review\")"));
        assert!(!pane.contains(".label(\"Open editor\")"));
        assert!(!pane.contains(".label(\"Close\")"));
        assert!(pane.contains("when_some(latest_diagnostic"));
        assert!(pane.contains("when_some(latest_recovery"));
        assert!(!pane.contains(&permanent_diagnostics));
        assert!(!pane.contains(&permanent_recoveries));
    }

    #[test]
    fn sidebar_resize_handles_overlay_layout_and_persist_on_release() {
        let shell = include_str!("native_shell.rs");
        let preferences = include_str!("../preferences.rs");
        let sessions_handle = ["sessions-resize-", "handle"].concat();
        let inspector_handle = ["inspector-resize-", "handle"].concat();
        let double_click = ["event.click_count ", ">= 2"].concat();

        assert!(shell.contains(&sessions_handle));
        assert!(shell.contains(&inspector_handle));
        assert!(shell.contains(".absolute()"));
        assert!(shell.contains(".cursor_ew_resize()"));
        assert!(shell.contains(&double_click));
        assert!(shell.contains("finish_panel_resize"));
        assert!(preferences.contains("sessions_panel_width: u32"));
        assert!(preferences.contains("context_panel_width: u32"));
    }

    #[test]
    fn usage_only_projection_delta_is_throttled_for_inspector() {
        let usage = desktop::projection::DesktopProjectionDelta {
            context: desktop::projection::ContextDirtyFlags::USAGE,
            ..Default::default()
        };
        assert!(inspector_projection_dirty(&usage));
        assert!(!inspector_projection_immediate_dirty(&usage));

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

        let shell = include_str!("native_shell.rs");
        assert!(shell.contains("const INSPECTOR_TELEMETRY_REFRESH_INTERVAL"));
        assert!(shell.contains("Duration::from_millis(250)"));
        assert!(shell.contains("schedule_inspector_telemetry_refresh(cx)"));
    }

    #[test]
    fn conversation_header_rendering_is_owned_by_a_non_streaming_child_entity() {
        let shell = include_str!("native_shell.rs");
        let header = include_str!("native_shell/conversation_header.rs");
        let actions = include_str!("../actions.rs");
        let header_id = [".id(\"conversation-", "header\")"].concat();
        let old_model_cycle = ["SelectNext", "Model"].concat();
        let old_profile_cycle = ["SelectNext", "Profile"].concat();
        let old_session_profile_cycle = ["SelectNext", "SessionProfile"].concat();

        assert!(!shell.contains(&header_id));
        assert!(header.contains(&header_id));
        assert!(shell.contains("conversation_header: gpui::Entity<ConversationHeader>"));
        assert!(shell.contains(".child(self.conversation_header.clone())"));
        assert!(header.contains("impl EventEmitter<ConversationHeaderEvent>"));
        assert!(shell.contains("ConversationHeaderEvent::ToggleSessions"));
        assert!(shell.contains("ConversationHeaderEvent::ToggleInspector"));
        assert!(shell.contains("ConversationHeaderEvent::Reload"));
        assert!(shell.contains("ConversationHeaderEvent::SelectModel(model_id)"));
        assert!(shell.contains("ConversationHeaderEvent::SelectSessionProfile(profile_id)"));
        assert!(shell.contains("ConversationHeaderEvent::SelectThinking(level)"));
        assert!(header.contains("SelectModel(Arc<str>)"));
        assert!(header.contains("SelectSessionProfile(Arc<str>)"));
        assert!(header.contains("SelectThinking(DesktopThinkingLevel)"));
        assert!(header.contains("desktop-header-model-selector"));
        assert!(header.contains("desktop-header-profile-selector"));
        assert!(header.contains("desktop-header-thinking-selector"));
        assert!(header.contains(".checked(option.id == current_model_id)"));
        assert!(header.contains(".checked(option.id == current_profile_id)"));
        assert!(header.contains(".scrollable(model_options.len() > 8)"));
        assert!(header.contains(".scrollable(profile_options.len() > 8)"));
        assert!(header.contains("DesktopThinkingLevel::ALL.iter().fold("));
        assert!(header.contains(".checked(level == view_model.thinking_selection)"));
        for owner in [shell, header, actions] {
            assert!(!owner.contains(&old_model_cycle));
            assert!(!owner.contains(&old_profile_cycle));
            assert!(!owner.contains(&old_session_profile_cycle));
        }
        assert!(!header.contains("ConversationHeaderEvent::CycleThinking"));
        assert!(shell.contains("ConversationHeaderEvent::Abort"));
        assert!(header.contains("ConversationHeaderViewModel"));
        assert!(shell.contains("conversation_header_view_model"));
        assert!(!header.contains("WeakEntity"));
        assert!(!header.contains("NativeShell"));
        assert!(!header.contains("owner.read(cx)"));
        assert!(!header.contains("DesktopProjection"));
        assert!(!header.contains("copy-conversation-block"));
        assert!(!header.contains("reload-local-resources"));
        assert!(header.contains("header-overflow"));
        assert!(header.contains("Reload local resources"));
        assert!(header.contains("Inspector"));
        assert!(header.contains("DesktopIcon::PanelLeft"));
        assert!(header.contains("DesktopIcon::PanelRight"));
        assert!(header.contains("DesktopIcon::Overflow"));
        assert!(header.contains("DesktopSelector::new("));
        assert!(!header.contains(".label(\"Sessions\")"));
        assert!(!header.contains(".label(\"Inspector\")"));
        assert!(!header.contains(".label(\"...\")"));
        assert!(header.contains("thinking: Arc<str>"));
        assert!(header.contains("thinking_selection: DesktopThinkingLevel"));
        assert!(!header.contains("conversation_controller.render_dirty_sequences"));
    }

    #[test]
    fn streaming_only_projection_delta_does_not_dirty_conversation_header() {
        let mut streaming = desktop::projection::DesktopProjectionDelta {
            cursor: true,
            conversation: true,
            tools: true,
            ..Default::default()
        };
        assert!(!conversation_header_projection_dirty(&streaming));

        streaming.lifecycle = true;
        assert!(conversation_header_projection_dirty(&streaming));
    }

    #[test]
    fn transient_notices_are_owned_by_a_bounded_toast_host() {
        let shell = include_str!("native_shell.rs");
        let host = include_str!("native_shell/toast_host.rs");
        let commands = include_str!("native_shell/commands.rs");
        let sessions = include_str!("native_shell/session_controller.rs");
        let exact_assignment = ["self.preference_notice", " = Some(message);"].concat();
        let any_direct_assignment = [".preference_notice", " = Some("].concat();

        assert!(shell.contains("toast_host: gpui::Entity<ToastHost>"));
        assert!(shell.contains(".child(toast_host)"));
        assert!(shell.contains("fn notify_toast_host"));
        assert!(shell.contains("preference_notice_revision"));
        assert_eq!(shell.matches(&exact_assignment).count(), 1);
        assert_eq!(shell.matches(&any_direct_assignment).count(), 1);
        for notice_owner in [commands, sessions] {
            assert!(notice_owner.contains("set_preference_notice("));
            assert!(!notice_owner.contains(&any_direct_assignment));
        }
        assert!(host.contains("MAX_VISIBLE_TOASTS: usize = 3"));
        assert!(host.contains("Duration::from_secs(6)"));
        assert!(host.contains(".on_hover("));
        assert!(host.contains("on_focus_in"));
        assert!(host.contains("on_focus_out"));
        assert!(host.contains("Dismiss notification"));
        assert!(host.contains(".role(Role::Status)"));
        assert!(!host.contains("WeakEntity"));
        assert!(!host.contains("NativeShell"));
        assert!(!host.contains("DesktopProjection"));
    }

    #[test]
    fn overlay_rendering_is_owned_by_a_typed_child_entity() {
        let shell = include_str!("native_shell.rs");
        let host = include_str!("native_shell/overlay_host.rs");
        let authorization_id = ["\"authorization-", "overlay\""].concat();
        let palette_id = ["\"command-palette-", "overlay\""].concat();
        let narrow_sessions_id = ["\"narrow-sessions-", "overlay\""].concat();

        assert!(!shell.contains(&authorization_id));
        assert!(!shell.contains(&palette_id));
        assert!(!shell.contains(&narrow_sessions_id));
        assert!(host.contains(&authorization_id));
        assert!(host.contains(&palette_id));
        assert!(host.contains(&narrow_sessions_id));
        assert!(shell.contains("overlay_host: gpui::Entity<OverlayHost>"));
        assert!(shell.contains(".child(overlay_host)"));
        assert!(host.contains("impl EventEmitter<OverlayHostEvent>"));
        assert!(host.contains("struct OverlayViewModel"));
        assert!(host.contains("view_model: Option<OverlayViewModel>"));
        assert!(host.contains("DecideAuthorization"));
        assert!(host.contains("fn authorization_detail("));
        assert!(host.contains("DesktopCriticalTone::Neutral"));
        assert!(host.contains("DesktopCriticalTone::Affirmative"));
        assert!(host.contains("DesktopCriticalTone::Dangerous"));
        assert!(host.contains("font_family(MONOSPACE_FONT_FAMILY)"));
        assert!(!host.contains("\"1 · Deny\""));
        assert!(!host.contains("\"2 · Allow once\""));
        assert!(!host.contains("\"3 · Allow for operation\""));
        assert!(shell.contains("this.decide_tool_authorization("));
        assert!(shell.contains("Self::on_trap_overlay_focus"));
        assert!(host.contains("self.inspector_pane.clone()"));
        assert!(host.contains("self.sessions_pane.clone()"));
        assert!(!host.contains("OverlaySessionView"));
        assert!(!host.contains("session_catalog_pending"));
        assert!(!host.contains("OpenSession"));
        assert!(!host.contains("WeakEntity<NativeShell>"));
        assert!(!host.contains("owner.read(cx)"));
        assert!(!host.contains("DesktopProjection"));
        assert!(!host.contains("command_ledger"));
        assert!(!host.contains("session_controller"));
        assert!(shell.contains("fn overlay_view_model(&self) -> OverlayViewModel"));
        assert!(shell.contains("authorization_focus: FocusHandle"));
        assert!(shell.contains("active_overlay: Option<DesktopOverlayKind>"));
        assert!(!host.contains("try_decide_tool_authorization"));
        assert!(!host.contains("command_ledger.reserve"));
    }

    #[test]
    fn streaming_only_projection_delta_does_not_dirty_overlay_host() {
        let mut streaming = desktop::projection::DesktopProjectionDelta {
            cursor: true,
            conversation: true,
            tools: true,
            ..Default::default()
        };
        assert!(!overlay_host_projection_dirty(&streaming));

        streaming.authorizations = true;
        assert!(overlay_host_projection_dirty(&streaming));
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
mod commands;
mod composer_pane;
mod conversation_controller;
mod conversation_header;
mod conversation_pane;
mod desktop_controls;
mod desktop_style;
mod home_pane;
mod inspector_pane;
mod overlay_host;
mod session_controller;
mod sessions_pane;
mod streaming_text;
mod toast_host;
mod update;

use commands::{DirectCommandUpdate, ProjectionCommandCompletions};
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
    ConversationHeader, ConversationHeaderEvent, ConversationHeaderSelectorOption,
    ConversationHeaderViewModel,
};
#[cfg(test)]
use conversation_pane::CONVERSATION_RAIL_WIDTH;
use conversation_pane::{ConversationPane, ConversationPaneEvent, ConversationPaneViewModel};
use home_pane::{HomePane, HomePaneEvent, HomePaneViewModel};
use inspector_pane::{
    InspectorChangedFileView, InspectorDiagnosticView, InspectorPane, InspectorPaneEvent,
    InspectorPaneViewModel, InspectorRecoveryView,
};
use overlay_host::{OverlayAuthorizationView, OverlayHost, OverlayHostEvent, OverlayViewModel};
use session_controller::SessionController;
use sessions_pane::{SessionRuntimeState, SessionsPane, SessionsPaneEvent, SessionsPaneViewModel};
use toast_host::{ToastHost, ToastNotice};
use update::ProjectionDirtyRouting;
#[cfg(test)]
use update::{
    conversation_header_projection_dirty, inspector_projection_dirty,
    inspector_projection_immediate_dirty, overlay_host_projection_dirty, root_projection_dirty,
};
