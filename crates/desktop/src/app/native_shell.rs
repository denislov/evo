use coding_agent::api::authorization::{ToolAuthorizationDecision, ToolAuthorizationIdentity};
use coding_agent::api::embedding::CodingAgentThinkingLevel;
use coding_agent::api::event::CodingAgentRecoveryResolution;
use coding_agent::api::review::CodingAgentFileReviewRequest;
use desktop::conversation::{
    ComposerAdmission, ComposerState, ComposerSubmissionKind, ConversationBlockKind,
    ConversationItemKey, ConversationItemKind, ConversationRowLayoutInput,
    ConversationRowLayoutState, ConversationRowRenderCache, ConversationRowRenderData,
    ConversationRowRenderSource, ConversationViewport, TRANSCRIPT_ROW_MAX_HEIGHT,
    conversation_block_height, conversation_copy_text, conversation_width_bucket,
};
use desktop::file_review::DesktopFileReviewDocument;
use desktop::preferences::{DesktopPreferences, PreferenceWriter};
use desktop::projection::{DesktopProjection, DesktopProjectionLifecycle, DesktopRecoveryStatus};
use desktop::runtime::{
    DesktopRecoveryAction, DesktopRecoveryIdentity, DesktopRuntimeBridge,
    DesktopRuntimeCommandHandle, DesktopRuntimeSelectionKind,
};
use desktop::shell::{
    CONTEXT_PANEL_MAX_WIDTH, CONTEXT_PANEL_MIN_WIDTH, CONTEXT_PANEL_WIDTH, FocusState, FocusTarget,
    MIN_CONVERSATION_WIDTH, PanelVisibility, SESSION_PANEL_MAX_WIDTH, SESSION_PANEL_MIN_WIDTH,
    SESSION_PANEL_WIDTH, SemanticColor, SemanticStatus, SemanticTheme, ShellLayout, UI_FONT_FAMILY,
    truncate_label,
};
use gpui::{
    ClipboardItem, Context, FocusHandle, Focusable as _, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Render, ScrollStrategy,
    Styled as _, Subscription, Window, WindowBounds, div, prelude::*, px, rgb, size,
};
use gpui_component::{
    VirtualListScrollHandle,
    input::{InputEvent, InputState},
};
use std::borrow::Cow;
use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::actions::{
    self, AbortActiveOperation, AuthorizationAllowForOperation, AuthorizationAllowOnce,
    AuthorizationDeny, CopySelectedConversation, DesktopCommandPalette, DesktopPaletteCommand,
    EscapeHierarchy, FocusComposer, FocusNextRegion, FocusPreviousRegion, FollowLatestOutput,
    NewSession, OpenCommandPalette, OpenFileSurface, PALETTE_ENTRIES, PaletteConfirm, PaletteNext,
    PalettePrevious, SelectNextConversation, SelectPreviousConversation, SubmitComposer,
    ToggleContextPanel, ToggleSelectedConversationDetails, TrapOverlayFocus,
};
use crate::command_ledger::{DesktopCommandIntent, DesktopCommandLedger};

const MAX_RUNTIME_UPDATES_PER_FRAME: usize = 64;
const MAX_DIRTY_CONVERSATION_SEQUENCES: usize = 256;
const CONVERSATION_RESIZE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(67);
const INSPECTOR_TELEMETRY_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
const MAX_EXPANDED_CONVERSATION_DETAILS: usize = 256;
const COLLAPSED_CONVERSATION_DETAIL_HEIGHT: f32 = 36.;
const MAX_COMPOSER_SESSION_STATES: usize = 256;

#[derive(Debug, Default)]
struct InputRenderLatencyProbe {
    pending_change: Cell<Option<Instant>>,
    #[cfg(test)]
    last_observed: Cell<Option<Duration>>,
}

impl InputRenderLatencyProbe {
    fn mark_changed(&self) {
        self.mark_changed_at(Instant::now());
    }

    fn mark_changed_at(&self, now: Instant) {
        self.pending_change.set(Some(now));
    }

    fn observe_render(&self) {
        let _ = self.observe_render_at(Instant::now());
    }

    fn observe_render_at(&self, now: Instant) -> Option<Duration> {
        let latency = now.saturating_duration_since(self.pending_change.take()?);
        tracing::trace!(
            target: "desktop",
            latency_micros = u64::try_from(latency.as_micros()).unwrap_or(u64::MAX),
            "desktop.input.to_render"
        );
        #[cfg(test)]
        self.last_observed.set(Some(latency));
        Some(latency)
    }
}
const SESSION_CATALOG_REFRESH_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Clone, Copy)]
struct ConversationBlockVisual {
    glyph: &'static str,
    surface: SemanticColor,
    accent: SemanticColor,
    align_right: bool,
}

fn conversation_focus_accent(focused: bool, theme: SemanticTheme) -> SemanticColor {
    if focused { theme.accent } else { theme.border }
}

fn conversation_distance_to_bottom(offset_y: f32, max_offset_y: f32) -> f32 {
    (max_offset_y.max(0.0) + offset_y.min(0.0)).max(0.0)
}

fn minimum_duration(
    current: Option<std::time::Duration>,
    next: Option<std::time::Duration>,
) -> Option<std::time::Duration> {
    match (current, next) {
        (Some(current), Some(next)) => Some(current.min(next)),
        (Some(current), None) => Some(current),
        (None, next) => next,
    }
}

#[cfg(test)]
fn inspector_projection_dirty(delta: &desktop::projection::DesktopProjectionDelta) -> bool {
    inspector_projection_immediate_dirty(delta)
        || delta
            .context
            .contains(desktop::projection::ContextDirtyFlags::USAGE)
}

fn inspector_projection_immediate_dirty(
    delta: &desktop::projection::DesktopProjectionDelta,
) -> bool {
    delta
        .context
        .contains(desktop::projection::ContextDirtyFlags::OPERATIONS)
        || delta
            .context
            .contains(desktop::projection::ContextDirtyFlags::DELEGATIONS)
        || delta
            .context
            .contains(desktop::projection::ContextDirtyFlags::CHANGES)
        || delta.diagnostics
        || delta.recoveries
        || delta.session
        || delta.profiles
        || delta.capabilities
        || delta.lifecycle
}

fn status_projection_dirty(delta: &desktop::projection::DesktopProjectionDelta) -> bool {
    delta
        .context
        .contains(desktop::projection::ContextDirtyFlags::OPERATIONS)
        || delta.authorizations
        || delta.terminal
        || delta.recoveries
        || delta.session
        || delta.profiles
        || delta.capabilities
        || delta.lifecycle
}

fn inspector_telemetry_refresh_delay(last_refresh: Option<Instant>, now: Instant) -> Duration {
    last_refresh.map_or(Duration::ZERO, |last_refresh| {
        INSPECTOR_TELEMETRY_REFRESH_INTERVAL
            .saturating_sub(now.saturating_duration_since(last_refresh))
    })
}

fn conversation_header_projection_dirty(
    delta: &desktop::projection::DesktopProjectionDelta,
) -> bool {
    delta
        .context
        .contains(desktop::projection::ContextDirtyFlags::OPERATIONS)
        || delta.lifecycle
        || delta.session
}

fn overlay_host_projection_dirty(delta: &desktop::projection::DesktopProjectionDelta) -> bool {
    conversation_header_projection_dirty(delta) || delta.authorizations
}

fn root_projection_dirty(
    projection_replaced: bool,
    delta: Option<&desktop::projection::DesktopProjectionDelta>,
) -> bool {
    projection_replaced || delta.is_some_and(|delta| delta.authorizations)
}

fn conversation_row_target_height(
    row: &ConversationRowRenderData,
    expanded_details: &HashSet<String>,
    panel_width: u32,
) -> f32 {
    if expanded_details.contains(row.item_key.row_id()) {
        return row.measured_height;
    }
    let collapsed = match row.kind {
        ConversationBlockKind::Assistant if !row.detail.is_empty() => Some(
            conversation_block_height(row.kind, &row.text, "", panel_width),
        ),
        ConversationBlockKind::Tool if !row.text.is_empty() || !row.detail.is_empty() => {
            Some(conversation_block_height(row.kind, "", "", panel_width))
        }
        _ => None,
    };
    collapsed.map_or(row.measured_height, |height| {
        (height + COLLAPSED_CONVERSATION_DETAIL_HEIGHT).min(TRANSCRIPT_ROW_MAX_HEIGHT)
    })
}

fn upsert_indexed_item<T>(
    items: &mut Vec<T>,
    existing_index: Option<usize>,
    mut desired_index: usize,
    item: T,
) -> usize {
    if let Some(existing_index) = existing_index {
        if existing_index == desired_index {
            items[existing_index] = item;
            return existing_index;
        }
        items.remove(existing_index);
        if existing_index < desired_index {
            desired_index = desired_index.saturating_sub(1);
        }
    }
    desired_index = desired_index.min(items.len());
    items.insert(desired_index, item);
    desired_index
}

fn message_conversation_block_id(message: &desktop::projection::DesktopMessageOverlay) -> String {
    message.message_id.as_ref().map_or_else(
        || format!("assistant:{}:{}", message.operation_id, message.turn_id),
        |message_id| format!("assistant:{message_id}"),
    )
}

fn tool_conversation_block_id(tool: &desktop::projection::DesktopToolOverlay) -> String {
    format!("tool:{}", tool.tool_call_id)
}

fn conversation_block_visual(
    kind: ConversationBlockKind,
    is_error: bool,
    theme: SemanticTheme,
) -> ConversationBlockVisual {
    match kind {
        ConversationBlockKind::User => ConversationBlockVisual {
            glyph: "YOU",
            surface: theme.user_surface,
            accent: theme.accent,
            align_right: true,
        },
        ConversationBlockKind::Assistant => ConversationBlockVisual {
            glyph: "AI",
            surface: theme.assistant_surface,
            accent: theme.text,
            align_right: false,
        },
        ConversationBlockKind::Tool => ConversationBlockVisual {
            glyph: "TOOL",
            surface: if is_error {
                theme.diagnostic_surface
            } else {
                theme.tool_surface
            },
            accent: if is_error {
                theme.danger
            } else {
                theme.muted_text
            },
            align_right: false,
        },
        ConversationBlockKind::Delegation => ConversationBlockVisual {
            glyph: "AGENT",
            surface: theme.summary_surface,
            accent: theme.accent,
            align_right: false,
        },
        ConversationBlockKind::CompactionSummary | ConversationBlockKind::BranchSummary => {
            ConversationBlockVisual {
                glyph: "SUMMARY",
                surface: theme.summary_surface,
                accent: theme.muted_text,
                align_right: false,
            }
        }
        ConversationBlockKind::Diagnostic => ConversationBlockVisual {
            glyph: "ISSUE",
            surface: theme.diagnostic_surface,
            accent: theme.danger,
            align_right: false,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DesktopThinkingSelection {
    Default,
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
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

fn composer_running_mode_for(
    modes: &HashMap<String, ComposerRunningMode>,
    session_id: &str,
) -> ComposerRunningMode {
    modes.get(session_id).copied().unwrap_or_default()
}

fn reconcile_composer_session_state(
    composer: &mut ComposerState,
    drafts: &mut HashMap<String, String>,
    previous_session_id: &str,
    current_session_id: &str,
) -> bool {
    if current_session_id == previous_session_id {
        return false;
    }
    if composer.draft().is_empty() {
        drafts.remove(previous_session_id);
    } else {
        if drafts.len() >= MAX_COMPOSER_SESSION_STATES
            && !drafts.contains_key(previous_session_id)
            && let Some(stale) = drafts.keys().next().cloned()
        {
            drafts.remove(&stale);
        }
        drafts.insert(previous_session_id.to_owned(), composer.draft().to_owned());
    }
    let draft = drafts.remove(current_session_id).unwrap_or_default();
    composer.edit(draft);
    true
}

impl DesktopThinkingSelection {
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

#[derive(Default)]
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
}

pub(super) struct NativeShell {
    runtime: Option<DesktopRuntimeCommandHandle>,
    runtime_updates: VecDeque<desktop::runtime::DesktopRuntimeUpdate>,
    projection: DesktopProjection,
    preferences: DesktopPreferences,
    preference_writer: Option<PreferenceWriter>,
    preference_notice: Option<String>,
    conversation_viewport: ConversationViewport,
    conversation_scroll: VirtualListScrollHandle,
    conversation_layout: ConversationRowLayoutState,
    conversation_live_layout: ConversationRowLayoutState,
    conversation_render_cache: ConversationRowRenderCache,
    conversation_render_rows: Vec<ConversationRowRenderData>,
    conversation_render_heights: Vec<f32>,
    conversation_row_sizes: Rc<Vec<gpui::Size<gpui::Pixels>>>,
    conversation_render_full_dirty: bool,
    conversation_render_live_dirty: bool,
    conversation_render_dirty_sequences: VecDeque<u64>,
    conversation_render_sequence_overflow: bool,
    conversation_render_width_bucket: Option<u32>,
    conversation_width_pending: Option<(u32, Instant)>,
    conversation_height_refresh_deadline: Option<Instant>,
    conversation_height_refresh_full: bool,
    conversation_expanded_details: HashSet<String>,
    inspector_telemetry_last_refresh: Option<Instant>,
    inspector_telemetry_refresh_deadline: Option<Instant>,
    conversation_pane: gpui::Entity<ConversationPane>,
    conversation_header: gpui::Entity<ConversationHeader>,
    sessions_pane: gpui::Entity<SessionsPane>,
    composer_pane: gpui::Entity<ComposerPane>,
    inspector_pane: gpui::Entity<InspectorPane>,
    inspector_section: InspectorSection,
    status_bar: gpui::Entity<StatusBar>,
    overlay_host: gpui::Entity<OverlayHost>,
    composer: ComposerState,
    composer_input_latency: InputRenderLatencyProbe,
    composer_input: gpui::Entity<InputState>,
    sessions_search_input: gpui::Entity<InputState>,
    composer_needs_sync: bool,
    composer_session_drafts: HashMap<String, String>,
    composer_running_modes: HashMap<String, ComposerRunningMode>,
    command_ledger: DesktopCommandLedger,
    focus: FocusState,
    sessions_focus: FocusHandle,
    conversation_focus: FocusHandle,
    context_focus: FocusHandle,
    status_focus: FocusHandle,
    authorization_focus: FocusHandle,
    command_palette_focus: FocusHandle,
    narrow_sessions_focus: FocusHandle,
    thinking_selection: DesktopThinkingSelection,
    file_review: DesktopFileReviewState,
    command_palette: DesktopCommandPalette,
    active_overlay: Option<DesktopOverlayKind>,
    narrow_sessions_open: bool,
    narrow_context_open: bool,
    session_catalog: Vec<desktop::runtime::DesktopSessionCatalogEntry>,
    omitted_sessions: usize,
    session_catalog_refresh_deadline: Option<Instant>,
    panel_resize: Option<PanelResizeState>,
    focus_input_modality: FocusInputModality,
    _subscriptions: Vec<Subscription>,
}

pub(super) struct NativeShellInit {
    pub(super) runtime: DesktopRuntimeBridge,
    pub(super) projection: DesktopProjection,
    pub(super) preferences: DesktopPreferences,
    pub(super) preference_writer: Option<PreferenceWriter>,
    pub(super) preference_notice: Option<String>,
    pub(super) initial_session_id: Option<String>,
}

impl NativeShell {
    pub(super) fn new(init: NativeShellInit, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let NativeShellInit {
            runtime,
            projection,
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
        let status_focus = cx.focus_handle().tab_stop(true).tab_index(5);
        let authorization_focus = cx.focus_handle().tab_stop(true).tab_index(3);
        let command_palette_focus = cx.focus_handle().tab_stop(true).tab_index(3);
        let narrow_sessions_focus = cx.focus_handle().tab_stop(true).tab_index(3);
        let composer_input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder("Describe the change you want to make…")
        });
        let sessions_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search sessions…"));
        let owner = cx.weak_entity();
        let conversation_pane = cx.new(|_| ConversationPane::new(owner.clone()));
        let conversation_header = cx.new(|_| ConversationHeader::new(owner.clone()));
        let sessions_pane = cx.new(|_| SessionsPane::new(owner.clone()));
        let composer_pane = cx.new(|_| ComposerPane::new(owner.clone()));
        let inspector_pane = cx.new(|_| InspectorPane::new(owner.clone()));
        let status_bar = cx.new(|_| StatusBar::new(owner.clone()));
        let overlay_host = cx.new(|_| OverlayHost::new(owner));

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
            cx.on_focus(&status_focus, window, |this, window, cx| {
                this.record_focus(FocusTarget::Status, window, cx);
            }),
            cx.subscribe_in(
                &composer_input,
                window,
                |this, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        let _span = tracing::trace_span!("desktop.input.change").entered();
                        this.composer_input_latency.mark_changed();
                        this.composer.edit(input.read(cx).value().to_string());
                        this.notify_composer_pane(cx);
                    }
                    InputEvent::Focus => {
                        this.record_focus(FocusTarget::Composer, window, cx);
                    }
                    InputEvent::PressEnter { secondary: true } => {
                        if !this.root_action_blocked_by_overlay(window, cx) {
                            this.submit_primary_composer(cx);
                        }
                    }
                    InputEvent::Blur => this.notify_composer_pane(cx),
                    InputEvent::PressEnter { secondary: false } => {}
                },
            ),
            cx.subscribe_in(
                &sessions_search_input,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.notify_sessions_pane(cx);
                    }
                },
            ),
            cx.subscribe_in(
                &conversation_pane,
                window,
                |this, pane, event: &ConversationPaneEvent, window, cx| match event {
                    ConversationPaneEvent::Select { block_id, durable } => {
                        this.record_focus(FocusTarget::Conversation, window, cx);
                        if *durable {
                            this.conversation_viewport
                                .select(block_id.clone(), this.projection.conversation());
                        } else {
                            this.conversation_viewport.select_live(block_id.clone());
                        }
                        pane.update(cx, |_, cx| cx.notify());
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
                    ConversationPaneEvent::ToggleDetails { block_id } => {
                        this.toggle_conversation_details(block_id, cx);
                    }
                    ConversationPaneEvent::FollowLatest => this.follow_latest(cx),
                },
            ),
            cx.subscribe_in(
                &conversation_header,
                window,
                |this, _, event: &ConversationHeaderEvent, window, cx| match event {
                    ConversationHeaderEvent::ToggleSessions => this.toggle_sessions(window, cx),
                    ConversationHeaderEvent::ToggleContext => this.toggle_context(window, cx),
                    ConversationHeaderEvent::Reload => this.reload_local_resources(cx),
                    ConversationHeaderEvent::CopySelected => {
                        this.copy_selected_conversation(cx);
                    }
                    ConversationHeaderEvent::Abort => this.abort_active_operation(cx),
                },
            ),
            cx.subscribe_in(
                &sessions_pane,
                window,
                |this, _, event: &SessionsPaneEvent, _, cx| match event {
                    SessionsPaneEvent::Create => this.create_session(cx),
                    SessionsPaneEvent::Refresh => this.request_session_catalog(cx),
                    SessionsPaneEvent::Open(session_id) => {
                        this.open_session(session_id.clone(), cx);
                    }
                },
            ),
            cx.subscribe_in(
                &composer_pane,
                window,
                |this, _, event: &ComposerPaneEvent, _, cx| match event {
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
                |this, _, event: &InspectorPaneEvent, _, cx| match event {
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
                &status_bar,
                window,
                |this, _, event: &StatusBarEvent, _, cx| match event {
                    StatusBarEvent::SelectNextModel => this.select_next_model(cx),
                    StatusBarEvent::SelectNextSessionProfile => {
                        this.select_next_session_profile(cx);
                    }
                    StatusBarEvent::CycleThinking => this.cycle_thinking_selection(cx),
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
                    OverlayHostEvent::CreateSession => this.create_session(cx),
                    OverlayHostEvent::RefreshSessions => this.request_session_catalog(cx),
                    OverlayHostEvent::OpenSession(session_id) => {
                        this.open_session(session_id.clone(), cx);
                    }
                    OverlayHostEvent::DecideAuthorization { identity, decision } => {
                        this.decide_tool_authorization(identity.clone(), decision.clone(), cx);
                    }
                },
            ),
            cx.observe_window_bounds(window, Self::window_bounds_changed),
        ];

        composer_input.focus_handle(cx).focus(window);
        cx.spawn(async move |this, cx| {
            let runtime_shutdown = runtime_shutdown;
            while let Some(updates) = runtime_events.next_update_batch().await {
                let Some(this) = this.upgrade() else {
                    break;
                };
                if this
                    .update(cx, |this, cx| {
                        this.runtime_updates.extend(updates);
                        this.poll_runtime(cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
            let _ = runtime_shutdown.shutdown(&mut runtime_events).await;
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |this, cx| this.request_session_catalog(cx));
        })
        .detach();

        Self {
            runtime: Some(runtime_commands),
            runtime_updates: VecDeque::new(),
            projection,
            preferences,
            preference_writer,
            preference_notice,
            conversation_viewport: ConversationViewport::new(8),
            conversation_scroll: VirtualListScrollHandle::new(),
            conversation_layout: ConversationRowLayoutState::default(),
            conversation_live_layout: ConversationRowLayoutState::default(),
            conversation_render_cache: ConversationRowRenderCache::default(),
            conversation_render_rows: Vec::new(),
            conversation_render_heights: Vec::new(),
            conversation_row_sizes: Rc::new(Vec::new()),
            conversation_render_full_dirty: true,
            conversation_render_live_dirty: true,
            conversation_render_dirty_sequences: VecDeque::new(),
            conversation_render_sequence_overflow: false,
            conversation_render_width_bucket: None,
            conversation_width_pending: None,
            conversation_height_refresh_deadline: None,
            conversation_height_refresh_full: false,
            conversation_expanded_details: HashSet::new(),
            inspector_telemetry_last_refresh: None,
            inspector_telemetry_refresh_deadline: None,
            conversation_pane,
            conversation_header,
            sessions_pane,
            composer_pane,
            inspector_pane,
            inspector_section: InspectorSection::default(),
            status_bar,
            overlay_host,
            composer: ComposerState::default(),
            composer_input_latency: InputRenderLatencyProbe::default(),
            composer_input,
            sessions_search_input,
            composer_needs_sync: false,
            composer_session_drafts: HashMap::new(),
            composer_running_modes: HashMap::new(),
            command_ledger,
            focus: FocusState::default(),
            sessions_focus,
            conversation_focus,
            context_focus,
            status_focus,
            authorization_focus,
            command_palette_focus,
            narrow_sessions_focus,
            thinking_selection: DesktopThinkingSelection::Default,
            file_review: DesktopFileReviewState::default(),
            command_palette: DesktopCommandPalette::default(),
            active_overlay: None,
            narrow_sessions_open: false,
            narrow_context_open: false,
            session_catalog: Vec::new(),
            omitted_sessions: 0,
            session_catalog_refresh_deadline: None,
            panel_resize: None,
            focus_input_modality: FocusInputModality::default(),
            _subscriptions: subscriptions,
        }
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
                }
                ResizablePanel::Context => {
                    self.preferences.context_panel_width = CONTEXT_PANEL_WIDTH;
                    self.notify_inspector_pane(cx);
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
                cx.notify();
            }
            ResizablePanel::Context if self.preferences.context_panel_width != width => {
                self.preferences.context_panel_width = width;
                self.notify_inspector_pane(cx);
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
        self.notify_status_bar(cx);
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
        if previous == FocusTarget::Status || target == FocusTarget::Status {
            self.notify_status_bar(cx);
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
            self.composer_input.focus_handle(cx).focus(window);
        }
        self.schedule_preferences();
        self.notify_inspector_pane(cx);
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
        let mut status_bar_dirty = false;
        let mut conversation_header_dirty = false;
        let mut overlay_host_dirty = false;
        let mut root_dirty = false;
        while applied < MAX_RUNTIME_UPDATES_PER_FRAME {
            let Some(update) = self.runtime_updates.pop_front() else {
                break;
            };
            if !matches!(
                &update,
                desktop::runtime::DesktopRuntimeUpdate::ProductEvent { .. }
            ) {
                status_bar_dirty = true;
                conversation_header_dirty = true;
                overlay_host_dirty = true;
                root_dirty = true;
            }
            let update = match update {
                desktop::runtime::DesktopRuntimeUpdate::FileReviewed { command_id, review } => {
                    let request =
                        CodingAgentFileReviewRequest::new(review.change.clone(), review.revision);
                    if self.command_ledger.complete(
                        command_id,
                        &DesktopCommandIntent::FileReview {
                            request: request.clone(),
                        },
                    ) {
                        self.file_review = DesktopFileReviewState::Ready(
                            DesktopFileReviewDocument::from_product(review),
                        );
                        self.preference_notice = Some("Changed-file review loaded.".into());
                        inspector_pane_dirty = true;
                    }
                    applied += 1;
                    continue;
                }
                desktop::runtime::DesktopRuntimeUpdate::ExternalEditorOpened {
                    command_id,
                    project_relative_path,
                } => {
                    if self.command_ledger.complete(
                        command_id,
                        &DesktopCommandIntent::ExternalEditor {
                            project_relative_path: project_relative_path.clone(),
                        },
                    ) {
                        self.preference_notice = Some(format!(
                            "Opened {} in the configured editor.",
                            truncate_label(&project_relative_path, 48)
                        ));
                        inspector_pane_dirty = true;
                    }
                    applied += 1;
                    continue;
                }
                desktop::runtime::DesktopRuntimeUpdate::SessionsListed {
                    command_id,
                    sessions,
                    omitted,
                } => {
                    if self
                        .command_ledger
                        .complete(command_id, &DesktopCommandIntent::ListSessions)
                    {
                        self.session_catalog = sessions;
                        self.omitted_sessions = omitted;
                        self.preference_notice = Some(if omitted == 0 {
                            format!("Loaded {} session(s).", self.session_catalog.len())
                        } else {
                            format!(
                                "Loaded {} session(s); {omitted} older session(s) omitted.",
                                self.session_catalog.len()
                            )
                        });
                        sessions_pane_dirty = true;
                        self.schedule_session_catalog_refresh(cx);
                    }
                    applied += 1;
                    continue;
                }
                update => update,
            };
            let composer_pane_state_before = self.composer_pane_state();
            let reload_completion = match &update {
                desktop::runtime::DesktopRuntimeUpdate::Reloaded {
                    command_id,
                    metadata,
                } if self
                    .command_ledger
                    .matches(*command_id, &DesktopCommandIntent::Reload) =>
                {
                    Some((
                        *command_id,
                        metadata.project.resources.skill_names.len(),
                        metadata.project.resources.prompt_template_names.len(),
                        metadata.project.profiles.len(),
                    ))
                }
                _ => None,
            };
            let selection_completion = match &update {
                desktop::runtime::DesktopRuntimeUpdate::SelectionChanged {
                    command_id,
                    selection,
                    ..
                } if self
                    .command_ledger
                    .matches(*command_id, &DesktopCommandIntent::Selection(*selection)) =>
                {
                    Some((*command_id, *selection))
                }
                _ => None,
            };
            let recovery_completion = match &update {
                desktop::runtime::DesktopRuntimeUpdate::RecoveryChanged {
                    command_id,
                    action,
                    recovery_id,
                    ..
                } if self.command_ledger.matches(
                    *command_id,
                    &DesktopCommandIntent::Recovery {
                        recovery_id: recovery_id.clone(),
                        action: *action,
                    },
                ) =>
                {
                    Some((*command_id, *action, recovery_id.clone()))
                }
                _ => None,
            };
            let resync_completion = match &update {
                desktop::runtime::DesktopRuntimeUpdate::Resynced { command_id, .. }
                    if self
                        .command_ledger
                        .matches(*command_id, &DesktopCommandIntent::Resync) =>
                {
                    Some(*command_id)
                }
                _ => None,
            };
            let session_completion = match &update {
                desktop::runtime::DesktopRuntimeUpdate::SessionChanged { command_id, .. } => self
                    .command_ledger
                    .intent(*command_id)
                    .filter(|intent| {
                        matches!(
                            intent,
                            DesktopCommandIntent::CreateSession
                                | DesktopCommandIntent::OpenSession { .. }
                        )
                    })
                    .cloned()
                    .map(|intent| (*command_id, intent)),
                _ => None,
            };
            match &update {
                desktop::runtime::DesktopRuntimeUpdate::PromptAccepted { command_id } => {
                    if self
                        .command_ledger
                        .complete(*command_id, &DesktopCommandIntent::Prompt)
                        && self.composer.accepted(*command_id).is_ok()
                    {
                        self.composer_needs_sync = true;
                        self.conversation_render_live_dirty = true;
                    }
                }
                desktop::runtime::DesktopRuntimeUpdate::PromptStarted { command_id, .. } => {
                    if self
                        .composer
                        .submitted()
                        .is_some_and(|submitted| submitted.command_id != *command_id)
                    {
                        self.preference_notice =
                            Some("Prompt start did not match the submitted command.".into());
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
                    self.preference_notice =
                        Some(format!("Abort accepted for {}.", receipt.operation_id));
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
                        self.preference_notice = Some(format!(
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
                    self.preference_notice =
                        Some(format!("Authorization decision accepted: {decision}."));
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
                    self.preference_notice = Some(safe_runtime_rejection_notice(
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
                    self.preference_notice = Some(format!(
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
                    self.preference_notice = Some(format!(
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
                        self.preference_notice = Some(notice);
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
                    self.preference_notice = Some(safe_runtime_rejection_notice(
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
                    self.preference_notice = Some(safe_runtime_rejection_notice(*command, code));
                    inspector_pane_dirty = true;
                }
                desktop::runtime::DesktopRuntimeUpdate::CommandRejected {
                    command_id,
                    command:
                        command @ (desktop::runtime::DesktopRuntimeCommandKind::Resync
                        | desktop::runtime::DesktopRuntimeCommandKind::CreateSession
                        | desktop::runtime::DesktopRuntimeCommandKind::OpenSession
                        | desktop::runtime::DesktopRuntimeCommandKind::ListSessions),
                    code,
                    ..
                } if self
                    .command_ledger
                    .complete_rejection(*command_id, *command)
                    .is_some() =>
                {
                    self.preference_notice = Some(safe_runtime_rejection_notice(*command, code));
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
                        self.file_review = DesktopFileReviewState::Failed {
                            request,
                            code: code.clone(),
                        };
                        self.preference_notice = Some(format!(
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
                    self.preference_notice = Some(format!(
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
                        self.preference_notice = Some(format!(
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
            let had_active_operation = self.projection.snapshot().active_operation.is_some();
            let previous_session_id = self.projection.snapshot().session.session_id.clone();
            let outcome = self.projection.apply(update);
            if outcome.is_replaced() {
                self.reconcile_composer_session(&previous_session_id);
            }
            if root_projection_dirty(outcome.is_replaced(), outcome.delta()) {
                root_dirty = true;
            }
            if outcome.is_replaced() || outcome.delta().is_some_and(|delta| delta.authorizations) {
                composer_pane_dirty = true;
            }
            if had_active_operation != self.projection.snapshot().active_operation.is_some() {
                sessions_pane_dirty = true;
            }
            let conversation_dirty = outcome
                .delta()
                .is_some_and(|delta| delta.conversation || delta.tools);
            if outcome.is_replaced()
                || outcome
                    .delta()
                    .is_some_and(inspector_projection_immediate_dirty)
            {
                inspector_pane_dirty = true;
            } else if outcome.delta().is_some_and(|delta| {
                delta
                    .context
                    .contains(desktop::projection::ContextDirtyFlags::USAGE)
            }) {
                inspector_telemetry_dirty = true;
            }
            if outcome.is_replaced() || outcome.delta().is_some_and(status_projection_dirty) {
                status_bar_dirty = true;
            }
            if outcome.is_replaced()
                || outcome
                    .delta()
                    .is_some_and(conversation_header_projection_dirty)
            {
                conversation_header_dirty = true;
            }
            if outcome.is_replaced() || outcome.delta().is_some_and(overlay_host_projection_dirty) {
                overlay_host_dirty = true;
            }
            if outcome.is_replaced() {
                sessions_pane_dirty = true;
                self.conversation_render_full_dirty = true;
                self.conversation_render_live_dirty = true;
                self.conversation_render_dirty_sequences.clear();
                self.conversation_render_sequence_overflow = false;
            } else if conversation_dirty {
                self.conversation_render_live_dirty = true;
                if self.conversation_render_sequence_overflow {
                    // A bounded tail reconcile is already required.
                } else if self.conversation_render_dirty_sequences.len()
                    == MAX_DIRTY_CONVERSATION_SEQUENCES
                {
                    self.conversation_render_dirty_sequences.clear();
                    self.conversation_render_sequence_overflow = true;
                } else {
                    self.conversation_render_dirty_sequences
                        .push_back(self.projection.cursor().last_event_sequence);
                }
            }
            let file_changes_dirty = outcome.delta().is_some_and(|delta| {
                delta
                    .context
                    .contains(desktop::projection::ContextDirtyFlags::CHANGES)
            });
            if let Some(command_id) = resync_completion
                && self
                    .command_ledger
                    .complete(command_id, &DesktopCommandIntent::Resync)
            {
                self.preference_notice = Some(if outcome.is_replaced() {
                    "Runtime state resynchronized.".into()
                } else {
                    "Resync response failed projection validation.".into()
                });
                if outcome.is_replaced() {
                    self.request_session_catalog(cx);
                }
            }
            if let Some((command_id, intent)) = session_completion
                && self.command_ledger.complete(command_id, &intent)
            {
                sessions_pane_dirty = true;
                self.preference_notice = Some(if outcome.is_replaced() {
                    match intent {
                        DesktopCommandIntent::CreateSession => "Created a new session.".into(),
                        DesktopCommandIntent::OpenSession { .. } => {
                            "Opened the requested session.".into()
                        }
                        _ => unreachable!("session completion was filtered by typed intent"),
                    }
                } else {
                    "Session response failed projection validation; resync is required.".into()
                });
                if outcome.is_replaced() {
                    self.request_session_catalog(cx);
                }
            }
            if let Some((command_id, skill_count, prompt_count, profile_count)) = reload_completion
                && self
                    .command_ledger
                    .complete(command_id, &DesktopCommandIntent::Reload)
            {
                self.preference_notice = Some(if outcome.is_replaced() {
                    format!(
                        "Reloaded {skill_count} skills, {prompt_count} prompts, and \
                         {profile_count} profiles."
                    )
                } else {
                    "Reload response failed projection validation; resync is required.".into()
                });
            }
            if let Some((command_id, selection)) = selection_completion
                && self
                    .command_ledger
                    .complete(command_id, &DesktopCommandIntent::Selection(selection))
            {
                self.preference_notice = Some(if outcome.is_replaced() {
                    match selection {
                        DesktopRuntimeSelectionKind::Model => format!(
                            "Future prompts will use model {}.",
                            truncate_label(&self.projection.project().selected_model_id, 28)
                        ),
                        DesktopRuntimeSelectionKind::SessionProfile => format!(
                            "Session profile changed to {}.",
                            truncate_label(
                                self.projection
                                    .snapshot()
                                    .session
                                    .default_agent_profile_id
                                    .as_str(),
                                28
                            )
                        ),
                    }
                } else {
                    "Selection response failed projection validation; resync is required.".into()
                });
            }
            if let Some((command_id, action, recovery_id)) = recovery_completion
                && self.command_ledger.complete(
                    command_id,
                    &DesktopCommandIntent::Recovery {
                        recovery_id: recovery_id.clone(),
                        action,
                    },
                )
            {
                self.preference_notice = Some(if outcome.is_replaced() {
                    format!(
                        "Recovery {} accepted for {}.",
                        recovery_action_label(action),
                        truncate_label(&recovery_id, 28)
                    )
                } else {
                    "Recovery changed, but its snapshot failed projection validation; resync \
                         is required."
                        .into()
                });
            }
            if outcome.is_replaced() {
                if self.projection.snapshot().active_operation.is_none()
                    && self.composer.submitted().is_some()
                {
                    if let Some((live_id, durable_id)) = self
                        .composer
                        .reconcile_completed_submission(self.projection.conversation())
                    {
                        self.conversation_viewport
                            .reconcile_live_selection(&live_id, &durable_id);
                    }
                    self.composer_needs_sync = true;
                }
                let visible_blocks = self.visible_conversation_count();
                let content_revision = self.projection.cursor().last_event_sequence;
                self.conversation_viewport.reconcile_hydration(
                    self.projection.conversation(),
                    visible_blocks,
                    content_revision,
                );
                if self.conversation_viewport.follow_latest() && visible_blocks > 0 {
                    self.conversation_scroll
                        .scroll_to_item(visible_blocks - 1, ScrollStrategy::Bottom);
                }
            } else if conversation_dirty {
                let visible_blocks = self.visible_conversation_count();
                let content_revision = self.projection.cursor().last_event_sequence;
                self.conversation_viewport
                    .on_content_changed(visible_blocks, content_revision);
                if self.conversation_viewport.follow_latest() && visible_blocks > 0 {
                    self.conversation_scroll
                        .scroll_to_item(visible_blocks - 1, ScrollStrategy::Bottom);
                }
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
                && !self
                    .projection
                    .snapshot()
                    .pending_authorizations
                    .iter()
                    .any(|request| request.authorization_id == authorization_id)
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
                status_bar_dirty = true;
                conversation_header_dirty = true;
                overlay_host_dirty = true;
            }
            applied += 1;
        }
        if let Some(writer) = &self.preference_writer
            && let Some(error) = writer.take_error()
        {
            self.preference_notice = Some(error);
            status_bar_dirty = true;
        }
        let conversation_needs_refresh =
            self.conversation_render_full_dirty || self.conversation_render_live_dirty;
        if conversation_needs_refresh && !self.refresh_conversation_rows_at_current_width(cx) {
            root_dirty = true;
        }
        if root_dirty {
            cx.notify();
        }
        if sessions_pane_dirty {
            self.notify_sessions_pane(cx);
        }
        if composer_pane_dirty {
            self.notify_composer_pane(cx);
        }
        if inspector_pane_dirty {
            self.notify_inspector_pane(cx);
        } else if inspector_telemetry_dirty {
            self.schedule_inspector_telemetry_refresh(cx);
        }
        if status_bar_dirty {
            self.notify_status_bar(cx);
        }
        if conversation_header_dirty {
            self.notify_conversation_header(cx);
        }
        if overlay_host_dirty {
            self.notify_overlay_host(cx);
        }
        !matches!(
            self.projection.lifecycle(),
            DesktopProjectionLifecycle::Stopped
        )
    }

    fn notify_sessions_pane(&self, cx: &mut Context<Self>) {
        self.sessions_pane.update(cx, |_, cx| cx.notify());
        self.notify_status_bar(cx);
        self.notify_overlay_host(cx);
    }

    fn composer_pane_state(&self) -> (bool, bool, bool, bool) {
        (
            matches!(self.composer.admission(), ComposerAdmission::Pending { .. }),
            self.projection.snapshot().active_operation.is_some(),
            self.composer.submitted().is_some(),
            self.composer.rejection().is_some(),
        )
    }

    fn active_composer_running_mode(&self) -> ComposerRunningMode {
        composer_running_mode_for(
            &self.composer_running_modes,
            self.projection.snapshot().session.session_id.as_str(),
        )
    }

    fn set_active_composer_running_mode(
        &mut self,
        mode: ComposerRunningMode,
        cx: &mut Context<Self>,
    ) {
        let session_id = self.projection.snapshot().session.session_id.clone();
        if self.composer_running_modes.len() >= MAX_COMPOSER_SESSION_STATES
            && !self.composer_running_modes.contains_key(&session_id)
            && let Some(stale) = self.composer_running_modes.keys().next().cloned()
        {
            self.composer_running_modes.remove(&stale);
        }
        self.composer_running_modes.insert(session_id, mode);
        self.notify_composer_pane(cx);
    }

    fn reconcile_composer_session(&mut self, previous_session_id: &str) {
        let current_session_id = self.projection.snapshot().session.session_id.clone();
        if reconcile_composer_session_state(
            &mut self.composer,
            &mut self.composer_session_drafts,
            previous_session_id,
            &current_session_id,
        ) {
            self.composer_needs_sync = true;
        }
    }

    fn notify_composer_pane(&self, cx: &mut Context<Self>) {
        self.composer_pane.update(cx, |_, cx| cx.notify());
    }

    fn notify_inspector_pane(&mut self, cx: &mut Context<Self>) {
        self.inspector_telemetry_last_refresh = Some(Instant::now());
        self.inspector_telemetry_refresh_deadline = None;
        self.inspector_pane.update(cx, |_, cx| cx.notify());
        self.notify_status_bar(cx);
    }

    fn schedule_inspector_telemetry_refresh(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let delay = inspector_telemetry_refresh_delay(self.inspector_telemetry_last_refresh, now);
        if delay.is_zero() {
            self.inspector_telemetry_last_refresh = Some(now);
            self.inspector_telemetry_refresh_deadline = None;
            self.inspector_pane.update(cx, |_, cx| cx.notify());
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
                    this.inspector_pane.update(cx, |_, cx| cx.notify());
                }
            });
        })
        .detach();
    }

    fn notify_status_bar(&self, cx: &mut Context<Self>) {
        self.status_bar.update(cx, |_, cx| cx.notify());
    }

    fn notify_conversation_header(&self, cx: &mut Context<Self>) {
        self.conversation_header.update(cx, |_, cx| cx.notify());
    }

    fn notify_overlay_host(&self, cx: &mut Context<Self>) {
        self.overlay_host.update(cx, |_, cx| cx.notify());
    }

    fn schedule_preferences(&mut self) {
        if let Some(writer) = &self.preference_writer {
            writer.schedule(self.preferences.clone());
        }
    }

    fn reconcile_file_review(&mut self) {
        let request = match &self.file_review {
            DesktopFileReviewState::Empty => return,
            DesktopFileReviewState::Loading(request)
            | DesktopFileReviewState::Failed { request, .. } => request.clone(),
            DesktopFileReviewState::Ready(document) => document.request.clone(),
        };
        let remains_current = self
            .projection
            .snapshot()
            .context
            .changes
            .iter()
            .any(|change| {
                change.operation_id == request.change.operation_id
                    && change.tool_call_id == request.change.tool_call_id
                    && change.path == request.change.path
                    && change.updated_sequence == request.revision.value()
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
            self.file_review = DesktopFileReviewState::Empty;
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
            self.composer_input.focus_handle(cx).focus(window);
        }
        self.schedule_preferences();
        self.notify_inspector_pane(cx);
        cx.notify();
    }

    fn visible_conversation_count(&self) -> usize {
        self.projection.conversation().blocks().len()
            + usize::from(self.composer.submitted().is_some())
            + self.projection.messages().len()
            + self.projection.tools().len()
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
            self.composer_input.focus_handle(cx).focus(window);
        }
        self.schedule_preferences();
        cx.notify();
    }

    fn reserve_command(&mut self, intent: DesktopCommandIntent) -> Option<u64> {
        match self.command_ledger.reserve(intent) {
            Ok(command_id) => Some(command_id),
            Err(error) => {
                self.preference_notice = Some(error.to_string());
                None
            }
        }
    }

    fn request_resync_if_needed(&mut self) {
        if self.projection.lifecycle() != DesktopProjectionLifecycle::NeedsResync
            || self.command_ledger.contains(&DesktopCommandIntent::Resync)
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
            self.preference_notice = Some(message);
        }
    }

    fn create_session(&mut self, cx: &mut Context<Self>) {
        if self.projection.snapshot().active_operation.is_some()
            || self.composer.submitted().is_some()
            || self.command_ledger.contains_where(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
                )
            })
        {
            return;
        }
        let intent = DesktopCommandIntent::CreateSession;
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_status_bar(cx);
            cx.notify();
            return;
        };
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_create_session(command_id)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => self.preference_notice = Some("Creating a new session…".into()),
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.preference_notice = Some(message);
            }
        }
        self.notify_sessions_pane(cx);
        cx.notify();
    }

    fn request_session_catalog(&mut self, cx: &mut Context<Self>) {
        if self
            .command_ledger
            .contains(&DesktopCommandIntent::ListSessions)
        {
            return;
        }
        let intent = DesktopCommandIntent::ListSessions;
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_status_bar(cx);
            cx.notify();
            return;
        };
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_list_sessions(command_id)
                    .map_err(|error| error.to_string())
            },
        );
        if let Err(message) = admission {
            self.command_ledger.complete(command_id, &intent);
            self.preference_notice = Some(message);
            self.schedule_session_catalog_refresh(cx);
        }
        self.notify_sessions_pane(cx);
        cx.notify();
    }

    fn schedule_session_catalog_refresh(&mut self, cx: &mut Context<Self>) {
        let deadline = Instant::now() + SESSION_CATALOG_REFRESH_INTERVAL;
        if self
            .session_catalog_refresh_deadline
            .is_some_and(|scheduled| scheduled <= deadline)
        {
            return;
        }
        self.session_catalog_refresh_deadline = Some(deadline);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(SESSION_CATALOG_REFRESH_INTERVAL)
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.session_catalog_refresh_deadline == Some(deadline) {
                    this.session_catalog_refresh_deadline = None;
                    if this.projection.snapshot().active_operation.is_none() {
                        this.request_session_catalog(cx);
                    } else {
                        this.schedule_session_catalog_refresh(cx);
                    }
                }
            });
        })
        .detach();
    }

    fn open_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        if self.projection.snapshot().active_operation.is_some()
            || self.composer.submitted().is_some()
            || self.command_ledger.contains_where(|intent| {
                matches!(
                    intent,
                    DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
                )
            })
        {
            self.preference_notice =
                Some("Session switching is available only while the runtime is idle.".into());
            cx.notify();
            return;
        }
        if session_id == self.projection.snapshot().session.session_id {
            self.preference_notice = Some("The requested session is already active.".into());
            cx.notify();
            return;
        }
        let intent = DesktopCommandIntent::OpenSession {
            session_id: session_id.clone(),
        };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            cx.notify();
            return;
        };
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_open_session(command_id, &session_id)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.preference_notice = Some(format!(
                    "Opening session {}…",
                    truncate_label(&session_id, 32)
                ));
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.preference_notice = Some(message);
            }
        }
        self.notify_sessions_pane(cx);
        cx.notify();
    }

    fn switch_next_session(&mut self, cx: &mut Context<Self>) {
        if self.session_catalog.is_empty() {
            self.preference_notice = Some("Loading the session catalog…".into());
            self.request_session_catalog(cx);
            return;
        }
        let active = self.projection.snapshot().session.session_id.as_str();
        let current = self
            .session_catalog
            .iter()
            .position(|session| session.session_id == active);
        let next = current.map_or(0, |index| (index + 1) % self.session_catalog.len());
        let session_id = self.session_catalog[next].session_id.clone();
        if session_id == active {
            self.preference_notice = Some("No other project session is available.".into());
            self.notify_status_bar(cx);
            cx.notify();
            return;
        }
        self.open_session(session_id, cx);
    }

    fn submit_composer(&mut self, cx: &mut Context<Self>) {
        let intent = DesktopCommandIntent::Prompt;
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_status_bar(cx);
            cx.notify();
            return;
        };
        let payload = match self
            .composer
            .begin_submit(command_id, ComposerSubmissionKind::Prompt)
        {
            Ok(payload) => payload.to_owned(),
            Err(error) => {
                self.command_ledger.complete(command_id, &intent);
                self.preference_notice = Some(error.to_string());
                self.notify_composer_pane(cx);
                self.notify_status_bar(cx);
                cx.notify();
                return;
            }
        };
        let thinking_level = self.thinking_selection.explicit();
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_submit_prompt(command_id, &payload, thinking_level)
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
        if self.projection.snapshot().active_operation.is_some() {
            self.submit_active_control(self.active_composer_running_mode().submission_kind(), cx);
        } else {
            self.submit_composer(cx);
        }
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
        if kind == ComposerSubmissionKind::Prompt {
            self.preference_notice =
                Some("Prompt submissions must use the idle composer action.".into());
            self.notify_status_bar(cx);
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
            self.notify_status_bar(cx);
            cx.notify();
            return;
        };
        let payload = match self.composer.begin_submit(command_id, kind) {
            Ok(payload) => payload.to_owned(),
            Err(error) => {
                self.command_ledger.complete(command_id, &intent);
                self.preference_notice = Some(error.to_string());
                self.notify_composer_pane(cx);
                self.notify_status_bar(cx);
                cx.notify();
                return;
            }
        };
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                let result = match kind {
                    ComposerSubmissionKind::Steer => runtime.try_steer(command_id, &payload),
                    ComposerSubmissionKind::FollowUp => runtime.try_follow_up(command_id, &payload),
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
        let Some(operation_id) = self.projection.snapshot().active_operation.clone() else {
            self.preference_notice = Some("No active operation is available to abort.".into());
            self.notify_status_bar(cx);
            cx.notify();
            return;
        };
        let intent = DesktopCommandIntent::Abort { operation_id };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_status_bar(cx);
            cx.notify();
            return;
        };
        let admission = self.runtime.as_ref().map_or_else(
            || Err("desktop runtime is stopped".to_owned()),
            |runtime| {
                runtime
                    .try_abort(command_id)
                    .map_err(|error| error.to_string())
            },
        );
        match admission {
            Ok(()) => {
                self.preference_notice = Some("Abort requested…".into());
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.preference_notice = Some(message);
            }
        }
        self.notify_status_bar(cx);
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn reload_local_resources(&mut self, cx: &mut Context<Self>) {
        let intent = DesktopCommandIntent::Reload;
        if self.command_ledger.contains(&intent) {
            return;
        }
        if self.projection.snapshot().active_operation.is_some()
            || self.composer.submitted().is_some()
        {
            self.preference_notice =
                Some("Reload is available only while the runtime is idle.".into());
            self.notify_status_bar(cx);
            cx.notify();
            return;
        }
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_status_bar(cx);
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
                self.preference_notice = Some("Reloading local resources…".into());
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.preference_notice = Some(message);
            }
        }
        self.notify_status_bar(cx);
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
        if self.projection.snapshot().active_operation.is_some()
            || self.composer.submitted().is_some()
        {
            self.preference_notice =
                Some("Recovery actions are available only while the runtime is idle.".into());
            self.notify_status_bar(cx);
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
                self.preference_notice = Some(format!(
                    "Submitting recovery {}…",
                    recovery_action_label(action)
                ));
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.preference_notice = Some(message);
            }
        }
        self.notify_inspector_pane(cx);
        cx.notify();
    }

    fn next_model_id(&self) -> Option<String> {
        let project = self.projection.project();
        let candidates = project
            .models
            .iter()
            .filter(|model| model.supports_text && (model.configured || model.selected))
            .collect::<Vec<_>>();
        if candidates.len() < 2 {
            return None;
        }
        let current = candidates
            .iter()
            .position(|model| model.id == project.selected_model_id)
            .unwrap_or(0);
        Some(candidates[(current + 1) % candidates.len()].id.clone())
    }

    fn next_session_profile_id(&self) -> Option<String> {
        let profiles = &self.projection.project().profiles;
        if profiles.len() < 2 {
            return None;
        }
        let current_profile = self
            .projection
            .snapshot()
            .session
            .default_agent_profile_id
            .as_str();
        let next = profiles
            .iter()
            .position(|profile| profile.id.as_str() == current_profile)
            .map_or(0, |current| (current + 1) % profiles.len());
        Some(profiles[next].id.as_str().to_owned())
    }

    fn submit_selection(
        &mut self,
        selection: DesktopRuntimeSelectionKind,
        id: String,
        cx: &mut Context<Self>,
    ) {
        if self
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Selection(_)))
        {
            return;
        }
        if self.projection.snapshot().active_operation.is_some()
            || self.composer.submitted().is_some()
        {
            self.preference_notice =
                Some("Model and profile selection is available only while idle.".into());
            self.notify_status_bar(cx);
            cx.notify();
            return;
        }
        let intent = DesktopCommandIntent::Selection(selection);
        let Some(command_id) = self.reserve_command(intent.clone()) else {
            self.notify_status_bar(cx);
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
                self.preference_notice = Some("Applying selection…".into());
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.preference_notice = Some(message);
            }
        }
        self.notify_status_bar(cx);
        self.notify_conversation_header(cx);
        cx.notify();
    }

    fn select_next_model(&mut self, cx: &mut Context<Self>) {
        let Some(model_id) = self.next_model_id() else {
            self.preference_notice = Some("No other configured text model is available.".into());
            self.notify_status_bar(cx);
            cx.notify();
            return;
        };
        self.submit_selection(DesktopRuntimeSelectionKind::Model, model_id, cx);
    }

    fn select_next_session_profile(&mut self, cx: &mut Context<Self>) {
        let Some(profile_id) = self.next_session_profile_id() else {
            self.preference_notice = Some("No other session profile is available.".into());
            self.notify_status_bar(cx);
            cx.notify();
            return;
        };
        self.submit_selection(DesktopRuntimeSelectionKind::SessionProfile, profile_id, cx);
    }

    fn cycle_thinking_selection(&mut self, cx: &mut Context<Self>) {
        self.thinking_selection = self.thinking_selection.next();
        let label = self.thinking_selection.label(
            self.projection
                .project()
                .settings
                .default_thinking_level
                .as_deref(),
        );
        self.preference_notice = Some(format!("Future prompts will use thinking {label}."));
        self.notify_status_bar(cx);
        cx.notify();
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
            self.notify_status_bar(cx);
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
                self.preference_notice = Some("Authorization decision pending…".into());
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.preference_notice = Some(message);
            }
        }
        self.notify_status_bar(cx);
        self.notify_overlay_host(cx);
        cx.notify();
    }

    fn copy_selected_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self
            .conversation_viewport
            .copy_selected(self.projection.conversation())
        else {
            self.preference_notice =
                Some("Select a committed conversation block before copying.".into());
            self.notify_status_bar(cx);
            cx.notify();
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.preference_notice = Some("Selected conversation block copied.".into());
        self.notify_status_bar(cx);
        cx.notify();
    }

    fn copy_conversation_row(&mut self, block_id: &str, cx: &mut Context<Self>) {
        let text = self
            .projection
            .conversation()
            .block(block_id)
            .map(desktop::conversation::ConversationBlock::copy_text)
            .or_else(|| {
                self.conversation_render_rows
                    .iter()
                    .find(|row| row.item_key.row_id() == block_id)
                    .map(|row| conversation_copy_text(&row.text, &row.detail))
            });
        let Some(text) = text else {
            self.preference_notice = Some("Message is no longer available to copy.".into());
            self.notify_status_bar(cx);
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.preference_notice = Some("Conversation message copied.".into());
        self.notify_status_bar(cx);
    }

    fn toggle_conversation_details(&mut self, block_id: &str, cx: &mut Context<Self>) {
        if !self.conversation_expanded_details.remove(block_id) {
            if self.conversation_expanded_details.len() >= MAX_EXPANDED_CONVERSATION_DETAILS {
                self.conversation_expanded_details.clear();
            }
            self.conversation_expanded_details
                .insert(block_id.to_owned());
        }
        self.conversation_render_full_dirty = true;
        if !self.refresh_conversation_rows_at_current_width(cx) {
            cx.notify();
        }
    }

    fn select_adjacent_conversation(&mut self, reverse: bool, cx: &mut Context<Self>) {
        let row_count = self.conversation_render_rows.len();
        if row_count == 0 {
            self.preference_notice = Some("The conversation is empty.".into());
            self.notify_status_bar(cx);
            return;
        }
        let current = self.conversation_viewport.selected_block_id();
        let current_index = current.and_then(|selected| {
            self.conversation_render_rows
                .iter()
                .position(|row| row.item_key.row_id() == selected)
        });
        let next_index = adjacent_conversation_index(row_count, current_index, reverse)
            .expect("non-empty conversation has an adjacent selection");
        let row = &self.conversation_render_rows[next_index];
        if row.durable {
            self.conversation_viewport.select(
                row.item_key.row_id().to_owned(),
                self.projection.conversation(),
            );
        } else {
            self.conversation_viewport
                .select_live(row.item_key.row_id().to_owned());
        }
        self.conversation_scroll.scroll_to_item(
            next_index,
            if reverse {
                ScrollStrategy::Top
            } else {
                ScrollStrategy::Bottom
            },
        );
        self.conversation_pane.update(cx, |_, cx| cx.notify());
        self.notify_conversation_header(cx);
    }

    fn copy_keyboard_selected_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(block_id) = self
            .conversation_viewport
            .selected_block_id()
            .map(str::to_owned)
        else {
            self.preference_notice = Some("Select a conversation message before copying.".into());
            self.notify_status_bar(cx);
            return;
        };
        self.copy_conversation_row(&block_id, cx);
    }

    fn toggle_keyboard_selected_conversation_details(&mut self, cx: &mut Context<Self>) {
        let Some(block_id) = self
            .conversation_viewport
            .selected_block_id()
            .map(str::to_owned)
        else {
            return;
        };
        let has_details = self
            .conversation_render_rows
            .iter()
            .find(|row| row.item_key.row_id() == block_id)
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
            self.preference_notice = Some("Another file review is already pending.".into());
            self.notify_status_bar(cx);
            cx.notify();
            return;
        }
        let command_id = match self.command_ledger.reserve(intent.clone()) {
            Ok(command_id) => command_id,
            Err(error) => {
                self.preference_notice = Some(error.to_string());
                self.notify_status_bar(cx);
                cx.notify();
                return;
            }
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
                self.file_review = DesktopFileReviewState::Loading(request);
                self.preference_notice = Some("Loading changed-file review…".into());
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.preference_notice = Some(message);
            }
        }
        self.notify_inspector_pane(cx);
        cx.notify();
    }

    fn copy_review_path(&mut self, cx: &mut Context<Self>) {
        let DesktopFileReviewState::Ready(document) = &self.file_review else {
            self.preference_notice =
                Some("Load a changed-file review before copying its path.".into());
            self.notify_status_bar(cx);
            cx.notify();
            return;
        };
        let export = document.path_clipboard_export();
        cx.write_to_clipboard(ClipboardItem::new_string(export.text));
        self.preference_notice = Some(if export.truncated {
            "Bounded changed-file path copied (truncated).".into()
        } else {
            "Changed-file path copied.".into()
        });
        self.notify_status_bar(cx);
        cx.notify();
    }

    fn copy_file_review(&mut self, cx: &mut Context<Self>) {
        let DesktopFileReviewState::Ready(document) = &self.file_review else {
            self.preference_notice = Some("Load a changed-file review before copying it.".into());
            self.notify_status_bar(cx);
            cx.notify();
            return;
        };
        let export = document.clipboard_export();
        cx.write_to_clipboard(ClipboardItem::new_string(export.text));
        self.preference_notice = Some(if export.truncated {
            "Bounded file review copied (truncated at the clipboard limit).".into()
        } else {
            "File review copied.".into()
        });
        self.notify_status_bar(cx);
        cx.notify();
    }

    fn open_review_in_external_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.preferences.external_editor.clone() else {
            self.preference_notice = Some(
                "Configure desktop.external_editor with a program and literal argv first.".into(),
            );
            self.notify_status_bar(cx);
            cx.notify();
            return;
        };
        let DesktopFileReviewState::Ready(document) = &self.file_review else {
            self.preference_notice = Some("Load a changed-file review before opening it.".into());
            self.notify_status_bar(cx);
            cx.notify();
            return;
        };
        let Some(target) = document.external_editor_target.clone() else {
            self.preference_notice = Some("This review has no external-editor target.".into());
            self.notify_status_bar(cx);
            cx.notify();
            return;
        };
        let project_relative_path = target.project_relative_path().to_owned();
        let intent = DesktopCommandIntent::ExternalEditor {
            project_relative_path: project_relative_path.clone(),
        };
        let command_id = match self.command_ledger.reserve(intent.clone()) {
            Ok(command_id) => command_id,
            Err(error) => {
                self.preference_notice = Some(error.to_string());
                self.notify_status_bar(cx);
                cx.notify();
                return;
            }
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
                self.preference_notice = Some(format!(
                    "Validating {} before editor launch…",
                    truncate_label(&project_relative_path, 48)
                ));
            }
            Err(message) => {
                self.command_ledger.complete(command_id, &intent);
                self.preference_notice = Some(message);
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
        if self.active_overlay.is_none() {
            self.focus.open_overlay();
        }
        self.active_overlay = Some(overlay);
        match overlay {
            DesktopOverlayKind::Authorization => self.authorization_focus.focus(window),
            DesktopOverlayKind::CommandPalette => self.command_palette_focus.focus(window),
            DesktopOverlayKind::NarrowSessions => self.narrow_sessions_focus.focus(window),
            DesktopOverlayKind::NarrowContext => self.context_focus.focus(window),
        }
        self.notify_overlay_host(cx);
        cx.notify();
    }

    fn dismiss_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_overlay = None;
        self.focus.close_overlay(self.layout(window));
        self.focus_active_target(window, cx);
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
            self.preference_notice = Some(format!(
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
        self.preference_notice = Some(
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
                    "Use the context surface or close it before workspace shortcuts."
                }
            }
            .into(),
        );
        self.focus_active_target(window, cx);
        cx.notify();
        true
    }

    fn follow_latest(&mut self, cx: &mut Context<Self>) {
        let block_count = self.visible_conversation_count();
        self.conversation_viewport.resume_latest(block_count);
        if block_count > 0 {
            self.conversation_scroll
                .scroll_to_item(block_count - 1, ScrollStrategy::Bottom);
        }
        cx.notify();
    }

    fn reconcile_conversation_scroll(&mut self, cx: &mut Context<Self>) {
        let offset_y = f32::from(self.conversation_scroll.offset().y);
        let max_offset_y = f32::from(self.conversation_scroll.max_offset().height);
        let distance_to_bottom = conversation_distance_to_bottom(offset_y, max_offset_y);
        if self
            .conversation_viewport
            .reconcile_scroll_distance(distance_to_bottom)
        {
            cx.notify();
        }
    }

    fn review_next_file(&mut self, cx: &mut Context<Self>) {
        let changes = &self.projection.snapshot().context.changes;
        if changes.is_empty() {
            self.preference_notice = Some("No changed file is available for review.".into());
            cx.notify();
            return;
        }
        let current = match &self.file_review {
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
            .recoveries()
            .iter()
            .find(|recovery| {
                recovery.status == DesktopRecoveryStatus::Pending && recovery.authoritative
            })
            .and_then(|recovery| recovery.identity.clone());
        let Some(identity) = identity else {
            self.preference_notice = Some("No authoritative pending recovery is available.".into());
            self.notify_status_bar(cx);
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
            DesktopPaletteCommand::ToggleContext => self.toggle_context(window, cx),
            DesktopPaletteCommand::FocusSessions => {
                self.focus_target(FocusTarget::Sessions, window, cx);
            }
            DesktopPaletteCommand::FocusConversation => {
                self.focus_target(FocusTarget::Conversation, window, cx);
            }
            DesktopPaletteCommand::FocusComposer => {
                self.focus_target(FocusTarget::Composer, window, cx);
            }
            DesktopPaletteCommand::FocusContext => {
                self.focus_target(FocusTarget::Context, window, cx);
            }
            DesktopPaletteCommand::SubmitPrompt => {
                if self.projection.snapshot().active_operation.is_some() {
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
            DesktopPaletteCommand::SelectNextModel => self.select_next_model(cx),
            DesktopPaletteCommand::SelectNextProfile => self.select_next_session_profile(cx),
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
                self.preference_notice = Some(if self.preferences.reduced_motion {
                    "Reduced motion enabled; desktop transitions remain static.".into()
                } else {
                    "Reduced motion disabled; idle presentation remains static.".into()
                });
                self.notify_status_bar(cx);
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
        if !self.projection.snapshot().pending_authorizations.is_empty() {
            self.preference_notice = Some("Resolve authorization before opening commands.".into());
            self.authorization_focus.focus(window);
            self.notify_status_bar(cx);
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
                self.preference_notice =
                    Some("Authorization requires Deny, Allow once, or Allow for operation.".into());
                self.authorization_focus.focus(window);
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
            None if !matches!(self.file_review, DesktopFileReviewState::Empty) => {
                self.file_review = DesktopFileReviewState::Empty;
                self.preference_notice = Some("Closed the changed-file review.".into());
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

    fn on_toggle_context_panel(
        &mut self,
        _: &ToggleContextPanel,
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
            .snapshot()
            .pending_authorizations
            .first()
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

    fn rebuild_conversation_render_rows(&mut self, panel_width: u32) {
        let conversation = self.projection.conversation();
        let session_id = conversation.session_id.as_str();
        let expected_count = conversation.blocks().len()
            + usize::from(self.composer.submitted().is_some())
            + self.projection.messages().len()
            + self.projection.tools().len();
        let mut rows = Vec::with_capacity(expected_count);
        self.conversation_render_cache.begin_frame();

        for block in conversation.blocks() {
            let promote_detail = block.kind != ConversationBlockKind::Assistant
                && block.text.is_empty()
                && !block.detail.is_empty();
            rows.push(self.conversation_render_cache.resolve(
                ConversationRowRenderSource {
                    item_key: ConversationItemKey::new(
                        session_id,
                        ConversationItemKind::Durable(block.kind),
                        &block.id,
                    ),
                    source_revision: block.source_revision,
                    title: Cow::Borrowed(&block.title),
                    text: if promote_detail {
                        &block.detail
                    } else {
                        &block.text
                    },
                    detail: if promote_detail { "" } else { &block.detail },
                    kind: block.kind,
                    done: block.done,
                    is_error: block.is_error,
                    image_count: block.image_count,
                    truncated: block.truncated,
                    durable: true,
                },
                panel_width,
            ));
        }

        if let Some(submitted) = self.composer.submitted() {
            let row_id = submitted.block_id();
            rows.push(self.conversation_render_cache.resolve(
                ConversationRowRenderSource {
                    item_key: ConversationItemKey::new(
                        session_id,
                        ConversationItemKind::Submitted,
                        &row_id,
                    ),
                    source_revision: submitted.command_id,
                    title: Cow::Borrowed("You · submitted"),
                    text: &submitted.payload,
                    detail: "",
                    kind: ConversationBlockKind::User,
                    done: false,
                    is_error: false,
                    image_count: 0,
                    truncated: false,
                    durable: false,
                },
                panel_width,
            ));
        }

        for message in self.projection.messages() {
            let row_id = message_conversation_block_id(message);
            rows.push(self.conversation_render_cache.resolve(
                ConversationRowRenderSource {
                    item_key: ConversationItemKey::new(
                        session_id,
                        ConversationItemKind::LiveMessage,
                        &row_id,
                    ),
                    source_revision: message.updated_sequence,
                    title: Cow::Borrowed("Assistant · live"),
                    text: &message.text,
                    detail: &message.thinking,
                    kind: ConversationBlockKind::Assistant,
                    done: matches!(
                        message.status,
                        desktop::projection::DesktopMessageStatus::Completed
                    ),
                    is_error: false,
                    image_count: 0,
                    truncated: message.truncated,
                    durable: false,
                },
                panel_width,
            ));
        }

        for tool in self.projection.tools() {
            let row_id = tool_conversation_block_id(tool);
            rows.push(self.conversation_render_cache.resolve(
                ConversationRowRenderSource {
                    item_key: ConversationItemKey::new(
                        session_id,
                        ConversationItemKind::LiveTool,
                        &row_id,
                    ),
                    source_revision: tool.updated_sequence,
                    title: Cow::Owned(format!("Tool · {}", tool.name)),
                    text: &tool.detail,
                    detail: &tool.arguments,
                    kind: ConversationBlockKind::Tool,
                    done: !matches!(tool.status, desktop::projection::DesktopToolStatus::Running),
                    is_error: matches!(tool.status, desktop::projection::DesktopToolStatus::Failed),
                    image_count: 0,
                    truncated: tool.truncated,
                    durable: false,
                },
                panel_width,
            ));
        }

        self.conversation_render_cache.finish_frame();
        debug_assert_eq!(rows.len(), expected_count);
        self.conversation_render_rows = rows;
        self.conversation_render_dirty_sequences.clear();
        self.conversation_render_sequence_overflow = false;
    }

    fn rebuild_live_conversation_render_rows(&mut self, panel_width: u32) {
        let durable_count = self.projection.conversation().blocks().len();
        if self.conversation_render_rows.len() < durable_count
            || self.conversation_render_rows[..durable_count]
                .iter()
                .any(|row| !row.durable)
        {
            self.rebuild_conversation_render_rows(panel_width);
            self.conversation_render_full_dirty = false;
            return;
        }

        let session_id = self.projection.conversation().session_id.clone();
        self.conversation_render_rows.truncate(durable_count);
        self.conversation_render_cache.begin_frame();

        if let Some(submitted) = self.composer.submitted() {
            let row_id = submitted.block_id();
            self.conversation_render_rows
                .push(self.conversation_render_cache.resolve(
                    ConversationRowRenderSource {
                        item_key: ConversationItemKey::new(
                            &session_id,
                            ConversationItemKind::Submitted,
                            &row_id,
                        ),
                        source_revision: submitted.command_id,
                        title: Cow::Borrowed("You · submitted"),
                        text: &submitted.payload,
                        detail: "",
                        kind: ConversationBlockKind::User,
                        done: false,
                        is_error: false,
                        image_count: 0,
                        truncated: false,
                        durable: false,
                    },
                    panel_width,
                ));
        }

        for message in self.projection.messages() {
            let row_id = message_conversation_block_id(message);
            self.conversation_render_rows
                .push(self.conversation_render_cache.resolve(
                    ConversationRowRenderSource {
                        item_key: ConversationItemKey::new(
                            &session_id,
                            ConversationItemKind::LiveMessage,
                            &row_id,
                        ),
                        source_revision: message.updated_sequence,
                        title: Cow::Borrowed("Assistant · live"),
                        text: &message.text,
                        detail: &message.thinking,
                        kind: ConversationBlockKind::Assistant,
                        done: matches!(
                            message.status,
                            desktop::projection::DesktopMessageStatus::Completed
                        ),
                        is_error: false,
                        image_count: 0,
                        truncated: message.truncated,
                        durable: false,
                    },
                    panel_width,
                ));
        }

        for tool in self.projection.tools() {
            let row_id = tool_conversation_block_id(tool);
            self.conversation_render_rows
                .push(self.conversation_render_cache.resolve(
                    ConversationRowRenderSource {
                        item_key: ConversationItemKey::new(
                            &session_id,
                            ConversationItemKind::LiveTool,
                            &row_id,
                        ),
                        source_revision: tool.updated_sequence,
                        title: Cow::Owned(format!("Tool · {}", tool.name)),
                        text: &tool.detail,
                        detail: &tool.arguments,
                        kind: ConversationBlockKind::Tool,
                        done: !matches!(
                            tool.status,
                            desktop::projection::DesktopToolStatus::Running
                        ),
                        is_error: matches!(
                            tool.status,
                            desktop::projection::DesktopToolStatus::Failed
                        ),
                        image_count: 0,
                        truncated: tool.truncated,
                        durable: false,
                    },
                    panel_width,
                ));
        }
        self.conversation_render_cache.finish_incremental();
        self.conversation_render_dirty_sequences.clear();
        self.conversation_render_sequence_overflow = false;
    }

    fn update_conversation_rows_by_sequence(
        &mut self,
        panel_width: u32,
    ) -> Result<Option<std::time::Duration>, ()> {
        if self.conversation_render_sequence_overflow
            || self.conversation_render_dirty_sequences.is_empty()
        {
            return Err(());
        }

        let durable_count = self.projection.conversation().blocks().len();
        let submitted_count = usize::from(self.composer.submitted().is_some());
        let session_id = self.projection.conversation().session_id.clone();
        let sequences = std::mem::take(&mut self.conversation_render_dirty_sequences);
        let now = Instant::now();
        let mut next_refresh_after = None;
        self.conversation_render_cache.begin_frame();

        for sequence in sequences {
            if let Some((position, message)) = self
                .projection
                .messages()
                .iter()
                .enumerate()
                .find(|(_, message)| message.updated_sequence == sequence)
            {
                let row_id = message_conversation_block_id(message);
                let row = self.conversation_render_cache.resolve(
                    ConversationRowRenderSource {
                        item_key: ConversationItemKey::new(
                            &session_id,
                            ConversationItemKind::LiveMessage,
                            &row_id,
                        ),
                        source_revision: message.updated_sequence,
                        title: Cow::Borrowed("Assistant · live"),
                        text: &message.text,
                        detail: &message.thinking,
                        kind: ConversationBlockKind::Assistant,
                        done: matches!(
                            message.status,
                            desktop::projection::DesktopMessageStatus::Completed
                        ),
                        is_error: false,
                        image_count: 0,
                        truncated: message.truncated,
                        durable: false,
                    },
                    panel_width,
                );
                let desired_index = durable_count + submitted_count + position;
                let refresh = self.upsert_conversation_render_row(
                    durable_count,
                    desired_index,
                    row,
                    panel_width,
                    now,
                );
                next_refresh_after = minimum_duration(next_refresh_after, refresh);
            }

            if let Some((position, tool)) = self
                .projection
                .tools()
                .iter()
                .enumerate()
                .find(|(_, tool)| tool.updated_sequence == sequence)
            {
                let row_id = tool_conversation_block_id(tool);
                let row = self.conversation_render_cache.resolve(
                    ConversationRowRenderSource {
                        item_key: ConversationItemKey::new(
                            &session_id,
                            ConversationItemKind::LiveTool,
                            &row_id,
                        ),
                        source_revision: tool.updated_sequence,
                        title: Cow::Owned(format!("Tool · {}", tool.name)),
                        text: &tool.detail,
                        detail: &tool.arguments,
                        kind: ConversationBlockKind::Tool,
                        done: !matches!(
                            tool.status,
                            desktop::projection::DesktopToolStatus::Running
                        ),
                        is_error: matches!(
                            tool.status,
                            desktop::projection::DesktopToolStatus::Failed
                        ),
                        image_count: 0,
                        truncated: tool.truncated,
                        durable: false,
                    },
                    panel_width,
                );
                let desired_index =
                    durable_count + submitted_count + self.projection.messages().len() + position;
                let refresh = self.upsert_conversation_render_row(
                    durable_count,
                    desired_index,
                    row,
                    panel_width,
                    now,
                );
                next_refresh_after = minimum_duration(next_refresh_after, refresh);
            }
        }
        self.conversation_render_cache.finish_incremental();
        self.live_conversation_rows_match_projection()
            .then_some(next_refresh_after)
            .ok_or(())
    }

    fn upsert_conversation_render_row(
        &mut self,
        durable_count: usize,
        desired_index: usize,
        row: ConversationRowRenderData,
        panel_width: u32,
        now: Instant,
    ) -> Option<std::time::Duration> {
        let layout = self.conversation_live_layout.resolve_one(
            ConversationRowLayoutInput {
                key: row.item_key.stable_id().to_owned(),
                target_height: conversation_row_target_height(
                    &row,
                    &self.conversation_expanded_details,
                    panel_width,
                ),
                streaming: !row.done,
            },
            panel_width,
            now,
        );
        let existing_index = self.conversation_render_rows[durable_count..]
            .iter()
            .position(|candidate| candidate.item_key == row.item_key)
            .map(|index| durable_count + index);
        let row_index = upsert_indexed_item(
            &mut self.conversation_render_rows,
            existing_index,
            desired_index,
            row,
        );
        let height_index = upsert_indexed_item(
            &mut self.conversation_render_heights,
            existing_index,
            desired_index,
            layout.height,
        );
        let size_index = upsert_indexed_item(
            Rc::make_mut(&mut self.conversation_row_sizes),
            existing_index,
            desired_index,
            size(px(0.), px(layout.height)),
        );
        debug_assert_eq!(row_index, height_index);
        debug_assert_eq!(row_index, size_index);
        layout.next_refresh_after
    }

    fn live_conversation_rows_match_projection(&self) -> bool {
        let durable_count = self.projection.conversation().blocks().len();
        if self.conversation_render_rows.len() != self.visible_conversation_count() {
            return false;
        }
        let mut index = durable_count;
        if let Some(submitted) = self.composer.submitted() {
            let Some(row) = self.conversation_render_rows.get(index) else {
                return false;
            };
            if row.item_key.row_id() != submitted.block_id()
                || row.source_revision != submitted.command_id
            {
                return false;
            }
            index += 1;
        }
        for message in self.projection.messages() {
            let Some(row) = self.conversation_render_rows.get(index) else {
                return false;
            };
            if row.item_key.row_id() != message_conversation_block_id(message)
                || row.source_revision != message.updated_sequence
            {
                return false;
            }
            index += 1;
        }
        for tool in self.projection.tools() {
            let Some(row) = self.conversation_render_rows.get(index) else {
                return false;
            };
            if row.item_key.row_id() != tool_conversation_block_id(tool)
                || row.source_revision != tool.updated_sequence
            {
                return false;
            }
            index += 1;
        }
        index == self.conversation_render_rows.len()
    }

    fn conversation_width_for_render(&mut self, requested: u32) -> (u32, Option<(u32, Instant)>) {
        let Some(active) = self.conversation_render_width_bucket else {
            self.conversation_width_pending = None;
            return (requested, None);
        };
        if active == requested {
            self.conversation_width_pending = None;
            return (active, None);
        }

        let now = Instant::now();
        if let Some((pending, deadline)) = self.conversation_width_pending
            && pending == requested
        {
            if now >= deadline {
                self.conversation_width_pending = None;
                self.conversation_render_full_dirty = true;
                return (requested, None);
            }
            return (active, None);
        }

        let deadline = now + CONVERSATION_RESIZE_DEBOUNCE;
        self.conversation_width_pending = Some((requested, deadline));
        (active, Some((requested, deadline)))
    }

    fn prepare_conversation_rows(
        &mut self,
        layout_width: u32,
    ) -> Option<(std::time::Duration, bool)> {
        let _span = tracing::trace_span!(
            "desktop.render.prepare_rows",
            layout_width,
            visible_rows = self.visible_conversation_count()
        )
        .entered();
        let visible_conversation_count = self.visible_conversation_count();
        let previous_scroll_top = (!self.conversation_viewport.follow_latest())
            .then(|| (-f32::from(self.conversation_scroll.offset().y)).max(0.0));
        let full_render_update = self.conversation_render_full_dirty
            || self.conversation_render_width_bucket != Some(layout_width);
        let mut paused_scroll_top = None;
        let mut next_refresh_after = None;
        let mut refresh_requires_full = false;
        if full_render_update {
            self.rebuild_conversation_render_rows(layout_width);
            let row_layout_inputs = self
                .conversation_render_rows
                .iter()
                .map(|row| ConversationRowLayoutInput {
                    key: row.item_key.stable_id().to_owned(),
                    target_height: conversation_row_target_height(
                        row,
                        &self.conversation_expanded_details,
                        layout_width,
                    ),
                    streaming: !row.done,
                })
                .collect::<Vec<_>>();
            let row_layout = self.conversation_layout.resolve(
                row_layout_inputs,
                layout_width,
                Instant::now(),
                previous_scroll_top,
            );
            paused_scroll_top = row_layout.paused_scroll_top;
            next_refresh_after = row_layout.next_refresh_after;
            refresh_requires_full = next_refresh_after.is_some();
            self.conversation_render_heights = row_layout.heights;
            self.conversation_row_sizes = Rc::new(
                self.conversation_render_heights
                    .iter()
                    .map(|height| size(px(0.), px(*height)))
                    .collect(),
            );

            let durable_count = self.projection.conversation().blocks().len();
            let live_inputs = self.conversation_render_rows[durable_count..]
                .iter()
                .map(|row| ConversationRowLayoutInput {
                    key: row.item_key.stable_id().to_owned(),
                    target_height: conversation_row_target_height(
                        row,
                        &self.conversation_expanded_details,
                        layout_width,
                    ),
                    streaming: !row.done,
                })
                .collect();
            let _ = self.conversation_live_layout.resolve(
                live_inputs,
                layout_width,
                Instant::now(),
                None,
            );
            self.conversation_render_width_bucket = Some(layout_width);
            self.conversation_render_full_dirty = false;
            self.conversation_render_live_dirty = false;
        } else if self.conversation_render_live_dirty {
            let durable_count = self.projection.conversation().blocks().len();
            match self.update_conversation_rows_by_sequence(layout_width) {
                Ok(refresh) => {
                    next_refresh_after = refresh;
                    self.conversation_render_sequence_overflow = false;
                }
                Err(()) => {
                    self.rebuild_live_conversation_render_rows(layout_width);
                    let live_inputs = self.conversation_render_rows[durable_count..]
                        .iter()
                        .map(|row| ConversationRowLayoutInput {
                            key: row.item_key.stable_id().to_owned(),
                            target_height: conversation_row_target_height(
                                row,
                                &self.conversation_expanded_details,
                                layout_width,
                            ),
                            streaming: !row.done,
                        })
                        .collect();
                    let live_layout = self.conversation_live_layout.resolve(
                        live_inputs,
                        layout_width,
                        Instant::now(),
                        None,
                    );
                    next_refresh_after = live_layout.next_refresh_after;
                    self.conversation_render_heights.truncate(durable_count);
                    self.conversation_render_heights
                        .extend(live_layout.heights.iter().copied());
                    let sizes = Rc::make_mut(&mut self.conversation_row_sizes);
                    sizes.truncate(durable_count);
                    sizes.extend(
                        live_layout
                            .heights
                            .into_iter()
                            .map(|height| size(px(0.), px(height))),
                    );
                }
            }
            self.conversation_render_live_dirty = false;
        }
        let mut text_phase_requires_full = false;
        let text_phase_refresh = self
            .conversation_render_rows
            .iter()
            .filter_map(|row| {
                let delay = row.next_text_phase_after?;
                text_phase_requires_full |= row.durable;
                Some(delay)
            })
            .min();
        next_refresh_after = minimum_duration(next_refresh_after, text_phase_refresh);
        refresh_requires_full |= text_phase_requires_full;
        debug_assert_eq!(
            self.conversation_render_rows.len(),
            visible_conversation_count
        );
        debug_assert_eq!(
            self.conversation_render_heights.len(),
            visible_conversation_count
        );
        if let (Some(previous_scroll_top), Some(adjusted_scroll_top)) =
            (previous_scroll_top, paused_scroll_top)
            && (previous_scroll_top - adjusted_scroll_top).abs() > 0.5
        {
            let mut offset = self.conversation_scroll.offset();
            offset.y = px(-adjusted_scroll_top);
            self.conversation_scroll.set_offset(offset);
        }
        if self.conversation_viewport.follow_latest() && visible_conversation_count > 0 {
            self.conversation_scroll
                .scroll_to_item(visible_conversation_count - 1, ScrollStrategy::Bottom);
        }
        next_refresh_after.map(|delay| (delay, refresh_requires_full))
    }

    fn refresh_conversation_rows_at_width(&mut self, layout_width: u32, cx: &mut Context<Self>) {
        let pane_dirty = self.conversation_render_full_dirty
            || self.conversation_render_live_dirty
            || self.conversation_render_width_bucket != Some(layout_width);
        let refresh = self.prepare_conversation_rows(layout_width);
        if pane_dirty {
            self.conversation_pane.update(cx, |_, cx| cx.notify());
        }
        self.schedule_conversation_height_refresh(refresh, cx);
    }

    fn refresh_conversation_rows_at_current_width(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(layout_width) = self.conversation_render_width_bucket else {
            return false;
        };
        self.refresh_conversation_rows_at_width(layout_width, cx);
        true
    }

    fn schedule_conversation_height_refresh(
        &mut self,
        refresh: Option<(std::time::Duration, bool)>,
        cx: &mut Context<Self>,
    ) {
        let Some((delay, requires_full)) = refresh else {
            return;
        };
        let deadline = Instant::now() + delay;
        if self
            .conversation_height_refresh_deadline
            .is_none_or(|scheduled| scheduled > deadline)
        {
            self.conversation_height_refresh_deadline = Some(deadline);
            self.conversation_height_refresh_full = requires_full;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
                let _ = this.update(cx, |this, cx| {
                    if this.conversation_height_refresh_deadline == Some(deadline) {
                        this.conversation_height_refresh_deadline = None;
                        if this.conversation_height_refresh_full {
                            this.conversation_render_full_dirty = true;
                        } else {
                            this.conversation_render_live_dirty = true;
                        }
                        this.conversation_height_refresh_full = false;
                        let _ = this.refresh_conversation_rows_at_current_width(cx);
                    }
                });
            })
            .detach();
        } else if requires_full {
            self.conversation_height_refresh_full = true;
        }
    }

    fn focus_active_target(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.focus.active() {
            FocusTarget::Sessions => self.sessions_focus.focus(window),
            FocusTarget::Conversation => self.conversation_focus.focus(window),
            FocusTarget::Composer => self.composer_input.focus_handle(cx).focus(window),
            FocusTarget::Context => self.context_focus.focus(window),
            FocusTarget::Status => self.status_focus.focus(window),
            FocusTarget::Overlay => match self.active_overlay {
                Some(DesktopOverlayKind::Authorization) => self.authorization_focus.focus(window),
                Some(DesktopOverlayKind::CommandPalette) => {
                    self.command_palette_focus.focus(window);
                }
                Some(DesktopOverlayKind::NarrowSessions) => {
                    self.narrow_sessions_focus.focus(window);
                }
                Some(DesktopOverlayKind::NarrowContext) => self.context_focus.focus(window),
                None => self.composer_input.focus_handle(cx).focus(window),
            },
        }
    }

    fn semantic_status(&self) -> SemanticStatus {
        match self.projection.lifecycle() {
            DesktopProjectionLifecycle::Failed | DesktopProjectionLifecycle::NeedsResync => {
                SemanticStatus::Error
            }
            DesktopProjectionLifecycle::Stopped => SemanticStatus::Warning,
            DesktopProjectionLifecycle::Running
                if !self.projection.snapshot().pending_authorizations.is_empty() =>
            {
                SemanticStatus::Authorization
            }
            DesktopProjectionLifecycle::Running
                if self.projection.snapshot().active_operation.is_some() =>
            {
                SemanticStatus::Running
            }
            DesktopProjectionLifecycle::Running => SemanticStatus::Idle,
        }
    }

    fn status_color(&self, status: SemanticStatus) -> gpui::Rgba {
        let theme = SemanticTheme::GEEK_DARK;
        match status {
            SemanticStatus::Idle => rgb(theme.muted_text.value()),
            SemanticStatus::Running => rgb(theme.accent.value()),
            SemanticStatus::Warning | SemanticStatus::Authorization => rgb(theme.warning.value()),
            SemanticStatus::Error => rgb(theme.danger.value()),
        }
    }
}

impl Render for NativeShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _span = tracing::trace_span!("desktop.render").entered();
        if self.composer_needs_sync {
            let draft = self.composer.draft().to_owned();
            self.composer_input.update(cx, |input, cx| {
                input.set_value(draft, window, cx);
            });
            self.composer_needs_sync = false;
        }
        let theme = SemanticTheme::GEEK_DARK;
        let layout = self.layout(window);
        self.focus.reconcile_layout(layout);
        let requested_layout_width = conversation_width_bucket(layout.workspace.width);
        let (layout_width, width_refresh) =
            self.conversation_width_for_render(requested_layout_width);
        if let Some((requested, deadline)) = width_refresh {
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(CONVERSATION_RESIZE_DEBOUNCE)
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.conversation_width_pending == Some((requested, deadline)) {
                        this.conversation_render_full_dirty = true;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        self.refresh_conversation_rows_at_width(layout_width, cx);
        let authorization_present = !self.projection.snapshot().pending_authorizations.is_empty();
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
                        .id("context-resize-handle")
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

        let conversation = div()
            .id("conversation-panel")
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
            .child(self.composer_pane.clone());

        let status_bar = self.status_bar.clone();

        let overlay_host = self.overlay_host.clone();

        div()
            .key_context(actions::ROOT_KEY_CONTEXT)
            .on_action(cx.listener(Self::on_open_command_palette))
            .on_action(cx.listener(Self::on_open_file_surface))
            .on_action(cx.listener(Self::on_new_session))
            .on_action(cx.listener(Self::on_focus_composer))
            .on_action(cx.listener(Self::on_submit_composer))
            .on_action(cx.listener(Self::on_abort_active_operation))
            .on_action(cx.listener(Self::on_escape_hierarchy))
            .on_action(cx.listener(Self::on_follow_latest_output))
            .on_action(cx.listener(Self::on_toggle_context_panel))
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
            .text_sm()
            .bg(rgb(theme.canvas.value()))
            .text_color(rgb(theme.text.value()))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .children(sessions_panel)
                    .child(conversation)
                    .children(context_panel),
            )
            .child(status_bar)
            .child(overlay_host)
    }
}

fn focus_target_label(target: FocusTarget) -> &'static str {
    match target {
        FocusTarget::Sessions => "Sessions",
        FocusTarget::Conversation => "Conversation",
        FocusTarget::Composer => "Composer",
        FocusTarget::Context => "Context",
        FocusTarget::Status => "Status",
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
    use std::cell::RefCell;

    use coding_agent::api::authorization::{
        ToolAuthorizationPreview, ToolAuthorizationRequest, ToolAuthorizationRisk,
        ToolAuthorizationScope,
    };
    use coding_agent::api::client::{
        CodingAgentContextSnapshot, CodingAgentFileChangeSnapshot, CodingAgentRecoveryPending,
        CodingAgentSnapshot, CodingAgentSnapshotCursor, UI_SNAPSHOT_PROTOCOL_VERSION,
    };
    use coding_agent::api::embedding::{
        CodingAgentEmbeddingSnapshot, CodingAgentResourceSummary, CodingAgentSettingsSummary,
    };
    use coding_agent::api::view::{
        CodingAgentCapabilities, CodingAgentSessionView, CodingAgentTranscriptSnapshot, ProfileId,
    };
    use gpui::TestAppContext;
    use gpui_component::{Theme, ThemeMode};

    fn visual_test_snapshot() -> desktop::runtime::DesktopRuntimeHydratedSnapshot {
        let session_id = "desktop-visual-test".to_owned();
        desktop::runtime::DesktopRuntimeHydratedSnapshot {
            project: CodingAgentEmbeddingSnapshot {
                cwd: std::path::PathBuf::from("/desktop-visual-test"),
                global_config_dir: std::path::PathBuf::from("/desktop-visual-test/config"),
                selected_model_id: "test-model".into(),
                default_agent_profile_id: ProfileId::from("default"),
                models: Vec::new(),
                profiles: Vec::new(),
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
                    stream_id: "desktop-visual-test-stream".into(),
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

    fn initialize_visual_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            actions::bind_keys(cx);
            Theme::change(ThemeMode::Dark, None, cx);
        });
    }

    fn add_visual_shell<'a>(
        cx: &'a mut TestAppContext,
        runtime: DesktopRuntimeBridge,
        projection: DesktopProjection,
    ) -> (gpui::Entity<NativeShell>, &'a mut gpui::VisualTestContext) {
        let shell_slot = Rc::new(RefCell::new(None));
        let shell_slot_for_window = Rc::clone(&shell_slot);
        let (_, visual_cx) = cx.add_window_view(move |window, cx| {
            let shell = cx.new(|cx| {
                NativeShell::new(
                    NativeShellInit {
                        runtime,
                        projection,
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
            .expect("visual shell entity was captured");
        (shell, visual_cx)
    }

    fn desktop_region_bounds(
        cx: &mut gpui::VisualTestContext,
    ) -> [Option<gpui::Bounds<gpui::Pixels>>; 5] {
        [
            cx.debug_bounds("desktop-sessions-panel"),
            cx.debug_bounds("desktop-conversation-panel"),
            cx.debug_bounds("desktop-composer-panel"),
            cx.debug_bounds("desktop-context-panel"),
            cx.debug_bounds("desktop-status-panel"),
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
        assert!(medium[4].is_some());
        assert_eq!(f32::from(medium[0].unwrap().size.width), 240.);
        assert_eq!(f32::from(medium[1].unwrap().size.width), 760.);
        assert_eq!(f32::from(medium[2].unwrap().size.width), 760.);
        assert_eq!(f32::from(medium[4].unwrap().size.width), 1_000.);

        cx.simulate_resize(size(px(700.), px(900.)));
        cx.run_until_parked();
        let narrow = desktop_region_bounds(cx);
        assert!(narrow[1].is_some());
        assert!(narrow[2].is_some());
        assert!(narrow[4].is_some());
        assert_eq!(f32::from(narrow[1].unwrap().size.width), 700.);
        assert_eq!(f32::from(narrow[2].unwrap().size.width), 700.);
        assert_eq!(f32::from(narrow[4].unwrap().size.width), 700.);

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
    fn native_shell_primary_controls_keep_minimum_hit_targets(cx: &mut TestAppContext) {
        initialize_visual_test(cx);
        let (_, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            visual_test_projection(),
        );

        for width in [1_300., 700.] {
            cx.simulate_resize(size(px(width), px(900.)));
            cx.run_until_parked();
            for selector in [
                "desktop-hit-toggle-sessions",
                "desktop-hit-submit-composer",
                "desktop-hit-cycle-model",
            ] {
                assert_minimum_hit_target(cx, selector);
            }
        }

        cx.simulate_resize(size(px(1_300.), px(900.)));
        cx.run_until_parked();
        assert_minimum_hit_target(cx, "desktop-hit-create-session");
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
        assert_eq!(probe.last_observed.get(), Some(Duration::from_millis(5)));
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
            shell.composer_input_latency.mark_changed_at(changed_at);
            shell.notify_composer_pane(cx);
        });
        cx.run_until_parked();

        assert!(shell.read_with(cx, |shell, _| {
            shell.composer_input_latency.pending_change.get().is_none()
                && shell
                    .composer_input_latency
                    .last_observed
                    .get()
                    .is_some_and(|latency| latency <= changed_at.elapsed())
        }));
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
            },
        );
        let projection = DesktopProjection::new(snapshot)
            .expect("code-copy visual fixture is a valid product projection");
        let (_, cx) = add_visual_shell(
            cx,
            DesktopRuntimeBridge::disconnected_for_test(),
            projection,
        );
        cx.run_until_parked();

        let bounds = cx
            .debug_bounds("desktop-copy-markdown-code")
            .expect("final Markdown code block exposes a copy action");
        assert!(f32::from(bounds.size.height) >= 32.);
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("fn main() { println!(\"exact\"); }".into())
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

        inspector.update(cx, |_, cx| {
            cx.emit(InspectorPaneEvent::RequestFileReview(
                review_request.clone(),
            ));
        });
        cx.run_until_parked();
        assert!(
            runtime_harness
                .drain_command_kinds()
                .contains(&desktop::runtime::DesktopRuntimeCommandKind::ReviewChangedFile)
        );
        assert!(shell.read_with(cx, |shell, _| {
            matches!(
                &shell.file_review,
                DesktopFileReviewState::Loading(request) if request == &review_request
            )
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
        let mut selection = DesktopThinkingSelection::Default;
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
        assert_eq!(selection.next(), DesktopThinkingSelection::Default);
    }

    #[test]
    fn composer_mode_and_draft_are_scoped_to_the_active_session() {
        let mut modes = HashMap::new();
        assert_eq!(
            composer_running_mode_for(&modes, "session-a"),
            ComposerRunningMode::SteerNow
        );
        modes.insert("session-a".into(), ComposerRunningMode::QueueNext);
        assert_eq!(
            composer_running_mode_for(&modes, "session-a").submission_kind(),
            ComposerSubmissionKind::FollowUp
        );
        assert_eq!(
            composer_running_mode_for(&modes, "session-b").submission_kind(),
            ComposerSubmissionKind::Steer
        );

        let mut composer = ComposerState::default();
        let mut drafts = HashMap::from([("session-b".to_owned(), "draft b".to_owned())]);
        composer.edit("draft a");
        assert!(reconcile_composer_session_state(
            &mut composer,
            &mut drafts,
            "session-a",
            "session-b"
        ));
        assert_eq!(composer.draft(), "draft b");
        assert!(reconcile_composer_session_state(
            &mut composer,
            &mut drafts,
            "session-b",
            "session-a"
        ));
        assert_eq!(composer.draft(), "draft a");
        assert!(!reconcile_composer_session_state(
            &mut composer,
            &mut drafts,
            "session-a",
            "session-a"
        ));
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
        assert_eq!(long_assistant, TRANSCRIPT_ROW_MAX_HEIGHT);
    }

    #[test]
    fn secondary_message_details_are_collapsed_by_default_and_height_aware() {
        let mut cache = ConversationRowRenderCache::default();
        let reasoning = "reasoning line\n".repeat(20);
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
                truncated: false,
                durable: true,
            },
            900,
        );
        let collapsed = conversation_row_target_height(&assistant, &HashSet::new(), 900);
        let expanded_ids = HashSet::from([assistant.item_key.row_id().to_owned()]);
        let expanded = conversation_row_target_height(&assistant, &expanded_ids, 900);
        assert!(collapsed < expanded);
        assert_eq!(expanded, assistant.measured_height);

        let pane = include_str!("native_shell/conversation_pane.rs");
        assert!(pane.contains("Reasoning · collapsed"));
        assert!(pane.contains("output + arguments collapsed"));
        assert!(pane.contains("ConversationPaneEvent::ToggleDetails"));
        assert!(pane.contains("group_hover(hover_group"));
        assert!(pane.contains(".absolute()"));
        assert!(pane.contains("USER_MESSAGE_WIDTH_PERCENT as f32 / 100."));
        assert!(pane.contains(".max_w(px(USER_MESSAGE_MAX_WIDTH as f32))"));
        assert!(pane.contains("card.max_w(px(ASSISTANT_MESSAGE_MAX_WIDTH as f32))"));
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
    fn conversation_kinds_have_distinct_visual_surfaces() {
        let theme = SemanticTheme::GEEK_DARK;
        let user = conversation_block_visual(ConversationBlockKind::User, false, theme);
        let assistant = conversation_block_visual(ConversationBlockKind::Assistant, false, theme);
        let tool = conversation_block_visual(ConversationBlockKind::Tool, false, theme);
        let failed_tool = conversation_block_visual(ConversationBlockKind::Tool, true, theme);
        let delegation = conversation_block_visual(ConversationBlockKind::Delegation, false, theme);
        let diagnostic = conversation_block_visual(ConversationBlockKind::Diagnostic, true, theme);

        assert!(user.align_right);
        assert!(!assistant.align_right);
        assert_ne!(user.surface, assistant.surface);
        assert_ne!(assistant.surface, tool.surface);
        assert_ne!(tool.surface, failed_tool.surface);
        assert_eq!(failed_tool.surface, diagnostic.surface);
        assert_ne!(tool.accent, failed_tool.accent);
        assert_eq!(tool.accent, theme.muted_text);
        assert_eq!(delegation.accent, theme.accent);
        assert_ne!(delegation.surface, theme.thinking_surface);
    }

    #[test]
    fn desktop_typography_uses_system_ui_with_local_monospace_data_regions() {
        let shell = include_str!("native_shell.rs");
        let conversation = include_str!("native_shell/conversation_pane.rs");
        let sessions = include_str!("native_shell/sessions_pane.rs");
        let inspector = include_str!("native_shell/inspector_pane.rs");
        let status = include_str!("native_shell/status_bar.rs");
        let overlays = include_str!("native_shell/overlay_host.rs");

        assert!(shell.contains(".font_family(UI_FONT_FAMILY)"));
        for local_data_surface in [conversation, sessions, inspector, status, overlays] {
            assert!(local_data_surface.contains("MONOSPACE_FONT_FAMILY"));
        }
        assert!(conversation.contains("theme.reasoning.value()"));
        assert!(!conversation.contains("border_color(rgb(visual.accent.value()))"));
    }

    #[test]
    fn conversation_focus_uses_the_existing_header_divider_without_panel_geometry() {
        let theme = SemanticTheme::GEEK_DARK;
        assert_eq!(conversation_focus_accent(false, theme), theme.border);
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
        assert!(!pane.contains(".id((\"conversation-block\", index))"));
        assert!(!source.contains("(\"transcript-markdown\", index)"));
        assert!(!source.contains("(\"transcript-detail-markdown\", index)"));
        let legacy_per_render_sanitizer =
            ["bounded_markdown_preview(", "&block.text", ")"].concat();
        assert!(!source.contains(&legacy_per_render_sanitizer));
    }

    #[test]
    fn composer_and_transcript_source_do_not_restore_fixed_heights() {
        let source = include_str!("native_shell.rs");
        assert!(source.contains(".auto_grow(2, 8)"));
        assert!(source.contains("row.measured_height"));
        let fixed_row_height = [".h(px(", "220.))"].concat();
        let fixed_composer_height = [".h(px(", "COMPOSER_HEIGHT"].concat();
        assert!(!source.contains(&fixed_row_height));
        assert!(!source.contains(&fixed_composer_height));
    }

    #[test]
    fn conversation_list_sizes_persist_and_full_history_work_is_dirty_gated() {
        let source = include_str!("native_shell.rs");
        assert!(source.contains("conversation_row_sizes: Rc<Vec<gpui::Size<gpui::Pixels>>>"));
        assert!(source.contains("let transcript_rows = self.conversation_row_sizes.clone()"));
        assert!(source.contains("else if self.conversation_render_live_dirty"));
        assert!(source.contains("self.conversation_render_rows.truncate(durable_count)"));
        assert!(source.contains("Rc::make_mut(&mut self.conversation_row_sizes)"));
        assert!(source.contains("CONVERSATION_RESIZE_DEBOUNCE"));
        assert!(source.contains("Duration::from_millis(67)"));
        assert!(source.contains("update_conversation_rows_by_sequence(layout_width)"));
        assert!(source.contains("message.updated_sequence == sequence"));
        assert!(source.contains("tool.updated_sequence == sequence"));

        let render = source
            .split_once("impl Render for NativeShell")
            .expect("native render implementation remains present")
            .1;
        let prepare_call = ["prepare_conversation_", "rows("].concat();
        let refresh_call = ["refresh_conversation_rows_", "at_width(layout_width, cx)"].concat();
        let legacy_rebuild_call = ["rebuild_conversation_", "render_rows("].concat();
        assert_eq!(source.matches(&prepare_call).count(), 2);
        assert_eq!(render.matches(&prepare_call).count(), 0);
        assert_eq!(render.matches(&refresh_call).count(), 1);
        assert_eq!(render.matches(&legacy_rebuild_call).count(), 0);
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
        assert!(pane.contains("WeakEntity<NativeShell>"));
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

        assert!(!shell.contains(&sessions_panel_id));
        assert!(pane.contains(&sessions_panel_id));
        assert!(shell.contains("sessions_pane: gpui::Entity<SessionsPane>"));
        assert!(shell.contains("let sessions_pane = cx.new("));
        assert!(shell.contains(".child(self.sessions_pane.clone())"));
        assert!(pane.contains("impl EventEmitter<SessionsPaneEvent>"));
        assert!(pane.contains("WeakEntity<NativeShell>"));
        assert!(shell.contains("fn notify_sessions_pane("));
        assert!(!pane.contains("conversation_render_rows"));
        assert!(!pane.contains("conversation_render_dirty_sequences"));
    }

    #[test]
    fn composer_rendering_and_input_changes_are_isolated_in_a_child_entity() {
        let shell = include_str!("native_shell.rs");
        let pane = include_str!("native_shell/composer_pane.rs");
        let composer_panel_id = [".id(\"composer-", "panel\")"].concat();
        let input_constructor = ["Input", "::new(&input)"].concat();

        assert!(!shell.contains(&composer_panel_id));
        assert!(pane.contains(&composer_panel_id));
        assert!(shell.contains("composer_pane: gpui::Entity<ComposerPane>"));
        assert!(shell.contains(".child(self.composer_pane.clone())"));
        assert!(pane.contains("impl EventEmitter<ComposerPaneEvent>"));
        assert!(pane.contains(&input_constructor));
        assert!(shell.contains("InputEvent::Change =>"));
        assert!(shell.contains("this.notify_composer_pane(cx)"));
        assert!(shell.contains("ComposerPaneEvent::SubmitRunning"));
        assert!(shell.contains("ComposerSubmissionKind::Steer"));
        assert!(shell.contains("ComposerPaneEvent::SetRunningMode"));
        assert!(shell.contains("ComposerSubmissionKind::FollowUp"));
        assert!(pane.contains("composer-mode-steer"));
        assert!(pane.contains("composer-mode-follow-up"));
        assert!(pane.contains("submit-running-composer"));
        let legacy_steer_button = ["Button::new(\"steer-", "operation\")"].concat();
        let legacy_follow_up_button = ["Button::new(\"follow-up-", "operation\")"].concat();
        assert!(!pane.contains(&legacy_steer_button));
        assert!(!pane.contains(&legacy_follow_up_button));
        assert!(shell.contains("composer_session_drafts: HashMap<String, String>"));
        assert!(shell.contains("composer_running_modes: HashMap<String, ComposerRunningMode>"));
        assert!(!pane.contains("conversation_render_dirty_sequences"));
    }

    #[test]
    fn inspector_rendering_is_owned_by_a_non_streaming_child_entity() {
        let shell = include_str!("native_shell.rs");
        let pane = include_str!("native_shell/inspector_pane.rs");
        let context_panel_id = [".id(\"context-", "panel\")"].concat();

        assert!(!shell.contains(&context_panel_id));
        assert!(pane.contains(&context_panel_id));
        assert!(shell.contains("inspector_pane: gpui::Entity<InspectorPane>"));
        assert!(shell.contains(".child(self.inspector_pane.clone())"));
        assert!(pane.contains("impl EventEmitter<InspectorPaneEvent>"));
        assert!(pane.contains("RequestFileReview(CodingAgentFileReviewRequest)"));
        assert!(pane.contains("identity: DesktopRecoveryIdentity"));
        assert!(shell.contains("this.submit_recovery_action(identity.clone(), *action, cx)"));
        assert!(shell.contains("fn notify_inspector_pane("));
        assert!(!pane.contains("conversation_render_dirty_sequences"));
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
        let search_placeholder = ["placeholder(\"Search ", "sessions…\")"].concat();
        let refresh_interval = ["Duration::from_secs(", "15)"].concat();
        let active_duplicate = ["current_session_", "label"].concat();

        assert!(shell.contains(&search_placeholder));
        assert!(shell.contains("schedule_session_catalog_refresh"));
        assert!(shell.contains(&refresh_interval));
        assert!(pane.contains("relative_session_time"));
        assert!(pane.contains("Current task"));
        assert!(pane.contains("Recent task"));
        assert!(pane.contains("session_catalog.iter()"));
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
        let context_handle = ["context-resize-", "handle"].concat();
        let double_click = ["event.click_count ", ">= 2"].concat();

        assert!(shell.contains(&sessions_handle));
        assert!(shell.contains(&context_handle));
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
        assert!(!status_projection_dirty(&usage));

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
        let header_id = [".id(\"conversation-", "header\")"].concat();

        assert!(!shell.contains(&header_id));
        assert!(header.contains(&header_id));
        assert!(shell.contains("conversation_header: gpui::Entity<ConversationHeader>"));
        assert!(shell.contains(".child(self.conversation_header.clone())"));
        assert!(header.contains("impl EventEmitter<ConversationHeaderEvent>"));
        assert!(shell.contains("ConversationHeaderEvent::ToggleSessions"));
        assert!(shell.contains("ConversationHeaderEvent::ToggleContext"));
        assert!(shell.contains("ConversationHeaderEvent::Reload"));
        assert!(shell.contains("ConversationHeaderEvent::CopySelected"));
        assert!(shell.contains("ConversationHeaderEvent::Abort"));
        assert!(!header.contains("conversation_render_dirty_sequences"));
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
    fn status_bar_rendering_is_owned_by_a_non_streaming_child_entity() {
        let shell = include_str!("native_shell.rs");
        let bar = include_str!("native_shell/status_bar.rs");
        let status_panel_id = [".id(\"status-", "panel\")"].concat();

        assert!(!shell.contains(&status_panel_id));
        assert!(bar.contains(&status_panel_id));
        assert!(shell.contains("status_bar: gpui::Entity<StatusBar>"));
        assert!(shell.contains("let status_bar = self.status_bar.clone()"));
        assert!(bar.contains("impl EventEmitter<StatusBarEvent>"));
        assert!(shell.contains("StatusBarEvent::SelectNextModel"));
        assert!(shell.contains("this.select_next_model(cx)"));
        assert!(shell.contains("StatusBarEvent::SelectNextSessionProfile"));
        assert!(shell.contains("this.select_next_session_profile(cx)"));
        assert!(shell.contains("StatusBarEvent::CycleThinking"));
        assert!(shell.contains("this.cycle_thinking_selection(cx)"));
        assert!(!bar.contains("conversation_render_dirty_sequences"));
    }

    #[test]
    fn streaming_only_projection_delta_does_not_dirty_status_bar() {
        let mut streaming = desktop::projection::DesktopProjectionDelta {
            cursor: true,
            conversation: true,
            tools: true,
            ..Default::default()
        };
        assert!(!status_projection_dirty(&streaming));

        streaming.authorizations = true;
        assert!(status_projection_dirty(&streaming));
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
        assert!(host.contains("DecideAuthorization"));
        assert!(shell.contains("this.decide_tool_authorization("));
        assert!(shell.contains("Self::on_trap_overlay_focus"));
        assert!(host.contains("owner.inspector_pane.clone()"));
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
mod composer_pane;
mod conversation_header;
mod conversation_pane;
mod inspector_pane;
mod overlay_host;
mod sessions_pane;
mod status_bar;
mod streaming_text;

use composer_pane::{ComposerPane, ComposerPaneEvent};
use conversation_header::{ConversationHeader, ConversationHeaderEvent};
use conversation_pane::{ConversationPane, ConversationPaneEvent};
use inspector_pane::{InspectorPane, InspectorPaneEvent};
use overlay_host::{OverlayHost, OverlayHostEvent};
use sessions_pane::{SessionsPane, SessionsPaneEvent};
use status_bar::{StatusBar, StatusBarEvent};
