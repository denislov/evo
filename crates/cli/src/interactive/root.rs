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
use coding_agent::api::view::{CapabilityStatus, CodingAgentCapabilities, ProfileId};

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
            Self::UseAgentProfile(_) => InteractiveAction::AgentProfileUse,
        }
    }
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

impl InteractiveRoot {
    pub(super) fn new_with_theme_models_and_settings(
        cwd: PathBuf,
        model_id: String,
        session_label: String,
        theme: TuiTheme,
        available_models: Vec<CodingAgentModelCatalogEntry>,
        settings: CodingAgentSettingsSnapshot,
        auth_snapshot: CodingAgentAuthSnapshot,
    ) -> Self {
        let submitted = Arc::new(Mutex::new(None));
        let submitted_for_callback = Arc::clone(&submitted);
        let scroll_command = Arc::new(Mutex::new(None));
        let page_up_command = Arc::clone(&scroll_command);
        let page_down_command = Arc::clone(&scroll_command);
        let keybindings =
            KeybindingsManager::new(keybindings::default_keybindings(), Default::default());
        let mut editor = Editor::new(keybindings.clone());
        editor.set_on_submit(Box::new(move |text| {
            *submitted_for_callback.lock().unwrap() = Some(text.to_string());
        }));
        editor.set_on_scroll_page_up(Box::new(move || {
            *page_up_command.lock().unwrap() = Some(TranscriptScrollCommand::PageUp);
        }));
        editor.set_on_scroll_page_down(Box::new(move || {
            *page_down_command.lock().unwrap() = Some(TranscriptScrollCommand::PageDown);
        }));
        editor.set_focused(true);
        editor.set_autocomplete_provider(Box::new(CombinedAutocompleteProvider::new(
            Vec::new(),
            &cwd,
        )));

        let mut transcript = Transcript::new();
        transcript.push(TranscriptItem::system(welcome_line()));
        let settings_list = build_settings_list(settings.clone(), &theme, keybindings.clone());
        let default_agent_profile_id = ProfileId::from("default");
        let profile_catalog = CodingAgentProfileCatalog::default();
        let mut focus_ring = FocusRing::new([
            InteractiveRegion::Conversation,
            InteractiveRegion::Context,
            InteractiveRegion::Composer,
        ]);
        focus_ring.focus(InteractiveRegion::Composer);
        let modal_overlay = TransientOverlayBridge::default();
        let support_overlay = TransientOverlayBridge::default();

        Self {
            transcript,
            local: InteractiveLocalState {
                transcript_view: TranscriptViewState::default(),
                render_cache: TranscriptRenderCache::new(),
                editor,
                keybindings,
                submitted,
                scroll_command,
                focus_ring,
                context_tab: ContextTab::Ops,
                context_selection: [0; 4],
                context_scroll: [0; 4],
                context_viewport_height: 1,
                context_change_timing: HashMap::new(),
                context_detail: None,
                context_open: false,
                context_restore_focus: InteractiveRegion::Composer,
                mouse_hits: HitMap::new(),
                modal_overlay,
                support_overlay,
                modal_overlay_handle: None,
                support_overlay_handle: None,
                selecting_tree: false,
                tree_selector: None,
                selected_tree_entry_id: None,
                pending_tree_label_change: None,
                selected_model: None,
                selected_thinking_level: None,
                pending_permission_mode: None,
                selecting_model: false,
                model_selection_selected: 0,
                selected_session: None,
                selected_session_hydrate: false,
                selecting_session: false,
                session_selection_selected: 0,
                selecting_settings: false,
                settings_list,
                settings_command: None,
                auth_command: None,
            },
            pending_command: None,
            pending_delegation_confirmation_command: None,
            delegation_confirmation_menu: None,
            pending_delegation_rejection_reason: None,
            tool_authorizations: VecDeque::new(),
            tool_authorization_selected: 0,
            pending_tool_authorization_decision: None,
            profile_menu: None,
            pending_profile_task: None,
            action: InteractiveAction::None,
            status: InteractiveStatus::Idle,
            viewport_width: 80,
            viewport_height: 24,
            terminal_capabilities: TerminalCapabilities {
                images: None,
                true_color: false,
                hyperlinks: false,
            },
            shared_projection: UiProjection::new(),
            child_conversations: HashMap::new(),
            child_conversation_order: VecDeque::new(),
            active_child_operation_id: None,
            main_transcript: None,
            main_tool_authorizations: None,
            conversation_viewport_width: 1,
            conversation_viewport_height: 1,
            cwd,
            model_id,
            session_label,
            model: None,
            thinking_level: CodingAgentThinkingLevel::default(),
            permission_mode: ToolAuthorizationMode::default(),
            available_models,
            model_rotation: Vec::new(),
            session_query: CodingAgentSessionQuery::disabled(),
            session_choices: Vec::new(),
            active_session: None,
            active_leaf_id: None,
            settings,
            auth_snapshot,
            stats: FooterStats::default(),
            tool_output_expanded: false,
            spinner_frame: 0,
            slash_suggestion_selected: 0,
            slash_suggestions_dismissed_for: None,
            last_empty_editor_escape_at: None,
            theme,
            resolved_theme: None,
            resource_commands: Vec::new(),
            profile_catalog,
            default_agent_profile_id,
            clipboard: Arc::new(SystemClipboard),
        }
    }

    pub(super) fn with_resolved_theme(mut self, resolved_theme: CodingAgentThemeSnapshot) -> Self {
        self.resolved_theme = Some(resolved_theme);
        self
    }

    pub(super) fn take_action(&mut self) -> InteractiveAction {
        std::mem::replace(&mut self.action, InteractiveAction::None)
    }

    pub(super) fn transient_overlay_components(
        &self,
    ) -> (
        crate::interactive::transient_overlay::TransientOverlay,
        crate::interactive::transient_overlay::TransientOverlay,
    ) {
        (
            self.local.support_overlay.component(),
            self.local.modal_overlay.component(),
        )
    }

    pub(super) fn install_transient_overlay_handles(
        &mut self,
        support: OverlayHandle,
        modal: OverlayHandle,
    ) {
        self.local.support_overlay_handle = Some(support);
        self.local.modal_overlay_handle = Some(modal);
    }

    pub(super) fn transient_overlay_handles(&self) -> Option<(OverlayHandle, OverlayHandle)> {
        Some((
            self.local.support_overlay_handle?,
            self.local.modal_overlay_handle?,
        ))
    }

    pub(super) fn prepare_transient_overlays(
        &mut self,
        terminal_width: usize,
    ) -> TransientOverlayProjection {
        let modal_role = if self.local.context_detail.is_some() {
            match shell_layout_mode(terminal_width) {
                ShellLayoutMode::Wide => TransientOverlayRole::ContextRailDetail,
                ShellLayoutMode::Medium => TransientOverlayRole::ContextDrawerDetail,
                ShellLayoutMode::Narrow => TransientOverlayRole::ContextPageDetail,
            }
        } else {
            TransientOverlayRole::ModalDialog
        };
        let modal_width = modal_overlay_width(modal_role, terminal_width);
        let modal_lines = self.render_modal_surface(modal_width.max(1));
        let support_width = terminal_width.saturating_sub(4).clamp(1, 72);
        let mut support_lines = self.render_transient_prompts(support_width);
        let support_role = if support_lines.is_empty() {
            support_lines = if modal_lines.is_empty() {
                self.render_completion_surface(support_width)
            } else {
                Vec::new()
            };
            TransientOverlayRole::ComposerAssistance
        } else {
            TransientOverlayRole::SupportPrompt
        };
        let modal_visible = !modal_lines.is_empty();
        let support_visible = !support_lines.is_empty();
        self.local.modal_overlay.set_lines(modal_lines);
        self.local.support_overlay.set_lines(support_lines);

        let composer_height = self
            .render_editor_box(terminal_width.max(1))
            .len()
            .clamp(1, MAX_COMPOSER_HEIGHT);
        TransientOverlayProjection {
            modal_visible,
            support_visible,
            bottom_margin: composer_height.saturating_add(1),
            support_role,
            modal_role,
        }
    }

    pub(super) fn drain_modal_overlay_input(&mut self) {
        for event in self.local.modal_overlay.take_pending_input() {
            input::handle_root_input(self, &event);
        }
    }

    pub(super) fn take_pending_tool_authorization_decision(
        &mut self,
    ) -> Option<(ToolAuthorizationRequest, ToolAuthorizationDecision)> {
        self.pending_tool_authorization_decision.take()
    }

    pub(super) fn restore_tool_authorization(&mut self, request: ToolAuthorizationRequest) {
        if self
            .tool_authorizations
            .iter()
            .all(|pending| pending.authorization_id != request.authorization_id)
        {
            self.tool_authorizations.push_front(request);
        }
        self.tool_authorization_selected = 0;
    }

    pub(super) fn take_selected_model(&mut self) -> Option<CodingAgentModelCatalogEntry> {
        self.local.selected_model.take()
    }

    pub(super) fn take_selected_thinking_level(&mut self) -> Option<CodingAgentThinkingLevel> {
        self.local.selected_thinking_level.take()
    }

    /// Record a permission-mode change for the status bar immediately and mark
    /// it pending so the runtime session can be switched once connected.
    pub(super) fn set_permission_mode(&mut self, mode: ToolAuthorizationMode) {
        self.permission_mode = mode;
        self.local.pending_permission_mode = Some(mode);
    }

    pub(super) fn take_pending_permission_mode(&mut self) -> Option<ToolAuthorizationMode> {
        self.local.pending_permission_mode.take()
    }

    pub(super) fn set_default_agent_profile_id(&mut self, profile_id: ProfileId) {
        self.profile_catalog.sync_default_agent_profile(&profile_id);
        self.default_agent_profile_id = profile_id;
        self.slash_suggestion_selected = 0;
        self.slash_suggestions_dismissed_for = None;
    }

    pub(super) fn display_default_agent_profile_id(&self) -> &ProfileId {
        self.shared_projection
            .session()
            .map(|session| &session.default_agent_profile_id)
            .unwrap_or(&self.default_agent_profile_id)
    }

    pub(super) fn install_shared_snapshot(&mut self, snapshot: CodingAgentSnapshot) {
        self.shared_projection = UiProjection::from_snapshot(snapshot);
        Self::update_context_local_state(&mut self.local, self.shared_projection.context());
        self.clamp_context_navigation();
    }

    pub(super) fn apply_shared_product_event(&mut self, event: &ProductEvent) {
        self.shared_projection.apply_product_event(event);
        Self::update_context_local_state(&mut self.local, self.shared_projection.context());
        self.clamp_context_navigation();
    }

    pub(super) fn drain_shared_ui_events(&mut self) -> Vec<UiEvent> {
        self.shared_projection.drain()
    }

    pub(super) fn apply_shared_child_ui_events(&mut self) {
        for (operation_id, events) in self.shared_projection.drain_children() {
            self.ensure_child_conversation(&operation_id);
            if self.active_child_operation_id.as_deref() == Some(operation_id.as_str()) {
                apply_child_events(&mut self.transcript, &mut self.tool_authorizations, events);
            } else {
                self.child_conversations
                    .entry(operation_id)
                    .or_insert_with(ChildConversationState::new)
                    .apply_events(events);
            }
        }
    }

    fn ensure_child_conversation(&mut self, operation_id: &str) {
        if self.child_conversations.contains_key(operation_id) {
            return;
        }
        while self.child_conversation_order.len() >= MAX_CHILD_CONVERSATIONS {
            let evict = self.child_conversation_order.iter().position(|candidate| {
                self.active_child_operation_id.as_deref() != Some(candidate.as_str())
            });
            let Some(index) = evict else {
                break;
            };
            if let Some(evicted) = self.child_conversation_order.remove(index) {
                self.child_conversations.remove(&evicted);
            }
        }
        self.child_conversation_order
            .push_back(operation_id.to_owned());
        self.child_conversations
            .insert(operation_id.to_owned(), ChildConversationState::new());
    }

    pub(super) fn apply_root_events(&mut self, events: Vec<UiEvent>) {
        let Some(mut main_transcript) = self.main_transcript.take() else {
            self.apply_events(events);
            return;
        };
        let mut main_authorizations = self.main_tool_authorizations.take().unwrap_or_default();
        std::mem::swap(&mut self.transcript, &mut main_transcript);
        std::mem::swap(&mut self.tool_authorizations, &mut main_authorizations);
        self.apply_events(events);
        std::mem::swap(&mut self.tool_authorizations, &mut main_authorizations);
        std::mem::swap(&mut self.transcript, &mut main_transcript);
        self.main_transcript = Some(main_transcript);
        self.main_tool_authorizations = Some(main_authorizations);
    }

    fn open_selected_child_conversation(&mut self) -> bool {
        if self.active_child_operation_id.is_some() {
            return false;
        }
        let operation_id = self
            .local
            .transcript_view
            .selected()
            .and_then(|block_id| self.transcript.item_for_block(block_id))
            .and_then(|item| match item {
                TranscriptItem::Tool { name, args, .. } if name == "delegation" => args
                    .get("childOperationId")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned),
                _ => None,
            });
        let Some(operation_id) = operation_id else {
            return false;
        };
        let Some(conversation) = self.child_conversations.get_mut(&operation_id) else {
            return false;
        };
        let child_transcript = std::mem::replace(&mut conversation.transcript, Transcript::new());
        let main_transcript = std::mem::replace(&mut self.transcript, child_transcript);
        let child_authorizations = std::mem::take(&mut conversation.authorizations);
        let main_authorizations =
            std::mem::replace(&mut self.tool_authorizations, child_authorizations);
        self.main_transcript = Some(main_transcript);
        self.main_tool_authorizations = Some(main_authorizations);
        self.active_child_operation_id = Some(operation_id);
        self.refresh_shell_focus();
        self.local.transcript_view = TranscriptViewState::default();
        self.local.render_cache.clear();
        true
    }

    fn close_child_conversation(&mut self) -> bool {
        let Some(operation_id) = self.active_child_operation_id.take() else {
            return false;
        };
        let Some(main_transcript) = self.main_transcript.take() else {
            return false;
        };
        let main_authorizations = self.main_tool_authorizations.take().unwrap_or_default();
        let child_transcript = std::mem::replace(&mut self.transcript, main_transcript);
        let child_authorizations =
            std::mem::replace(&mut self.tool_authorizations, main_authorizations);
        let conversation = self
            .child_conversations
            .entry(operation_id)
            .or_insert_with(ChildConversationState::new);
        conversation.transcript = child_transcript;
        conversation.authorizations = child_authorizations;
        self.refresh_shell_focus();
        self.local.transcript_view = TranscriptViewState::default();
        self.local.render_cache.clear();
        true
    }

    fn update_context_local_state(
        local: &mut InteractiveLocalState,
        projection: &CodingAgentContextSnapshot,
    ) {
        let now = Instant::now();
        for change in &projection.changes {
            let timing = local
                .context_change_timing
                .entry(change.path.clone())
                .or_insert((change.updated_sequence, now));
            if timing.0 != change.updated_sequence {
                *timing = (change.updated_sequence, now);
            }
        }
        local
            .context_change_timing
            .retain(|path, _| projection.changes.iter().any(|change| change.path == *path));
    }

    fn clamp_context_navigation(&mut self) {
        for tab in ContextTab::ALL {
            let index = tab.index();
            let count = if tab == ContextTab::Usage {
                self.context_usage_lines(self.viewport_width).len()
            } else {
                self.context_items(tab).len()
            };
            self.local.context_selection[index] =
                self.local.context_selection[index].min(count.saturating_sub(1));
            self.local.context_scroll[index] = self.local.context_scroll[index]
                .min(count.saturating_sub(self.local.context_viewport_height.max(1)));
        }
    }

    fn move_context_selection(&mut self, direction: isize) {
        let items = self.context_items(self.local.context_tab);
        if items.is_empty() {
            return;
        }
        let index = self.local.context_tab.index();
        self.local.context_selection[index] = if direction < 0 {
            self.local.context_selection[index].saturating_sub(1)
        } else {
            self.local.context_selection[index]
                .saturating_add(1)
                .min(items.len() - 1)
        };
        self.ensure_context_selection_visible();
    }

    fn ensure_context_selection_visible(&mut self) {
        let index = self.local.context_tab.index();
        let selected = self.local.context_selection[index];
        let viewport = self.local.context_viewport_height.max(1);
        if selected < self.local.context_scroll[index] {
            self.local.context_scroll[index] = selected;
        } else if selected >= self.local.context_scroll[index].saturating_add(viewport) {
            self.local.context_scroll[index] = selected.saturating_add(1).saturating_sub(viewport);
        }
    }

    fn scroll_context(&mut self, rows: isize) {
        let index = self.local.context_tab.index();
        let count = if self.local.context_tab == ContextTab::Usage {
            self.context_usage_lines(self.viewport_width).len()
        } else {
            self.context_items(self.local.context_tab).len()
        };
        let maximum = count.saturating_sub(self.local.context_viewport_height.max(1));
        self.local.context_scroll[index] = if rows < 0 {
            self.local.context_scroll[index].saturating_sub(rows.unsigned_abs())
        } else {
            self.local.context_scroll[index]
                .saturating_add(rows as usize)
                .min(maximum)
        };
    }

    fn open_selected_context_detail(&mut self) -> bool {
        let items = self.context_items(self.local.context_tab);
        let Some(item) = items.get(self.local.context_selection[self.local.context_tab.index()])
        else {
            return false;
        };
        self.local.context_detail = Some(ContextDetail {
            title: item.detail_title.clone(),
            lines: item.detail_lines.clone(),
            scroll: 0,
        });
        true
    }

    pub(super) fn has_context_detail(&self) -> bool {
        self.local.context_detail.is_some()
    }

    pub(super) fn handle_context_detail_input(&mut self, event: &InputEvent) -> bool {
        let Some(detail) = self.local.context_detail.as_mut() else {
            return false;
        };
        if matches_key(event, "escape")
            || matches_key(event, "enter")
            || matches_key(event, "ctrl+c")
        {
            self.local.context_detail = None;
            return true;
        }
        if matches_key(event, "pageup") || matches_key(event, "up") {
            detail.scroll = detail
                .scroll
                .saturating_sub(if matches_key(event, "pageup") { 8 } else { 1 });
            return true;
        }
        if matches_key(event, "pagedown") || matches_key(event, "down") {
            detail.scroll = detail
                .scroll
                .saturating_add(if matches_key(event, "pagedown") { 8 } else { 1 });
            return true;
        }
        true
    }

    pub(super) fn take_selected_session(&mut self) -> Option<SessionChoice> {
        self.local.selected_session.take()
    }

    pub(super) fn take_selected_session_hydrate(&mut self) -> bool {
        std::mem::take(&mut self.local.selected_session_hydrate)
    }

    pub(super) fn take_selected_tree_entry_id(&mut self) -> Option<String> {
        self.local.selected_tree_entry_id.take()
    }

    pub(super) fn take_pending_tree_label_change(&mut self) -> Option<(String, Option<String>)> {
        self.local.pending_tree_label_change.take()
    }

    pub(super) fn apply_tree_label_update(
        &mut self,
        entry_id: &str,
        label: Option<String>,
        updated_at: String,
    ) {
        if let Some(selector) = self.local.tree_selector.as_mut() {
            let timestamp = label.as_ref().map(|_| updated_at);
            selector.update_node_label(entry_id, label, timestamp);
        }
    }

    pub(super) fn take_settings_command(&mut self) -> Option<CodingAgentSettingsCommand> {
        self.local.settings_command.take()
    }

    pub(super) fn take_auth_command(&mut self) -> Option<CodingAgentAuthCommand> {
        self.local.auth_command.take()
    }

    pub(super) fn take_submitted(&mut self) -> Option<String> {
        self.local.submitted.lock().unwrap().take()
    }

    pub(super) fn queue_command(&mut self, command: PendingInteractiveCommand) {
        self.action = command.action();
        self.pending_command = Some(command);
    }

    pub(super) fn take_pending_command(&mut self) -> Option<PendingInteractiveCommand> {
        self.pending_command.take()
    }

    pub(super) fn take_pending_delegation_confirmation_command(
        &mut self,
    ) -> Option<PendingDelegationConfirmationCommand> {
        self.pending_delegation_confirmation_command.take()
    }

    pub(super) fn take_scroll_command(&mut self) -> Option<TranscriptScrollCommand> {
        self.local.scroll_command.lock().unwrap().take()
    }

    pub(super) fn open_delegation_confirmation_menu(
        &mut self,
        pending: Vec<PendingDelegationConfirmation>,
    ) {
        self.delegation_confirmation_menu = Some(DelegationConfirmationMenuState::new(pending));
        self.pending_delegation_rejection_reason = None;
        self.profile_menu = None;
        self.pending_profile_task = None;
        self.local.editor.set_text("");
        self.slash_suggestion_selected = 0;
        self.slash_suggestions_dismissed_for = None;
    }

    fn enqueue_delegation_confirmation(&mut self, pending: PendingDelegationConfirmation) {
        if let Some(menu) = self.delegation_confirmation_menu.as_mut() {
            menu.upsert(pending);
        } else {
            self.delegation_confirmation_menu =
                Some(DelegationConfirmationMenuState::new(vec![pending]));
        }
        self.pending_delegation_rejection_reason = None;
        self.profile_menu = None;
        self.pending_profile_task = None;
    }

    fn resolve_delegation_confirmation(&mut self, operation_id: &str, tool_call_id: &str) {
        let Some(menu) = self.delegation_confirmation_menu.as_mut() else {
            return;
        };
        menu.remove(operation_id, tool_call_id);
        if menu.is_empty() {
            self.delegation_confirmation_menu = None;
        }
    }

    pub(super) fn has_active_delegation_confirmation_menu(&self) -> bool {
        self.delegation_confirmation_menu.is_some()
    }

    pub(super) fn handle_delegation_confirmation_menu_input(&mut self, event: &InputEvent) -> bool {
        let Some(menu) = self.delegation_confirmation_menu.as_mut() else {
            return false;
        };
        let outcome = menu.handle_input(&self.local.keybindings, event);
        match outcome {
            DelegationConfirmationMenuOutcome::None => {}
            DelegationConfirmationMenuOutcome::Close => {
                self.delegation_confirmation_menu = None;
                self.local.editor.set_text("");
            }
            DelegationConfirmationMenuOutcome::Approve {
                operation_id,
                tool_call_id,
            } => {
                self.delegation_confirmation_menu = None;
                self.pending_delegation_confirmation_command =
                    Some(PendingDelegationConfirmationCommand::Approve {
                        selection: PendingDelegationConfirmationSelection {
                            operation_id: Some(operation_id),
                            tool_call_id,
                        },
                    });
                self.action = InteractiveAction::DelegationConfirmation;
            }
            DelegationConfirmationMenuOutcome::Reject {
                operation_id,
                tool_call_id,
            } => {
                self.delegation_confirmation_menu = None;
                self.pending_delegation_confirmation_command =
                    Some(PendingDelegationConfirmationCommand::Reject {
                        selection: PendingDelegationConfirmationSelection {
                            operation_id: Some(operation_id),
                            tool_call_id,
                        },
                        reason: None,
                    });
                self.action = InteractiveAction::DelegationConfirmation;
            }
            DelegationConfirmationMenuOutcome::RejectWithReason {
                operation_id,
                tool_call_id,
            } => {
                self.delegation_confirmation_menu = None;
                self.pending_delegation_rejection_reason = Some(PendingDelegationRejectionReason {
                    selection: PendingDelegationConfirmationSelection {
                        operation_id: Some(operation_id),
                        tool_call_id,
                    },
                });
                self.local.editor.set_text("");
            }
        }
        true
    }

    fn render_delegation_confirmation_menu(&mut self, width: usize) -> Vec<String> {
        let Some(menu) = self.delegation_confirmation_menu.as_mut() else {
            return Vec::new();
        };
        menu.render(width)
    }

    pub(super) fn has_pending_tool_authorization(&self) -> bool {
        !self.tool_authorizations.is_empty()
    }

    pub(super) fn handle_tool_authorization_input(&mut self, event: &InputEvent) -> bool {
        if self.tool_authorizations.is_empty() || matches_key(event, "ctrl+c") {
            return false;
        }
        if matches_key(event, "escape") {
            self.resolve_current_tool_authorization(ToolAuthorizationDecision::Deny {
                reason: None,
            });
            return true;
        }
        if self.local.keybindings.matches(event, "tui.select.up") {
            self.tool_authorization_selected = (self.tool_authorization_selected + 2) % 3;
            return true;
        }
        if self.local.keybindings.matches(event, "tui.select.down") {
            self.tool_authorization_selected = (self.tool_authorization_selected + 1) % 3;
            return true;
        }
        let InputEvent::Key(key_event) = event else {
            return true;
        };
        if key_event.kind == KeyEventKind::Release {
            return true;
        }
        if matches!(key_event.key, Key::Tab) {
            if key_event.modifiers.contains(KeyModifiers::SHIFT) {
                self.tool_authorization_selected = (self.tool_authorization_selected + 2) % 3;
            } else {
                self.tool_authorization_selected = (self.tool_authorization_selected + 1) % 3;
            }
            return true;
        }
        if self.local.keybindings.matches(event, "tui.select.confirm") {
            let decision = match self.tool_authorization_selected {
                0 => ToolAuthorizationDecision::AllowOnce,
                1 => ToolAuthorizationDecision::AllowForOperation,
                _ => ToolAuthorizationDecision::Deny { reason: None },
            };
            self.resolve_current_tool_authorization(decision);
        }
        true
    }

    fn resolve_current_tool_authorization(&mut self, decision: ToolAuthorizationDecision) {
        let Some(request) = self.tool_authorizations.pop_front() else {
            return;
        };
        self.tool_authorization_selected = 0;
        self.pending_tool_authorization_decision = Some((request, decision));
        self.action = InteractiveAction::ToolAuthorization;
    }

    fn render_tool_authorization(&self, width: usize) -> Vec<String> {
        let Some(request) = self.tool_authorizations.front() else {
            return Vec::new();
        };
        let color = color_enabled();
        let content_width = width.saturating_sub(3).max(1);
        let mut inner = vec![fit_line(
            &paint_with(
                &format!("Tool authorization (1/{})", self.tool_authorizations.len()),
                &WARNING,
                color,
            ),
            content_width,
        )];
        inner.push(fit_line(
            &format!(
                "  tool: {}  risk: {}  operation: {}",
                request.tool_name,
                tool_authorization_risk_label(request.risk),
                request.operation_id
            ),
            content_width,
        ));
        inner.push(fit_line(
            &format!("  {}", request.preview.summary),
            content_width,
        ));
        if let Some(path) = request.preview.path.as_deref() {
            inner.push(fit_line(&format!("  path: {path}"), content_width));
        }
        if let Some(cwd) = request.preview.cwd.as_deref() {
            inner.push(fit_line(&format!("  cwd: {cwd}"), content_width));
        }
        if let Some(command) = request.preview.command.as_deref() {
            for (index, command_line) in command.lines().take(3).enumerate() {
                let label = if index == 0 { "command" } else { "       " };
                inner.push(fit_line(
                    &format!("  {label}: {command_line}"),
                    content_width,
                ));
            }
        }
        if let Some(content) = request.preview.content_preview.as_deref() {
            inner.push(fit_line("  preview:", content_width));
            for content_line in content.lines().take(6) {
                inner.push(fit_line(&format!("    {content_line}"), content_width));
            }
        }
        for (index, label) in ["Allow once", "Allow for operation", "Deny"]
            .into_iter()
            .enumerate()
        {
            let marker = if index == self.tool_authorization_selected {
                "->"
            } else {
                "  "
            };
            let line = format!("{marker} {label}");
            if index == self.tool_authorization_selected {
                inner.push(fit_line(&paint_with(&line, &USER, color), content_width));
            } else {
                inner.push(fit_line(&line, content_width));
            }
        }
        inner.push(fit_line(
            &paint_with(
                "Up/Down or Tab choose · Enter confirm · Esc deny · Ctrl+C abort operation",
                &SYSTEM,
                color,
            ),
            content_width,
        ));

        if width < 5 {
            return inner
                .into_iter()
                .map(|line| fit_line(&line, width))
                .collect();
        }

        // Visible warning-colored border so the authorization dialog reads as
        // a modal surface instead of plain transcript text.
        framed_modal_lines(inner, width, &WARNING, color)
    }

    pub(super) fn has_active_profile_menu(&self) -> bool {
        self.profile_menu.is_some()
    }

    pub(super) fn has_pending_delegation_rejection_reason(&self) -> bool {
        self.pending_delegation_rejection_reason.is_some()
    }

    pub(super) fn handle_pending_delegation_rejection_reason_input(
        &mut self,
        event: &InputEvent,
    ) -> bool {
        let Some(pending_reason) = self.pending_delegation_rejection_reason.clone() else {
            return false;
        };
        if matches_key(event, "escape") || matches_key(event, "ctrl+c") {
            self.pending_delegation_rejection_reason = None;
            self.local.editor.set_text("");
            self.transcript
                .push(TranscriptItem::system("Delegation rejection canceled"));
            return true;
        }

        let before_text = self.local.editor.text().to_string();
        self.local.editor.handle_input(event);
        if self.local.editor.text() != before_text {
            self.slash_suggestion_selected = 0;
            self.slash_suggestions_dismissed_for = None;
        }
        if let Some(command) = self.take_scroll_command() {
            let page_rows = self.viewport_height.saturating_sub(2).max(1);
            match command {
                TranscriptScrollCommand::PageUp => self.transcript.scroll_page_up(page_rows),
                TranscriptScrollCommand::PageDown => self.transcript.scroll_page_down(page_rows),
            }
        }
        let Some(text) = self.take_submitted() else {
            return true;
        };
        let reason = text.trim().to_string();
        self.pending_delegation_confirmation_command =
            Some(PendingDelegationConfirmationCommand::Reject {
                selection: pending_reason.selection,
                reason: (!reason.is_empty()).then_some(reason),
            });
        self.pending_delegation_rejection_reason = None;
        self.local.editor.set_text("");
        self.action = InteractiveAction::DelegationConfirmation;
        true
    }

    pub(super) fn has_pending_profile_task(&self) -> bool {
        self.pending_profile_task.is_some()
    }

    pub(super) fn open_agent_menu(&mut self) {
        self.delegation_confirmation_menu = None;
        self.pending_delegation_rejection_reason = None;
        self.profile_menu = Some(ProfileMenuState::agent());
        self.pending_profile_task = None;
        self.local.editor.set_text("");
        self.slash_suggestion_selected = 0;
        self.slash_suggestions_dismissed_for = None;
    }

    pub(super) fn open_team_menu(&mut self) {
        self.delegation_confirmation_menu = None;
        self.pending_delegation_rejection_reason = None;
        self.profile_menu = Some(ProfileMenuState::team());
        self.pending_profile_task = None;
        self.local.editor.set_text("");
        self.slash_suggestion_selected = 0;
        self.slash_suggestions_dismissed_for = None;
    }

    pub(super) fn handle_profile_menu_input(&mut self, event: &InputEvent) -> bool {
        let default_agent_profile_id = self.display_default_agent_profile_id().clone();
        let Some(menu) = self.profile_menu.as_mut() else {
            return false;
        };
        let outcome = menu.handle_input(
            &self.local.keybindings,
            event,
            &self.profile_catalog,
            &default_agent_profile_id,
        );
        match outcome {
            ProfileMenuOutcome::None => {}
            ProfileMenuOutcome::Close => {
                self.profile_menu = None;
                self.local.editor.set_text("");
            }
            ProfileMenuOutcome::SetDefaultAgent(profile_id) => {
                self.profile_menu = None;
                self.set_default_agent_profile_id(profile_id.clone());
                self.queue_command(PendingInteractiveCommand::UseAgentProfile(
                    profile_id.clone(),
                ));
                self.transcript.push(TranscriptItem::system(format!(
                    "Default agent profile: {profile_id}"
                )));
            }
            ProfileMenuOutcome::BeginAgentTask(profile_id) => {
                self.profile_menu = None;
                self.pending_profile_task = Some(PendingProfileTask::Agent { profile_id });
                self.local.editor.set_text("");
            }
            ProfileMenuOutcome::BeginTeamTask(team_id) => {
                self.profile_menu = None;
                self.pending_profile_task = Some(PendingProfileTask::Team { team_id });
                self.local.editor.set_text("");
            }
        }
        true
    }

    pub(super) fn handle_pending_profile_task_input(&mut self, event: &InputEvent) -> bool {
        let Some(pending_task) = self.pending_profile_task.clone() else {
            return false;
        };
        if matches_key(event, "escape") || matches_key(event, "ctrl+c") {
            self.pending_profile_task = None;
            self.local.editor.set_text("");
            self.transcript
                .push(TranscriptItem::system("Profile task canceled"));
            return true;
        }

        let before_text = self.local.editor.text().to_string();
        self.local.editor.handle_input(event);
        if self.local.editor.text() != before_text {
            self.slash_suggestion_selected = 0;
            self.slash_suggestions_dismissed_for = None;
        }
        if let Some(command) = self.take_scroll_command() {
            let page_rows = self.viewport_height.saturating_sub(2).max(1);
            match command {
                TranscriptScrollCommand::PageUp => self.transcript.scroll_page_up(page_rows),
                TranscriptScrollCommand::PageDown => self.transcript.scroll_page_down(page_rows),
            }
        }
        let Some(text) = self.take_submitted() else {
            return true;
        };
        let task = text.trim().to_string();
        if task.is_empty() {
            self.transcript
                .push(TranscriptItem::system("Profile task requires text"));
            return true;
        }
        self.local.editor.add_to_history(&task);
        match pending_task {
            PendingProfileTask::Agent { profile_id } => {
                self.queue_command(PendingInteractiveCommand::AgentInvocation(
                    PendingAgentInvocationRequest { profile_id, task },
                ));
            }
            PendingProfileTask::Team { team_id } => {
                self.queue_command(PendingInteractiveCommand::AgentTeam(
                    PendingAgentTeamRequest { team_id, task },
                ));
            }
        }
        self.pending_profile_task = None;
        true
    }

    fn render_profile_menu(&mut self, width: usize) -> Vec<String> {
        let default_agent_profile_id = self.display_default_agent_profile_id().clone();
        let Some(menu) = self.profile_menu.as_mut() else {
            return Vec::new();
        };
        menu.render(&self.profile_catalog, &default_agent_profile_id, width)
    }

    fn render_pending_delegation_rejection_reason(&self, width: usize) -> Vec<String> {
        let Some(pending_reason) = &self.pending_delegation_rejection_reason else {
            return Vec::new();
        };
        let operation_id = pending_reason
            .selection
            .operation_id
            .as_deref()
            .unwrap_or("unknown-operation");
        let text = format!(
            "Delegation rejection reason for {operation_id} {}: enter reason, then press Enter",
            pending_reason.selection.tool_call_id
        );
        vec![fit_line(
            &paint_with(&text, &SYSTEM, color_enabled()),
            width,
        )]
    }

    fn render_pending_profile_task(&self, width: usize) -> Vec<String> {
        let Some(pending_task) = &self.pending_profile_task else {
            return Vec::new();
        };
        let text = match pending_task {
            PendingProfileTask::Agent { profile_id } => {
                format!("Agent {profile_id}: enter task, then press Enter")
            }
            PendingProfileTask::Team { team_id } => {
                format!("Team {team_id}: enter task, then press Enter")
            }
        };
        vec![fit_line(
            &paint_with(&text, &SYSTEM, color_enabled()),
            width,
        )]
    }

    pub(super) fn apply_prompt_context(&mut self, prompt_context: &PromptContext) {
        self.cwd = prompt_context.cwd.clone();
        self.model_id = prompt_context.model_summary.id.clone();
        self.model = Some(prompt_context.model_summary.clone());
        self.thinking_level = prompt_context.thinking_level.unwrap_or_default();
        self.available_models = prompt_context.model_choices.clone();
        self.model_rotation = prompt_context.model_rotation.clone();
        self.session_query = prompt_context.session_query.clone();
        self.session_choices = prompt_context.session_choices.clone();
        self.theme = prompt_context.theme.clone();
        self.settings = prompt_context.settings_snapshot();
        self.local.settings_list = build_settings_list(
            self.settings.clone(),
            &self.theme,
            self.local.keybindings.clone(),
        );
        self.local.render_cache.clear();
        self.auth_snapshot = prompt_context.auth_controller.snapshot();
        self.resource_commands = prompt_context.resource_commands.clone();
        self.profile_catalog = prompt_context.profile_catalog.clone();
        self.set_default_agent_profile_id(prompt_context.default_agent_profile_id.clone());
    }

    pub(super) fn resource_prompt_invocation(
        &self,
        command: &ParsedSlashCommand,
    ) -> Option<PromptInvocation> {
        let skill = if self.settings.runtime.enable_skill_commands {
            self.resource_commands.iter().find(|resource| {
                resource.kind == CodingAgentResourceCommandKind::Skill
                    && resource.command == command.name
            })
        } else {
            None
        };
        skill
            .or_else(|| {
                self.resource_commands.iter().find(|resource| {
                    resource.kind == CodingAgentResourceCommandKind::PromptTemplate
                        && resource.command == command.name
                })
            })
            .map(|resource| resource.prompt_invocation(&command.args))
    }

    pub(super) fn all_slash_commands(&self) -> Vec<slash::BuiltinSlashCommand> {
        let mut commands = slash::builtin_slash_commands();
        for resource in &self.resource_commands {
            if resource.kind == CodingAgentResourceCommandKind::Skill
                && !self.settings.runtime.enable_skill_commands
            {
                continue;
            }
            commands.push(slash::BuiltinSlashCommand {
                name: resource.command.clone(),
                description: resource.description.clone(),
            });
        }
        commands
    }

    pub(super) fn push_user(&mut self, prompt: String) {
        self.transcript.push(TranscriptItem::user(prompt));
    }

    pub(super) fn apply_events(&mut self, events: Vec<UiEvent>) {
        let previous_scroll_offset = self.transcript.scroll_offset();
        let previous_rows = if previous_scroll_offset > 0 {
            Some(self.transcript_row_snapshot(MAX_TOOL_RESULT_LINES))
        } else {
            None
        };
        let mut mutation = TranscriptMutation::default();
        for event in events {
            match event {
                UiEvent::ToolAuthorizationRequired { request } => {
                    if self
                        .tool_authorizations
                        .iter()
                        .all(|pending| pending.authorization_id != request.authorization_id)
                        && self
                            .pending_tool_authorization_decision
                            .as_ref()
                            .is_none_or(|(pending, _)| {
                                pending.authorization_id != request.authorization_id
                            })
                    {
                        self.tool_authorizations.push_back(request);
                    }
                }
                UiEvent::ToolAuthorizationResolved { authorization_id } => {
                    self.tool_authorizations
                        .retain(|request| request.authorization_id != authorization_id);
                    if self
                        .pending_tool_authorization_decision
                        .as_ref()
                        .is_some_and(|(request, _)| request.authorization_id == authorization_id)
                    {
                        self.pending_tool_authorization_decision = None;
                    }
                    self.tool_authorization_selected = self.tool_authorization_selected.min(2);
                }
                UiEvent::DelegationConfirmationRequired { pending } => {
                    self.enqueue_delegation_confirmation(pending);
                }
                UiEvent::DelegationConfirmationResolved {
                    operation_id,
                    tool_call_id,
                } => {
                    self.resolve_delegation_confirmation(&operation_id, &tool_call_id);
                }
                UiEvent::UsageUpdate {
                    input,
                    output,
                    cache_read,
                    cache_write,
                    cost,
                    context_tokens,
                } => {
                    // Accumulate delta values from the stateless bridge.
                    // This ensures hydration-seeded stats are preserved:
                    //   root.stats starts at 0 (fresh) or at the hydrated
                    //   cumulative value, and each UsageUpdate adds to it.
                    self.stats.input = self.stats.input.saturating_add(input);
                    self.stats.output = self.stats.output.saturating_add(output);
                    self.stats.cache_read = self.stats.cache_read.saturating_add(cache_read);
                    self.stats.cache_write = self.stats.cache_write.saturating_add(cache_write);
                    self.stats.cost += cost;
                    self.stats.context_tokens = context_tokens;
                }
                other => mutation.extend(self.transcript.apply_event_with_mutation(other)),
            }
        }
        if let Some(previous_rows) = previous_rows {
            let anchor_start_row = Some(
                previous_rows
                    .total_rows()
                    .saturating_sub(previous_scroll_offset)
                    .saturating_sub(self.conversation_viewport_height.max(1)),
            );
            let row_delta_below_anchor = self.transcript_row_delta_since(
                previous_rows,
                mutation.changed_indices(),
                MAX_TOOL_RESULT_LINES,
                anchor_start_row,
            );
            self.transcript.preserve_scrolled_view_after_hidden_change(
                previous_scroll_offset,
                row_delta_below_anchor,
            );
        }
    }

    pub(super) fn set_status(&mut self, status: InteractiveStatus) {
        if status == InteractiveStatus::Idle {
            self.spinner_frame = 0;
        }
        self.status = status;
    }

    pub(super) fn handle_slash_command(&mut self, command: ParsedSlashCommand) {
        commands::handle_slash_command(self, command);
    }

    pub(super) fn handle_empty_editor_escape(&mut self) {
        let action = self.settings.presentation.double_escape_action;
        if action == CodingAgentDoubleEscapeAction::None {
            self.last_empty_editor_escape_at = None;
            return;
        }

        let now = Instant::now();
        let is_double_escape = self
            .last_empty_editor_escape_at
            .is_some_and(|previous| now.duration_since(previous) < DOUBLE_ESCAPE_WINDOW);
        if !is_double_escape {
            self.last_empty_editor_escape_at = Some(now);
            return;
        }

        self.last_empty_editor_escape_at = None;
        match action {
            CodingAgentDoubleEscapeAction::Fork => self.handle_slash_command(ParsedSlashCommand {
                name: "fork".to_string(),
                args: String::new(),
                original: "/fork".to_string(),
            }),
            CodingAgentDoubleEscapeAction::Tree => self.handle_slash_command(ParsedSlashCommand {
                name: "tree".to_string(),
                args: String::new(),
                original: "/tree".to_string(),
            }),
            CodingAgentDoubleEscapeAction::None => {}
        }
    }

    pub(super) fn clear_empty_editor_escape(&mut self) {
        self.last_empty_editor_escape_at = None;
    }

    fn set_selected_model(&mut self, model: CodingAgentModelCatalogEntry) {
        self.set_selected_model_with_thinking(model, None);
    }

    pub(super) fn set_selected_model_with_thinking(
        &mut self,
        model: CodingAgentModelCatalogEntry,
        thinking_level: Option<CodingAgentThinkingLevel>,
    ) {
        self.model_id = model.id.clone();
        self.model = Some(model.clone());
        self.thinking_level = thinking_level.unwrap_or_default();
        self.local.selected_model = Some(model);
        self.local.selected_thinking_level = thinking_level;
        self.local.selecting_model = false;
        self.local.model_selection_selected = 0;
        self.local.editor.set_text("");
        let suffix = thinking_level
            .map(|level| format!(" (thinking: {level})"))
            .unwrap_or_default();
        self.transcript.push(TranscriptItem::system(format!(
            "Model set: {}{}",
            self.model_id, suffix
        )));
    }

    pub(super) fn cycle_model_rotation(&mut self, reverse: bool) {
        if self.model_rotation.is_empty() {
            return;
        }
        let len = self.model_rotation.len();
        let next_index = match self
            .model_rotation
            .iter()
            .position(|model| model.id == self.model_id)
        {
            Some(index) if reverse => (index + len - 1) % len,
            Some(index) => (index + 1) % len,
            None if reverse => len - 1,
            None => 0,
        };
        let model = self.model_rotation[next_index].clone();
        self.set_selected_model(model);
    }

    pub(super) fn set_selected_session(&mut self, choice: SessionChoice) {
        self.session_label = choice.display_name().to_string();
        self.local.selected_session = Some(choice.clone());
        self.local.selected_session_hydrate = true;
        self.set_active_session_choice(choice.clone());
        self.local.selecting_session = false;
        self.local.session_selection_selected = 0;
        self.local.editor.set_text("");
        self.transcript.push(TranscriptItem::system(format!(
            "Session selected: {}",
            choice.display_name()
        )));
    }

    pub(super) fn apply_hydrated_session(
        &mut self,
        hydrated: HydratedSession,
        notice: Option<String>,
    ) {
        self.session_label = hydrated.choice.display_name().to_string();
        let mut choice = hydrated.choice.clone();
        choice.active_leaf_id = hydrated.leaf_id.clone();
        self.set_active_session_choice(choice);
        // Restore cumulative token/cost stats so the footer reflects the
        // entire session immediately after resume, without waiting for the
        // next turn to emit a UsageUpdate event.
        self.stats = FooterStats {
            input: hydrated.cumulative_usage.input,
            output: hydrated.cumulative_usage.output,
            cache_read: hydrated.cumulative_usage.cache_read,
            cache_write: hydrated.cumulative_usage.cache_write,
            cost: hydrated.cumulative_usage.cost,
            context_tokens: hydrated.cumulative_usage.last_context_tokens,
        };

        let mut transcript = Transcript::new();
        if let Some(first) = self.transcript.items().first().cloned() {
            transcript.push(first);
        }
        for item in hydrated.transcript_items {
            transcript.push(item);
        }
        if let Some(notice) = notice {
            transcript.push(TranscriptItem::system(notice));
        }
        self.transcript = transcript;
        self.local.render_cache.clear();
    }

    pub(super) fn set_active_session_choice(&mut self, choice: SessionChoice) {
        self.active_leaf_id = choice.active_leaf_id.clone();
        self.active_session = Some(choice);
    }

    pub(super) fn clear_active_session(&mut self) {
        self.active_session = None;
        self.active_leaf_id = None;
    }

    pub(super) fn render_state(&self) -> InteractiveRenderState {
        InteractiveRenderState {
            editor_text: self.local.editor.text().to_string(),
            editor_cursor: self.local.editor.cursor(),
            transcript_revision: self.transcript.revision(),
            transcript_view_revision: self.local.transcript_view.revision(),
            selected_transcript_block: self.local.transcript_view.selected(),
            transcript_scroll_offset: self.transcript.scroll_offset(),
            transcript_has_new_output_below: self.transcript.has_new_output_below(),
            focused_region: self.local.focus_ring.current(),
            context_tab: self.local.context_tab,
            context_projection: Some(self.shared_projection.context().clone()),
            capabilities: self.shared_projection.capabilities().cloned(),
            context_selection: self.local.context_selection,
            context_scroll: self.local.context_scroll,
            context_detail: self.local.context_detail.clone(),
            context_open: self.local.context_open,
            status: self.status,
            stats: self.stats,
            tool_output_expanded: self.tool_output_expanded,
            spinner_frame: self.spinner_frame,
            permission_mode: self.permission_mode,
            slash_suggestion_selected: self.slash_suggestion_selected,
            slash_suggestions_dismissed_for: self.slash_suggestions_dismissed_for.clone(),
            selecting_settings: self.local.selecting_settings,
            selecting_tree: self.local.selecting_tree,
            tree_selector_state: self
                .local
                .tree_selector
                .as_ref()
                .map(|selector| selector.render_state()),
            settings: self.settings.clone(),
            auth_snapshot: self.auth_snapshot.clone(),
            theme_name: self.theme.name.clone(),
            settings_selected_item_id: self
                .local
                .settings_list
                .selected_item()
                .map(|item| item.id.clone()),
            selecting_model: self.local.selecting_model,
            model_selection_selected: self.local.model_selection_selected,
            selecting_session: self.local.selecting_session,
            session_selection_selected: self.local.session_selection_selected,
            delegation_confirmation_menu_state: self
                .delegation_confirmation_menu
                .as_ref()
                .map(|menu| menu.render_state()),
            pending_delegation_rejection_reason: self.pending_delegation_rejection_reason.clone(),
            tool_authorization_ids: self
                .tool_authorizations
                .iter()
                .map(|request| request.authorization_id.clone())
                .collect(),
            tool_authorization_selected: self.tool_authorization_selected,
            profile_menu_state: self.profile_menu.as_ref().map(|menu| menu.render_state()),
            pending_profile_task: self.pending_profile_task.clone(),
        }
    }

    pub(super) fn editor_border_style(&self) -> Style {
        if !self.local.editor.focused() {
            self.resolved_theme
                .as_ref()
                .map_or(self.theme.editor.border, |resolved| {
                    Style::fg(crate::interactive::theme::to_color(
                        resolved.foreground(CodingAgentThemeForeground::BorderMuted),
                    ))
                })
        } else if self.local.selecting_model
            || self.local.selecting_settings
            || self.local.selecting_session
            || self.delegation_confirmation_menu.is_some()
            || !self.tool_authorizations.is_empty()
            || self.pending_delegation_rejection_reason.is_some()
            || self.profile_menu.is_some()
            || self.pending_profile_task.is_some()
        {
            self.theme.editor.menu_border
        } else if let Some(resolved) = &self.resolved_theme {
            // Editor border reflects the active thinking level, mirroring TS
            // `getThinkingBorderColor`. Bash-mode border (TS
            // `getBashModeBorderColor`) is not yet wired: Rust has no
            // bash-mode input state.
            Style::fg(crate::interactive::theme::to_color(
                resolved.foreground(Self::thinking_border_token(self.thinking_level)),
            ))
        } else {
            self.theme.editor.active_border
        }
    }

    /// Map a thinking level to its border color token, mirroring TS
    /// `getThinkingBorderColor`.
    fn thinking_border_token(level: CodingAgentThinkingLevel) -> CodingAgentThemeForeground {
        match level {
            CodingAgentThinkingLevel::Off => CodingAgentThemeForeground::ThinkingOff,
            CodingAgentThinkingLevel::Minimal => CodingAgentThemeForeground::ThinkingMinimal,
            CodingAgentThinkingLevel::Low => CodingAgentThemeForeground::ThinkingLow,
            CodingAgentThinkingLevel::Medium => CodingAgentThemeForeground::ThinkingMedium,
            CodingAgentThinkingLevel::High => CodingAgentThemeForeground::ThinkingHigh,
            CodingAgentThinkingLevel::XHigh => CodingAgentThemeForeground::ThinkingXhigh,
        }
    }

    fn render_slash_suggestions(&mut self, width: usize) -> Vec<String> {
        if (shell_layout_mode(self.viewport_width) == ShellLayoutMode::Narrow
            && self.local.context_open)
            || self.local.selecting_model
            || self.local.selecting_settings
            || self.local.selecting_session
            || self.delegation_confirmation_menu.is_some()
            || self.profile_menu.is_some()
            || self.pending_profile_task.is_some()
        {
            return Vec::new();
        }

        let commands = self.all_slash_commands();
        slash::render_suggestions(
            self.local.editor.text(),
            self.local.editor.cursor(),
            self.slash_suggestions_dismissed_for.as_deref(),
            &mut self.slash_suggestion_selected,
            width,
            &commands,
            &self.theme.select_list,
        )
    }

    fn render_settings_menu(&mut self, width: usize) -> Vec<String> {
        if !self.local.selecting_settings {
            return Vec::new();
        }
        let mut lines = vec![fit_line("Settings", width)];
        lines.extend(self.local.settings_list.render(width));
        lines
    }

    fn apply_settings_value(&mut self, id: &str, value: &str) {
        let command = match id {
            "theme" => {
                self.settings.presentation.theme = Some(value.to_string());
                self.apply_builtin_theme(value);
                CodingAgentSettingsCommand::set_theme(value)
            }
            "auto_compaction" => {
                let enabled = value == "on";
                self.settings.runtime.auto_compaction = enabled;
                CodingAgentSettingsCommand::SetAutoCompaction(enabled)
            }
            "steering_mode" => {
                let mode = if value == "all" {
                    CodingAgentQueueMode::All
                } else {
                    CodingAgentQueueMode::OneAtATime
                };
                self.settings.runtime.steering_mode = mode;
                CodingAgentSettingsCommand::SetSteeringMode(mode)
            }
            "follow_up_mode" => {
                let mode = if value == "all" {
                    CodingAgentQueueMode::All
                } else {
                    CodingAgentQueueMode::OneAtATime
                };
                self.settings.runtime.follow_up_mode = mode;
                CodingAgentSettingsCommand::SetFollowUpMode(mode)
            }
            "show_progress" => {
                let visible = value == "on";
                self.settings.presentation.show_progress = visible;
                CodingAgentSettingsCommand::SetProgressVisibility(visible)
            }
            "auto_resize_images" => {
                let enabled = value == "on";
                self.settings.runtime.auto_resize_images = enabled;
                CodingAgentSettingsCommand::SetImageAutoResize(enabled)
            }
            "block_images" => {
                let enabled = value == "on";
                self.settings.runtime.block_images = enabled;
                CodingAgentSettingsCommand::SetImageBlocking(enabled)
            }
            "enable_skill_commands" => {
                let enabled = value == "on";
                self.settings.runtime.enable_skill_commands = enabled;
                CodingAgentSettingsCommand::SetSkillCommands(enabled)
            }
            "hide_thinking_block" => {
                let hidden = value == "on";
                self.settings.presentation.hide_thinking_block = hidden;
                CodingAgentSettingsCommand::SetThinkingVisibility(!hidden)
            }
            "quiet_startup" => {
                let quiet = value == "on";
                self.settings.presentation.quiet_startup = quiet;
                CodingAgentSettingsCommand::SetQuietStartup(quiet)
            }
            "clear_on_shrink" => {
                let enabled = value == "on";
                self.settings.presentation.clear_on_shrink = enabled;
                CodingAgentSettingsCommand::SetClearOnShrink(enabled)
            }
            "double_escape_action" => {
                let action = match value {
                    "fork" => CodingAgentDoubleEscapeAction::Fork,
                    "none" => CodingAgentDoubleEscapeAction::None,
                    _ => CodingAgentDoubleEscapeAction::Tree,
                };
                self.settings.presentation.double_escape_action = action;
                CodingAgentSettingsCommand::SetDoubleEscapeAction(action)
            }
            "default_thinking_level" => {
                let Ok(level) = value.parse::<CodingAgentThinkingLevel>() else {
                    return;
                };
                self.settings.runtime.default_thinking_level = Some(level);
                self.thinking_level = level;
                self.local.selected_thinking_level = Some(level);
                CodingAgentSettingsCommand::SetDefaultThinkingLevel(level)
            }
            "http_idle_timeout" => {
                let Some((_, timeout_ms)) = HTTP_IDLE_TIMEOUT_CHOICES
                    .iter()
                    .find(|(label, _)| *label == value)
                else {
                    return;
                };
                self.settings.runtime.http_idle_timeout_ms = *timeout_ms;
                CodingAgentSettingsCommand::SetHttpIdleTimeoutMs(*timeout_ms)
            }
            _ => return,
        };
        self.local.settings_command = Some(command);
    }

    /// Apply a built-in theme by name ("dark"/"light").
    fn apply_builtin_theme(&mut self, name: &str) {
        let snapshot = match name {
            "light" => CodingAgentThemeSnapshot::light(),
            _ => CodingAgentThemeSnapshot::dark(),
        };
        self.apply_theme_snapshot(snapshot);
    }

    /// Install a fully resolved product theme projection.
    pub(super) fn apply_theme_snapshot(&mut self, snapshot: CodingAgentThemeSnapshot) {
        self.theme = crate::interactive::theme::tui_theme_from_snapshot(&snapshot);
        self.resolved_theme = Some(snapshot);
        self.local.render_cache.clear();
    }

    /// Build a `MarkdownTheme` for the active resolved theme, wiring the
    /// syntax-highlight callback (TS `getMarkdownTheme` + `highlightCode`).
    /// Falls back to the palette theme's markdown styles when no resolved
    /// theme is set.
    fn markdown_theme(&self) -> MarkdownTheme {
        let mut md = match &self.resolved_theme {
            Some(resolved) => markdown_theme_from_resolved(resolved),
            None => self.theme.markdown.clone(),
        };
        if let Some(resolved) = &self.resolved_theme {
            let resolved = resolved.clone();
            md.highlight_code = Some(std::sync::Arc::new(
                move |code: &str, lang: Option<&str>| {
                    crate::interactive::syntax::highlight_code(code, lang, &resolved)
                },
            ));
        }
        md
    }

    /// Build the [`TranscriptRenderOptions`] used by transcript block
    /// rendering. Resolves styles from the active [`ResolvedTheme`] when
    /// available, falling back to the built-in palette otherwise.
    fn transcript_render_options(
        &self,
        width: usize,
        max_tool_result_lines: usize,
    ) -> TranscriptRenderOptions<'static> {
        TranscriptRenderOptions {
            width,
            max_tool_result_lines,
            color: color_enabled(),
            markdown_theme: self.markdown_theme(),
            hide_thinking_block: self.settings.presentation.hide_thinking_block,
            hidden_thinking_label: "Thinking...",
            styles: TranscriptStyles::from_theme(self.resolved_theme.as_ref()),
            view: Some(self.local.transcript_view.snapshot()),
            selected_block: (self.local.focus_ring.current()
                == Some(InteractiveRegion::Conversation))
            .then(|| self.local.transcript_view.selected())
            .flatten(),
            selection_gutter: true,
            show_images: self.settings.presentation.show_images,
            image_width_cells: self.settings.presentation.image_width_cells,
            terminal_capabilities: self.terminal_capabilities,
        }
    }

    pub(super) fn handle_settings_input(&mut self, event: &InputEvent) -> bool {
        if !self.local.selecting_settings {
            return false;
        }

        let before = self
            .local
            .settings_list
            .selected_item()
            .map(|item| (item.id.clone(), item.current_value.clone()));
        self.local.settings_list.handle_input(event);
        let after = self
            .local
            .settings_list
            .selected_item()
            .map(|item| (item.id.clone(), item.current_value.clone()));

        if let (Some((before_id, before_value)), Some((after_id, after_value))) = (before, after)
            && before_id == after_id
            && before_value != after_value
        {
            self.apply_settings_value(&after_id, &after_value);
        }
        true
    }

    pub(super) fn queue_auth_command(&mut self, command: CodingAgentAuthCommand) {
        self.local.auth_command = Some(command);
    }

    fn render_model_selector(&mut self, width: usize) -> Vec<String> {
        if !self.local.selecting_model {
            return Vec::new();
        }
        model_selector::render(
            &self.available_models,
            self.local.editor.text(),
            &mut self.local.model_selection_selected,
            width,
        )
    }

    fn render_session_selector(&mut self, width: usize) -> Vec<String> {
        if !self.local.selecting_session {
            return Vec::new();
        }
        session_selector::render(
            &self.session_choices,
            self.local.editor.text(),
            &mut self.local.session_selection_selected,
            width,
        )
    }

    fn render_editor_box(&mut self, width: usize) -> Vec<String> {
        let editor_width = width.saturating_sub(2);
        let editor_lines = self.local.editor.render_input(editor_width);
        let border = editor_border_line(width, &self.editor_border_style(), color_enabled());
        let mut lines = Vec::with_capacity(editor_lines.len() + 2);
        lines.push(border.clone());
        for (index, line) in editor_lines.into_iter().enumerate() {
            let prompt = if index == 0 { "> " } else { "  " };
            lines.push(fit_line(&format!("{prompt}{line}"), width));
        }
        lines.push(border);
        lines
    }

    pub(super) fn set_terminal_capabilities(&mut self, capabilities: TerminalCapabilities) {
        if self.terminal_capabilities != capabilities {
            self.terminal_capabilities = capabilities;
            self.local.render_cache.clear();
        }
    }

    fn sync_transcript_view(&mut self) {
        self.local.transcript_view.sync(&self.transcript);
    }

    pub(super) fn toggle_all_transcript_blocks(&mut self) -> bool {
        self.sync_transcript_view();
        let previous_scroll_offset = self.transcript.scroll_offset();
        let previous_rows = (previous_scroll_offset > 0).then(|| self.transcript_total_rows());
        let changed = self.local.transcript_view.toggle_all(&self.transcript);
        if changed && let Some(previous_rows) = previous_rows {
            let current_rows = self.transcript_total_rows();
            self.transcript.preserve_scrolled_view_after_row_change(
                previous_scroll_offset,
                previous_rows,
                current_rows,
            );
        }
        changed
    }

    pub(super) fn uses_per_block_transcript_view(&self) -> bool {
        true
    }

    pub(super) fn handle_shell_input(&mut self, event: &InputEvent) -> bool {
        if self.local.selecting_model
            || self.local.selecting_session
            || self.local.selecting_settings
        {
            return false;
        }
        if let InputEvent::Mouse(mouse) = event {
            return self.handle_shell_mouse(*mouse);
        }

        if matches_key(event, "escape") && self.close_child_conversation() {
            return true;
        }

        let mode = shell_layout_mode(self.viewport_width);
        if self.local.context_open && mode != ShellLayoutMode::Wide && matches_key(event, "escape")
        {
            self.close_context_overlay();
            return true;
        }
        if matches_key(event, "escape")
            && self.local.focus_ring.current() != Some(InteractiveRegion::Composer)
            && self.local.focus_ring.focus(InteractiveRegion::Composer)
        {
            self.apply_region_focus();
            return true;
        }
        if self.local.keybindings.matches(event, "app.context.toggle") {
            self.toggle_context(mode);
            return true;
        }

        let editor_accepts_tab = self.local.focus_ring.current()
            == Some(InteractiveRegion::Composer)
            && !self.local.editor.text().is_empty();
        if self.local.keybindings.matches(event, "app.focus.next") && !editor_accepts_tab {
            self.local.focus_ring.focus_next();
            self.apply_region_focus();
            return true;
        }
        if self.local.keybindings.matches(event, "app.focus.previous") {
            self.local.focus_ring.focus_previous();
            self.apply_region_focus();
            return true;
        }

        match self.local.focus_ring.current() {
            Some(InteractiveRegion::Conversation) => {
                self.sync_transcript_view();
                if self.local.keybindings.matches(event, "tui.select.up") || matches_key(event, "k")
                {
                    if self.local.transcript_view.select_previous(&self.transcript) {
                        self.ensure_selected_transcript_visible();
                    }
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.down")
                    || matches_key(event, "j")
                {
                    if self.local.transcript_view.select_next(&self.transcript) {
                        self.ensure_selected_transcript_visible();
                    }
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.confirm") {
                    if !self.open_selected_child_conversation() {
                        self.toggle_selected_transcript_block();
                    }
                    return true;
                }
                if matches_key(event, "space") || matches_key(event, "ctrl+o") {
                    self.toggle_selected_transcript_block();
                    return true;
                }
                if self
                    .local
                    .keybindings
                    .matches(event, "app.transcript.arguments")
                {
                    self.toggle_selected_transcript_arguments();
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.pageUp") {
                    self.transcript
                        .scroll_page_up(self.conversation_viewport_height.max(1));
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.pageDown") {
                    self.transcript
                        .scroll_page_down(self.conversation_viewport_height.max(1));
                    return true;
                }
                if matches_key(event, "home") {
                    self.local.transcript_view.select_first(&self.transcript);
                    self.ensure_selected_transcript_visible();
                    return true;
                }
                if matches_key(event, "end") {
                    self.local.transcript_view.select_last(&self.transcript);
                    self.ensure_selected_transcript_visible();
                    return true;
                }
            }
            Some(InteractiveRegion::Context) => {
                if self
                    .local
                    .keybindings
                    .matches(event, "app.context.previousTab")
                {
                    self.local.context_tab = self.local.context_tab.previous();
                    self.clamp_context_navigation();
                    return true;
                }
                if self.local.keybindings.matches(event, "app.context.nextTab") {
                    self.local.context_tab = self.local.context_tab.next();
                    self.clamp_context_navigation();
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.up") || matches_key(event, "k")
                {
                    if self.local.context_tab == ContextTab::Usage {
                        self.scroll_context(-1);
                    } else {
                        self.move_context_selection(-1);
                    }
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.down")
                    || matches_key(event, "j")
                {
                    if self.local.context_tab == ContextTab::Usage {
                        self.scroll_context(1);
                    } else {
                        self.move_context_selection(1);
                    }
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.pageUp") {
                    self.scroll_context(-(self.local.context_viewport_height.max(1) as isize));
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.pageDown") {
                    self.scroll_context(self.local.context_viewport_height.max(1) as isize);
                    return true;
                }
                if self.local.keybindings.matches(event, "tui.select.confirm") {
                    self.open_selected_context_detail();
                    return true;
                }
            }
            Some(InteractiveRegion::Composer) | None => return false,
        }

        if matches_key(event, "ctrl+c")
            || matches_key(event, "ctrl+o")
            || self.local.keybindings.matches(event, "app.model.next")
            || self.local.keybindings.matches(event, "app.model.previous")
        {
            return false;
        }
        true
    }

    fn handle_shell_mouse(&mut self, event: MouseEvent) -> bool {
        let point = Point::new(event.column, event.row);
        let target = self.local.mouse_hits.hit(point).copied();
        match event.kind {
            MouseEventKind::ScrollUp => {
                if target.is_some_and(InteractiveHitTarget::is_conversation) {
                    self.transcript.scroll_page_up(MOUSE_SCROLL_ROWS);
                } else if matches!(
                    target,
                    Some(
                        InteractiveHitTarget::Context
                            | InteractiveHitTarget::ContextTab(_)
                            | InteractiveHitTarget::ContextRow(_)
                    )
                ) {
                    self.scroll_context(-(MOUSE_SCROLL_ROWS as isize));
                }
            }
            MouseEventKind::ScrollDown => {
                if target.is_some_and(InteractiveHitTarget::is_conversation) {
                    self.transcript.scroll_page_down(MOUSE_SCROLL_ROWS);
                } else if matches!(
                    target,
                    Some(
                        InteractiveHitTarget::Context
                            | InteractiveHitTarget::ContextTab(_)
                            | InteractiveHitTarget::ContextRow(_)
                    )
                ) {
                    self.scroll_context(MOUSE_SCROLL_ROWS as isize);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => match target {
                Some(InteractiveHitTarget::TranscriptDisclosure(block_id)) => {
                    self.focus_shell_region(InteractiveRegion::Conversation);
                    self.select_transcript_block(block_id);
                    self.toggle_selected_transcript_block();
                }
                Some(InteractiveHitTarget::TranscriptBlock(block_id)) => {
                    self.focus_shell_region(InteractiveRegion::Conversation);
                    self.select_transcript_block(block_id);
                }
                Some(InteractiveHitTarget::Conversation) => {
                    self.focus_shell_region(InteractiveRegion::Conversation);
                }
                Some(InteractiveHitTarget::Context) => {
                    self.focus_shell_region(InteractiveRegion::Context);
                }
                Some(InteractiveHitTarget::ContextTab(tab)) => {
                    self.focus_shell_region(InteractiveRegion::Context);
                    self.local.context_tab = tab;
                    self.clamp_context_navigation();
                }
                Some(InteractiveHitTarget::ContextRow(index)) => {
                    self.focus_shell_region(InteractiveRegion::Context);
                    self.local.context_selection[self.local.context_tab.index()] = index;
                    self.ensure_context_selection_visible();
                }
                Some(InteractiveHitTarget::Composer) => {
                    self.focus_shell_region(InteractiveRegion::Composer);
                }
                None => {}
            },
            _ => {}
        }
        true
    }

    fn focus_shell_region(&mut self, region: InteractiveRegion) {
        if self.local.focus_ring.focus(region) {
            self.apply_region_focus();
        }
    }

    fn toggle_context(&mut self, mode: ShellLayoutMode) {
        if mode == ShellLayoutMode::Wide {
            self.local.focus_ring.focus(InteractiveRegion::Context);
            self.apply_region_focus();
            return;
        }
        if self.local.context_open {
            self.close_context_overlay();
        } else {
            self.local.context_restore_focus = self
                .local
                .focus_ring
                .current()
                .unwrap_or(InteractiveRegion::Composer);
            self.local.context_open = true;
            self.refresh_shell_focus();
        }
    }

    fn close_context_overlay(&mut self) {
        self.local.context_open = false;
        self.refresh_shell_focus();
        self.local
            .focus_ring
            .focus(self.local.context_restore_focus);
        self.apply_region_focus();
    }

    fn refresh_shell_focus(&mut self) {
        if self.active_child_operation_id.is_some() {
            self.local
                .focus_ring
                .set_items([InteractiveRegion::Conversation]);
            self.local.focus_ring.focus(InteractiveRegion::Conversation);
            self.apply_region_focus();
            return;
        }
        match shell_layout_mode(self.viewport_width) {
            ShellLayoutMode::Wide => {
                self.local.context_open = false;
                self.local.focus_ring.set_items([
                    InteractiveRegion::Conversation,
                    InteractiveRegion::Context,
                    InteractiveRegion::Composer,
                ]);
            }
            ShellLayoutMode::Medium | ShellLayoutMode::Narrow if self.local.context_open => {
                self.local
                    .focus_ring
                    .set_items([InteractiveRegion::Context]);
                self.local.focus_ring.focus(InteractiveRegion::Context);
            }
            ShellLayoutMode::Medium | ShellLayoutMode::Narrow => {
                self.local
                    .focus_ring
                    .set_items([InteractiveRegion::Conversation, InteractiveRegion::Composer]);
            }
        }
        self.apply_region_focus();
    }

    fn apply_region_focus(&mut self) {
        self.local
            .editor
            .set_focused(self.local.focus_ring.current() == Some(InteractiveRegion::Composer));
    }

    fn shell_layout(&self, composer_height: usize) -> ShellLayout {
        let width = self.viewport_width.max(1);
        let height = self.viewport_height.max(1);
        let mode = shell_layout_mode(width);
        let status_height = usize::from(height >= 2);
        let context_page = mode == ShellLayoutMode::Narrow && self.local.context_open;
        let maximum_composer = height.saturating_sub(status_height + 1).max(1);
        let composer_height = if context_page {
            0
        } else {
            composer_height.clamp(1, maximum_composer)
        };
        let rows = Layout::vertical(
            Rect::new(0, 0, width, height),
            &[
                Constraint::Fill(1),
                Constraint::Length(composer_height),
                Constraint::Length(status_height),
            ],
        );
        let work = rows[0];
        let composer = rows[1];
        let status = rows[2];

        match mode {
            ShellLayoutMode::Wide => {
                let context_width = (width / 3).clamp(26, 38).min(width.saturating_sub(2));
                let columns = Layout::horizontal(
                    work,
                    &[
                        Constraint::Fill(1),
                        Constraint::Length(1),
                        Constraint::Length(context_width),
                    ],
                );
                let side_rows = if work.height >= TIPS_MIN_HEIGHT {
                    Layout::vertical(
                        columns[2],
                        &[
                            Constraint::Fill(1),
                            Constraint::Length(1),
                            Constraint::Length(4),
                        ],
                    )
                } else {
                    Layout::vertical(columns[2], &[Constraint::Fill(1)])
                };
                ShellLayout {
                    mode,
                    conversation: columns[0],
                    conversation_context_divider: Some(columns[1]),
                    context_drawer_divider: None,
                    context: Some(side_rows[0]),
                    context_tips_divider: (side_rows.len() == 3).then(|| side_rows[1]),
                    tips: (side_rows.len() == 3).then(|| side_rows[2]),
                    composer,
                    status,
                    work,
                }
            }
            ShellLayoutMode::Medium => {
                let (context_drawer_divider, context) = if self.local.context_open {
                    let overlay_width = (width * 2 / 5).clamp(26, 38).min(width);
                    let drawer = Rect::new(
                        width.saturating_sub(overlay_width),
                        work.y,
                        overlay_width,
                        work.height,
                    );
                    (
                        Some(Rect::new(
                            drawer.x,
                            drawer.y,
                            1.min(drawer.width),
                            drawer.height,
                        )),
                        Some(Rect::new(
                            drawer.x.saturating_add(1),
                            drawer.y,
                            drawer.width.saturating_sub(1),
                            drawer.height,
                        )),
                    )
                } else {
                    (None, None)
                };
                ShellLayout {
                    mode,
                    conversation: work,
                    conversation_context_divider: None,
                    context_drawer_divider,
                    context,
                    context_tips_divider: None,
                    tips: None,
                    composer,
                    status,
                    work,
                }
            }
            ShellLayoutMode::Narrow => ShellLayout {
                mode,
                conversation: work,
                conversation_context_divider: None,
                context_drawer_divider: None,
                context: self.local.context_open.then_some(work),
                context_tips_divider: None,
                tips: None,
                composer,
                status,
                work,
            },
        }
    }

    fn rebuild_mouse_hit_regions(
        &mut self,
        layout: ShellLayout,
        conversation_body: Rect,
        transcript_total_rows: usize,
        block_rows: &[(TranscriptBlockId, TranscriptBlockRows)],
    ) {
        self.local.mouse_hits.clear();
        self.local.mouse_hits.push(HitRegion::new(
            layout.conversation,
            InteractiveHitTarget::Conversation,
        ));

        let (viewport_start, viewport_end) = transcript_viewport_bounds(
            transcript_total_rows,
            conversation_body.height,
            self.transcript.scroll_offset(),
        );
        for &(block_id, rows) in block_rows {
            let visible_start = rows.start.max(viewport_start);
            let visible_end = rows.end.min(viewport_end);
            if visible_start >= visible_end {
                continue;
            }
            let block_rect = Rect::new(
                conversation_body.x,
                conversation_body
                    .y
                    .saturating_add(visible_start.saturating_sub(viewport_start)),
                conversation_body.width,
                visible_end.saturating_sub(visible_start),
            );
            self.local.mouse_hits.push(HitRegion::new(
                block_rect,
                InteractiveHitTarget::TranscriptBlock(block_id),
            ));

            if rows.start >= viewport_start
                && rows.start < viewport_end
                && self
                    .transcript
                    .item_for_block(block_id)
                    .is_some_and(TranscriptItem::foldable)
            {
                self.local.mouse_hits.push(HitRegion::new(
                    Rect::new(
                        conversation_body.x,
                        conversation_body
                            .y
                            .saturating_add(rows.start.saturating_sub(viewport_start)),
                        conversation_body.width,
                        1,
                    ),
                    InteractiveHitTarget::TranscriptDisclosure(block_id),
                ));
            }
        }

        if let Some(context) = layout.context {
            self.local
                .mouse_hits
                .push(HitRegion::new(context, InteractiveHitTarget::Context));
            let mut tab_x = context.x.saturating_add(10);
            for (tab, label) in visible_context_tabs(context.width, self.local.context_tab) {
                let tab_width = visible_width(label)
                    .saturating_add(usize::from(tab == self.local.context_tab) * 2);
                if tab_x < context.right() {
                    self.local.mouse_hits.push(HitRegion::new(
                        Rect::new(
                            tab_x,
                            context.y,
                            tab_width.min(context.right().saturating_sub(tab_x)),
                            1,
                        ),
                        InteractiveHitTarget::ContextTab(tab),
                    ));
                }
                tab_x = tab_x.saturating_add(tab_width + 1);
            }
            if self.local.context_tab != ContextTab::Usage {
                let item_count = self.context_items(self.local.context_tab).len();
                let scroll = self.local.context_scroll[self.local.context_tab.index()];
                for (visible_index, item_index) in (scroll..item_count)
                    .take(context.height.saturating_sub(1))
                    .enumerate()
                {
                    self.local.mouse_hits.push(HitRegion::new(
                        Rect::new(
                            context.x,
                            context.y.saturating_add(1 + visible_index),
                            context.width,
                            1,
                        ),
                        InteractiveHitTarget::ContextRow(item_index),
                    ));
                }
            }
        }
        self.local.mouse_hits.push(HitRegion::new(
            layout.composer,
            InteractiveHitTarget::Composer,
        ));
    }

    fn render_fullscreen_shell(&mut self, width: usize) -> Vec<String> {
        let editor_lines = self.render_editor_box(width);
        let composer_height = editor_lines.len().clamp(1, MAX_COMPOSER_HEIGHT);
        let layout = self.shell_layout(composer_height);
        let mut frame = Frame::new(self.viewport_width, self.viewport_height);

        let conversation_body = panel_body(layout.conversation);
        self.conversation_viewport_width = conversation_body.width.max(1);
        self.conversation_viewport_height = conversation_body.height.max(1);
        let max_tool_result_lines = MAX_TOOL_RESULT_LINES;
        self.sync_transcript_view();
        let opts =
            self.transcript_render_options(conversation_body.width.max(1), max_tool_result_lines);
        let transcript_viewport = self.local.render_cache.render_viewport(
            &self.transcript,
            &opts,
            conversation_body.height,
            self.transcript.scroll_offset(),
        );
        self.rebuild_mouse_hit_regions(
            layout,
            conversation_body,
            transcript_viewport.total_rows,
            &transcript_viewport.block_rows,
        );
        frame.draw(
            Rect::new(
                layout.conversation.x,
                layout.conversation.y,
                layout.conversation.width,
                1.min(layout.conversation.height),
            ),
            &[self.panel_header(
                &self.conversation_header_title(layout.conversation.width),
                InteractiveRegion::Conversation,
                layout.conversation.width,
            )],
        );
        frame.draw(conversation_body, &transcript_viewport.lines);

        let divider_style = self.semantic_style(CodingAgentThemeForeground::BorderMuted, SYSTEM);
        if let Some(separator) = layout.conversation_context_divider {
            let line = paint_with("│", &divider_style, color_enabled());
            frame.draw(separator, &vec![line; separator.height]);
        }
        if let Some(separator) = layout.context_drawer_divider {
            let line = paint_with("│", &divider_style, color_enabled());
            frame.draw(separator, &vec![line; separator.height]);
        }
        if let Some(context) = layout.context {
            let context_lines = self.render_context_region(context.width, context.height);
            if layout.mode != ShellLayoutMode::Wide {
                frame.fill(context, "");
            }
            frame.draw(context, &context_lines);
        }
        if let Some(tips) = layout.tips {
            frame.draw(tips, &self.render_tips_region(tips.width, tips.height));
        }
        if let Some(divider) = layout.context_tips_divider {
            frame.draw(
                divider,
                &[paint_with(
                    &fit_line("─ Tips ", divider.width),
                    &divider_style,
                    color_enabled(),
                )],
            );
            if let Some(vertical) = layout.conversation_context_divider {
                frame.draw(
                    Rect::new(vertical.x, divider.y, vertical.width, 1),
                    &[paint_with("├", &divider_style, color_enabled())],
                );
            }
        }

        if !layout.composer.is_empty() {
            let composer_lines = tail_lines(&editor_lines, layout.composer.height);
            frame.draw(layout.composer, &composer_lines);
        }
        if !layout.status.is_empty() {
            frame.draw(
                layout.status,
                &[self.render_status_bar(layout.status.width)],
            );
        }

        frame.into_lines()
    }

    fn panel_header(&self, title: &str, region: InteractiveRegion, width: usize) -> String {
        let focused = self.local.focus_ring.current() == Some(region);
        let prefix = if focused { "▌ " } else { "  " };
        let fallback = if focused { USER } else { SYSTEM };
        let token = if focused {
            CodingAgentThemeForeground::BorderAccent
        } else {
            CodingAgentThemeForeground::BorderMuted
        };
        let style = self.semantic_style(token, fallback);
        fit_line(
            &paint_with(&format!("{prefix}{title}"), &style, color_enabled()),
            width,
        )
    }

    fn semantic_style(&self, token: CodingAgentThemeForeground, fallback: Style) -> Style {
        self.resolved_theme.as_ref().map_or(fallback, |resolved| {
            Style::fg(crate::interactive::theme::to_color(
                resolved.foreground(token),
            ))
        })
    }

    fn conversation_header_title(&self, width: usize) -> String {
        let base = if let Some(operation_id) = self.active_child_operation_id.as_deref() {
            let short = short_id(operation_id);
            let delegation = self
                .shared_projection
                .context()
                .delegations
                .iter()
                .find(|delegation| delegation.child_operation_id.as_deref() == Some(operation_id));
            let wide = delegation.map_or_else(
                || format!("Child · {short} · Esc back"),
                |delegation| {
                    format!(
                        "Child · {} · {} · {short} · Esc back",
                        delegation.target_id, delegation.status
                    )
                },
            );
            if visible_width(&wide).saturating_add(2) <= width {
                wide
            } else {
                let compact = format!("Child · {short} · Esc back");
                if visible_width(&compact).saturating_add(2) <= width {
                    compact
                } else {
                    format!("Child · {short} · Esc")
                }
            }
        } else {
            "Conversation".into()
        };

        let (scroll_status, compact_scroll_status) = if self.transcript.has_new_output_below() {
            (
                "↓ new output below · End latest".into(),
                "↓ new · End".into(),
            )
        } else if self.transcript.scroll_offset() > 0 {
            (
                format!("↑ {} rows · End latest", self.transcript.scroll_offset()),
                format!("↑{} · End", self.transcript.scroll_offset()),
            )
        } else {
            return base;
        };
        for status in [scroll_status, compact_scroll_status] {
            let candidate = format!("{base} · {status}");
            if visible_width(&candidate).saturating_add(2) <= width {
                return candidate;
            }
        }
        base
    }

    fn render_context_region(&mut self, width: usize, height: usize) -> Vec<String> {
        if width == 0 || height == 0 {
            return Vec::new();
        }
        let active_tab_style = self
            .semantic_style(CodingAgentThemeForeground::Accent, USER)
            .bold();
        let inactive_tab_style = self.semantic_style(CodingAgentThemeForeground::Muted, SYSTEM);
        let tabs = visible_context_tabs(width, self.local.context_tab)
            .into_iter()
            .map(|(tab, label)| {
                if tab == self.local.context_tab {
                    paint_with(&format!("[{label}]"), &active_tab_style, color_enabled())
                } else {
                    paint_with(label, &inactive_tab_style, color_enabled())
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let mut lines = vec![self.panel_header(
            &format!("Context {tabs}"),
            InteractiveRegion::Context,
            width,
        )];
        self.local.context_viewport_height = height.saturating_sub(1).max(1);
        let body = if self.local.context_tab == ContextTab::Usage {
            self.context_usage_lines(width)
        } else {
            self.context_list_lines()
        }
        .into_iter()
        .map(|line| self.style_context_body_line(line))
        .collect::<Vec<_>>();
        let scroll = self.local.context_scroll[self.local.context_tab.index()].min(
            body.len()
                .saturating_sub(self.local.context_viewport_height),
        );
        self.local.context_scroll[self.local.context_tab.index()] = scroll;
        lines.extend(
            body.into_iter()
                .skip(scroll)
                .take(self.local.context_viewport_height),
        );
        lines.truncate(height.max(1));
        lines
            .into_iter()
            .map(|line| fit_line(&line, width))
            .collect()
    }

    fn style_context_body_line(&self, line: String) -> String {
        if line.is_empty() {
            return line;
        }
        let (token, fallback, bold) = if matches!(
            line.as_str(),
            "session totals" | "latest turn" | "context window"
        ) {
            (CodingAgentThemeForeground::Accent, USER, true)
        } else if line.starts_with('›') {
            (CodingAgentThemeForeground::Accent, USER, false)
        } else if line.starts_with("no ") || line.contains("unavailable") {
            (CodingAgentThemeForeground::Muted, SYSTEM, false)
        } else {
            (CodingAgentThemeForeground::Text, Style::default(), false)
        };
        let mut style = self.semantic_style(token, fallback);
        style.bold = bold;
        paint_with(&line, &style, color_enabled())
    }

    fn context_list_lines(&mut self) -> Vec<String> {
        let items = self.context_items(self.local.context_tab);
        if items.is_empty() {
            return vec![
                match self.local.context_tab {
                    ContextTab::Ops => "no operations yet",
                    ContextTab::Changes => "no successful file changes yet",
                    ContextTab::Agents => "no agent inventory available",
                    ContextTab::Usage => "usage unavailable",
                }
                .into(),
            ];
        }
        let index = self.local.context_tab.index();
        self.local.context_selection[index] =
            self.local.context_selection[index].min(items.len() - 1);
        let selected = self.local.context_selection[index];
        let viewport = self.local.context_viewport_height.max(1);
        if selected < self.local.context_scroll[index] {
            self.local.context_scroll[index] = selected;
        } else if selected >= self.local.context_scroll[index].saturating_add(viewport) {
            self.local.context_scroll[index] = selected.saturating_add(1).saturating_sub(viewport);
        }
        items
            .into_iter()
            .enumerate()
            .map(|(item_index, item)| {
                let marker = if item_index == selected { "›" } else { " " };
                format!("{marker} {}", item.summary)
            })
            .collect()
    }

    fn context_items(&self, tab: ContextTab) -> Vec<ContextListItem> {
        match tab {
            ContextTab::Ops => self
                .shared_projection
                .context()
                .operations
                .iter()
                .map(|operation| self.operation_context_item(operation))
                .collect(),
            ContextTab::Changes => self
                .shared_projection
                .context()
                .changes
                .iter()
                .map(|change| self.change_context_item(change))
                .collect(),
            ContextTab::Agents => self.agent_context_items(),
            ContextTab::Usage => Vec::new(),
        }
    }

    fn operation_context_item(&self, operation: &CodingAgentOperationSnapshot) -> ContextListItem {
        let elapsed = self.operation_elapsed(operation);
        let cancellable = operation_status_is_running(operation.status)
            && self
                .shared_projection
                .context()
                .operations
                .iter()
                .find(|candidate| operation_status_is_running(candidate.status))
                .is_some_and(|candidate| candidate.operation_id == operation.operation_id)
            && self
                .shared_projection
                .capabilities()
                .is_some_and(|capabilities| {
                    matches!(capabilities.abort, CapabilityStatus::Available)
                });
        let cancel = if cancellable { " cancel" } else { "" };
        let summary = format!(
            "{:<9} {} {}{cancel}",
            operation_status_as_str(operation.status),
            operation.kind,
            elapsed
        );
        let mut detail_lines = vec![
            format!("kind: {}", operation.kind),
            format!("operation: {}", operation.operation_id),
            format!("status: {}", operation_status_as_str(operation.status)),
            format!("elapsed: {elapsed}"),
            format!(
                "cancel: {}",
                if cancellable {
                    "available"
                } else {
                    "unavailable"
                }
            ),
            format!(
                "parent: {}",
                operation.parent_operation_id.as_deref().unwrap_or("none")
            ),
            format!(
                "root: {}",
                operation.root_operation_id.as_deref().unwrap_or("none")
            ),
        ];
        if let Some(failure) = &operation.failure {
            detail_lines.push(format!("failure: {failure}"));
        }
        if operation.diagnostics.is_empty() {
            detail_lines.push("diagnostics: none".into());
        } else {
            detail_lines.push("diagnostics:".into());
            detail_lines.extend(
                operation
                    .diagnostics
                    .iter()
                    .map(|diagnostic| format!("- {diagnostic}")),
            );
        }
        ContextListItem {
            summary,
            detail_title: format!("Operation {}", short_id(&operation.operation_id)),
            detail_lines,
        }
    }

    fn change_context_item(&self, change: &CodingAgentFileChangeSnapshot) -> ContextListItem {
        let stats = match (change.added_lines, change.removed_lines) {
            (Some(added), Some(removed)) => format!(" +{added}/-{removed}"),
            (Some(added), None) => format!(" +{added}"),
            (None, Some(removed)) => format!(" -{removed}"),
            (None, None) => String::new(),
        };
        let age = self
            .local
            .context_change_timing
            .get(&change.path)
            .map(|(_, seen_at)| Instant::now().saturating_duration_since(*seen_at));
        let updated = age.map_or_else(
            || format!("event #{}", change.updated_sequence),
            |age| {
                if age.as_secs() == 0 {
                    format!("event #{} · now", change.updated_sequence)
                } else {
                    format!(
                        "event #{} · {} ago",
                        change.updated_sequence,
                        format_duration(age)
                    )
                }
            },
        );
        let mut detail_lines = vec![
            format!("path: {}", change.path),
            format!("mutation: {}", change.mutation_kind),
            format!("operation: {}", change.operation_id),
            format!(
                "tool call: {}",
                change.tool_call_id.as_deref().unwrap_or("unavailable")
            ),
            format!("updated: {updated}"),
            format!(
                "first changed line: {}",
                change
                    .first_changed_line
                    .map(|line| line.to_string())
                    .unwrap_or_else(|| "unavailable".into())
            ),
            format!(
                "diff stats: {}",
                if stats.is_empty() {
                    "unavailable"
                } else {
                    stats.trim()
                }
            ),
        ];
        if let Some(diff) = &change.diff {
            detail_lines.push("diff:".into());
            detail_lines.extend(diff.lines().map(ToOwned::to_owned));
        } else {
            detail_lines.push("diff: unavailable".into());
        }
        ContextListItem {
            summary: format!(
                "{:<8} {}{} · {}",
                change.mutation_kind,
                abbreviate_path(&change.path, 18),
                stats,
                if age.is_some_and(|age| age.as_secs() == 0) {
                    "now".into()
                } else {
                    age.map(format_duration).unwrap_or_else(|| "--".into())
                }
            ),
            detail_title: format!("Change {}", abbreviate_path(&change.path, 40)),
            detail_lines,
        }
    }

    fn operation_elapsed(&self, operation: &CodingAgentOperationSnapshot) -> String {
        self.shared_projection
            .operation_elapsed(operation)
            .map(format_duration)
            .unwrap_or_else(|| "--".into())
    }

    fn agent_context_items(&self) -> Vec<ContextListItem> {
        let mut items = Vec::new();
        let default_agent_profile_id = self.display_default_agent_profile_id();
        let active = self
            .profile_catalog
            .agent(default_agent_profile_id.as_str());
        if let Some(profile) = active {
            let mut details = vec![
                format!("id: {}", profile.id),
                format!("name: {}", profile.display_name),
                format!(
                    "description: {}",
                    profile.description.as_deref().unwrap_or("unavailable")
                ),
                format!(
                    "model: {}",
                    profile.model_id.as_deref().unwrap_or("session default")
                ),
                format!(
                    "tools: {}",
                    nonempty_join(&profile.tools, "session defaults")
                ),
                format!("skills: {}", nonempty_join(&profile.skills, "none")),
                format!("max delegation depth: {}", profile.delegation.max_depth),
                format!(
                    "max parallel children: {}",
                    profile.delegation.max_parallel_children
                ),
            ];
            details.push(format!(
                "delegation: agents={} teams={}",
                profile.delegation.allow_agents, profile.delegation.allow_teams
            ));
            items.push(ContextListItem {
                summary: format!("active  {} · {}", profile.id, profile.display_name),
                detail_title: format!("Agent profile {}", profile.id),
                detail_lines: details,
            });

            if profile.delegation.allow_agents {
                for profile_id in &profile.delegation.agent_targets {
                    if let Some(target) = self.profile_catalog.agent(profile_id.as_str()) {
                        items.push(ContextListItem {
                            summary: format!("agent   {} · {}", target.id, target.display_name),
                            detail_title: format!("Delegation target {}", target.id),
                            detail_lines: vec![
                                "kind: agent".into(),
                                format!("id: {}", target.id),
                                format!("name: {}", target.display_name),
                                format!(
                                    "description: {}",
                                    target.description.as_deref().unwrap_or("unavailable")
                                ),
                                format!(
                                    "tools: {}",
                                    nonempty_join(&target.tools, "session defaults")
                                ),
                                format!("skills: {}", nonempty_join(&target.skills, "none")),
                            ],
                        });
                    }
                }
            }
            if profile.delegation.allow_teams {
                for profile_id in &profile.delegation.team_targets {
                    if let Some(target) = self.profile_catalog.team(profile_id.as_str()) {
                        items.push(ContextListItem {
                            summary: format!("team    {} · {}", target.id, target.display_name),
                            detail_title: format!("Delegation team {}", target.id),
                            detail_lines: vec![
                                "kind: team".into(),
                                format!("id: {}", target.id),
                                format!("name: {}", target.display_name),
                                format!(
                                    "description: {}",
                                    target.description.as_deref().unwrap_or("unavailable")
                                ),
                                format!(
                                    "members: {}",
                                    target
                                        .members
                                        .iter()
                                        .map(ToString::to_string)
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                            ],
                        });
                    }
                }
            }
        } else {
            items.push(ContextListItem {
                summary: format!("active  {default_agent_profile_id} · unavailable"),
                detail_title: "Active agent profile unavailable".into(),
                detail_lines: vec![format!("id: {default_agent_profile_id}")],
            });
        }

        items.extend(
            self.shared_projection
                .context()
                .delegations
                .iter()
                .map(|delegation| {
                    let mut detail_lines = vec![
                        format!("kind: {}", delegation.target_kind),
                        format!("target: {}", delegation.target_id),
                        format!("status: {}", delegation.status),
                        format!("tool call: {}", delegation.tool_call_id),
                        format!(
                            "child operation: {}",
                            delegation
                                .child_operation_id
                                .as_deref()
                                .unwrap_or("unavailable")
                        ),
                        format!("task: {}", delegation.task),
                    ];
                    if let Some(summary) = &delegation.summary {
                        detail_lines.push(format!("summary: {summary}"));
                    }
                    if let Some(failure) = &delegation.failure {
                        detail_lines.push(format!("failure: {failure}"));
                    }
                    ContextListItem {
                        summary: format!(
                            "child   {} {} · {}",
                            delegation.target_id, delegation.target_kind, delegation.status
                        ),
                        detail_title: format!(
                            "Delegated {} {}",
                            delegation.target_kind, delegation.target_id
                        ),
                        detail_lines,
                    }
                }),
        );
        items
    }

    fn context_usage_lines(&self, width: usize) -> Vec<String> {
        let usage = &self.shared_projection.context().usage;
        let mut lines = vec![
            "session totals".into(),
            format!("input       {}", format_token_total(usage.input)),
            format!("output      {}", format_token_total(usage.output)),
            format!("cache read  {}", format_token_total(usage.cache_read)),
            format!("cache write {}", format_token_total(usage.cache_write)),
            format!(
                "cost         {}",
                usage
                    .cost
                    .map(|cost| format!("${cost:.4}"))
                    .unwrap_or_else(|| "unavailable".into())
            ),
            String::new(),
            "latest turn".into(),
        ];
        if let Some(turn) = &usage.latest_turn {
            lines.extend([
                format!("turn         {}", short_id(&turn.turn_id)),
                format!("input        {}", format_tokens(turn.input)),
                format!("output       {}", format_tokens(turn.output)),
                format!("cache read   {}", format_tokens(turn.cache_read)),
                format!("cache write  {}", format_tokens(turn.cache_write)),
                format!(
                    "cost          {}",
                    turn.cost
                        .map(|cost| format!("${cost:.4}"))
                        .unwrap_or_else(|| "unavailable".into())
                ),
            ]);
        } else {
            lines.push("unavailable".into());
        }
        lines.push(String::new());
        lines.push("context window".into());
        let context_tokens = usage
            .latest_turn
            .as_ref()
            .and_then(|turn| turn.context_tokens);
        let context_window = usage.context_window;
        lines.push(match (context_tokens, context_window) {
            (Some(tokens), Some(window)) if window > 0 => {
                let exact = format!("{}/{}", format_tokens(tokens), format_tokens(window));
                let percent = format!("{}%", context_percentage(tokens, window));
                let fixed_width = visible_width("used          ")
                    .saturating_add(2)
                    .saturating_add(1 + visible_width(&percent))
                    .saturating_add(1 + visible_width(&exact));
                let gauge_width = width.saturating_sub(fixed_width).min(12);
                let gauge_width = usize::from(gauge_width >= 3) * gauge_width;
                format!(
                    "used          {} {exact}",
                    context_gauge(tokens, window, gauge_width, !color_enabled()),
                )
            }
            (Some(tokens), Some(0)) => {
                format!("used          unavailable ({})", format_tokens(tokens))
            }
            _ => "used          unavailable".into(),
        });
        lines.push(format!(
            "model         {}",
            usage.model_id.as_deref().unwrap_or("unavailable")
        ));
        lines
    }

    fn render_tips_region(&self, width: usize, height: usize) -> Vec<String> {
        let key = |id: &str| {
            self.local
                .keybindings
                .get_keys(id)
                .into_iter()
                .next()
                .unwrap_or_else(|| "?".into())
        };
        let mut tips: Vec<(u8, usize, String)> = Vec::new();
        let mut order = 0;
        let mut push = |priority: u8, text: String| {
            tips.push((priority, order, text));
            order += 1;
        };

        if self.active_child_operation_id.is_some() {
            push(0, format!("{}  back", key("tui.select.cancel")));
        } else if !self.tool_authorizations.is_empty() {
            push(0, format!("{}  choose", key("tui.select.confirm")));
            push(0, format!("{}  deny", key("tui.select.cancel")));
        } else if self.local.selecting_settings
            || self.local.selecting_model
            || self.local.selecting_session
            || self.local.selecting_tree
            || self.delegation_confirmation_menu.is_some()
            || self.profile_menu.is_some()
        {
            push(0, format!("{}  close", key("tui.select.cancel")));
            push(
                1,
                format!(
                    "{} / {}  select",
                    key("tui.select.up"),
                    key("tui.select.down")
                ),
            );
        }
        match self.local.focus_ring.current() {
            Some(InteractiveRegion::Conversation) => {
                push(
                    1,
                    format!(
                        "{} / {}  select",
                        key("tui.select.up"),
                        key("tui.select.down")
                    ),
                );
                if self
                    .local
                    .transcript_view
                    .selected()
                    .and_then(|block_id| self.transcript.item_for_block(block_id))
                    .is_some_and(TranscriptItem::foldable)
                {
                    push(0, format!("{}  disclose", key("tui.select.confirm")));
                }
                if self
                    .local
                    .transcript_view
                    .selected_has_tool_arguments(&self.transcript)
                {
                    push(1, format!("{}  arguments", key("app.transcript.arguments")));
                }
            }
            Some(InteractiveRegion::Context) => {
                push(
                    1,
                    format!(
                        "{} / {}  tabs",
                        key("app.context.previousTab"),
                        key("app.context.nextTab")
                    ),
                );
                push(
                    1,
                    format!(
                        "{} / {}  {}",
                        key("tui.select.up"),
                        key("tui.select.down"),
                        if self.local.context_tab == ContextTab::Usage {
                            "scroll"
                        } else {
                            "select"
                        }
                    ),
                );
                if self.local.context_tab != ContextTab::Usage
                    && !self.context_items(self.local.context_tab).is_empty()
                {
                    push(0, format!("{}  detail", key("tui.select.confirm")));
                }
                if self
                    .shared_projection
                    .capabilities()
                    .is_some_and(|capabilities| {
                        matches!(capabilities.abort, CapabilityStatus::Available)
                    })
                {
                    push(0, format!("{}  cancel", key("app.interrupt")));
                }
            }
            Some(InteractiveRegion::Composer) => {
                push(0, format!("{}  submit", key("tui.input.submit")));
            }
            None => {}
        }
        push(8, format!("{}  context", key("app.context.toggle")));
        push(
            9,
            format!(
                "{} / {}  focus",
                key("app.focus.next"),
                key("app.focus.previous")
            ),
        );
        tips.sort_by_key(|(priority, insertion, _)| (*priority, *insertion));
        let mut lines = tips
            .into_iter()
            .map(|(priority, _, tip)| {
                let fallback = if priority <= 1 { USER } else { SYSTEM };
                let token = if priority <= 1 {
                    CodingAgentThemeForeground::Accent
                } else {
                    CodingAgentThemeForeground::Muted
                };
                let style = self.semantic_style(token, fallback);
                fit_line(&paint_with(&tip, &style, color_enabled()), width)
            })
            .collect::<Vec<_>>();
        lines.truncate(height);
        lines
    }

    /// The currently active model for display (context window, reasoning,
    /// provider). Distinct from `selected_model`, which is consumed by
    /// `take_selected_model` to apply a pending change to the agent.
    fn current_model(&self) -> Option<&CodingAgentModelCatalogEntry> {
        self.model.as_ref()
    }

    fn render_status_bar(&self, width: usize) -> String {
        let active_kind = self
            .shared_projection
            .context()
            .operations
            .iter()
            .find(|operation| operation_status_is_running(operation.status))
            .map(|operation| operation.kind.as_str());
        let (state, state_token, state_fallback) = match self.status {
            InteractiveStatus::Idle => (
                "● idle".to_string(),
                CodingAgentThemeForeground::Success,
                STATUS_IDLE,
            ),
            InteractiveStatus::Running => active_kind.map_or_else(
                || {
                    (
                        running_status_text(self.spinner_frame),
                        CodingAgentThemeForeground::Accent,
                        STATUS_RUNNING,
                    )
                },
                |kind| {
                    (
                        format!("{} {kind}", running_status_text(self.spinner_frame)),
                        CodingAgentThemeForeground::Accent,
                        STATUS_RUNNING,
                    )
                },
            ),
        };
        let state = paint_with(
            &state,
            &self.semantic_style(state_token, state_fallback),
            color_enabled(),
        );
        let mut segments = vec![state];
        let permission_token = match self.permission_mode {
            ToolAuthorizationMode::Plan => CodingAgentThemeForeground::Warning,
            ToolAuthorizationMode::Ask => CodingAgentThemeForeground::Accent,
            ToolAuthorizationMode::Yolo => CodingAgentThemeForeground::Error,
        };
        segments.push(paint_with(
            &format!("{}", self.permission_mode).to_uppercase(),
            &self.semantic_style(permission_token, SYSTEM),
            color_enabled(),
        ));
        let context_usage = self
            .shared_projection
            .context()
            .usage
            .latest_turn
            .as_ref()
            .and_then(|turn| turn.context_tokens)
            .zip(self.shared_projection.context().usage.context_window)
            .map(|(tokens, window)| {
                if window == 0 {
                    return paint_with(
                        &format!("ctx unavailable ({})", format_tokens(tokens)),
                        &self.semantic_style(CodingAgentThemeForeground::Muted, SYSTEM),
                        color_enabled(),
                    );
                }
                let bar_width = if width >= WIDE_LAYOUT_MIN_WIDTH {
                    7
                } else if width >= MEDIUM_LAYOUT_MIN_WIDTH {
                    4
                } else {
                    0
                };
                let text = if bar_width == 0 {
                    format!("ctx {}%", context_percentage(tokens, window))
                } else {
                    format!(
                        "ctx {}",
                        context_gauge(tokens, window, bar_width, !color_enabled())
                    )
                };
                let percent = context_percentage(tokens, window);
                let token = if percent > 90 {
                    CodingAgentThemeForeground::Error
                } else if percent > 70 {
                    CodingAgentThemeForeground::Warning
                } else {
                    CodingAgentThemeForeground::Accent
                };
                paint_with(&text, &self.semantic_style(token, SYSTEM), color_enabled())
            });
        let cost = self.shared_projection.context().usage.cost.map(|cost| {
            paint_with(
                &format!("${cost:.4}"),
                &self.semantic_style(CodingAgentThemeForeground::Muted, SYSTEM),
                color_enabled(),
            )
        });
        if context_usage.is_some() || cost.is_some() {
            segments.push(
                [context_usage, cost]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        segments.push(paint_with(
            self.display_default_agent_profile_id().as_str(),
            &self.semantic_style(CodingAgentThemeForeground::Text, Style::default()),
            color_enabled(),
        ));
        segments.push(paint_with(
            &format!(
                "{} · {}",
                self.current_model()
                    .map(|model| model.id.as_str())
                    .unwrap_or("no-model"),
                self.thinking_level
            ),
            &self.semantic_style(CodingAgentThemeForeground::Text, Style::default()),
            color_enabled(),
        ));
        segments.push(paint_with(
            &abbreviate_cwd(&self.cwd),
            &self.semantic_style(CodingAgentThemeForeground::Muted, SYSTEM),
            color_enabled(),
        ));

        let mut rendered = format!(" {}", segments[0]);
        for segment in segments.into_iter().skip(1) {
            let candidate = format!("{rendered}   {segment}");
            if visible_width(&candidate) > width {
                break;
            }
            rendered = candidate;
        }
        fit_line(&rendered, width)
    }

    fn render_transient_prompts(&self, width: usize) -> Vec<String> {
        let mut lines = self.render_pending_delegation_rejection_reason(width);
        lines.extend(self.render_pending_profile_task(width));
        lines
    }

    fn render_modal_surface(&mut self, width: usize) -> Vec<String> {
        let content_width = width.saturating_sub(3).max(1);
        if self.local.selecting_tree {
            if let Some(ref selector) = self.local.tree_selector {
                return self.framed_modal(selector.render(content_width), width);
            }
            return Vec::new();
        }
        if !self.tool_authorizations.is_empty() {
            return self.render_tool_authorization(width);
        }
        if self.delegation_confirmation_menu.is_some() {
            let lines = self.render_delegation_confirmation_menu(content_width);
            return self.framed_modal(lines, width);
        }
        if self.profile_menu.is_some() {
            let lines = self.render_profile_menu(content_width);
            return self.framed_modal(lines, width);
        }
        if self.local.selecting_model {
            let lines = self.render_model_selector(content_width);
            return self.framed_modal(lines, width);
        }
        if self.local.selecting_session {
            let lines = self.render_session_selector(content_width);
            return self.framed_modal(lines, width);
        }
        if self.local.selecting_settings {
            let lines = self.render_settings_menu(content_width);
            return self.framed_modal(lines, width);
        }
        if self.local.context_detail.is_some() {
            let lines = self.render_context_detail(content_width);
            return self.framed_modal(lines, width);
        }
        Vec::new()
    }

    fn framed_modal(&self, lines: Vec<String>, width: usize) -> Vec<String> {
        let border_style = self.semantic_style(CodingAgentThemeForeground::Border, SYSTEM);
        framed_modal_lines(lines, width, &border_style, color_enabled())
    }

    fn render_context_detail(&mut self, width: usize) -> Vec<String> {
        let Some(detail) = self.local.context_detail.as_mut() else {
            return Vec::new();
        };
        let viewport = self.viewport_height.saturating_sub(8).clamp(3, 20);
        detail.scroll = detail
            .scroll
            .min(detail.lines.len().saturating_sub(viewport));
        let mut lines = vec![fit_line(&detail.title, width)];
        lines.extend(
            detail
                .lines
                .iter()
                .skip(detail.scroll)
                .take(viewport)
                .map(|line| fit_line(line, width)),
        );
        lines.push(fit_line("Up/Down scroll · Enter/Esc close", width));
        lines
    }

    fn render_completion_surface(&mut self, width: usize) -> Vec<String> {
        let slash = self.render_slash_suggestions(width);
        if slash.is_empty() {
            self.local.editor.render_assistance(width)
        } else {
            slash
        }
    }

    fn transcript_row_snapshot(&mut self, max_tool_result_lines: usize) -> TranscriptRowSnapshot {
        self.sync_transcript_view();
        let opts =
            self.transcript_render_options(self.transcript_render_width(), max_tool_result_lines);
        self.local
            .render_cache
            .row_snapshot(&self.transcript, &opts)
    }

    fn transcript_row_delta_since(
        &mut self,
        snapshot: TranscriptRowSnapshot,
        changed_indices: &[usize],
        max_tool_result_lines: usize,
        anchor_start_row: Option<usize>,
    ) -> isize {
        self.sync_transcript_view();
        let opts =
            self.transcript_render_options(self.transcript_render_width(), max_tool_result_lines);
        self.local.render_cache.row_delta_since(
            &self.transcript,
            &opts,
            snapshot,
            changed_indices,
            anchor_start_row,
        )
    }

    fn transcript_render_width(&self) -> usize {
        self.conversation_viewport_width.max(1)
    }

    fn transcript_total_rows(&mut self) -> usize {
        let opts =
            self.transcript_render_options(self.transcript_render_width(), MAX_TOOL_RESULT_LINES);
        self.local
            .render_cache
            .row_snapshot(&self.transcript, &opts)
            .total_rows()
    }

    fn toggle_selected_transcript_block(&mut self) -> bool {
        let previous_scroll_offset = self.transcript.scroll_offset();
        let previous_rows = (previous_scroll_offset > 0).then(|| self.transcript_total_rows());
        let changed = self.local.transcript_view.toggle_selected(&self.transcript);
        if !changed {
            return false;
        }
        // Keep the viewport anchored: when scrolled, the previously visible
        // rows stay put and the expanded rows push the content below them
        // downward; when pinned to the tail the new rows simply extend the
        // tail. Scrolling the expanded block to the viewport top (what
        // ensure_selected_transcript_visible does for navigation) would jump
        // the transcript to the top and hide everything before the block.
        if let Some(previous_rows) = previous_rows {
            let current_rows = self.transcript_total_rows();
            self.transcript.preserve_scrolled_view_after_row_change(
                previous_scroll_offset,
                previous_rows,
                current_rows,
            );
        }
        true
    }

    fn select_transcript_block(&mut self, block_id: TranscriptBlockId) -> bool {
        self.sync_transcript_view();
        let changed = self
            .local
            .transcript_view
            .select(&self.transcript, block_id);
        if changed {
            self.ensure_selected_transcript_visible();
        }
        changed
    }

    fn toggle_selected_transcript_arguments(&mut self) -> bool {
        let previous_scroll_offset = self.transcript.scroll_offset();
        let previous_rows = (previous_scroll_offset > 0).then(|| self.transcript_total_rows());
        let changed = self
            .local
            .transcript_view
            .toggle_selected_arguments(&self.transcript);
        if !changed {
            return false;
        }
        if let Some(previous_rows) = previous_rows {
            let current_rows = self.transcript_total_rows();
            self.transcript.preserve_scrolled_view_after_row_change(
                previous_scroll_offset,
                previous_rows,
                current_rows,
            );
        }
        true
    }

    fn ensure_selected_transcript_visible(&mut self) {
        let Some(selected) = self.local.transcript_view.selected() else {
            return;
        };
        let opts =
            self.transcript_render_options(self.transcript_render_width(), MAX_TOOL_RESULT_LINES);
        let Some(rows) = self
            .local
            .render_cache
            .block_rows(&self.transcript, &opts, selected)
        else {
            return;
        };
        self.transcript.ensure_row_range_visible(
            rows.total_rows,
            rows.start,
            rows.end,
            self.conversation_viewport_height.max(1),
        );
    }

    pub(super) fn handle_slash_suggestion_input(&mut self, event: &InputEvent) -> bool {
        if self.local.selecting_model
            || self.local.selecting_settings
            || self.local.selecting_session
            || self.delegation_confirmation_menu.is_some()
            || self.profile_menu.is_some()
            || self.pending_profile_task.is_some()
        {
            return false;
        }
        let commands = self.all_slash_commands();
        slash::handle_suggestion_input(
            &self.local.keybindings,
            event,
            &mut self.local.editor,
            &mut self.slash_suggestion_selected,
            &mut self.slash_suggestions_dismissed_for,
            &commands,
        )
    }

    pub(super) fn handle_model_selection_input(&mut self, event: &InputEvent) -> bool {
        if !self.local.selecting_model {
            return false;
        }

        match model_selector::handle_input(
            &self.local.keybindings,
            event,
            &mut self.local.editor,
            &mut self.local.model_selection_selected,
            &self.available_models,
        ) {
            model_selector::SelectorInput::Handled => {}
            model_selector::SelectorInput::Cancel => {
                self.local.selecting_model = false;
                self.local.model_selection_selected = 0;
                self.local.editor.set_text("");
                self.transcript.push(TranscriptItem::system(
                    "Model selection canceled".to_string(),
                ));
            }
            model_selector::SelectorInput::Confirm(Some(model_index)) => {
                let model = self.available_models[model_index].clone();
                self.set_selected_model(model);
            }
            model_selector::SelectorInput::Confirm(None) => {}
        }
        true
    }

    pub(super) fn handle_tree_selection_input(&mut self, event: &InputEvent) -> bool {
        if !self.local.selecting_tree {
            return false;
        }

        let Some(selector) = self.local.tree_selector.as_mut() else {
            return false;
        };

        match selector.handle_input(&self.local.keybindings, event) {
            TreeSelectorInput::Cancel => {
                self.local.selecting_tree = false;
                self.local.tree_selector = None;
                self.local.selected_tree_entry_id = None;
                self.local.editor.set_text("");
            }
            TreeSelectorInput::Confirm(Some(entry_id)) => {
                self.local.selected_tree_entry_id = Some(entry_id);
                self.local.selecting_tree = false;
                self.local.tree_selector = None;
            }
            TreeSelectorInput::Confirm(None) => {}
            TreeSelectorInput::EditLabel { .. } => {
                // Label edit is handled inside the selector state
            }
            TreeSelectorInput::SaveLabel { entry_id, label } => {
                self.local.pending_tree_label_change = Some((entry_id, label));
            }
            TreeSelectorInput::Handled => {}
        }
        true
    }

    pub(super) fn handle_session_selection_input(&mut self, event: &InputEvent) -> bool {
        if !self.local.selecting_session {
            return false;
        }

        match session_selector::handle_input(
            &self.local.keybindings,
            event,
            &mut self.local.editor,
            &mut self.local.session_selection_selected,
            &self.session_choices,
        ) {
            session_selector::SelectorInput::Handled => {}
            session_selector::SelectorInput::Cancel => {
                self.local.selecting_session = false;
                self.local.session_selection_selected = 0;
                self.local.editor.set_text("");
                self.transcript.push(TranscriptItem::system(
                    "Session selection canceled".to_string(),
                ));
            }
            session_selector::SelectorInput::Confirm(Some(session_index)) => {
                let choice = self.session_choices[session_index].clone();
                self.set_selected_session(choice);
            }
            session_selector::SelectorInput::Confirm(None) => {}
        }
        true
    }
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

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() >= 60 {
        format!(
            "{}m{:02}s",
            duration.as_secs() / 60,
            duration.as_secs() % 60
        )
    } else if duration.as_secs() > 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

fn short_id(value: &str) -> String {
    const MAX: usize = 10;
    let mut characters = value.chars();
    let short = characters.by_ref().take(MAX).collect::<String>();
    if characters.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

fn abbreviate_path(path: &str, max_characters: usize) -> String {
    let characters = path.chars().collect::<Vec<_>>();
    if characters.len() <= max_characters {
        return path.to_owned();
    }
    let keep = max_characters.saturating_sub(1);
    format!(
        "…{}",
        characters[characters.len().saturating_sub(keep)..]
            .iter()
            .collect::<String>()
    )
}

fn nonempty_join(values: &[String], empty: &str) -> String {
    if values.is_empty() {
        empty.into()
    } else {
        values.join(", ")
    }
}

fn format_token_total(count: u64) -> String {
    u32::try_from(count).map_or_else(
        |_| format!("{:.1}B", count as f64 / 1_000_000_000.0),
        format_tokens,
    )
}

fn context_percentage(tokens: u32, window: u32) -> u64 {
    debug_assert!(
        window > 0,
        "zero context windows are rendered as unavailable"
    );
    (u64::from(tokens) * 100 + u64::from(window) / 2) / u64::from(window)
}

fn context_gauge(tokens: u32, window: u32, bar_width: usize, ascii: bool) -> String {
    debug_assert!(
        window > 0,
        "zero context windows are rendered as unavailable"
    );
    let percent = context_percentage(tokens, window);
    if bar_width == 0 {
        return format!("{percent}%");
    }
    let filled = ((u64::from(tokens) * bar_width as u64 + u64::from(window) / 2)
        / u64::from(window))
    .min(bar_width as u64) as usize;
    let (filled_glyph, empty_glyph) = if ascii { ('#', '-') } else { ('█', '░') };
    format!(
        "[{}{}] {percent}%",
        filled_glyph.to_string().repeat(filled),
        empty_glyph
            .to_string()
            .repeat(bar_width.saturating_sub(filled))
    )
}

fn transcript_viewport_bounds(
    total_rows: usize,
    height: usize,
    scroll_offset: usize,
) -> (usize, usize) {
    if height == 0 || total_rows == 0 {
        return (0, 0);
    }
    let max_offset = total_rows.saturating_sub(height);
    let offset = scroll_offset.min(max_offset);
    let end = total_rows.saturating_sub(offset);
    (end.saturating_sub(height), end)
}

fn shell_layout_mode(width: usize) -> ShellLayoutMode {
    if width >= WIDE_LAYOUT_MIN_WIDTH {
        ShellLayoutMode::Wide
    } else if width >= MEDIUM_LAYOUT_MIN_WIDTH {
        ShellLayoutMode::Medium
    } else {
        ShellLayoutMode::Narrow
    }
}

/// The rendered column width of the modal overlay for a given role, matching
/// the overlay geometry in `transient_overlay_options` and the tui overlay
/// width resolution so modal content (including its border) is sized to the
/// visible surface instead of the full terminal.
fn modal_overlay_width(role: TransientOverlayRole, terminal_width: usize) -> usize {
    let available = match role {
        TransientOverlayRole::ModalDialog => terminal_width.saturating_sub(4),
        _ => terminal_width,
    };
    let requested = match role {
        TransientOverlayRole::ModalDialog => 72,
        TransientOverlayRole::ContextRailDetail => 38,
        TransientOverlayRole::ContextDrawerDetail => available.saturating_mul(40) / 100,
        _ => available,
    };
    requested.min(available).max(1)
}

fn visible_context_tabs(width: usize, active: ContextTab) -> Vec<(ContextTab, &'static str)> {
    if width < 26 {
        return vec![(active, active.label())];
    }
    let full_width = 10
        + ContextTab::ALL
            .iter()
            .map(|tab| tab.label().len() + usize::from(*tab == active) * 2)
            .sum::<usize>()
        + ContextTab::ALL.len().saturating_sub(1);
    ContextTab::ALL
        .into_iter()
        .map(|tab| {
            let label = if full_width <= width {
                tab.label()
            } else {
                tab.compact_label()
            };
            (tab, label)
        })
        .collect()
}

fn panel_body(panel: Rect) -> Rect {
    Rect::new(
        panel.x,
        panel.y.saturating_add(usize::from(panel.height > 0)),
        panel.width,
        panel.height.saturating_sub(1),
    )
}

fn tail_lines(lines: &[String], height: usize) -> Vec<String> {
    if height == 0 {
        return Vec::new();
    }
    lines[lines.len().saturating_sub(height)..].to_vec()
}

fn build_settings_list(
    settings: CodingAgentSettingsSnapshot,
    theme: &TuiTheme,
    keybindings: KeybindingsManager,
) -> SettingsList {
    SettingsList::with_options(
        vec![
            SettingItem::new("theme", "Theme", theme.name.clone())
                .values(["dark", "light"])
                .description("Change the active interface theme"),
            SettingItem::new(
                "auto_compaction",
                "Auto compact",
                if settings.runtime.auto_compaction {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Automatically compact context before it exceeds the model window"),
            SettingItem::new(
                "steering_mode",
                "Steering mode",
                settings.runtime.steering_mode.as_str(),
            )
            .values(["one-at-a-time", "all"])
            .description("Enter while streaming queues steering messages ('one-at-a-time' delivers one at a time)"),
            SettingItem::new(
                "follow_up_mode",
                "Follow-up mode",
                settings.runtime.follow_up_mode.as_str(),
            )
            .values(["one-at-a-time", "all"])
            .description("Queue follow-up messages until agent stops"),
            SettingItem::new(
                "show_progress",
                "Terminal progress",
                if settings.presentation.show_progress {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Show progress indicators in terminal tab bar"),
            SettingItem::new(
                "auto_resize_images",
                "Auto-resize images",
                if settings.runtime.auto_resize_images {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Resize large images to 2000\u{d7}2000 max for better model compatibility"),
            SettingItem::new(
                "block_images",
                "Block images",
                if settings.runtime.block_images {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Prevent images from being sent to LLM providers"),
            SettingItem::new(
                "enable_skill_commands",
                "Skill commands",
                if settings.runtime.enable_skill_commands {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Register skills as /skill:name commands"),
            SettingItem::new(
                "hide_thinking_block",
                "Hide thinking",
                if settings.presentation.hide_thinking_block {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Hide thinking blocks in assistant responses"),
            SettingItem::new(
                "quiet_startup",
                "Quiet startup",
                if settings.presentation.quiet_startup {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Disable verbose printing at startup"),
            SettingItem::new(
                "clear_on_shrink",
                "Clear on shrink",
                if settings.presentation.clear_on_shrink {
                    "on"
                } else {
                    "off"
                },
            )
            .values(["on", "off"])
            .description("Clear empty rows when content shrinks (may cause flicker)"),
            SettingItem::new(
                "double_escape_action",
                "Double-escape action",
                settings.presentation.double_escape_action.as_str(),
            )
            .values(["tree", "fork", "none"])
            .description("Action when pressing Escape twice with empty editor"),
            SettingItem::new(
                "default_thinking_level",
                "Thinking level",
                settings
                    .runtime
                    .default_thinking_level
                    .unwrap_or_default()
                    .to_string(),
            )
            .values(["off", "minimal", "low", "medium", "high", "xhigh"])
            .description(
                "Default reasoning depth; DeepSeek maps minimal/low/medium/high to high and xhigh to max",
            ),
            SettingItem::new(
                "http_idle_timeout",
                "HTTP idle timeout",
                format_http_idle_timeout_ms(settings.runtime.http_idle_timeout_ms),
            )
            .values(HTTP_IDLE_TIMEOUT_CHOICES.map(|(label, _)| label))
            .description("Maximum idle gap while waiting for HTTP provider response data"),
        ],
        16,
        keybindings,
        SettingsListOptions {
            enable_search: false,
        },
    )
}

fn tool_authorization_risk_label(risk: ToolAuthorizationRisk) -> &'static str {
    match risk {
        ToolAuthorizationRisk::ExternalRead => "external read",
        ToolAuthorizationRisk::FilesystemMutation => "filesystem mutation",
        ToolAuthorizationRisk::ShellExecution => "shell execution",
        ToolAuthorizationRisk::DeclaredSideEffect => "declared side effect",
        ToolAuthorizationRisk::Unknown => "unknown",
    }
}

fn format_http_idle_timeout_ms(timeout_ms: u64) -> String {
    HTTP_IDLE_TIMEOUT_CHOICES
        .iter()
        .find(|(_, value)| *value == timeout_ms)
        .map(|(label, _)| (*label).to_string())
        .unwrap_or_else(|| format!("{} sec", timeout_ms as f64 / 1000.0))
}
