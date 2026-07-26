use coding_agent::api::authorization::{
    ToolAuthorizationDecision, ToolAuthorizationIdentity, ToolAuthorizationScope,
};
use coding_agent::api::embedding::CodingAgentThinkingLevel;
use coding_agent::api::event::CodingAgentRecoveryResolution;
use coding_agent::api::review::CodingAgentFileReviewRequest;
use desktop::conversation::{
    ComposerAdmission, ComposerState, ComposerSubmissionKind, ConversationBlockKind,
    ConversationRowLayoutInput, ConversationRowLayoutState, ConversationRowRenderCache,
    ConversationRowRenderData, ConversationRowRenderSource, ConversationViewport,
    conversation_width_bucket,
};
#[cfg(test)]
use desktop::conversation::{TRANSCRIPT_ROW_MAX_HEIGHT, conversation_block_height};
use desktop::file_review::{
    DesktopFileReviewDocument, DesktopReviewLineKind, MAX_VISIBLE_FILE_CHANGES,
};
use desktop::preferences::{DesktopPreferences, PreferenceWriter};
use desktop::projection::{DesktopProjection, DesktopProjectionLifecycle, DesktopRecoveryStatus};
use desktop::runtime::{
    DesktopRecoveryAction, DesktopRecoveryIdentity, DesktopRuntimeBridge,
    DesktopRuntimeCommandHandle, DesktopRuntimeSelectionKind,
};
use desktop::shell::{
    CONTEXT_PANEL_WIDTH, FocusState, FocusTarget, PanelVisibility, SESSION_PANEL_WIDTH,
    STATUS_HEIGHT, SemanticColor, SemanticStatus, SemanticTheme, ShellLayout, truncate_label,
};
use gpui::{
    AnyElement, ClipboardItem, Context, ElementId, FocusHandle, Focusable as _, IntoElement,
    ParentElement as _, Render, ScrollStrategy, SharedString, Styled as _, Subscription, Window,
    WindowBounds, div, prelude::*, px, relative, rgb, rgba, size,
};
use gpui_component::{
    Disableable as _, VirtualListScrollHandle,
    button::Button,
    input::{Input, InputEvent, InputState},
    text::TextView,
    v_virtual_list,
};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use crate::actions::{
    self, AbortActiveOperation, AuthorizationAllowForOperation, AuthorizationAllowOnce,
    AuthorizationDeny, DesktopCommandPalette, DesktopPaletteCommand, EscapeHierarchy,
    FocusComposer, FocusNextRegion, FocusPreviousRegion, FollowLatestOutput, NewSession,
    OpenCommandPalette, OpenFileSurface, PALETTE_ENTRIES, PaletteConfirm, PaletteNext,
    PalettePrevious, SubmitComposer, ToggleContextPanel, TrapOverlayFocus,
};
use crate::command_ledger::{DesktopCommandIntent, DesktopCommandLedger};

const MAX_RUNTIME_UPDATES_PER_FRAME: usize = 64;
const COMPOSER_MIN_HEIGHT: f32 = 88.;
const COMPOSER_MAX_HEIGHT: f32 = 236.;
const CONVERSATION_RESIZE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(67);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConversationTextRenderMode {
    StreamingPlainText,
    FinalMarkdown,
}

fn conversation_text_render_mode(done: bool) -> ConversationTextRenderMode {
    if done {
        ConversationTextRenderMode::FinalMarkdown
    } else {
        ConversationTextRenderMode::StreamingPlainText
    }
}

fn conversation_distance_to_bottom(offset_y: f32, max_offset_y: f32) -> f32 {
    (max_offset_y.max(0.0) + offset_y.min(0.0)).max(0.0)
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

fn conversation_text_element(
    id: ElementId,
    text: Arc<str>,
    mode: ConversationTextRenderMode,
    window: &mut Window,
    cx: &mut Context<NativeShell>,
) -> AnyElement {
    let text = SharedString::new(text);
    match mode {
        ConversationTextRenderMode::StreamingPlainText => div()
            .w_full()
            .whitespace_normal()
            .child(text)
            .into_any_element(),
        ConversationTextRenderMode::FinalMarkdown => {
            TextView::markdown(id, text, window, cx).into_any_element()
        }
    }
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
                theme.warning
            },
            align_right: false,
        },
        ConversationBlockKind::Delegation => ConversationBlockVisual {
            glyph: "AGENT",
            surface: theme.thinking_surface,
            accent: theme.focus_ring,
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
    conversation_render_width_bucket: Option<u32>,
    conversation_width_pending: Option<(u32, Instant)>,
    conversation_height_refresh_deadline: Option<Instant>,
    conversation_height_refresh_full: bool,
    composer: ComposerState,
    composer_input: gpui::Entity<InputState>,
    composer_needs_sync: bool,
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
                        this.composer.edit(input.read(cx).value().to_string());
                        cx.notify();
                    }
                    InputEvent::Focus => {
                        this.record_focus(FocusTarget::Composer, window, cx);
                    }
                    InputEvent::PressEnter { secondary: true } => {
                        if !this.root_action_blocked_by_overlay(window, cx) {
                            this.submit_primary_composer(cx);
                        }
                    }
                    InputEvent::PressEnter { secondary: false } | InputEvent::Blur => {}
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
            conversation_render_width_bucket: None,
            conversation_width_pending: None,
            conversation_height_refresh_deadline: None,
            conversation_height_refresh_full: false,
            composer: ComposerState::default(),
            composer_input,
            composer_needs_sync: false,
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
        ShellLayout::resolve(
            u32::from(viewport.width),
            u32::from(viewport.height),
            self.visibility(),
        )
    }

    fn record_focus(&mut self, target: FocusTarget, window: &mut Window, cx: &mut Context<Self>) {
        let layout = self.layout(window);
        if self.focus.request(target, layout) {
            cx.notify();
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
        let forced_layout = ShellLayout::resolve(
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
        cx.notify();
    }

    fn poll_runtime(&mut self, cx: &mut Context<Self>) -> bool {
        if self.runtime.is_none() {
            return false;
        }
        let mut applied = 0;
        while applied < MAX_RUNTIME_UPDATES_PER_FRAME {
            let Some(update) = self.runtime_updates.pop_front() else {
                break;
            };
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
                    }
                    applied += 1;
                    continue;
                }
                update => update,
            };
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
                }
                desktop::runtime::DesktopRuntimeUpdate::PromptFinished {
                    command_id,
                    operation_id,
                    error,
                    ..
                } => {
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
                    self.command_ledger.clear();
                    self.reject_pending_composer(format!(
                        "desktop runtime failed ({})",
                        truncate_label(&error.code, 28)
                    ));
                }
                desktop::runtime::DesktopRuntimeUpdate::Stopped => {
                    self.command_ledger.clear();
                    self.reject_pending_composer("desktop runtime stopped".into());
                }
                _ => {}
            }
            let outcome = self.projection.apply(update);
            let conversation_dirty = outcome
                .delta()
                .is_some_and(|delta| delta.conversation || delta.tools);
            if outcome.is_replaced() {
                self.conversation_render_full_dirty = true;
                self.conversation_render_live_dirty = true;
            } else if conversation_dirty {
                self.conversation_render_live_dirty = true;
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
            applied += 1;
        }
        if let Some(writer) = &self.preference_writer
            && let Some(error) = writer.take_error()
        {
            self.preference_notice = Some(error);
        }
        if applied > 0 {
            cx.notify();
        }
        !matches!(
            self.projection.lifecycle(),
            DesktopProjectionLifecycle::Stopped
        )
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
        let dockable = ShellLayout::resolve(
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
            return;
        }
        self.preferences.sessions_panel_visible = !self.preferences.sessions_panel_visible;
        let layout = self.layout(window);
        self.focus.reconcile_layout(layout);
        if self.focus.active() == FocusTarget::Composer {
            self.composer_input.focus_handle(cx).focus(window);
        }
        self.schedule_preferences();
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
        let dockable = ShellLayout::resolve(
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
        }
        cx.notify();
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
            cx.notify();
            return;
        }
        self.open_session(session_id, cx);
    }

    fn submit_composer(&mut self, cx: &mut Context<Self>) {
        let intent = DesktopCommandIntent::Prompt;
        let Some(command_id) = self.reserve_command(intent.clone()) else {
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
        cx.notify();
    }

    fn submit_primary_composer(&mut self, cx: &mut Context<Self>) {
        if self.projection.snapshot().active_operation.is_some() {
            self.submit_active_control(ComposerSubmissionKind::Steer, cx);
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
            cx.notify();
            return;
        };
        let payload = match self.composer.begin_submit(command_id, kind) {
            Ok(payload) => payload.to_owned(),
            Err(error) => {
                self.command_ledger.complete(command_id, &intent);
                self.preference_notice = Some(error.to_string());
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
            cx.notify();
            return;
        };
        let intent = DesktopCommandIntent::Abort { operation_id };
        let Some(command_id) = self.reserve_command(intent.clone()) else {
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
            cx.notify();
            return;
        }
        let Some(command_id) = self.reserve_command(intent.clone()) else {
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
            cx.notify();
            return;
        }
        let intent = DesktopCommandIntent::Selection(selection);
        let Some(command_id) = self.reserve_command(intent.clone()) else {
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
        cx.notify();
    }

    fn select_next_model(&mut self, cx: &mut Context<Self>) {
        let Some(model_id) = self.next_model_id() else {
            self.preference_notice = Some("No other configured text model is available.".into());
            cx.notify();
            return;
        };
        self.submit_selection(DesktopRuntimeSelectionKind::Model, model_id, cx);
    }

    fn select_next_session_profile(&mut self, cx: &mut Context<Self>) {
        let Some(profile_id) = self.next_session_profile_id() else {
            self.preference_notice = Some("No other session profile is available.".into());
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
        cx.notify();
    }

    fn copy_selected_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self
            .conversation_viewport
            .copy_selected(self.projection.conversation())
        else {
            self.preference_notice =
                Some("Select a committed conversation block before copying.".into());
            cx.notify();
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.preference_notice = Some("Selected conversation block copied.".into());
        cx.notify();
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
            cx.notify();
            return;
        }
        let command_id = match self.command_ledger.reserve(intent.clone()) {
            Ok(command_id) => command_id,
            Err(error) => {
                self.preference_notice = Some(error.to_string());
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
        cx.notify();
    }

    fn copy_review_path(&mut self, cx: &mut Context<Self>) {
        let DesktopFileReviewState::Ready(document) = &self.file_review else {
            self.preference_notice =
                Some("Load a changed-file review before copying its path.".into());
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
        cx.notify();
    }

    fn copy_file_review(&mut self, cx: &mut Context<Self>) {
        let DesktopFileReviewState::Ready(document) = &self.file_review else {
            self.preference_notice = Some("Load a changed-file review before copying it.".into());
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
        cx.notify();
    }

    fn open_review_in_external_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.preferences.external_editor.clone() else {
            self.preference_notice = Some(
                "Configure desktop.external_editor with a program and literal argv first.".into(),
            );
            cx.notify();
            return;
        };
        let DesktopFileReviewState::Ready(document) = &self.file_review else {
            self.preference_notice = Some("Load a changed-file review before opening it.".into());
            cx.notify();
            return;
        };
        let Some(target) = document.external_editor_target.clone() else {
            self.preference_notice = Some("This review has no external-editor target.".into());
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
        cx.notify();
    }

    fn dismiss_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_overlay = None;
        self.focus.close_overlay(self.layout(window));
        self.focus_active_target(window, cx);
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

    fn on_palette_previous(&mut self, _: &PalettePrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.command_palette.move_selection(true);
        cx.notify();
    }

    fn on_palette_next(&mut self, _: &PaletteNext, _: &mut Window, cx: &mut Context<Self>) {
        self.command_palette.move_selection(false);
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
            let cache_key = format!("{session_id}:{}", block.id);
            rows.push(self.conversation_render_cache.resolve(
                ConversationRowRenderSource {
                    cache_key: &cache_key,
                    row_id: &block.id,
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
            let cache_key = format!("{session_id}:{row_id}");
            rows.push(self.conversation_render_cache.resolve(
                ConversationRowRenderSource {
                    cache_key: &cache_key,
                    row_id: &row_id,
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
            let cache_key = format!("{session_id}:{row_id}");
            rows.push(self.conversation_render_cache.resolve(
                ConversationRowRenderSource {
                    cache_key: &cache_key,
                    row_id: &row_id,
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
            let cache_key = format!("{session_id}:{row_id}");
            rows.push(self.conversation_render_cache.resolve(
                ConversationRowRenderSource {
                    cache_key: &cache_key,
                    row_id: &row_id,
                    source_revision: tool.updated_sequence,
                    title: Cow::Owned(format!("Tool · {} · live", tool.name)),
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
            let cache_key = format!("{session_id}:{row_id}");
            self.conversation_render_rows
                .push(self.conversation_render_cache.resolve(
                    ConversationRowRenderSource {
                        cache_key: &cache_key,
                        row_id: &row_id,
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
            let cache_key = format!("{session_id}:{row_id}");
            self.conversation_render_rows
                .push(self.conversation_render_cache.resolve(
                    ConversationRowRenderSource {
                        cache_key: &cache_key,
                        row_id: &row_id,
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
            let cache_key = format!("{session_id}:{row_id}");
            self.conversation_render_rows
                .push(self.conversation_render_cache.resolve(
                    ConversationRowRenderSource {
                        cache_key: &cache_key,
                        row_id: &row_id,
                        source_revision: tool.updated_sequence,
                        title: Cow::Owned(format!("Tool · {} · live", tool.name)),
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
                    key: row.cache_key.to_string(),
                    target_height: row.measured_height,
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
                    key: row.cache_key.to_string(),
                    target_height: row.measured_height,
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
            self.rebuild_live_conversation_render_rows(layout_width);
            let live_inputs = self.conversation_render_rows[durable_count..]
                .iter()
                .map(|row| ConversationRowLayoutInput {
                    key: row.cache_key.to_string(),
                    target_height: row.measured_height,
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
            self.conversation_render_live_dirty = false;
        }
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
        let requested_layout_width = conversation_width_bucket(layout.conversation.width);
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
        if let Some((delay, requires_full)) = self.prepare_conversation_rows(layout_width) {
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
                            cx.notify();
                        }
                    });
                })
                .detach();
            } else if requires_full {
                self.conversation_height_refresh_full = true;
            }
        }
        let status = self.semantic_status();
        let authorization_request = self
            .projection
            .snapshot()
            .pending_authorizations
            .first()
            .cloned();
        self.reconcile_authorization_overlay(authorization_request.is_some(), window, cx);
        let snapshot = self.projection.snapshot();
        let project = self.projection.project();
        let session_id = truncate_label(&snapshot.session.session_id, 24);
        let cwd = truncate_label(&project.cwd.display().to_string(), 54);
        let operation_count = snapshot.context.operations.len();
        let change_count = snapshot.context.changes.len();
        let delegation_count = snapshot.context.delegations.len();
        let event_count = self.projection.recent_events().len();
        let message_count = self.projection.messages().len();
        let tool_count = self.projection.tools().len();
        let visible_conversation_count = self.visible_conversation_count();
        let unseen_conversation_updates = self.conversation_viewport.unseen_updates();
        let follow_latest_label = if unseen_conversation_updates == 0 {
            "Latest ↓".to_owned()
        } else {
            format!("↓ {unseen_conversation_updates} new")
        };
        let omitted_transcript_count = self.projection.conversation().omitted_blocks();
        let notice = self.preference_notice.clone();
        let composer_pending =
            matches!(self.composer.admission(), ComposerAdmission::Pending { .. });
        let composer_running = snapshot.active_operation.is_some();
        let awaiting_prompt_start = self.composer.submitted().is_some() && !composer_running;
        let composer_disabled = composer_pending || awaiting_prompt_start;
        let abort_pending = self
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Abort { .. }));
        let reload_pending = self.command_ledger.contains(&DesktopCommandIntent::Reload);
        let selection_pending = self
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Selection(_)));
        let recovery_pending = self
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::Recovery { .. }));
        let session_pending = self.command_ledger.contains_where(|intent| {
            matches!(
                intent,
                DesktopCommandIntent::CreateSession | DesktopCommandIntent::OpenSession { .. }
            )
        });
        let session_catalog_pending = self
            .command_ledger
            .contains(&DesktopCommandIntent::ListSessions);
        let file_review_pending = self
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::FileReview { .. }));
        let external_editor_pending = self
            .command_ledger
            .contains_where(|intent| matches!(intent, DesktopCommandIntent::ExternalEditor { .. }));
        let reload_disabled =
            composer_running || awaiting_prompt_start || reload_pending || selection_pending;
        let selector_disabled =
            composer_running || awaiting_prompt_start || reload_pending || selection_pending;
        let changed_file_rows = snapshot
            .context
            .changes
            .iter()
            .take(MAX_VISIBLE_FILE_CHANGES)
            .enumerate()
            .map(|(index, change)| {
                let request = CodingAgentFileReviewRequest::from(change);
                let label = format!(
                    "{}  {}",
                    truncate_label(&change.mutation_kind, 10),
                    truncate_label(&change.path, 38)
                );
                div().child(
                    Button::new(("changed-file-review", index))
                        .compact()
                        .label(label)
                        .tooltip("Load this product-authorized changed-file review")
                        .disabled(composer_running || awaiting_prompt_start || file_review_pending)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.request_file_review(request.clone(), cx);
                        })),
                )
            })
            .collect::<Vec<_>>();
        let omitted_changed_files = change_count.saturating_sub(changed_file_rows.len());
        let file_review_panel = match &self.file_review {
            DesktopFileReviewState::Empty => div()
                .text_sm()
                .text_color(rgb(theme.muted_text.value()))
                .child("Select a changed file to load a product-authorized preview."),
            DesktopFileReviewState::Loading(request) => div()
                .text_sm()
                .text_color(rgb(theme.warning.value()))
                .child(format!(
                    "Loading {}…",
                    truncate_label(&request.change.path, 44)
                )),
            DesktopFileReviewState::Failed { request, code } => {
                let retry = request.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .text_sm()
                    .text_color(rgb(theme.danger.value()))
                    .child(format!(
                        "{} unavailable ({})",
                        truncate_label(&request.change.path, 36),
                        truncate_label(code, 28)
                    ))
                    .child(
                        Button::new("retry-file-review")
                            .compact()
                            .label("Retry review")
                            .tooltip("Retry the current changed-file review")
                            .disabled(
                                composer_running || awaiting_prompt_start || file_review_pending,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.request_file_review(retry.clone(), cx);
                            })),
                    )
            }
            DesktopFileReviewState::Ready(document) => {
                let rows = document
                    .rows
                    .iter()
                    .enumerate()
                    .map(|(index, row)| {
                        let color = match row.kind {
                            DesktopReviewLineKind::Added => theme.success,
                            DesktopReviewLineKind::Removed => theme.danger,
                            DesktopReviewLineKind::FileHeader
                            | DesktopReviewLineKind::HunkHeader => theme.accent,
                            DesktopReviewLineKind::Fold => theme.warning,
                            DesktopReviewLineKind::Context => theme.muted_text,
                        };
                        let marker = match row.kind {
                            DesktopReviewLineKind::Added => "+",
                            DesktopReviewLineKind::Removed => "-",
                            DesktopReviewLineKind::Fold => "…",
                            DesktopReviewLineKind::FileHeader
                            | DesktopReviewLineKind::HunkHeader
                            | DesktopReviewLineKind::Context => " ",
                        };
                        div()
                            .id(("file-review-line", index))
                            .flex()
                            .gap_2()
                            .text_sm()
                            .text_color(rgb(color.value()))
                            .child(marker)
                            .child(row.text.clone())
                    })
                    .collect::<Vec<_>>();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .text_sm()
                    .child(
                        div()
                            .text_color(rgb(theme.text.value()))
                            .child(document.display_path.clone()),
                    )
                    .child(format!(
                        "{} · {} bytes · {} lines · {}",
                        document.mutation_kind,
                        document.total_bytes,
                        document.total_lines,
                        if document.using_diff {
                            "unified diff"
                        } else {
                            "file preview"
                        }
                    ))
                    .when(
                        document.source_truncated || document.rows_truncated,
                        |panel| {
                            panel.child(
                                div()
                                    .text_color(rgb(theme.warning.value()))
                                    .child("Preview bounded at desktop safety limits."),
                            )
                        },
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Button::new("copy-review-path")
                                    .compact()
                                    .label("Copy path")
                                    .tooltip("Copy the reviewed project-relative path")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.copy_review_path(cx);
                                    })),
                            )
                            .child(
                                Button::new("copy-file-review")
                                    .compact()
                                    .label("Copy review")
                                    .tooltip("Copy the bounded read-only file review")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.copy_file_review(cx);
                                    })),
                            )
                            .child(
                                Button::new("open-external-editor")
                                    .compact()
                                    .label("Open editor")
                                    .tooltip(
                                        "Revalidate and open this file in the configured editor",
                                    )
                                    .disabled(
                                        self.preferences.external_editor.is_none()
                                            || external_editor_pending
                                            || composer_running
                                            || awaiting_prompt_start,
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.open_review_in_external_editor(cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .mt_1()
                            .pl_2()
                            .border_l_1()
                            .border_color(rgb(theme.border.value()))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(rows),
                    )
            }
        };
        let composer_rejection = self.composer.rejection().map(str::to_owned);
        let composer_focused = self.composer_input.focus_handle(cx).is_focused(window);
        let conversation_focused = self.conversation_focus.is_focused(window);
        let conversation_focus_accent = conversation_focus_accent(conversation_focused, theme);
        let committed_selection = self
            .conversation_viewport
            .selected_block_id()
            .is_some_and(|id| self.projection.conversation().block(id).is_some());
        let runtime_state = runtime_state_label(self.projection.lifecycle(), composer_running);
        let stream_id = truncate_label(&snapshot.cursor.stream_id, 18);
        let active_operation = snapshot
            .active_operation
            .as_deref()
            .map(|operation_id| truncate_label(operation_id, 24))
            .unwrap_or_else(|| "—".into());
        let selected_model = truncate_label(&project.selected_model_id, 28);
        let selected_profile =
            truncate_label(snapshot.session.default_agent_profile_id.as_str(), 28);
        let status_model = truncate_label(&project.selected_model_id, 14);
        let status_profile = truncate_label(snapshot.session.default_agent_profile_id.as_str(), 12);
        let thinking_label = self
            .thinking_selection
            .label(project.settings.default_thinking_level.as_deref());
        let status_thinking = truncate_label(&thinking_label, 12);
        let model_cycle_available = project
            .models
            .iter()
            .filter(|model| model.supports_text && (model.configured || model.selected))
            .take(2)
            .count()
            > 1;
        let profile_cycle_available = project.profiles.len() > 1;
        let usage = &snapshot.context.usage;
        let usage_cost = usage_cost_label(usage.cost);
        let context_window = usage
            .context_window
            .map(|tokens| tokens.to_string())
            .unwrap_or_else(|| "—".into());
        let latest_recovery = self.projection.recoveries().front().map(|recovery| {
            (
                recovery_status_label(recovery.status),
                truncate_label(&recovery.recovery_id, 22),
                truncate_label(&recovery.operation_id, 22),
                truncate_label(&recovery.reason, 120),
                recovery.attempt_count,
                recovery.identity.clone().filter(|_| {
                    recovery.status == DesktopRecoveryStatus::Pending && recovery.authoritative
                }),
            )
        });
        let latest_diagnostic = self.projection.diagnostics().back().map(|diagnostic| {
            (
                diagnostic.sequence,
                diagnostic
                    .operation_id
                    .as_deref()
                    .map(|operation_id| truncate_label(operation_id, 22))
                    .unwrap_or_else(|| "global".into()),
                truncate_label(&diagnostic.message, 120),
                diagnostic.truncated,
            )
        });
        let latest_config_diagnostic = project.diagnostics.last().map(|diagnostic| {
            (
                truncate_label(&diagnostic.code, 28),
                truncate_label(&diagnostic.summary, 120),
            )
        });
        let latest_issue = self
            .projection
            .issues()
            .back()
            .map(|issue| truncate_label(&issue.code, 28));
        let skill_count = project.resources.skill_names.len();
        let prompt_template_count = project.resources.prompt_template_names.len();
        let context_file_count = project.resources.context_files.len();
        let profile_count = project.profiles.len();
        let model_count = project.models.len();
        let config_diagnostic_count = project.diagnostics.len();
        let transcript_rows = self.conversation_row_sizes.clone();
        let transcript_list = v_virtual_list(
            cx.entity(),
            "conversation-transcript",
            transcript_rows,
            |this, visible_range, window, cx| {
                visible_range
                    .filter_map(|index| {
                        let block = this.conversation_render_rows.get(index)?.clone();
                        let selected = this.conversation_viewport.selected_block_id()
                            == Some(block.row_id.as_ref());
                        let block_id = block.row_id.to_string();
                        let markdown_id = ElementId::Name(SharedString::new(
                            block.markdown_state_key.clone(),
                        ));
                        let detail_markdown_id = ElementId::Name(SharedString::new(
                            block.detail_markdown_state_key.clone(),
                        ));
                        let durable = block.durable;
                        let text_render_mode = conversation_text_render_mode(block.done);
                        let text = block.text.clone();
                        let detail_text = block.detail.clone();
                        let theme = SemanticTheme::GEEK_DARK;
                        let visual =
                            conversation_block_visual(block.kind, block.is_error, theme);
                        let row_height = this
                            .conversation_render_heights
                            .get(index)
                            .copied()
                            .unwrap_or(block.measured_height);
                        let is_assistant = block.kind == ConversationBlockKind::Assistant;
                        let is_tool = block.kind == ConversationBlockKind::Tool;
                        let terminal_label = if block.is_error {
                            Some("failed")
                        } else if !block.done {
                            Some("streaming")
                        } else {
                            None
                        };
                        Some(
                            div()
                                .id(("conversation-block", index))
                                .h(px(row_height))
                                .px_4()
                                .py_1()
                                .flex()
                                .items_start()
                                .when(visual.align_right, |row| row.justify_end())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if durable {
                                        this.conversation_viewport.select(
                                            block_id.clone(),
                                            this.projection.conversation(),
                                        );
                                    } else {
                                        this.conversation_viewport.select_live(block_id.clone());
                                    }
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .w_full()
                                        .h_full()
                                        .when(visual.align_right, |card| {
                                            card.w(relative(0.82))
                                        })
                                        .overflow_hidden()
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(if selected {
                                            rgb(theme.focus_ring.value())
                                        } else {
                                            rgb(visual.accent.value())
                                        })
                                        .bg(rgb(visual.surface.value()))
                                        .px_4()
                                        .py_3()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap_2()
                                                        .child(
                                                            div()
                                                                .px_2()
                                                                .py_1()
                                                                .rounded_md()
                                                                .bg(rgb(
                                                                    theme.elevated.value(),
                                                                ))
                                                                .text_xs()
                                                                .font_weight(
                                                                    gpui::FontWeight::SEMIBOLD,
                                                                )
                                                                .text_color(rgb(
                                                                    visual.accent.value(),
                                                                ))
                                                                .child(visual.glyph),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_sm()
                                                                .font_weight(
                                                                    gpui::FontWeight::MEDIUM,
                                                                )
                                                                .text_color(rgb(
                                                                    theme.text.value(),
                                                                ))
                                                                .child(SharedString::new(
                                                                    block.title.clone(),
                                                                )),
                                                        ),
                                                )
                                                .when_some(
                                                    terminal_label,
                                                    |header, label| {
                                                        header.child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(rgb(
                                                                    visual.accent.value(),
                                                                ))
                                                                .child(label),
                                                        )
                                                    },
                                                ),
                                        )
                                        .when(is_assistant && !detail_text.is_empty(), |card| {
                                            card.child(
                                                div()
                                                    .rounded_md()
                                                    .border_l_3()
                                                    .border_color(rgb(theme.focus_ring.value()))
                                                    .bg(rgb(theme.thinking_surface.value()))
                                                    .px_3()
                                                    .py_2()
                                                    .flex()
                                                    .flex_col()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_weight(
                                                                gpui::FontWeight::SEMIBOLD,
                                                            )
                                                            .text_color(rgb(
                                                                theme.focus_ring.value(),
                                                            ))
                                                            .child("◇ REASONING"),
                                                    )
                                                    .child(conversation_text_element(
                                                        detail_markdown_id.clone(),
                                                        detail_text.clone(),
                                                        text_render_mode,
                                                        window,
                                                        cx,
                                                    )),
                                            )
                                        })
                                        .when(!text.is_empty(), |card| {
                                            card.child(conversation_text_element(
                                                markdown_id,
                                                text,
                                                text_render_mode,
                                                window,
                                                cx,
                                            ))
                                        })
                                        .when(!is_assistant && !detail_text.is_empty(), |card| {
                                            card.child(
                                                div()
                                                    .mt_1()
                                                    .rounded_md()
                                                    .border_1()
                                                    .border_color(rgb(theme.border.value()))
                                                    .bg(rgb(theme.canvas.value()))
                                                    .px_3()
                                                    .py_2()
                                                    .when(is_tool, |detail| {
                                                        detail.font_family("monospace").text_xs()
                                                    })
                                                    .text_color(rgb(theme.muted_text.value()))
                                                    .child(conversation_text_element(
                                                        detail_markdown_id,
                                                        detail_text,
                                                        text_render_mode,
                                                        window,
                                                        cx,
                                                    )),
                                            )
                                        })
                                        .when(block.preview_truncated, |card| {
                                            card.child(
                                                div().text_color(rgb(theme.warning.value())).child(
                                                    "! preview truncated at desktop safety limit",
                                                ),
                                            )
                                        })
                                        .when(block.media_neutralized, |card| {
                                            card.child(
                                                div()
                                                    .text_color(rgb(theme.muted_text.value()))
                                                    .child(
                                                        "remote/inline media disabled in transcript",
                                                    ),
                                            )
                                        })
                                        .when(block.image_count > 0, |card| {
                                            card.child(format!(
                                                "▧ {} retained image attachment(s)",
                                                block.image_count
                                            ))
                                        }),
                                ),
                        )
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&self.conversation_scroll);

        let active_session_id = snapshot.session.session_id.clone();
        let session_rows = self
            .session_catalog
            .iter()
            .enumerate()
            .map(|(index, session)| {
                let target = session.session_id.clone();
                let active = target == active_session_id;
                let label = format!(
                    "{} {}",
                    if active { "●" } else { "○" },
                    truncate_label(&target, 24)
                );
                Button::new(("open-session", index))
                    .compact()
                    .label(label)
                    .tooltip(if active {
                        "Active coding-agent session"
                    } else {
                        "Open this coding-agent session"
                    })
                    .disabled(
                        active || composer_running || awaiting_prompt_start || session_pending,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_session(target.clone(), cx);
                    }))
            })
            .collect::<Vec<_>>();
        let narrow_session_rows = self
            .session_catalog
            .iter()
            .enumerate()
            .map(|(index, session)| {
                let target = session.session_id.clone();
                let active = target == active_session_id;
                Button::new(("narrow-open-session", index))
                    .label(format!(
                        "{} {} · {}",
                        if active { "●" } else { "○" },
                        truncate_label(&target, 32),
                        truncate_label(&session.updated_at, 20)
                    ))
                    .tooltip(if active {
                        "Active coding-agent session"
                    } else {
                        "Open this coding-agent session"
                    })
                    .disabled(
                        active || composer_running || awaiting_prompt_start || session_pending,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_session(target.clone(), cx);
                    }))
            })
            .collect::<Vec<_>>();
        let sessions_panel = layout.sessions.map(|_| {
            div()
                .id("sessions-panel")
                .track_focus(&self.sessions_focus)
                .w(px(SESSION_PANEL_WIDTH as f32))
                .h_full()
                .flex()
                .flex_col()
                .border_r_1()
                .border_color(rgb(theme.border.value()))
                .bg(rgb(theme.surface.value()))
                .when(self.sessions_focus.is_focused(window), |panel| {
                    panel.border_color(rgb(theme.focus_ring.value()))
                })
                .child(
                    div()
                        .h_12()
                        .px_4()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(rgb(theme.border.value()))
                        .child("SESSIONS")
                        .child(
                            Button::new("create-session")
                                .compact()
                                .label("New")
                                .tooltip("Create a new session · Ctrl/Cmd+N")
                                .disabled(
                                    composer_running || awaiting_prompt_start || session_pending,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.create_session(cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .rounded_md()
                                .p_3()
                                .bg(rgb(theme.elevated.value()))
                                .child(session_id),
                        )
                        .child(
                            Button::new("refresh-session-catalog")
                                .compact()
                                .label(if session_catalog_pending {
                                    "Loading sessions…"
                                } else {
                                    "Refresh sessions"
                                })
                                .tooltip("Load the bounded project session catalog")
                                .disabled(session_catalog_pending || composer_running)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.request_session_catalog(cx);
                                })),
                        )
                        .children(session_rows)
                        .when(self.omitted_sessions > 0, |panel| {
                            panel.child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(theme.warning.value()))
                                    .child(format!(
                                        "+ {} older session(s) omitted",
                                        self.omitted_sessions
                                    )),
                            )
                        }),
                )
        });

        let context_is_overlay = self.narrow_context_open;
        let context_panel = (layout.context.is_some() || context_is_overlay).then(|| {
            div()
                .id("context-panel")
                .when(context_is_overlay, |panel| {
                    panel
                        .key_context(actions::NARROW_CONTEXT_KEY_CONTEXT)
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .occlude()
                })
                .track_focus(&self.context_focus)
                .w(px(CONTEXT_PANEL_WIDTH as f32))
                .h_full()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(rgb(theme.border.value()))
                .bg(rgb(theme.surface.value()))
                .when(self.context_focus.is_focused(window), |panel| {
                    panel.border_color(rgb(theme.focus_ring.value()))
                })
                .child(
                    div()
                        .h_12()
                        .px_4()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(rgb(theme.border.value()))
                        .child("CONTEXT")
                        .child("Tab focus"),
                )
                .child(
                    div()
                        .id("context-details")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(div().text_color(rgb(theme.accent.value())).child("RUNTIME"))
                        .child(format!("state       {runtime_state}"))
                        .child(format!("stream      {stream_id}"))
                        .child(format!(
                            "sequence    {}",
                            snapshot.cursor.last_event_sequence
                        ))
                        .child(format!(
                            "generation  {}",
                            snapshot.cursor.capability_generation
                        ))
                        .child(format!("active op   {active_operation}"))
                        .child(
                            div()
                                .mt_2()
                                .text_color(rgb(theme.accent.value()))
                                .child("WORK"),
                        )
                        .child(format!("operations  {operation_count:>4}"))
                        .child(format!("changes     {change_count:>4}"))
                        .child(format!("delegations {delegation_count:>4}"))
                        .child(
                            div()
                                .mt_2()
                                .text_color(rgb(theme.accent.value()))
                                .child("CHANGED FILES"),
                        )
                        .children(changed_file_rows)
                        .when(omitted_changed_files > 0, |panel| {
                            panel.child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(theme.warning.value()))
                                    .child(format!(
                                        "+ {omitted_changed_files} more change(s) omitted at the desktop file-count limit"
                                    )),
                            )
                        })
                        .child(
                            div()
                                .mt_2()
                                .text_color(rgb(theme.accent.value()))
                                .child("FILE REVIEW"),
                        )
                        .child(file_review_panel)
                        .child(format!(
                            "diagnostics {diagnostic_count:>4}",
                            diagnostic_count = self.projection.diagnostics().len()
                        ))
                        .child(format!(
                            "recoveries  {recovery_count:>4}",
                            recovery_count = self.projection.recoveries().len()
                        ))
                        .child(
                            div()
                                .mt_2()
                                .text_color(rgb(theme.accent.value()))
                                .child("USAGE"),
                        )
                        .child(format!("input       {}", usage.input))
                        .child(format!("output      {}", usage.output))
                        .child(format!("cache read  {}", usage.cache_read))
                        .child(format!("cache write {}", usage.cache_write))
                        .child(format!(
                            "tokens      {}",
                            usage.input.saturating_add(usage.output)
                        ))
                        .child(format!("context     {context_window}"))
                        .child(format!("cost        {usage_cost}"))
                        .child(
                            div()
                                .mt_2()
                                .text_color(rgb(theme.accent.value()))
                                .child("LOCAL RESOURCES"),
                        )
                        .child(format!("model       {selected_model}"))
                        .child(format!("profile     {selected_profile}"))
                        .child(format!("thinking    {thinking_label}"))
                        .child(format!("models      {model_count}"))
                        .child(format!("profiles    {profile_count}"))
                        .child(format!("skills      {skill_count}"))
                        .child(format!("prompts     {prompt_template_count}"))
                        .child(format!("context     {context_file_count}"))
                        .child(format!("config diag {config_diagnostic_count}"))
                        .when_some(latest_recovery, |panel, recovery| {
                            panel
                                .child(
                                    div()
                                        .mt_2()
                                        .text_color(rgb(theme.warning.value()))
                                        .child("LATEST RECOVERY"),
                                )
                                .child(format!("status      {}", recovery.0))
                                .child(format!("recovery    {}", recovery.1))
                                .child(format!("operation   {}", recovery.2))
                                .child(format!("attempts    {}", recovery.4))
                                .child(format!("detail      {}", recovery.3))
                                .when_some(recovery.5, |panel, identity| {
                                    let retry_identity = identity.clone();
                                    let failed_identity = identity.clone();
                                    panel.child(
                                        div()
                                            .mt_2()
                                            .flex()
                                            .flex_wrap()
                                            .gap_2()
                                            .child(
                                                Button::new("retry-recovery")
                                                    .compact()
                                                    .label("Retry")
                                                    .tooltip("Retry this authoritative recovery")
                                                    .disabled(recovery_pending)
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.submit_recovery_action(
                                                                retry_identity.clone(),
                                                                DesktopRecoveryAction::Retry,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new("fail-recovery")
                                                    .compact()
                                                    .label("Mark failed")
                                                    .tooltip("Resolve this recovery as failed")
                                                    .disabled(recovery_pending)
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.submit_recovery_action(
                                                                failed_identity.clone(),
                                                                DesktopRecoveryAction::MarkFailed,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new("abort-recovery")
                                                    .compact()
                                                    .label("Abort")
                                                    .tooltip("Resolve this recovery as aborted")
                                                    .disabled(recovery_pending)
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.submit_recovery_action(
                                                                identity.clone(),
                                                                DesktopRecoveryAction::Abort,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            ),
                                    )
                                })
                        })
                        .when_some(latest_diagnostic, |panel, diagnostic| {
                            panel
                                .child(
                                    div()
                                        .mt_2()
                                        .text_color(rgb(theme.warning.value()))
                                        .child("LATEST DIAGNOSTIC"),
                                )
                                .child(format!("sequence    {}", diagnostic.0))
                                .child(format!("operation   {}", diagnostic.1))
                                .child(format!("detail      {}", diagnostic.2))
                                .when(diagnostic.3, |panel| panel.child("detail      [truncated]"))
                        })
                        .when_some(latest_config_diagnostic, |panel, diagnostic| {
                            panel
                                .child(
                                    div()
                                        .mt_2()
                                        .text_color(rgb(theme.warning.value()))
                                        .child("LATEST CONFIG DIAGNOSTIC"),
                                )
                                .child(format!("code        {}", diagnostic.0))
                                .child(format!("detail      {}", diagnostic.1))
                        })
                        .when_some(latest_issue, |panel, issue_code| {
                            panel
                                .child(
                                    div()
                                        .mt_2()
                                        .text_color(rgb(theme.danger.value()))
                                        .child("LATEST ISSUE"),
                                )
                                .child(format!("code        {issue_code}"))
                        })
                        .child(
                            div()
                                .mt_3()
                                .text_sm()
                                .text_color(rgb(theme.muted_text.value()))
                                .child(cwd),
                        ),
                )
        });

        let conversation = div()
            .id("conversation-panel")
            .track_focus(&self.conversation_focus)
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(theme.canvas.value()))
            .child(
                div()
                    .h_12()
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(conversation_focus_accent.value()))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(rgb(if conversation_focused {
                                theme.accent.value()
                            } else {
                                theme.text.value()
                            }))
                            .child("EVO · CONVERSATION"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("toggle-sessions")
                                    .compact()
                                    .label("Sessions")
                                    .tooltip("Show or hide Sessions")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_sessions(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("toggle-context")
                                    .compact()
                                    .label("Context")
                                    .tooltip("Show or hide Context · Ctrl/Cmd+\\")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_context(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("reload-local-resources")
                                    .compact()
                                    .label(if reload_pending {
                                        "Reloading…"
                                    } else {
                                        "Reload"
                                    })
                                    .tooltip("Reload product-owned local resources")
                                    .disabled(reload_disabled)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.reload_local_resources(cx);
                                    })),
                            )
                            .child(
                                Button::new("copy-conversation-block")
                                    .compact()
                                    .label("Copy")
                                    .tooltip("Copy the selected durable conversation block")
                                    .disabled(!committed_selection)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.copy_selected_conversation(cx);
                                    })),
                            )
                            .when(composer_running, |actions| {
                                actions.child(
                                    Button::new("abort-operation")
                                        .compact()
                                        .label(if abort_pending {
                                            "Aborting…"
                                        } else {
                                            "Abort"
                                        })
                                        .tooltip("Abort the active operation · Ctrl/Cmd+Esc")
                                        .disabled(abort_pending)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.abort_active_operation(cx);
                                        })),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .when(visible_conversation_count == 0, |content| {
                        content.child(
                            div()
                                .p_5()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .text_color(rgb(theme.muted_text.value()))
                                .child("Native runtime connected")
                                .child("No durable conversation blocks yet.")
                                .child(format!("project events  {event_count}"))
                                .child(format!("message overlays  {message_count}"))
                                .child(format!("tool overlays     {tool_count}")),
                        )
                    })
                    .when(visible_conversation_count > 0, |content| {
                        content
                            .when(omitted_transcript_count > 0, |content| {
                                content.child(
                                    div()
                                        .px_4()
                                        .py_2()
                                        .text_color(rgb(theme.warning.value()))
                                        .child(format!(
                                            "{omitted_transcript_count} older blocks omitted by \
                                             desktop retention bounds"
                                        )),
                                )
                            })
                            .child(
                                div()
                                    .id("conversation-scroll-region")
                                    .flex_1()
                                    .min_h_0()
                                    .on_scroll_wheel(cx.listener(|_, _, window, cx| {
                                        cx.defer_in(window, |this, _, cx| {
                                            this.reconcile_conversation_scroll(cx);
                                        });
                                    }))
                                    .child(transcript_list),
                            )
                            .when(!self.conversation_viewport.follow_latest(), |content| {
                                content.child(
                                    div()
                                        .absolute()
                                        .right_4()
                                        .bottom_4()
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(rgb(theme.accent.value()))
                                        .bg(rgb(theme.elevated.value()))
                                        .child(
                                            Button::new("follow-latest")
                                                .compact()
                                                .label(follow_latest_label.clone())
                                                .tooltip("Jump to latest output · End")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.follow_latest(cx);
                                                })),
                                        ),
                                )
                            })
                    }),
            )
            .child(
                div()
                    .id("composer-panel")
                    .min_h(px(COMPOSER_MIN_HEIGHT))
                    .max_h(px(COMPOSER_MAX_HEIGHT))
                    .flex_shrink_0()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(theme.border.value()))
                    .bg(rgb(theme.canvas.value()))
                    .when(composer_focused, |composer| {
                        composer.border_color(rgb(theme.focus_ring.value()))
                    })
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .gap_2()
                            .rounded_lg()
                            .border_1()
                            .border_color(rgb(theme.border.value()))
                            .bg(rgb(theme.elevated.value()))
                            .p_2()
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&self.composer_input)
                                        .appearance(false)
                                        .bordered(false)
                                        .focus_bordered(false)
                                        .disabled(composer_disabled),
                                ),
                            )
                            .child(
                                div()
                                    .w(px(116.))
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .justify_end()
                                    .when(!composer_running, |actions| {
                                        actions.child(
                                            Button::new("submit-composer")
                                                .label(if composer_pending {
                                                    "Sending…"
                                                } else {
                                                    "Send"
                                                })
                                                .tooltip("Send the composer draft · Ctrl/Cmd+Enter")
                                                .disabled(composer_disabled)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.submit_composer(cx);
                                                })),
                                        )
                                    })
                                    .when(composer_running, |actions| {
                                        actions
                                            .child(
                                                Button::new("steer-operation")
                                                    .compact()
                                                    .label(if composer_pending {
                                                        "Sending…"
                                                    } else {
                                                        "Steer"
                                                    })
                                                    .tooltip(
                                                        "Send the composer draft as steering input",
                                                    )
                                                    .disabled(composer_disabled)
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.submit_active_control(
                                                            ComposerSubmissionKind::Steer,
                                                            cx,
                                                        );
                                                    })),
                                            )
                                            .child(
                                                Button::new("follow-up-operation")
                                                    .compact()
                                                    .label("Follow up")
                                                    .tooltip(
                                                        "Queue the composer draft as a follow-up",
                                                    )
                                                    .disabled(composer_disabled)
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.submit_active_control(
                                                            ComposerSubmissionKind::FollowUp,
                                                            cx,
                                                        );
                                                    })),
                                            )
                                    })
                                    .when_some(composer_rejection, |actions, rejection| {
                                        actions.child(
                                            div()
                                                .text_xs()
                                                .text_color(rgb(theme.danger.value()))
                                                .child(truncate_label(&rejection, 22)),
                                        )
                                    }),
                            ),
                    ),
            );

        let status_bar = div()
            .id("status-panel")
            .track_focus(&self.status_focus)
            .h(px(STATUS_HEIGHT as f32))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(rgb(theme.border.value()))
            .bg(rgb(theme.elevated.value()))
            .when(self.status_focus.is_focused(window), |bar| {
                bar.border_color(rgb(theme.focus_ring.value()))
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .text_color(self.status_color(status))
                    .child(status.glyph())
                    .child(status.label()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_color(rgb(theme.muted_text.value()))
                    .child(
                        Button::new("cycle-model")
                            .compact()
                            .label(format!("M {status_model}"))
                            .tooltip("Select the next configured text model")
                            .disabled(selector_disabled || !model_cycle_available)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.select_next_model(cx);
                            })),
                    )
                    .child(
                        Button::new("cycle-session-profile")
                            .compact()
                            .label(format!("P {status_profile}"))
                            .tooltip("Select the next session agent profile")
                            .disabled(selector_disabled || !profile_cycle_available)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.select_next_session_profile(cx);
                            })),
                    )
                    .child(
                        Button::new("cycle-thinking")
                            .compact()
                            .label(format!("T {status_thinking}"))
                            .tooltip("Cycle the composer thinking override")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cycle_thinking_selection(cx);
                            })),
                    )
                    .child(format!(
                        "seq {}",
                        self.projection.cursor().last_event_sequence
                    ))
                    .child(if self.preferences.reduced_motion {
                        "motion reduced"
                    } else {
                        "motion static"
                    })
                    .child("commands Ctrl/Cmd+K · focus Ctrl+Tab")
                    .when_some(notice, |bar, notice| {
                        bar.child(
                            div()
                                .text_color(rgb(theme.warning.value()))
                                .child(truncate_label(&notice, 28)),
                        )
                    }),
            );

        let palette_rows = PALETTE_ENTRIES
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let command = entry.command;
                let selected = self.command_palette.selected() == index;
                let label = entry.shortcut.map_or_else(
                    || entry.label.to_owned(),
                    |shortcut| format!("{}    {shortcut}", entry.label),
                );
                div()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if selected {
                        theme.focus_ring.value()
                    } else {
                        theme.border.value()
                    }))
                    .child(
                        Button::new(("palette-command", index))
                            .label(label)
                            .tooltip(entry.semantic_label)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.command_palette.close();
                                this.dismiss_overlay(window, cx);
                                this.execute_palette_command(command, window, cx);
                            })),
                    )
            })
            .collect::<Vec<_>>();
        let command_palette_overlay = self.command_palette.is_open().then(|| {
            let max_height = px((f32::from(window.viewport_size().height) * 0.8).max(320.));
            div()
                .id("command-palette-overlay")
                .key_context(actions::PALETTE_KEY_CONTEXT)
                .absolute()
                .size_full()
                .occlude()
                .track_focus(&self.command_palette_focus)
                .bg(rgba(0x0b0e14dd))
                .p_4()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .id("command-palette-dialog")
                        .w_full()
                        .max_w(px(680.))
                        .max_h(max_height)
                        .overflow_y_scroll()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(theme.focus_ring.value()))
                        .bg(rgb(theme.elevated.value()))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_color(rgb(theme.accent.value()))
                                .child("COMMAND PALETTE · typed desktop actions"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(theme.muted_text.value()))
                                .child("Up/Down or Tab selects · Enter runs · Esc closes"),
                        )
                        .children(palette_rows),
                )
        });

        let narrow_sessions_overlay = self.narrow_sessions_open.then(|| {
            let max_height = px((f32::from(window.viewport_size().height) * 0.8).max(320.));
            div()
                .id("narrow-sessions-overlay")
                .key_context(actions::NARROW_SESSIONS_KEY_CONTEXT)
                .absolute()
                .size_full()
                .occlude()
                .track_focus(&self.narrow_sessions_focus)
                .bg(rgba(0x0b0e14dd))
                .p_4()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .id("narrow-sessions-dialog")
                        .w_full()
                        .max_w(px(520.))
                        .max_h(max_height)
                        .overflow_y_scroll()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(theme.focus_ring.value()))
                        .bg(rgb(theme.elevated.value()))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .text_color(rgb(theme.accent.value()))
                                .child("SESSIONS · narrow layout dialog")
                                .child("Esc closes"),
                        )
                        .child(
                            Button::new("narrow-create-session")
                                .label("New session · Ctrl/Cmd+N")
                                .tooltip("Create a new coding-agent session")
                                .disabled(
                                    composer_running || awaiting_prompt_start || session_pending,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.create_session(cx);
                                })),
                        )
                        .child(
                            Button::new("narrow-refresh-sessions")
                                .label(if session_catalog_pending {
                                    "Loading sessions…"
                                } else {
                                    "Refresh sessions"
                                })
                                .tooltip("Load the bounded project session catalog")
                                .disabled(session_catalog_pending || composer_running)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.request_session_catalog(cx);
                                })),
                        )
                        .children(narrow_session_rows)
                        .when(self.omitted_sessions > 0, |dialog| {
                            dialog.child(div().text_color(rgb(theme.warning.value())).child(
                                format!(
                                    "{} older session(s) omitted at the desktop limit",
                                    self.omitted_sessions
                                ),
                            ))
                        }),
                )
        });

        let authorization_overlay = authorization_request.map(|request| {
            let decision_pending = self.command_ledger.authorization().is_some_and(
                |(_, authorization_id, operation_id)| {
                    authorization_id == request.authorization_id
                        && operation_id == request.operation_id
                },
            );
            let mut details = vec![
                format!("operation  {}", request.operation_id),
                format!(
                    "tool       {} · {}",
                    request.tool_name, request.tool_call_id
                ),
                format!("risk       {:?}", request.risk),
                format!("scope      {}", authorization_scope_text(&request.scope)),
            ];
            if let Some(path) = request.preview.path.as_ref() {
                details.push(format!("path       {path}"));
            }
            if let Some(cwd) = request.preview.cwd.as_ref() {
                details.push(format!("cwd        {cwd}"));
            }
            if let Some(command) = request.preview.command.as_ref() {
                details.push(format!("command\n{command}"));
            }
            if let Some(content) = request.preview.content_preview.as_ref() {
                details.push(format!("content preview\n{content}"));
            }
            let identity = request.identity();
            let allow_once_identity = identity.clone();
            let allow_operation_identity = identity.clone();
            let deny_identity = identity;
            let max_height = px((f32::from(window.viewport_size().height) * 0.8).max(320.));
            div()
                .id("authorization-overlay")
                .key_context(actions::AUTHORIZATION_KEY_CONTEXT)
                .absolute()
                .size_full()
                .occlude()
                .track_focus(&self.authorization_focus)
                .bg(rgba(0x0b0e14dd))
                .p_4()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w_full()
                        .max_w(px(720.))
                        .max_h(max_height)
                        .overflow_hidden()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(theme.warning.value()))
                        .bg(rgb(theme.elevated.value()))
                        .p_5()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .text_color(rgb(theme.warning.value()))
                                .child("AUTHORIZATION REQUIRED")
                                .child(if decision_pending {
                                    "decision pending…"
                                } else {
                                    "explicit decision required"
                                }),
                        )
                        .child(
                            div()
                                .text_color(rgb(theme.text.value()))
                                .whitespace_normal()
                                .child(request.preview.summary),
                        )
                        .child(
                            div()
                                .id("authorization-details")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .children(details.into_iter().map(|detail| {
                                    div()
                                        .whitespace_normal()
                                        .text_color(rgb(theme.muted_text.value()))
                                        .child(detail)
                                })),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("deny-authorization")
                                        .label("1 · Deny")
                                        .tooltip("Deny this authorization request · 1")
                                        .disabled(decision_pending)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.decide_tool_authorization(
                                                deny_identity.clone(),
                                                ToolAuthorizationDecision::Deny {
                                                    reason: Some(
                                                        "denied from native desktop".into(),
                                                    ),
                                                },
                                                cx,
                                            );
                                        })),
                                )
                                .child(
                                    Button::new("allow-authorization-once")
                                        .label("2 · Allow once")
                                        .tooltip("Allow this exact request once · 2")
                                        .disabled(decision_pending)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.decide_tool_authorization(
                                                allow_once_identity.clone(),
                                                ToolAuthorizationDecision::AllowOnce,
                                                cx,
                                            );
                                        })),
                                )
                                .child(
                                    Button::new("allow-authorization-operation")
                                        .label("3 · Allow for operation")
                                        .tooltip("Allow this scope for the current operation · 3")
                                        .disabled(decision_pending)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.decide_tool_authorization(
                                                allow_operation_identity.clone(),
                                                ToolAuthorizationDecision::AllowForOperation,
                                                cx,
                                            );
                                        })),
                                ),
                        ),
                )
        });

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
            .on_action(cx.listener(Self::on_palette_previous))
            .on_action(cx.listener(Self::on_palette_next))
            .on_action(cx.listener(Self::on_palette_confirm))
            .on_action(cx.listener(Self::on_authorization_deny))
            .on_action(cx.listener(Self::on_authorization_allow_once))
            .on_action(cx.listener(Self::on_authorization_allow_for_operation))
            .on_action(cx.listener(Self::on_trap_overlay_focus))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .font_family("monospace")
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
            .children(narrow_sessions_overlay)
            .children(command_palette_overlay)
            .children(authorization_overlay)
    }
}

fn authorization_scope_text(scope: &ToolAuthorizationScope) -> String {
    match scope {
        ToolAuthorizationScope::Path { path } => format!("path · {path}"),
        ToolAuthorizationScope::FilesystemTarget {
            path,
            target_fingerprint,
        } => format!("filesystem target · {path} · {target_fingerprint}"),
        ToolAuthorizationScope::Shell {
            cwd,
            command_fingerprint,
        } => format!("shell · {cwd} · {command_fingerprint}"),
        ToolAuthorizationScope::ToolArguments { fingerprint } => {
            format!("tool arguments · {fingerprint}")
        }
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
    fn conversation_bottom_distance_matches_negative_gpui_offsets() {
        assert_eq!(conversation_distance_to_bottom(0.0, 640.0), 640.0);
        assert_eq!(conversation_distance_to_bottom(-400.0, 640.0), 240.0);
        assert_eq!(conversation_distance_to_bottom(-640.0, 640.0), 0.0);
        assert_eq!(conversation_distance_to_bottom(-641.0, 640.0), 0.0);
        assert_eq!(conversation_distance_to_bottom(4.0, 0.0), 0.0);
    }

    #[test]
    fn conversation_kinds_have_distinct_visual_surfaces() {
        let theme = SemanticTheme::GEEK_DARK;
        let user = conversation_block_visual(ConversationBlockKind::User, false, theme);
        let assistant = conversation_block_visual(ConversationBlockKind::Assistant, false, theme);
        let tool = conversation_block_visual(ConversationBlockKind::Tool, false, theme);
        let failed_tool = conversation_block_visual(ConversationBlockKind::Tool, true, theme);
        let diagnostic = conversation_block_visual(ConversationBlockKind::Diagnostic, true, theme);

        assert!(user.align_right);
        assert!(!assistant.align_right);
        assert_ne!(user.surface, assistant.surface);
        assert_ne!(assistant.surface, tool.surface);
        assert_ne!(tool.surface, failed_tool.surface);
        assert_eq!(failed_tool.surface, diagnostic.surface);
        assert_ne!(tool.accent, failed_tool.accent);
    }

    #[test]
    fn conversation_focus_uses_the_existing_header_divider_without_panel_geometry() {
        let theme = SemanticTheme::GEEK_DARK;
        assert_eq!(conversation_focus_accent(false, theme), theme.border);
        assert_eq!(conversation_focus_accent(true, theme), theme.accent);

        let source = include_str!("native_shell.rs");
        let conversation_start = source
            .find("let conversation = div()")
            .expect("conversation panel source remains present");
        let composer_start = source[conversation_start..]
            .find(".id(\"composer-panel\")")
            .map(|offset| conversation_start + offset)
            .expect("composer follows the conversation transcript");
        let conversation_source = &source[conversation_start..composer_start];

        assert!(conversation_source.contains("conversation_focus_accent.value()"));
        assert!(!conversation_source.contains("panel.border_1()"));
    }

    #[test]
    fn conversation_streaming_text_avoids_debounced_markdown_until_final() {
        assert_eq!(
            conversation_text_render_mode(false),
            ConversationTextRenderMode::StreamingPlainText
        );
        assert_eq!(
            conversation_text_render_mode(true),
            ConversationTextRenderMode::FinalMarkdown
        );

        let source = include_str!("native_shell.rs");
        assert!(source.contains("block.markdown_state_key.clone()"));
        assert!(source.contains("block.detail_markdown_state_key.clone()"));
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

        let render = source
            .split_once("impl Render for NativeShell")
            .expect("native render implementation remains present")
            .1;
        let prepare_call = ["prepare_conversation_", "rows("].concat();
        let legacy_rebuild_call = ["rebuild_conversation_", "render_rows("].concat();
        assert_eq!(render.matches(&prepare_call).count(), 1);
        assert_eq!(render.matches(&legacy_rebuild_call).count(), 0);
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
