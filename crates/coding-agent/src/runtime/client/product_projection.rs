use std::collections::{HashSet, VecDeque};

use crate::authorization::{ToolAuthorizationRequest, ToolAuthorizationScope};
use crate::events::{
    CodingAgentCapabilityProductEvent, CodingAgentDiagnosticProductEvent,
    CodingAgentMessageProductEvent, CodingAgentProductEvent, CodingAgentProductEventKind,
    CodingAgentProfileProductEvent, CodingAgentRuntimeProductEvent, CodingAgentToolProductEvent,
    CodingAgentWorkflowProductEvent,
};
use crate::profiles::ProfileId;
use crate::runtime::client::context_fold::{
    MAX_CONTEXT_CHANGES, MAX_CONTEXT_DELEGATIONS, ProductContextFoldChange,
    ProductContextPendingState, fold_product_context,
};
use crate::runtime::client::projection::CodingAgentSnapshot;
use crate::runtime::facade::context::{
    CodingAgentRecoveryPending, CodingAgentSessionTranscriptItem, CodingAgentTranscriptSnapshot,
};

const MAX_MESSAGES: usize = 64;
const MAX_TOOLS: usize = 128;
const MAX_DIAGNOSTICS: usize = 64;
const MAX_RECOVERIES: usize = 16;
const MAX_PENDING_AUTHORIZATIONS: usize = 64;
const MAX_ID_BYTES: usize = 256;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_THINKING_BYTES: usize = 512 * 1024;
const MAX_TOOL_BYTES: usize = 256 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 32 * 1024;
const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
const MAX_AUTHORIZATION_BYTES: usize = 16 * 1024;
const MAX_TRANSCRIPT_ITEMS: usize = 10_000;
const MAX_TRANSCRIPT_BYTES: usize = 32 * 1024 * 1024;
const MAX_TRANSCRIPT_IMAGE_BYTES: usize = 1024 * 1024;
const MAX_TRANSCRIPT_IMAGES_PER_ITEM: usize = 8;

/// Product areas changed by one accepted event or snapshot replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CodingAgentClientProjectionArea {
    Cursor,
    Session,
    Operations,
    Conversation,
    Tools,
    Authorizations,
    Delegations,
    Changes,
    Usage,
    Diagnostics,
    Recoveries,
    Profiles,
    Capabilities,
    Lifecycle,
}

/// Deterministic, duplicate-free invalidation set returned by the product reducer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodingAgentClientProjectionChanges {
    areas: Vec<CodingAgentClientProjectionArea>,
}

impl CodingAgentClientProjectionChanges {
    pub fn areas(&self) -> &[CodingAgentClientProjectionArea] {
        &self.areas
    }

    pub fn contains(&self, area: CodingAgentClientProjectionArea) -> bool {
        self.areas.contains(&area)
    }

    fn insert(&mut self, area: CodingAgentClientProjectionArea) {
        if !self.areas.contains(&area) {
            self.areas.push(area);
            self.areas.sort_unstable();
        }
    }
}

/// Safe reason why a disposable client projection must be replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentClientProjectionIssue {
    pub code: &'static str,
    pub summary: &'static str,
}

impl CodingAgentClientProjectionIssue {
    const fn new(code: &'static str, summary: &'static str) -> Self {
        Self { code, summary }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodingAgentClientProjectionApply {
    Applied(CodingAgentClientProjectionChanges),
    IgnoredDuplicate,
    NeedsResync(CodingAgentClientProjectionIssue),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentClientProjectionLifecycle {
    Running,
    NeedsResync,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentClientMessageStatus {
    Streaming,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentClientMessage {
    pub operation_id: String,
    pub turn_id: String,
    pub message_id: Option<String>,
    pub text: String,
    pub thinking: String,
    pub reasoning_duration_millis: Option<u64>,
    pub status: CodingAgentClientMessageStatus,
    /// Sequence of the event that first produced this message.
    ///
    /// Written once and never revised, so a presenter can interleave the
    /// message and tool queues back into the order the turn produced them.
    /// `updated_sequence` cannot serve that purpose: it advances on every
    /// delta, so a streaming row would keep migrating to the tail.
    pub started_sequence: u64,
    pub updated_sequence: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentClientToolStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentClientTool {
    pub operation_id: String,
    pub turn_id: String,
    pub tool_call_id: String,
    pub name: String,
    pub arguments: String,
    pub detail: String,
    pub status: CodingAgentClientToolStatus,
    /// Sequence of the event that first produced this tool call. See
    /// [`CodingAgentClientMessage::started_sequence`].
    pub started_sequence: u64,
    pub updated_sequence: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentClientDiagnostic {
    pub operation_id: Option<String>,
    pub code: String,
    pub summary: String,
    pub sequence: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingAgentClientRecoveryStatus {
    Pending,
    Resolved,
    Recovered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentClientRecovery {
    pub operation_id: String,
    pub recovery_id: String,
    pub operation_kind: Option<String>,
    pub status: CodingAgentClientRecoveryStatus,
    pub reason: String,
    pub record_version: Option<u64>,
    pub descriptor_revision: Option<u16>,
    pub capability_generation: Option<u64>,
    pub attempt_count: u32,
    pub last_attempt_at: Option<String>,
    pub next_attempt_at: Option<String>,
    pub updated_sequence: u64,
}

/// Complete bounded durable conversation replacement for one session leaf.
#[derive(Debug, Clone, PartialEq)]
pub struct CodingAgentClientTranscript {
    session_id: String,
    active_leaf_id: Option<String>,
    items: VecDeque<CodingAgentSessionTranscriptItem>,
    omitted_items: usize,
    retained_bytes: usize,
    truncated: bool,
}

impl CodingAgentClientTranscript {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn active_leaf_id(&self) -> Option<&str> {
        self.active_leaf_id.as_deref()
    }

    pub fn items(&self) -> &VecDeque<CodingAgentSessionTranscriptItem> {
        &self.items
    }

    pub const fn omitted_items(&self) -> usize {
        self.omitted_items
    }

    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Atomic initial or session-replacement input for one product client.
#[derive(Debug, Clone, PartialEq)]
pub struct CodingAgentClientBootstrap {
    pub snapshot: CodingAgentSnapshot,
    pub transcript: CodingAgentTranscriptSnapshot,
    pub pending_recoveries: Vec<CodingAgentRecoveryPending>,
}

/// Whether a replacement snapshot supersedes the event-folded live tail.
///
/// The live message, tool and diagnostic queues are folded from the event
/// stream, not from the snapshot. Only a caller that also installs a
/// replacement transcript — or one recovering from lost stream sync — has
/// something to put in their place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveTailRetention {
    Discard,
    Retain,
}

/// Adapter-neutral, disposable product state built from one snapshot plus
/// strictly ordered ProductEvents.
#[derive(Debug, Clone, PartialEq)]
pub struct CodingAgentClientProjection {
    snapshot: CodingAgentSnapshot,
    lifecycle: CodingAgentClientProjectionLifecycle,
    transcript: Option<CodingAgentClientTranscript>,
    messages: VecDeque<CodingAgentClientMessage>,
    tools: VecDeque<CodingAgentClientTool>,
    diagnostics: VecDeque<CodingAgentClientDiagnostic>,
    recoveries: VecDeque<CodingAgentClientRecovery>,
    context_pending: ProductContextPendingState,
    resync_issue: Option<CodingAgentClientProjectionIssue>,
}

impl CodingAgentClientProjection {
    pub fn new(
        mut snapshot: CodingAgentSnapshot,
    ) -> Result<Self, CodingAgentClientProjectionIssue> {
        validate_snapshot(&snapshot)?;
        sanitize_snapshot(&mut snapshot);
        Ok(Self {
            snapshot,
            lifecycle: CodingAgentClientProjectionLifecycle::Running,
            transcript: None,
            messages: VecDeque::new(),
            tools: VecDeque::new(),
            diagnostics: VecDeque::new(),
            recoveries: VecDeque::new(),
            context_pending: ProductContextPendingState::default(),
            resync_issue: None,
        })
    }

    pub fn from_bootstrap(
        bootstrap: CodingAgentClientBootstrap,
    ) -> Result<Self, CodingAgentClientProjectionIssue> {
        let CodingAgentClientBootstrap {
            snapshot,
            transcript,
            pending_recoveries,
        } = bootstrap;
        let mut projection = Self::new(snapshot)?;
        let transcript = project_transcript(&projection.snapshot, transcript)?;
        let recoveries = project_pending_recoveries(&projection.snapshot, pending_recoveries)?;
        projection.transcript = Some(transcript);
        projection.recoveries = recoveries;
        Ok(projection)
    }

    pub fn snapshot(&self) -> &CodingAgentSnapshot {
        &self.snapshot
    }

    pub const fn lifecycle(&self) -> CodingAgentClientProjectionLifecycle {
        self.lifecycle
    }

    pub fn transcript(&self) -> Option<&CodingAgentClientTranscript> {
        self.transcript.as_ref()
    }

    pub fn messages(&self) -> &VecDeque<CodingAgentClientMessage> {
        &self.messages
    }

    pub fn tools(&self) -> &VecDeque<CodingAgentClientTool> {
        &self.tools
    }

    pub fn diagnostics(&self) -> &VecDeque<CodingAgentClientDiagnostic> {
        &self.diagnostics
    }

    pub fn recoveries(&self) -> &VecDeque<CodingAgentClientRecovery> {
        &self.recoveries
    }

    pub fn resync_issue(&self) -> Option<&CodingAgentClientProjectionIssue> {
        self.resync_issue.as_ref()
    }

    pub fn replace_snapshot(
        &mut self,
        snapshot: CodingAgentSnapshot,
    ) -> Result<CodingAgentClientProjectionChanges, CodingAgentClientProjectionIssue> {
        self.replace_snapshot_with_retention(snapshot, LiveTailRetention::Discard)
    }

    /// Replace session metadata without disturbing the event-folded live tail.
    ///
    /// A metadata replacement carries no transcript, so the folded message and
    /// tool rows are the only record of an in-flight turn. Discarding them here
    /// blanks the streaming rows with nothing to take their place until the next
    /// full hydration, which reads as rows vanishing mid-turn.
    pub fn replace_metadata_snapshot(
        &mut self,
        snapshot: CodingAgentSnapshot,
    ) -> Result<CodingAgentClientProjectionChanges, CodingAgentClientProjectionIssue> {
        self.replace_snapshot_with_retention(snapshot, LiveTailRetention::Retain)
    }

    fn replace_snapshot_with_retention(
        &mut self,
        mut snapshot: CodingAgentSnapshot,
        retention: LiveTailRetention,
    ) -> Result<CodingAgentClientProjectionChanges, CodingAgentClientProjectionIssue> {
        validate_snapshot(&snapshot)?;
        if snapshot.session.session_id != self.snapshot.session.session_id {
            return Err(CodingAgentClientProjectionIssue::new(
                "snapshot_session_mismatch",
                "A metadata snapshot cannot replace a different session.",
            ));
        }
        if snapshot.cursor.stream_id != self.snapshot.cursor.stream_id {
            return Err(CodingAgentClientProjectionIssue::new(
                "snapshot_stream_mismatch",
                "A metadata snapshot cannot replace a different event stream.",
            ));
        }
        if snapshot.cursor.last_event_sequence < self.snapshot.cursor.last_event_sequence {
            return Err(CodingAgentClientProjectionIssue::new(
                "snapshot_cursor_regression",
                "A replacement snapshot cannot move the event cursor backwards.",
            ));
        }
        sanitize_snapshot(&mut snapshot);
        self.snapshot = snapshot;
        self.lifecycle = CodingAgentClientProjectionLifecycle::Running;
        if retention == LiveTailRetention::Discard {
            self.messages.clear();
            self.tools.clear();
            self.diagnostics.clear();
        }
        // Pending context folds belong to the snapshot's own context, which this
        // replacement supersedes either way.
        self.context_pending = ProductContextPendingState::default();
        self.resync_issue = None;
        Ok(all_projection_areas())
    }

    pub fn replace_bootstrap(
        &mut self,
        bootstrap: CodingAgentClientBootstrap,
    ) -> Result<CodingAgentClientProjectionChanges, CodingAgentClientProjectionIssue> {
        let replacement = Self::from_bootstrap(bootstrap)?;
        *self = replacement;
        Ok(all_projection_areas())
    }

    pub fn replace_transcript(
        &mut self,
        transcript: CodingAgentTranscriptSnapshot,
    ) -> Result<CodingAgentClientProjectionChanges, CodingAgentClientProjectionIssue> {
        let transcript = project_transcript(&self.snapshot, transcript)?;
        self.transcript = Some(transcript);
        let mut changes = CodingAgentClientProjectionChanges::default();
        changes.insert(CodingAgentClientProjectionArea::Conversation);
        Ok(changes)
    }

    pub fn replace_pending_recoveries(
        &mut self,
        pending: Vec<CodingAgentRecoveryPending>,
    ) -> Result<CodingAgentClientProjectionChanges, CodingAgentClientProjectionIssue> {
        let recoveries = project_pending_recoveries(&self.snapshot, pending)?;
        self.recoveries = recoveries;
        let mut changes = CodingAgentClientProjectionChanges::default();
        changes.insert(CodingAgentClientProjectionArea::Recoveries);
        Ok(changes)
    }

    pub fn apply(&mut self, event: &CodingAgentProductEvent) -> CodingAgentClientProjectionApply {
        if self.lifecycle != CodingAgentClientProjectionLifecycle::Running {
            return CodingAgentClientProjectionApply::NeedsResync(
                self.resync_issue.clone().unwrap_or_else(|| {
                    CodingAgentClientProjectionIssue::new(
                        "product_projection_not_running",
                        "The product projection requires a fresh snapshot.",
                    )
                }),
            );
        }
        if event.stream_id() != self.snapshot.cursor.stream_id {
            return self.require_resync(CodingAgentClientProjectionIssue::new(
                "product_event_stream_mismatch",
                "The product event belongs to a different stream.",
            ));
        }
        if event.sequence() <= self.snapshot.cursor.last_event_sequence {
            return CodingAgentClientProjectionApply::IgnoredDuplicate;
        }
        let Some(expected_sequence) = self.snapshot.cursor.last_event_sequence.checked_add(1)
        else {
            return self.require_resync(CodingAgentClientProjectionIssue::new(
                "product_event_cursor_exhausted",
                "The product event cursor is exhausted.",
            ));
        };
        if event.sequence() != expected_sequence {
            return self.require_resync(CodingAgentClientProjectionIssue::new(
                "product_event_cursor_gap",
                "The product event stream contains a gap.",
            ));
        }
        if event
            .session_id()
            .is_some_and(|session_id| session_id != self.snapshot.session.session_id)
        {
            return self.require_resync(CodingAgentClientProjectionIssue::new(
                "product_event_session_mismatch",
                "The product event belongs to a different session.",
            ));
        }
        if !operation_association_matches(&self.snapshot, event) {
            return self.require_resync(CodingAgentClientProjectionIssue::new(
                "product_event_operation_mismatch",
                "The product event does not belong to the submitted operation.",
            ));
        }
        if let Err(issue) = validate_authorization_event(&self.snapshot, event) {
            return self.require_resync(issue);
        }
        let next_generation = match validate_capability_generation(&self.snapshot, event) {
            Ok(generation) => generation,
            Err(issue) => return self.require_resync(issue),
        };

        let mut changes = CodingAgentClientProjectionChanges::default();
        changes.insert(CodingAgentClientProjectionArea::Cursor);
        self.snapshot.cursor.last_event_sequence = event.sequence();
        self.snapshot.cursor.capability_generation = next_generation;
        self.apply_profile(event, &mut changes);
        for change in fold_product_context(
            &mut self.snapshot.context,
            &mut self.context_pending,
            event,
            None,
        ) {
            changes.insert(match change {
                ProductContextFoldChange::Operations => CodingAgentClientProjectionArea::Operations,
                ProductContextFoldChange::Changes => CodingAgentClientProjectionArea::Changes,
                ProductContextFoldChange::Delegations => {
                    CodingAgentClientProjectionArea::Delegations
                }
                ProductContextFoldChange::Usage => CodingAgentClientProjectionArea::Usage,
            });
        }
        self.apply_message(event, &mut changes);
        self.apply_tool(event, &mut changes);
        self.apply_authorization(event, &mut changes);
        self.apply_diagnostic(event, &mut changes);
        self.apply_recovery(event, &mut changes);
        self.apply_runtime(event, &mut changes);
        CodingAgentClientProjectionApply::Applied(changes)
    }

    fn require_resync(
        &mut self,
        issue: CodingAgentClientProjectionIssue,
    ) -> CodingAgentClientProjectionApply {
        self.lifecycle = CodingAgentClientProjectionLifecycle::NeedsResync;
        self.resync_issue = Some(issue.clone());
        CodingAgentClientProjectionApply::NeedsResync(issue)
    }

    fn apply_profile(
        &mut self,
        event: &CodingAgentProductEvent,
        changes: &mut CodingAgentClientProjectionChanges,
    ) {
        if let CodingAgentProductEventKind::Profile(
            CodingAgentProfileProductEvent::DefaultChanged { profile_id },
        ) = event.event()
        {
            self.snapshot.session.default_agent_profile_id =
                ProfileId::from(bounded_text(profile_id, MAX_ID_BYTES));
            changes.insert(CodingAgentClientProjectionArea::Profiles);
            changes.insert(CodingAgentClientProjectionArea::Session);
        }
        if matches!(
            event.event(),
            CodingAgentProductEventKind::Capability(
                CodingAgentCapabilityProductEvent::Changed { .. }
            )
        ) {
            changes.insert(CodingAgentClientProjectionArea::Capabilities);
        }
    }

    fn apply_message(
        &mut self,
        event: &CodingAgentProductEvent,
        changes: &mut CodingAgentClientProjectionChanges,
    ) {
        let CodingAgentProductEventKind::Message(message) = event.event() else {
            return;
        };
        let (operation_id, turn_id, message_id) = match message {
            CodingAgentMessageProductEvent::Started {
                operation_id,
                turn_id,
                message_id,
            }
            | CodingAgentMessageProductEvent::Delta {
                operation_id,
                turn_id,
                message_id,
                ..
            }
            | CodingAgentMessageProductEvent::ThinkingDelta {
                operation_id,
                turn_id,
                message_id,
                ..
            }
            | CodingAgentMessageProductEvent::Completed {
                operation_id,
                turn_id,
                message_id,
                ..
            } => (operation_id, turn_id, message_id),
        };
        let index = self
            .messages
            .iter()
            .position(|current| {
                current.operation_id == *operation_id
                    && current.turn_id == *turn_id
                    && current.message_id == *message_id
            })
            .unwrap_or_else(|| {
                self.messages.push_back(CodingAgentClientMessage {
                    operation_id: bounded_text(operation_id, MAX_ID_BYTES),
                    turn_id: bounded_text(turn_id, MAX_ID_BYTES),
                    message_id: message_id
                        .as_deref()
                        .map(|value| bounded_text(value, MAX_ID_BYTES)),
                    text: String::new(),
                    thinking: String::new(),
                    reasoning_duration_millis: None,
                    status: CodingAgentClientMessageStatus::Streaming,
                    started_sequence: event.sequence(),
                    updated_sequence: event.sequence(),
                    truncated: false,
                });
                self.messages.len() - 1
            });
        let current = &mut self.messages[index];
        current.updated_sequence = event.sequence();
        match message {
            CodingAgentMessageProductEvent::Started { .. } => {}
            CodingAgentMessageProductEvent::Delta { text, .. } => {
                current.truncated |= append_bounded(&mut current.text, text, MAX_MESSAGE_BYTES);
            }
            CodingAgentMessageProductEvent::ThinkingDelta { text, .. } => {
                current.truncated |=
                    append_bounded(&mut current.thinking, text, MAX_THINKING_BYTES);
            }
            CodingAgentMessageProductEvent::Completed {
                final_text,
                reasoning_duration_millis,
                ..
            } => {
                let (text, truncated) = bounded_prefix(final_text, MAX_MESSAGE_BYTES);
                current.text = text;
                current.truncated |= truncated;
                current.status = CodingAgentClientMessageStatus::Completed;
                current.reasoning_duration_millis = *reasoning_duration_millis;
            }
        }
        trim_messages(&mut self.messages);
        changes.insert(CodingAgentClientProjectionArea::Conversation);
    }

    fn apply_tool(
        &mut self,
        event: &CodingAgentProductEvent,
        changes: &mut CodingAgentClientProjectionChanges,
    ) {
        let CodingAgentProductEventKind::Tool(tool) = event.event() else {
            return;
        };
        let (operation_id, turn_id, tool_call_id, name) = match tool {
            CodingAgentToolProductEvent::Started {
                operation_id,
                turn_id,
                tool_call_id,
                name,
                ..
            }
            | CodingAgentToolProductEvent::Updated {
                operation_id,
                turn_id,
                tool_call_id,
                name,
                ..
            }
            | CodingAgentToolProductEvent::Completed {
                operation_id,
                turn_id,
                tool_call_id,
                name,
                ..
            }
            | CodingAgentToolProductEvent::Failed {
                operation_id,
                turn_id,
                tool_call_id,
                name,
                ..
            } => (operation_id, turn_id, tool_call_id, name),
            CodingAgentToolProductEvent::AuthorizationRequired { .. }
            | CodingAgentToolProductEvent::AuthorizationApproved { .. }
            | CodingAgentToolProductEvent::AuthorizationDenied { .. }
            | CodingAgentToolProductEvent::AuthorizationCancelled { .. } => return,
        };
        let index = self
            .tools
            .iter()
            .position(|current| current.tool_call_id == *tool_call_id)
            .unwrap_or_else(|| {
                self.tools.push_back(CodingAgentClientTool {
                    operation_id: bounded_text(operation_id, MAX_ID_BYTES),
                    turn_id: bounded_text(turn_id, MAX_ID_BYTES),
                    tool_call_id: bounded_text(tool_call_id, MAX_ID_BYTES),
                    name: bounded_text(name, MAX_ID_BYTES),
                    arguments: String::new(),
                    detail: String::new(),
                    status: CodingAgentClientToolStatus::Running,
                    started_sequence: event.sequence(),
                    updated_sequence: event.sequence(),
                    truncated: false,
                });
                self.tools.len() - 1
            });
        let current = &mut self.tools[index];
        current.updated_sequence = event.sequence();
        match tool {
            CodingAgentToolProductEvent::Started { arguments_json, .. } => {
                let (arguments, truncated) = bounded_prefix(arguments_json, MAX_TOOL_BYTES);
                current.arguments = arguments;
                current.truncated |= truncated;
            }
            CodingAgentToolProductEvent::Updated { message, .. } => {
                let (detail, truncated) = bounded_prefix(message, MAX_TOOL_BYTES);
                current.detail = detail;
                current.truncated |= truncated;
            }
            CodingAgentToolProductEvent::Completed { summary, .. } => {
                let (detail, truncated) = bounded_prefix(summary, MAX_TOOL_BYTES);
                current.detail = detail;
                current.truncated |= truncated;
                current.status = CodingAgentClientToolStatus::Completed;
            }
            CodingAgentToolProductEvent::Failed { message, .. } => {
                let (detail, truncated) = bounded_prefix(message, MAX_TOOL_BYTES);
                current.detail = detail;
                current.truncated |= truncated;
                current.status = CodingAgentClientToolStatus::Failed;
            }
            CodingAgentToolProductEvent::AuthorizationRequired { .. }
            | CodingAgentToolProductEvent::AuthorizationApproved { .. }
            | CodingAgentToolProductEvent::AuthorizationDenied { .. }
            | CodingAgentToolProductEvent::AuthorizationCancelled { .. } => {}
        }
        trim_tools(&mut self.tools);
        changes.insert(CodingAgentClientProjectionArea::Tools);
    }

    fn apply_authorization(
        &mut self,
        event: &CodingAgentProductEvent,
        changes: &mut CodingAgentClientProjectionChanges,
    ) {
        let CodingAgentProductEventKind::Tool(tool) = event.event() else {
            return;
        };
        match tool {
            CodingAgentToolProductEvent::AuthorizationRequired { request } => {
                let mut request = request.clone();
                sanitize_authorization(&mut request);
                self.snapshot
                    .pending_authorizations
                    .retain(|current| current.authorization_id != request.authorization_id);
                self.snapshot.pending_authorizations.push(request);
                self.snapshot
                    .pending_authorizations
                    .truncate(MAX_PENDING_AUTHORIZATIONS);
            }
            CodingAgentToolProductEvent::AuthorizationApproved {
                authorization_id, ..
            }
            | CodingAgentToolProductEvent::AuthorizationDenied {
                authorization_id, ..
            }
            | CodingAgentToolProductEvent::AuthorizationCancelled {
                authorization_id, ..
            } => self
                .snapshot
                .pending_authorizations
                .retain(|current| current.authorization_id != *authorization_id),
            _ => return,
        }
        changes.insert(CodingAgentClientProjectionArea::Authorizations);
    }

    fn apply_diagnostic(
        &mut self,
        event: &CodingAgentProductEvent,
        changes: &mut CodingAgentClientProjectionChanges,
    ) {
        let CodingAgentProductEventKind::Diagnostic(
            CodingAgentDiagnosticProductEvent::Diagnostic { diagnostic },
        ) = event.event()
        else {
            return;
        };
        let (summary, truncated) = bounded_prefix(&diagnostic.summary, MAX_DIAGNOSTIC_BYTES);
        self.diagnostics.push_back(CodingAgentClientDiagnostic {
            operation_id: diagnostic
                .operation_id
                .as_deref()
                .map(|value| bounded_text(value, MAX_ID_BYTES)),
            code: bounded_text(&diagnostic.code, MAX_ID_BYTES),
            summary,
            sequence: event.sequence(),
            truncated,
        });
        while self.diagnostics.len() > MAX_DIAGNOSTICS {
            self.diagnostics.pop_front();
        }
        changes.insert(CodingAgentClientProjectionArea::Diagnostics);
    }

    fn apply_recovery(
        &mut self,
        event: &CodingAgentProductEvent,
        changes: &mut CodingAgentClientProjectionChanges,
    ) {
        let CodingAgentProductEventKind::Workflow(workflow) = event.event() else {
            return;
        };
        let recovery = match workflow {
            CodingAgentWorkflowProductEvent::OperationRecoveryPending {
                operation_id,
                recovery_id,
                reason,
                record_version,
                descriptor_revision,
                capability_generation,
                attempt_count,
                last_attempt_at,
                next_attempt_at,
            } => CodingAgentClientRecovery {
                operation_id: bounded_text(operation_id, MAX_ID_BYTES),
                recovery_id: bounded_text(recovery_id, MAX_ID_BYTES),
                operation_kind: self
                    .snapshot
                    .context
                    .operations
                    .iter()
                    .find(|operation| operation.operation_id == *operation_id)
                    .map(|operation| operation.kind.clone()),
                status: CodingAgentClientRecoveryStatus::Pending,
                reason: bounded_text(reason, MAX_DIAGNOSTIC_BYTES),
                record_version: Some(*record_version),
                descriptor_revision: Some(*descriptor_revision),
                capability_generation: *capability_generation,
                attempt_count: *attempt_count,
                last_attempt_at: last_attempt_at
                    .as_deref()
                    .map(|value| bounded_text(value, MAX_ID_BYTES)),
                next_attempt_at: next_attempt_at
                    .as_deref()
                    .map(|value| bounded_text(value, MAX_ID_BYTES)),
                updated_sequence: event.sequence(),
            },
            CodingAgentWorkflowProductEvent::OperationRecoveryResolved {
                operation_id,
                recovery_id,
                reason,
                record_version,
                descriptor_revision,
                capability_generation,
                ..
            } => CodingAgentClientRecovery {
                operation_id: bounded_text(operation_id, MAX_ID_BYTES),
                recovery_id: bounded_text(recovery_id, MAX_ID_BYTES),
                operation_kind: self
                    .snapshot
                    .context
                    .operations
                    .iter()
                    .find(|operation| operation.operation_id == *operation_id)
                    .map(|operation| operation.kind.clone()),
                status: CodingAgentClientRecoveryStatus::Resolved,
                reason: bounded_text(reason, MAX_DIAGNOSTIC_BYTES),
                record_version: Some(*record_version),
                descriptor_revision: Some(*descriptor_revision),
                capability_generation: *capability_generation,
                attempt_count: 0,
                last_attempt_at: None,
                next_attempt_at: None,
                updated_sequence: event.sequence(),
            },
            CodingAgentWorkflowProductEvent::OperationRecovered {
                operation_id,
                recovery_id,
                reason,
            } => CodingAgentClientRecovery {
                operation_id: bounded_text(operation_id, MAX_ID_BYTES),
                recovery_id: bounded_text(recovery_id, MAX_ID_BYTES),
                operation_kind: self
                    .snapshot
                    .context
                    .operations
                    .iter()
                    .find(|operation| operation.operation_id == *operation_id)
                    .map(|operation| operation.kind.clone()),
                status: CodingAgentClientRecoveryStatus::Recovered,
                reason: bounded_text(reason, MAX_DIAGNOSTIC_BYTES),
                record_version: None,
                descriptor_revision: None,
                capability_generation: None,
                attempt_count: 0,
                last_attempt_at: None,
                next_attempt_at: None,
                updated_sequence: event.sequence(),
            },
            _ => return,
        };
        self.recoveries
            .retain(|current| current.recovery_id != recovery.recovery_id);
        self.recoveries.push_front(recovery);
        self.recoveries.truncate(MAX_RECOVERIES);
        changes.insert(CodingAgentClientProjectionArea::Recoveries);
    }

    fn apply_runtime(
        &mut self,
        event: &CodingAgentProductEvent,
        changes: &mut CodingAgentClientProjectionChanges,
    ) {
        if matches!(
            event.event(),
            CodingAgentProductEventKind::Runtime(CodingAgentRuntimeProductEvent::ShutDown)
        ) {
            self.lifecycle = CodingAgentClientProjectionLifecycle::Stopped;
            changes.insert(CodingAgentClientProjectionArea::Lifecycle);
        }
    }
}

fn project_transcript(
    snapshot: &CodingAgentSnapshot,
    transcript: CodingAgentTranscriptSnapshot,
) -> Result<CodingAgentClientTranscript, CodingAgentClientProjectionIssue> {
    if transcript.session_id != snapshot.session.session_id {
        return Err(CodingAgentClientProjectionIssue::new(
            "transcript_session_mismatch",
            "The transcript belongs to a different session.",
        ));
    }
    let mut projected = CodingAgentClientTranscript {
        session_id: transcript.session_id,
        active_leaf_id: transcript
            .active_leaf_id
            .as_deref()
            .map(|value| bounded_text(value, MAX_ID_BYTES)),
        items: VecDeque::new(),
        omitted_items: 0,
        retained_bytes: 0,
        truncated: false,
    };
    for item in transcript.items {
        let (item, retained_bytes, truncated) = sanitize_transcript_item(item);
        projected.truncated |= truncated;
        projected.retained_bytes = projected.retained_bytes.saturating_add(retained_bytes);
        projected.items.push_back(item);
        while projected.items.len() > MAX_TRANSCRIPT_ITEMS
            || projected.retained_bytes > MAX_TRANSCRIPT_BYTES
        {
            let Some(evicted) = projected.items.pop_front() else {
                break;
            };
            projected.retained_bytes = projected
                .retained_bytes
                .saturating_sub(transcript_item_bytes(&evicted));
            projected.omitted_items = projected.omitted_items.saturating_add(1);
            projected.truncated = true;
        }
    }
    Ok(projected)
}

fn sanitize_transcript_item(
    item: CodingAgentSessionTranscriptItem,
) -> (CodingAgentSessionTranscriptItem, usize, bool) {
    let mut truncated = false;
    let item = match item {
        CodingAgentSessionTranscriptItem::User { text } => {
            let (text, was_truncated) = bounded_prefix(&text, MAX_MESSAGE_BYTES);
            truncated |= was_truncated;
            CodingAgentSessionTranscriptItem::User { text }
        }
        CodingAgentSessionTranscriptItem::Assistant {
            id,
            text,
            thinking,
            images,
            done,
            reasoning_duration_millis,
        } => {
            let (id, id_truncated) = bounded_prefix(&id, MAX_ID_BYTES);
            let (text, text_truncated) = bounded_prefix(&text, MAX_MESSAGE_BYTES);
            let (thinking, thinking_truncated) = bounded_prefix(&thinking, MAX_THINKING_BYTES);
            truncated |= id_truncated || text_truncated || thinking_truncated;
            let original_image_count = images.len();
            let images = images
                .into_iter()
                .take(MAX_TRANSCRIPT_IMAGES_PER_ITEM)
                .filter_map(|mut image| {
                    if image.data.len() > MAX_TRANSCRIPT_IMAGE_BYTES {
                        truncated = true;
                        return None;
                    }
                    let (mime_type, mime_truncated) =
                        bounded_prefix(&image.mime_type, MAX_ID_BYTES);
                    image.mime_type = mime_type;
                    truncated |= mime_truncated;
                    Some(image)
                })
                .collect::<Vec<_>>();
            truncated |= original_image_count > images.len();
            CodingAgentSessionTranscriptItem::Assistant {
                id,
                text,
                thinking,
                images,
                done,
                reasoning_duration_millis,
            }
        }
        CodingAgentSessionTranscriptItem::Tool {
            call_id,
            name,
            args,
            result,
            is_error,
            duration_millis,
        } => {
            let (call_id, call_id_truncated) = bounded_prefix(&call_id, MAX_ID_BYTES);
            let (name, name_truncated) = bounded_prefix(&name, MAX_ID_BYTES);
            let args =
                if serde_json::to_vec(&args).is_ok_and(|encoded| encoded.len() <= MAX_TOOL_BYTES) {
                    args
                } else {
                    truncated = true;
                    serde_json::json!({ "projectionTruncated": true })
                };
            let result = result.map(|value| {
                let (value, result_truncated) = bounded_prefix(&value, MAX_MESSAGE_BYTES);
                truncated |= result_truncated;
                value
            });
            truncated |= call_id_truncated || name_truncated;
            CodingAgentSessionTranscriptItem::Tool {
                call_id,
                name,
                args,
                result,
                is_error,
                duration_millis,
            }
        }
        CodingAgentSessionTranscriptItem::Delegation {
            tool_call_id,
            requesting_profile_id,
            target_kind,
            target_id,
            task,
            status,
            child_operation_id,
            summary,
        } => {
            let (tool_call_id, tool_truncated) = bounded_prefix(&tool_call_id, MAX_ID_BYTES);
            let (requesting_profile_id, requesting_truncated) =
                bounded_prefix(requesting_profile_id.as_str(), MAX_ID_BYTES);
            let (target_id, target_truncated) = bounded_prefix(target_id.as_str(), MAX_ID_BYTES);
            let (task, task_truncated) = bounded_prefix(&task, MAX_MESSAGE_BYTES);
            let (status, status_truncated) = bounded_prefix(&status, MAX_ID_BYTES);
            let child_operation_id = child_operation_id.map(|value| {
                let (value, child_truncated) = bounded_prefix(&value, MAX_ID_BYTES);
                truncated |= child_truncated;
                value
            });
            let summary = summary.map(|value| {
                let (value, summary_truncated) = bounded_prefix(&value, MAX_MESSAGE_BYTES);
                truncated |= summary_truncated;
                value
            });
            truncated |= tool_truncated
                || requesting_truncated
                || target_truncated
                || task_truncated
                || status_truncated;
            CodingAgentSessionTranscriptItem::Delegation {
                tool_call_id,
                requesting_profile_id: ProfileId::from(requesting_profile_id),
                target_kind,
                target_id: ProfileId::from(target_id),
                task,
                status,
                child_operation_id,
                summary,
            }
        }
        CodingAgentSessionTranscriptItem::CompactionSummary { summary } => {
            let (summary, was_truncated) = bounded_prefix(&summary, MAX_MESSAGE_BYTES);
            truncated |= was_truncated;
            CodingAgentSessionTranscriptItem::CompactionSummary { summary }
        }
        CodingAgentSessionTranscriptItem::BranchSummary { summary } => {
            let (summary, was_truncated) = bounded_prefix(&summary, MAX_MESSAGE_BYTES);
            truncated |= was_truncated;
            CodingAgentSessionTranscriptItem::BranchSummary { summary }
        }
        CodingAgentSessionTranscriptItem::Diagnostic { message } => {
            let (message, was_truncated) = bounded_prefix(&message, MAX_DIAGNOSTIC_BYTES);
            truncated |= was_truncated;
            CodingAgentSessionTranscriptItem::Diagnostic { message }
        }
    };
    let retained_bytes = transcript_item_bytes(&item);
    (item, retained_bytes, truncated)
}

fn transcript_item_bytes(item: &CodingAgentSessionTranscriptItem) -> usize {
    match item {
        CodingAgentSessionTranscriptItem::User { text } => text.len(),
        CodingAgentSessionTranscriptItem::Assistant {
            id,
            text,
            thinking,
            images,
            ..
        } => {
            id.len()
                + text.len()
                + thinking.len()
                + images
                    .iter()
                    .map(|image| image.mime_type.len() + image.data.len())
                    .sum::<usize>()
        }
        CodingAgentSessionTranscriptItem::Tool {
            call_id,
            name,
            args,
            result,
            ..
        } => {
            call_id.len()
                + name.len()
                + serde_json::to_vec(args).map_or(0, |encoded| encoded.len())
                + result.as_ref().map_or(0, String::len)
        }
        CodingAgentSessionTranscriptItem::Delegation {
            tool_call_id,
            requesting_profile_id,
            target_id,
            task,
            status,
            child_operation_id,
            summary,
            ..
        } => {
            tool_call_id.len()
                + requesting_profile_id.as_str().len()
                + target_id.as_str().len()
                + task.len()
                + status.len()
                + child_operation_id.as_ref().map_or(0, String::len)
                + summary.as_ref().map_or(0, String::len)
        }
        CodingAgentSessionTranscriptItem::CompactionSummary { summary }
        | CodingAgentSessionTranscriptItem::BranchSummary { summary } => summary.len(),
        CodingAgentSessionTranscriptItem::Diagnostic { message } => message.len(),
    }
}

fn project_pending_recoveries(
    snapshot: &CodingAgentSnapshot,
    pending: Vec<CodingAgentRecoveryPending>,
) -> Result<VecDeque<CodingAgentClientRecovery>, CodingAgentClientProjectionIssue> {
    let mut recovery_ids = HashSet::new();
    for recovery in &pending {
        if recovery.operation_id.is_empty()
            || recovery.operation_id.len() > MAX_ID_BYTES
            || recovery.recovery_id.is_empty()
            || recovery.recovery_id.len() > MAX_ID_BYTES
            || recovery.record_version == 0
            || recovery.descriptor_revision == 0
        {
            return Err(CodingAgentClientProjectionIssue::new(
                "recovery_identity_invalid",
                "A pending recovery identity is invalid.",
            ));
        }
        if !recovery_ids.insert(recovery.recovery_id.as_str()) {
            return Err(CodingAgentClientProjectionIssue::new(
                "recovery_identity_duplicate",
                "The pending recovery list contains duplicate identities.",
            ));
        }
        if recovery
            .capability_generation
            .is_some_and(|generation| generation > snapshot.cursor.capability_generation)
        {
            return Err(CodingAgentClientProjectionIssue::new(
                "recovery_capability_generation_invalid",
                "A pending recovery references a future capability generation.",
            ));
        }
    }
    Ok(pending
        .into_iter()
        .take(MAX_RECOVERIES)
        .map(|recovery| CodingAgentClientRecovery {
            operation_id: bounded_text(&recovery.operation_id, MAX_ID_BYTES),
            recovery_id: bounded_text(&recovery.recovery_id, MAX_ID_BYTES),
            operation_kind: recovery
                .operation_kind
                .as_deref()
                .map(|value| bounded_text(value, MAX_ID_BYTES)),
            status: CodingAgentClientRecoveryStatus::Pending,
            reason: "Durable recovery requires operator action.".into(),
            record_version: Some(recovery.record_version),
            descriptor_revision: Some(recovery.descriptor_revision),
            capability_generation: recovery.capability_generation,
            attempt_count: recovery.attempt_count,
            last_attempt_at: recovery
                .last_attempt_at
                .as_deref()
                .map(|value| bounded_text(value, MAX_ID_BYTES)),
            next_attempt_at: recovery
                .next_attempt_at
                .as_deref()
                .map(|value| bounded_text(value, MAX_ID_BYTES)),
            updated_sequence: snapshot.cursor.last_event_sequence,
        })
        .collect())
}

fn validate_snapshot(
    snapshot: &CodingAgentSnapshot,
) -> Result<(), CodingAgentClientProjectionIssue> {
    if snapshot.version.family != "ui_snapshot"
        || snapshot.version.major != snapshot.cursor.snapshot_protocol_major
    {
        return Err(CodingAgentClientProjectionIssue::new(
            "snapshot_protocol_mismatch",
            "The snapshot protocol does not match its cursor.",
        ));
    }
    if snapshot.cursor.stream_id.is_empty() {
        return Err(CodingAgentClientProjectionIssue::new(
            "snapshot_stream_missing",
            "The snapshot stream identity is missing.",
        ));
    }
    if snapshot.session.session_id.is_empty() {
        return Err(CodingAgentClientProjectionIssue::new(
            "snapshot_session_missing",
            "The snapshot session identity is missing.",
        ));
    }
    for request in &snapshot.pending_authorizations {
        validate_authorization_identity(request)?;
        if request.capability_generation != snapshot.cursor.capability_generation {
            return Err(CodingAgentClientProjectionIssue::new(
                "authorization_capability_generation_mismatch",
                "An authorization request has a stale capability generation.",
            ));
        }
    }
    Ok(())
}

fn validate_authorization_event(
    snapshot: &CodingAgentSnapshot,
    event: &CodingAgentProductEvent,
) -> Result<(), CodingAgentClientProjectionIssue> {
    let CodingAgentProductEventKind::Tool(CodingAgentToolProductEvent::AuthorizationRequired {
        request,
    }) = event.event()
    else {
        return Ok(());
    };
    validate_authorization_identity(request)?;
    if event.operation_id() != Some(request.operation_id.as_str()) {
        return Err(CodingAgentClientProjectionIssue::new(
            "authorization_operation_mismatch",
            "The authorization request does not match the event operation.",
        ));
    }
    if request.capability_generation != snapshot.cursor.capability_generation {
        return Err(CodingAgentClientProjectionIssue::new(
            "authorization_capability_generation_mismatch",
            "The authorization request has a stale capability generation.",
        ));
    }
    if event
        .capability_generation()
        .is_some_and(|generation| generation != request.capability_generation)
    {
        return Err(CodingAgentClientProjectionIssue::new(
            "authorization_capability_generation_mismatch",
            "The authorization request and event generations do not match.",
        ));
    }
    Ok(())
}

fn validate_authorization_identity(
    request: &ToolAuthorizationRequest,
) -> Result<(), CodingAgentClientProjectionIssue> {
    if [
        request.authorization_id.as_str(),
        request.operation_id.as_str(),
        request.turn_id.as_str(),
        request.tool_call_id.as_str(),
        request.tool_name.as_str(),
    ]
    .into_iter()
    .any(|value| value.is_empty() || value.len() > MAX_ID_BYTES)
    {
        return Err(CodingAgentClientProjectionIssue::new(
            "authorization_identity_invalid",
            "The authorization identity is invalid.",
        ));
    }
    Ok(())
}

fn validate_capability_generation(
    snapshot: &CodingAgentSnapshot,
    event: &CodingAgentProductEvent,
) -> Result<u64, CodingAgentClientProjectionIssue> {
    let current = snapshot.cursor.capability_generation;
    let Some(generation) = event.capability_generation() else {
        return Ok(current);
    };
    if generation == current {
        return Ok(generation);
    }
    if current.checked_add(1) == Some(generation)
        && matches!(
            event.event(),
            CodingAgentProductEventKind::Capability(CodingAgentCapabilityProductEvent::Changed {
                generation: payload_generation,
                ..
            }) if *payload_generation == generation
        )
    {
        return Ok(generation);
    }
    Err(CodingAgentClientProjectionIssue::new(
        "product_event_capability_generation_mismatch",
        "The product event capability generation is invalid.",
    ))
}

fn operation_association_matches(
    snapshot: &CodingAgentSnapshot,
    event: &CodingAgentProductEvent,
) -> bool {
    let Some(event_operation_id) = event.operation_id() else {
        return event.parent_operation_id().is_none() && event.root_operation_id().is_none();
    };
    if event.parent_operation_id() == Some(event_operation_id) {
        return false;
    }
    let Some(submitted) = snapshot.submitted_operation.as_ref() else {
        return true;
    };
    event_operation_id == submitted.operation_id
        || event.parent_operation_id() == Some(submitted.operation_id.as_str())
        || event.root_operation_id() == Some(submitted.operation_id.as_str())
}

fn sanitize_snapshot(snapshot: &mut CodingAgentSnapshot) {
    crate::runtime::client::context_fold::trim_context_operations(&mut snapshot.context.operations);
    for operation in &mut snapshot.context.operations {
        operation.operation_id = bounded_text(&operation.operation_id, MAX_ID_BYTES);
        operation.kind = bounded_text(&operation.kind, MAX_ID_BYTES);
        operation.parent_operation_id = operation
            .parent_operation_id
            .as_deref()
            .map(|value| bounded_text(value, MAX_ID_BYTES));
        operation.root_operation_id = operation
            .root_operation_id
            .as_deref()
            .map(|value| bounded_text(value, MAX_ID_BYTES));
        for diagnostic in &mut operation.diagnostics {
            *diagnostic = bounded_text(diagnostic, MAX_DIAGNOSTIC_BYTES);
        }
        operation.diagnostics.truncate(4);
        operation.failure = operation
            .failure
            .as_deref()
            .map(|value| bounded_text(value, MAX_DIAGNOSTIC_BYTES));
    }
    snapshot.context.changes.truncate(MAX_CONTEXT_CHANGES);
    for change in &mut snapshot.context.changes {
        change.path = bounded_text(&change.path, MAX_TOOL_BYTES);
        change.mutation_kind = bounded_text(&change.mutation_kind, MAX_ID_BYTES);
        change.operation_id = bounded_text(&change.operation_id, MAX_ID_BYTES);
        change.tool_call_id = change
            .tool_call_id
            .as_deref()
            .map(|value| bounded_text(value, MAX_ID_BYTES));
        change.diff = change
            .diff
            .as_deref()
            .map(|value| bounded_text(value, MAX_DIFF_BYTES));
    }
    snapshot
        .context
        .delegations
        .truncate(MAX_CONTEXT_DELEGATIONS);
    for delegation in &mut snapshot.context.delegations {
        delegation.tool_call_id = bounded_text(&delegation.tool_call_id, MAX_ID_BYTES);
        delegation.child_operation_id = delegation
            .child_operation_id
            .as_deref()
            .map(|value| bounded_text(value, MAX_ID_BYTES));
        delegation.target_kind = bounded_text(&delegation.target_kind, MAX_ID_BYTES);
        delegation.target_id = bounded_text(&delegation.target_id, MAX_ID_BYTES);
        delegation.task = bounded_text(&delegation.task, MAX_MESSAGE_BYTES);
        delegation.status = bounded_text(&delegation.status, MAX_ID_BYTES);
        delegation.summary = delegation
            .summary
            .as_deref()
            .map(|value| bounded_text(value, MAX_MESSAGE_BYTES));
        delegation.failure = delegation
            .failure
            .as_deref()
            .map(|value| bounded_text(value, MAX_DIAGNOSTIC_BYTES));
    }
    snapshot.drafts.truncate(MAX_PENDING_AUTHORIZATIONS);
    for draft in &mut snapshot.drafts {
        draft.id.0 = bounded_text(&draft.id.0, MAX_ID_BYTES);
        draft.text = bounded_text(&draft.text, MAX_MESSAGE_BYTES);
    }
    snapshot
        .pending_authorizations
        .truncate(MAX_PENDING_AUTHORIZATIONS);
    for request in &mut snapshot.pending_authorizations {
        sanitize_authorization(request);
    }
}

fn sanitize_authorization(request: &mut ToolAuthorizationRequest) {
    request.requested_at = bounded_text(&request.requested_at, MAX_ID_BYTES);
    request.preview.summary = bounded_text(&request.preview.summary, MAX_AUTHORIZATION_BYTES);
    request.preview.path = request
        .preview
        .path
        .as_deref()
        .map(|value| bounded_text(value, MAX_AUTHORIZATION_BYTES));
    request.preview.command = request
        .preview
        .command
        .as_deref()
        .map(|value| bounded_text(value, MAX_AUTHORIZATION_BYTES));
    request.preview.cwd = request
        .preview
        .cwd
        .as_deref()
        .map(|value| bounded_text(value, MAX_AUTHORIZATION_BYTES));
    request.preview.content_preview = request
        .preview
        .content_preview
        .as_deref()
        .map(|value| bounded_text(value, MAX_AUTHORIZATION_BYTES));
    match &mut request.scope {
        ToolAuthorizationScope::Path { path }
        | ToolAuthorizationScope::FilesystemTarget { path, .. } => {
            *path = bounded_text(path, MAX_AUTHORIZATION_BYTES);
        }
        ToolAuthorizationScope::Shell {
            cwd,
            command_fingerprint,
        } => {
            *cwd = bounded_text(cwd, MAX_AUTHORIZATION_BYTES);
            *command_fingerprint = bounded_text(command_fingerprint, MAX_ID_BYTES);
        }
        ToolAuthorizationScope::ToolArguments { fingerprint } => {
            *fingerprint = bounded_text(fingerprint, MAX_ID_BYTES);
        }
    }
    if let ToolAuthorizationScope::FilesystemTarget {
        target_fingerprint, ..
    } = &mut request.scope
    {
        *target_fingerprint = bounded_text(target_fingerprint, MAX_ID_BYTES);
    }
}

fn trim_messages(messages: &mut VecDeque<CodingAgentClientMessage>) {
    while messages.len() > MAX_MESSAGES {
        let index = messages
            .iter()
            .position(|message| message.status == CodingAgentClientMessageStatus::Completed)
            .unwrap_or(0);
        messages.remove(index);
    }
}

fn trim_tools(tools: &mut VecDeque<CodingAgentClientTool>) {
    while tools.len() > MAX_TOOLS {
        let index = tools
            .iter()
            .position(|tool| tool.status != CodingAgentClientToolStatus::Running)
            .unwrap_or(0);
        tools.remove(index);
    }
}

fn bounded_text(value: &str, max_bytes: usize) -> String {
    bounded_prefix(value, max_bytes).0
}

fn bounded_prefix(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn append_bounded(target: &mut String, value: &str, max_bytes: usize) -> bool {
    if target.len() >= max_bytes {
        return !value.is_empty();
    }
    let remaining = max_bytes - target.len();
    let (bounded, truncated) = bounded_prefix(value, remaining);
    target.push_str(&bounded);
    truncated
}

fn all_projection_areas() -> CodingAgentClientProjectionChanges {
    let mut changes = CodingAgentClientProjectionChanges::default();
    for area in [
        CodingAgentClientProjectionArea::Cursor,
        CodingAgentClientProjectionArea::Session,
        CodingAgentClientProjectionArea::Operations,
        CodingAgentClientProjectionArea::Conversation,
        CodingAgentClientProjectionArea::Tools,
        CodingAgentClientProjectionArea::Authorizations,
        CodingAgentClientProjectionArea::Delegations,
        CodingAgentClientProjectionArea::Changes,
        CodingAgentClientProjectionArea::Usage,
        CodingAgentClientProjectionArea::Diagnostics,
        CodingAgentClientProjectionArea::Recoveries,
        CodingAgentClientProjectionArea::Profiles,
        CodingAgentClientProjectionArea::Capabilities,
        CodingAgentClientProjectionArea::Lifecycle,
    ] {
        changes.insert(area);
    }
    changes
}
