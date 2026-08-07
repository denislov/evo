use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tui::api::component::OverlayHandle;
use tui::api::component::{Component, Editor, SettingItem, SettingsList, SettingsListOptions};
use tui::api::input::{
    CombinedAutocompleteProvider, InputEvent, Key, KeyEventKind, KeyModifiers, KeybindingsManager,
    MouseButton, MouseEvent, MouseEventKind, matches_key,
};
use tui::api::render::{
    Constraint, FocusRing, Frame, HitMap, HitRegion, Layout, Point, Rect, STATUS_IDLE,
    STATUS_RUNNING, SYSTEM, Style, USER, color_enabled, paint_with, visible_width,
};
use tui::api::terminal::TerminalCapabilities;
use tui::api::theme::{MarkdownTheme, TuiTheme};

use crate::interactive::app::{PromptContext, welcome_line};
use crate::interactive::clipboard::{ClipboardSink, SystemClipboard};
use crate::interactive::commands;
use crate::interactive::delegation_confirmation_menu::{
    DelegationConfirmationMenuOutcome, DelegationConfirmationMenuRenderState,
    DelegationConfirmationMenuState,
};
use crate::interactive::event_bridge::{MAX_CHILD_CONVERSATIONS, UiProjection};
use crate::interactive::input;
use crate::interactive::keybindings;
use crate::interactive::model_selector;
use crate::interactive::profile_menu::{
    PendingProfileTask, ProfileMenuOutcome, ProfileMenuRenderState, ProfileMenuState,
};
use crate::interactive::render::{
    TranscriptBlockRows, TranscriptRenderCache, TranscriptRenderOptions, TranscriptRowSnapshot,
    TranscriptStyles, WARNING, abbreviate_cwd, editor_border_line, fit_line, format_tokens,
    framed_modal_lines, markdown_theme_from_resolved, running_status_text,
};
use crate::interactive::session_actions::{HydratedSession, SessionChoice};
use crate::interactive::session_selector;
use crate::interactive::slash::{self, ParsedSlashCommand};
use crate::interactive::transcript::TranscriptMutation;
use crate::interactive::transcript::{TranscriptBlockId, TranscriptViewState};
use crate::interactive::transient_overlay::TransientOverlayBridge;
use crate::interactive::tree_selector::{TreeSelectorInput, TreeSelectorState};
use crate::interactive::{Transcript, TranscriptItem, UiEvent};
use coding_agent::api::authorization::{
    ToolAuthorizationDecision, ToolAuthorizationMode, ToolAuthorizationRequest,
    ToolAuthorizationRisk,
};
use coding_agent::api::client::{
    CodingAgentContextSnapshot, CodingAgentFileChangeSnapshot, CodingAgentOperationSnapshot,
    CodingAgentOperationStatus, CodingAgentSnapshot,
};
use coding_agent::api::embedding::{
    CodingAgentAuthCommand, CodingAgentAuthSnapshot, CodingAgentModelCatalogEntry,
    CodingAgentProfileCatalog, CodingAgentResourceCommand, CodingAgentResourceCommandKind,
    CodingAgentSessionQuery, CodingAgentThinkingLevel,
};
use coding_agent::api::event::CodingAgentProductEvent as ProductEvent;
use coding_agent::api::operation::{
    PendingDelegationConfirmation, PromptInvocation, SelfHealingEditReplacement,
};
use coding_agent::api::settings::{
    CodingAgentDoubleEscapeAction, CodingAgentQueueMode, CodingAgentSettingsCommand,
    CodingAgentSettingsSnapshot, CodingAgentThemeForeground, CodingAgentThemeSnapshot,
};

mod context;
mod layout;
mod menus;
mod session_state;
mod settings;
mod shell;
mod shell_input;
mod state;
mod transcript;

use coding_agent::api::view::{CapabilityStatus, CodingAgentCapabilities, ProfileId};
use layout::*;

const MAX_TOOL_RESULT_LINES: usize = 3;
const WIDE_LAYOUT_MIN_WIDTH: usize = 100;
const MEDIUM_LAYOUT_MIN_WIDTH: usize = 64;
const TIPS_MIN_HEIGHT: usize = 18;
const MAX_COMPOSER_HEIGHT: usize = 8;
const MOUSE_SCROLL_ROWS: usize = 3;
pub(super) const DOUBLE_ESCAPE_WINDOW: Duration = Duration::from_millis(500);

const HTTP_IDLE_TIMEOUT_CHOICES: [(&str, u64); 5] = [
    ("30 sec", 30_000),
    ("1 min", 60_000),
    ("2 min", 120_000),
    ("5 min", 300_000),
    ("disabled", 0),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InteractiveAction {
    None,
    Submit,
    FollowUp,
    CompactSession,
    BranchSummary,
    SelfHealingEdit,
    DelegationConfirmation,
    ToolAuthorization,
    AgentProfileUse,
    AgentInvocation,
    AgentTeam,
    MergeReview,
    AbortRunning,
    NewSession,
    ReloadResources,
    Fork,
    Exit,
}

#[derive(Debug, Clone)]
pub(super) enum PendingInteractiveCommand {
    Submit(String),
    SubmitResource {
        display_text: String,
        invocation: PromptInvocation,
    },
    FollowUp(String),
    Compact {
        instructions: Option<String>,
    },
    BranchSummary(PendingBranchSummaryRequest),
    Fork(PendingForkRequest),
    AgentInvocation(PendingAgentInvocationRequest),
    AgentTeam(PendingAgentTeamRequest),
    SelfHealingEdit(PendingSelfHealingEditRequest),
    MergeReview(PendingMergeReviewRequest),
    UseAgentProfile(ProfileId),
}

impl PendingInteractiveCommand {
    pub(super) const fn action(&self) -> InteractiveAction {
        match self {
            Self::Submit(_) | Self::SubmitResource { .. } => InteractiveAction::Submit,
            Self::FollowUp(_) => InteractiveAction::FollowUp,
            Self::Compact { .. } => InteractiveAction::CompactSession,
            Self::BranchSummary(_) => InteractiveAction::BranchSummary,
            Self::Fork(_) => InteractiveAction::Fork,
            Self::AgentInvocation(_) => InteractiveAction::AgentInvocation,
            Self::AgentTeam(_) => InteractiveAction::AgentTeam,
            Self::SelfHealingEdit(_) => InteractiveAction::SelfHealingEdit,
            Self::MergeReview(_) => InteractiveAction::MergeReview,
            Self::UseAgentProfile(_) => InteractiveAction::AgentProfileUse,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PendingMergeReviewRequest {
    List,
    Merge(String),
    Discard(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveRegion {
    Conversation,
    Context,
    Composer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextTab {
    Ops,
    Changes,
    Agents,
    Usage,
}

fn operation_status_as_str(status: CodingAgentOperationStatus) -> &'static str {
    match status {
        CodingAgentOperationStatus::Running => "running",
        CodingAgentOperationStatus::Completed => "completed",
        CodingAgentOperationStatus::Failed => "failed",
        CodingAgentOperationStatus::Aborted => "aborted",
        CodingAgentOperationStatus::Recovered => "recovered",
    }
}

fn operation_status_is_running(status: CodingAgentOperationStatus) -> bool {
    status == CodingAgentOperationStatus::Running
}

impl ContextTab {
    const ALL: [Self; 4] = [Self::Ops, Self::Changes, Self::Agents, Self::Usage];

    fn label(self) -> &'static str {
        match self {
            Self::Ops => "ops",
            Self::Changes => "changes",
            Self::Agents => "agents",
            Self::Usage => "usage",
        }
    }

    fn compact_label(self) -> &'static str {
        match self {
            Self::Ops => "ops",
            Self::Changes => "chg",
            Self::Agents => "ag",
            Self::Usage => "use",
        }
    }

    fn next(self) -> Self {
        let index = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    fn previous(self) -> Self {
        let index = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        Self::ALL[index.checked_sub(1).unwrap_or(Self::ALL.len() - 1)]
    }

    const fn index(self) -> usize {
        match self {
            Self::Ops => 0,
            Self::Changes => 1,
            Self::Agents => 2,
            Self::Usage => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContextDetail {
    title: String,
    lines: Vec<String>,
    scroll: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContextListItem {
    summary: String,
    detail_title: String,
    detail_lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellLayoutMode {
    Wide,
    Medium,
    Narrow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TransientOverlayProjection {
    pub(super) modal_visible: bool,
    pub(super) support_visible: bool,
    pub(super) bottom_margin: usize,
    pub(super) support_role: TransientOverlayRole,
    pub(super) modal_role: TransientOverlayRole,
}

/// Product-owned placement policy for fullscreen transient surfaces.
///
/// The generic overlay host in `tui` only understands geometry.  Keeping
/// these roles here prevents a modal dialog from accidentally inheriting the
/// composer-assistance placement policy (or vice versa).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TransientOverlayRole {
    ComposerAssistance,
    SupportPrompt,
    ModalDialog,
    ContextRailDetail,
    ContextDrawerDetail,
    ContextPageDetail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ShellLayout {
    mode: ShellLayoutMode,
    conversation: Rect,
    conversation_context_divider: Option<Rect>,
    context_drawer_divider: Option<Rect>,
    context: Option<Rect>,
    context_tips_divider: Option<Rect>,
    tips: Option<Rect>,
    composer: Rect,
    status: Rect,
    work: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveHitTarget {
    Conversation,
    Context,
    ContextTab(ContextTab),
    ContextRow(usize),
    Composer,
    TranscriptBlock(TranscriptBlockId),
    TranscriptDisclosure(TranscriptBlockId),
}

impl InteractiveHitTarget {
    fn is_conversation(self) -> bool {
        matches!(
            self,
            Self::Conversation | Self::TranscriptBlock(_) | Self::TranscriptDisclosure(_)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InteractiveStatus {
    Idle,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TranscriptScrollCommand {
    PageUp,
    PageDown,
}

/// Cumulative token/cost statistics and live context estimate for the footer.
///
/// Mirrors the values the TypeScript `FooterComponent.render` computes by
/// iterating session entries, plus the context estimate from `getContextUsage`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct FooterStats {
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_write: u32,
    pub cost: f64,
    /// Estimated context tokens from the last assistant usage. `None` means
    /// unknown (e.g. right after compaction, before the next LLM response).
    pub context_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingBranchSummaryRequest {
    pub(super) source_leaf_id: String,
    pub(super) target_leaf_id: String,
    pub(super) custom_instructions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingForkRequest {
    pub(super) target_leaf_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingAgentInvocationRequest {
    pub(super) profile_id: ProfileId,
    pub(super) task: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingAgentTeamRequest {
    pub(super) team_id: ProfileId,
    pub(super) task: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingSelfHealingEditModelRepair {
    pub(super) max_attempts: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingSelfHealingEditRequest {
    pub(super) path: String,
    pub(super) replacements: Vec<SelfHealingEditReplacement>,
    pub(super) check_command: Option<String>,
    pub(super) model_repair: Option<PendingSelfHealingEditModelRepair>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingDelegationConfirmationSelection {
    pub(super) operation_id: Option<String>,
    pub(super) tool_call_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PendingDelegationConfirmationCommand {
    List,
    Approve {
        selection: PendingDelegationConfirmationSelection,
    },
    Reject {
        selection: PendingDelegationConfirmationSelection,
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingDelegationRejectionReason {
    selection: PendingDelegationConfirmationSelection,
}

pub(super) struct InteractiveLocalState {
    transcript_view: TranscriptViewState,
    pub(super) render_cache: TranscriptRenderCache,
    pub(super) editor: Editor,
    pub(super) keybindings: KeybindingsManager,
    submitted: Arc<Mutex<Option<String>>>,
    scroll_command: Arc<Mutex<Option<TranscriptScrollCommand>>>,
    focus_ring: FocusRing<InteractiveRegion>,
    context_tab: ContextTab,
    context_selection: [usize; 4],
    context_scroll: [usize; 4],
    context_viewport_height: usize,
    context_change_timing: HashMap<String, (u64, Instant)>,
    context_detail: Option<ContextDetail>,
    context_open: bool,
    context_restore_focus: InteractiveRegion,
    mouse_hits: HitMap<InteractiveHitTarget>,
    pub(super) modal_overlay: TransientOverlayBridge,
    support_overlay: TransientOverlayBridge,
    modal_overlay_handle: Option<OverlayHandle>,
    support_overlay_handle: Option<OverlayHandle>,
    pub(super) selecting_tree: bool,
    pub(super) tree_selector: Option<TreeSelectorState>,
    pub(super) selected_tree_entry_id: Option<String>,
    pub(super) pending_tree_label_change: Option<(String, Option<String>)>,
    pub(super) selected_model: Option<CodingAgentModelCatalogEntry>,
    pub(super) selected_thinking_level: Option<CodingAgentThinkingLevel>,
    pub(super) pending_permission_mode: Option<ToolAuthorizationMode>,
    pub(super) selecting_model: bool,
    pub(super) model_selection_selected: usize,
    pub(super) selected_session: Option<SessionChoice>,
    pub(super) selected_session_hydrate: bool,
    pub(super) selecting_session: bool,
    pub(super) session_selection_selected: usize,
    pub(super) selecting_settings: bool,
    settings_list: SettingsList,
    settings_command: Option<CodingAgentSettingsCommand>,
    auth_command: Option<CodingAgentAuthCommand>,
}

pub(super) struct InteractiveRoot {
    pub(super) transcript: Transcript,
    pub(super) local: InteractiveLocalState,
    pending_command: Option<PendingInteractiveCommand>,
    pub(super) pending_delegation_confirmation_command:
        Option<PendingDelegationConfirmationCommand>,
    delegation_confirmation_menu: Option<DelegationConfirmationMenuState>,
    pending_delegation_rejection_reason: Option<PendingDelegationRejectionReason>,
    tool_authorizations: VecDeque<ToolAuthorizationRequest>,
    tool_authorization_selected: usize,
    pending_tool_authorization_decision:
        Option<(ToolAuthorizationRequest, ToolAuthorizationDecision)>,
    profile_menu: Option<ProfileMenuState>,
    pending_profile_task: Option<PendingProfileTask>,
    pub(super) action: InteractiveAction,
    pub(super) status: InteractiveStatus,
    pub(super) viewport_width: usize,
    pub(super) viewport_height: usize,
    terminal_capabilities: TerminalCapabilities,
    shared_projection: UiProjection,
    child_conversations: HashMap<String, ChildConversationState>,
    child_conversation_order: VecDeque<String>,
    active_child_operation_id: Option<String>,
    main_transcript: Option<Transcript>,
    main_tool_authorizations: Option<VecDeque<ToolAuthorizationRequest>>,
    conversation_viewport_width: usize,
    conversation_viewport_height: usize,
    pub(super) cwd: PathBuf,
    pub(super) model_id: String,
    pub(super) session_label: String,
    /// Currently active model for footer display. Distinct from
    /// `selected_model`, which is consumed to apply pending changes.
    pub(super) model: Option<CodingAgentModelCatalogEntry>,
    /// Currently active thinking level (never consumed by `take_*`).
    pub(super) thinking_level: CodingAgentThinkingLevel,
    /// Currently active permission policy for footer display. Distinct from
    /// `local.pending_permission_mode`, which is consumed to apply pending
    /// changes to the runtime session.
    pub(super) permission_mode: ToolAuthorizationMode,
    pub(super) available_models: Vec<CodingAgentModelCatalogEntry>,
    pub(super) model_rotation: Vec<CodingAgentModelCatalogEntry>,
    pub(super) session_query: CodingAgentSessionQuery,
    pub(super) session_choices: Vec<SessionChoice>,
    pub(super) active_session: Option<SessionChoice>,
    pub(super) active_leaf_id: Option<String>,
    pub(super) settings: CodingAgentSettingsSnapshot,
    pub(super) auth_snapshot: CodingAgentAuthSnapshot,
    pub(super) stats: FooterStats,
    pub(super) tool_output_expanded: bool,
    pub(super) spinner_frame: usize,
    pub(super) slash_suggestion_selected: usize,
    pub(super) slash_suggestions_dismissed_for: Option<String>,
    last_empty_editor_escape_at: Option<Instant>,
    pub(super) theme: TuiTheme,
    pub(super) resolved_theme: Option<CodingAgentThemeSnapshot>,
    pub(super) resource_commands: Vec<CodingAgentResourceCommand>,
    pub(super) profile_catalog: CodingAgentProfileCatalog,
    pub(super) default_agent_profile_id: ProfileId,
    pub(super) clipboard: Arc<dyn ClipboardSink>,
}

struct ChildConversationState {
    transcript: Transcript,
    authorizations: VecDeque<ToolAuthorizationRequest>,
}

const MAX_CHILD_TRANSCRIPT_ITEMS: usize = 1_024;

impl ChildConversationState {
    fn new() -> Self {
        Self {
            transcript: Transcript::new(),
            authorizations: VecDeque::new(),
        }
    }

    fn apply_events(&mut self, events: Vec<UiEvent>) {
        apply_child_events(&mut self.transcript, &mut self.authorizations, events);
    }
}

fn apply_child_events(
    transcript: &mut Transcript,
    authorizations: &mut VecDeque<ToolAuthorizationRequest>,
    events: Vec<UiEvent>,
) {
    for event in events {
        match event {
            UiEvent::ToolAuthorizationRequired { request } => {
                if !authorizations
                    .iter()
                    .any(|pending| pending.authorization_id == request.authorization_id)
                {
                    authorizations.push_back(request);
                }
            }
            UiEvent::ToolAuthorizationResolved { authorization_id } => {
                authorizations.retain(|pending| pending.authorization_id != authorization_id);
            }
            event => {
                transcript.apply_event_with_mutation(event);
            }
        }
    }
    transcript.retain_recent(MAX_CHILD_TRANSCRIPT_ITEMS);
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct InteractiveRenderState {
    editor_text: String,
    editor_cursor: usize,
    transcript_revision: u64,
    transcript_view_revision: u64,
    selected_transcript_block: Option<TranscriptBlockId>,
    transcript_scroll_offset: usize,
    transcript_has_new_output_below: bool,
    focused_region: Option<InteractiveRegion>,
    context_tab: ContextTab,
    context_projection: Option<CodingAgentContextSnapshot>,
    capabilities: Option<CodingAgentCapabilities>,
    context_selection: [usize; 4],
    context_scroll: [usize; 4],
    context_detail: Option<ContextDetail>,
    context_open: bool,
    status: InteractiveStatus,
    stats: FooterStats,
    tool_output_expanded: bool,
    spinner_frame: usize,
    permission_mode: ToolAuthorizationMode,
    slash_suggestion_selected: usize,
    slash_suggestions_dismissed_for: Option<String>,
    selecting_settings: bool,
    selecting_tree: bool,
    tree_selector_state: Option<crate::interactive::tree_selector::TreeSelectorRenderState>,
    settings: CodingAgentSettingsSnapshot,
    auth_snapshot: CodingAgentAuthSnapshot,
    theme_name: String,
    settings_selected_item_id: Option<String>,
    selecting_model: bool,
    model_selection_selected: usize,
    selecting_session: bool,
    session_selection_selected: usize,
    delegation_confirmation_menu_state: Option<DelegationConfirmationMenuRenderState>,
    pending_delegation_rejection_reason: Option<PendingDelegationRejectionReason>,
    tool_authorization_ids: Vec<String>,
    tool_authorization_selected: usize,
    profile_menu_state: Option<ProfileMenuRenderState>,
    pending_profile_task: Option<PendingProfileTask>,
}

impl Component for InteractiveRoot {
    fn render(&mut self, width: usize) -> Vec<String> {
        if width == 0 {
            return Vec::new();
        }
        self.render_fullscreen_shell(width)
    }

    fn handle_input(&mut self, event: &InputEvent) {
        input::handle_root_input(self, event);
    }

    fn set_viewport_size(&mut self, width: usize, height: usize) {
        let previous_mode = shell_layout_mode(self.viewport_width);
        let next_mode = shell_layout_mode(width.max(1));
        let context_owned_before_resize = self.local.context_open
            || self.local.focus_ring.current() == Some(InteractiveRegion::Context)
            || self.local.context_detail.is_some();
        self.viewport_width = width.max(1);
        self.viewport_height = height.max(1);
        if next_mode != ShellLayoutMode::Wide && context_owned_before_resize {
            if previous_mode == ShellLayoutMode::Wide
                && self.local.focus_ring.current() == Some(InteractiveRegion::Context)
            {
                self.local.context_restore_focus = InteractiveRegion::Conversation;
            }
            self.local.context_open = true;
        }
        self.refresh_shell_focus();
    }

    fn set_focused(&mut self, focused: bool) {
        if focused {
            self.apply_region_focus();
        } else {
            self.local.editor.set_focused(false);
        }
    }

    fn focused(&self) -> bool {
        self.local.editor.focused()
    }
}
