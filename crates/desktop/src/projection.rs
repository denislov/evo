use std::collections::{HashSet, VecDeque};

use coding_agent::api::client::{
    CodingAgentClientBootstrap, CodingAgentClientDiagnostic, CodingAgentClientMessage,
    CodingAgentClientMessageStatus, CodingAgentClientProjection, CodingAgentClientProjectionApply,
    CodingAgentClientProjectionArea, CodingAgentClientProjectionChanges,
    CodingAgentClientProjectionIssue, CodingAgentClientProjectionLifecycle,
    CodingAgentClientRecovery, CodingAgentClientRecoveryStatus, CodingAgentClientTool,
    CodingAgentClientToolStatus, CodingAgentSnapshot, CodingAgentSnapshotCursor,
};
use coding_agent::api::embedding::CodingAgentEmbeddingSnapshot;
use coding_agent::api::event::{
    CodingAgentProductEvent, CodingAgentProductEventKind, CodingAgentWorkflowProductEvent,
};

use crate::conversation::ConversationProjection;
use crate::runtime::{
    DesktopRecoveryIdentity, DesktopRuntimeError, DesktopRuntimeHydratedSnapshot,
    DesktopRuntimeMetadataSnapshot, DesktopRuntimeRecoverySnapshot, DesktopRuntimeResyncSnapshot,
    DesktopRuntimeUpdate,
};

pub const MAX_DESKTOP_EVENT_MARKERS: usize = 256;
pub const MAX_DESKTOP_PROJECTION_ISSUES: usize = 32;
#[cfg(test)]
pub const MAX_DESKTOP_MESSAGE_OVERLAYS: usize = 64;
#[cfg(test)]
pub(crate) const MAX_AUTHORIZATION_TEXT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopProjectionLifecycle {
    Running,
    NeedsResync,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopProjectionApply {
    Applied(DesktopProjectionDelta),
    Replaced(DesktopProjectionDelta),
    IgnoredDuplicate,
    NoChange,
    NeedsResync,
}

impl DesktopProjectionApply {
    #[cfg(test)]
    pub const fn is_applied(&self) -> bool {
        matches!(self, Self::Applied(_))
    }

    pub const fn is_replaced(&self) -> bool {
        matches!(self, Self::Replaced(_))
    }

    pub const fn delta(&self) -> Option<&DesktopProjectionDelta> {
        match self {
            Self::Applied(delta) | Self::Replaced(delta) => Some(delta),
            Self::IgnoredDuplicate | Self::NoChange | Self::NeedsResync => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextDirtyFlags(u8);

impl ContextDirtyFlags {
    pub const OPERATIONS: Self = Self(1 << 0);
    pub const DELEGATIONS: Self = Self(1 << 1);
    pub const CHANGES: Self = Self(1 << 2);
    pub const USAGE: Self = Self(1 << 3);

    pub const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }

    fn insert(&mut self, flag: Self) {
        self.0 |= flag.0;
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DesktopProjectionDelta {
    pub full_replace: bool,
    pub cursor: bool,
    pub session: bool,
    pub conversation: bool,
    pub tools: bool,
    pub authorizations: bool,
    pub context: ContextDirtyFlags,
    pub diagnostics: bool,
    pub recoveries: bool,
    pub profiles: bool,
    pub capabilities: bool,
    pub lifecycle: bool,
    pub terminal: bool,
}

impl DesktopProjectionDelta {
    fn from_client_changes(changes: &CodingAgentClientProjectionChanges, terminal: bool) -> Self {
        let mut delta = Self {
            terminal,
            ..Self::default()
        };
        for area in changes.areas() {
            match area {
                CodingAgentClientProjectionArea::Cursor => delta.cursor = true,
                CodingAgentClientProjectionArea::Session => delta.session = true,
                CodingAgentClientProjectionArea::Operations => {
                    delta.context.insert(ContextDirtyFlags::OPERATIONS)
                }
                CodingAgentClientProjectionArea::Conversation => delta.conversation = true,
                CodingAgentClientProjectionArea::Tools => delta.tools = true,
                CodingAgentClientProjectionArea::Authorizations => delta.authorizations = true,
                CodingAgentClientProjectionArea::Delegations => {
                    delta.context.insert(ContextDirtyFlags::DELEGATIONS)
                }
                CodingAgentClientProjectionArea::Changes => {
                    delta.context.insert(ContextDirtyFlags::CHANGES)
                }
                CodingAgentClientProjectionArea::Usage => {
                    delta.context.insert(ContextDirtyFlags::USAGE)
                }
                CodingAgentClientProjectionArea::Diagnostics => delta.diagnostics = true,
                CodingAgentClientProjectionArea::Recoveries => delta.recoveries = true,
                CodingAgentClientProjectionArea::Profiles => delta.profiles = true,
                CodingAgentClientProjectionArea::Capabilities => delta.capabilities = true,
                CodingAgentClientProjectionArea::Lifecycle => delta.lifecycle = true,
            }
        }
        delta
    }

    fn full_replace() -> Self {
        Self {
            full_replace: true,
            cursor: true,
            session: true,
            conversation: true,
            tools: true,
            authorizations: true,
            context: ContextDirtyFlags(
                ContextDirtyFlags::OPERATIONS.0
                    | ContextDirtyFlags::DELEGATIONS.0
                    | ContextDirtyFlags::CHANGES.0
                    | ContextDirtyFlags::USAGE.0,
            ),
            diagnostics: true,
            recoveries: true,
            profiles: true,
            capabilities: true,
            lifecycle: true,
            terminal: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DesktopProjectionCounters {
    pub full_transcript_hydrations: u64,
    pub transcript_items_hydrated: u64,
    pub conversation_blocks_allocated: u64,
    pub metadata_replacements: u64,
    pub recovery_replacements: u64,
    pub product_snapshot_replacements: u64,
    pub product_view_rebuilds: u64,
    pub incremental_message_updates: u64,
    pub incremental_tool_updates: u64,
    pub incremental_diagnostic_updates: u64,
    pub incremental_recovery_updates: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopProjectionIssue {
    pub code: String,
    pub message: String,
}

impl DesktopProjectionIssue {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl From<CodingAgentClientProjectionIssue> for DesktopProjectionIssue {
    fn from(issue: CodingAgentClientProjectionIssue) -> Self {
        Self::new(issue.code, issue.summary)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEventMarker {
    pub sequence: u64,
    pub family: String,
    pub kind: String,
    pub operation_id: Option<String>,
    pub terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopMessageStatus {
    Streaming,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopMessageOverlay {
    pub operation_id: String,
    pub turn_id: String,
    pub message_id: Option<String>,
    pub text: String,
    pub thinking: String,
    pub reasoning_duration_millis: Option<u64>,
    pub status: DesktopMessageStatus,
    /// Sequence this message was first seen at, so the presenter can interleave
    /// the live message and tool queues in the order the turn produced them.
    pub started_sequence: u64,
    pub updated_sequence: u64,
    pub truncated: bool,
}

impl From<&CodingAgentClientMessage> for DesktopMessageOverlay {
    fn from(message: &CodingAgentClientMessage) -> Self {
        Self {
            operation_id: message.operation_id.clone(),
            turn_id: message.turn_id.clone(),
            message_id: message.message_id.clone(),
            text: message.text.clone(),
            thinking: message.thinking.clone(),
            reasoning_duration_millis: message.reasoning_duration_millis,
            status: match message.status {
                CodingAgentClientMessageStatus::Streaming => DesktopMessageStatus::Streaming,
                CodingAgentClientMessageStatus::Completed => DesktopMessageStatus::Completed,
            },
            started_sequence: message.started_sequence,
            updated_sequence: message.updated_sequence,
            truncated: message.truncated,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopToolStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopToolOverlay {
    pub operation_id: String,
    pub turn_id: String,
    pub tool_call_id: String,
    pub name: String,
    pub arguments: String,
    pub detail: String,
    pub status: DesktopToolStatus,
    /// Sequence this tool call was first seen at. See
    /// [`DesktopMessageOverlay::started_sequence`].
    pub started_sequence: u64,
    pub updated_sequence: u64,
    pub truncated: bool,
}

impl From<&CodingAgentClientTool> for DesktopToolOverlay {
    fn from(tool: &CodingAgentClientTool) -> Self {
        Self {
            operation_id: tool.operation_id.clone(),
            turn_id: tool.turn_id.clone(),
            tool_call_id: tool.tool_call_id.clone(),
            name: tool.name.clone(),
            arguments: tool.arguments.clone(),
            detail: tool.detail.clone(),
            status: match tool.status {
                CodingAgentClientToolStatus::Running => DesktopToolStatus::Running,
                CodingAgentClientToolStatus::Completed => DesktopToolStatus::Completed,
                CodingAgentClientToolStatus::Failed => DesktopToolStatus::Failed,
            },
            started_sequence: tool.started_sequence,
            updated_sequence: tool.updated_sequence,
            truncated: tool.truncated,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopDiagnostic {
    pub operation_id: Option<String>,
    pub message: String,
    pub sequence: u64,
    pub truncated: bool,
}

impl From<&CodingAgentClientDiagnostic> for DesktopDiagnostic {
    fn from(diagnostic: &CodingAgentClientDiagnostic) -> Self {
        Self {
            operation_id: diagnostic.operation_id.clone(),
            message: diagnostic.summary.clone(),
            sequence: diagnostic.sequence,
            truncated: diagnostic.truncated,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRecoveryStatus {
    Pending,
    Resolved,
    Recovered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRecoveryProjection {
    pub operation_id: String,
    pub recovery_id: String,
    pub status: DesktopRecoveryStatus,
    pub reason: String,
    pub updated_sequence: u64,
    pub identity: Option<DesktopRecoveryIdentity>,
    pub attempt_count: u32,
    pub authoritative: bool,
}

/// GPUI-independent desktop view over the shared product projection.
///
/// `CodingAgentClientProjection` owns every product fact. This type only
/// retains desktop presentation state: bounded event markers, transcript
/// layout, issue notices, and compatibility-shaped overlay DTOs.
#[derive(Debug, Clone)]
pub struct DesktopProjection {
    project: CodingAgentEmbeddingSnapshot,
    product: CodingAgentClientProjection,
    conversation: ConversationProjection,
    lifecycle: DesktopProjectionLifecycle,
    recent_events: VecDeque<DesktopEventMarker>,
    messages: VecDeque<DesktopMessageOverlay>,
    tools: VecDeque<DesktopToolOverlay>,
    diagnostics: VecDeque<DesktopDiagnostic>,
    recoveries: VecDeque<DesktopRecoveryProjection>,
    authoritative_recovery_ids: HashSet<String>,
    issues: VecDeque<DesktopProjectionIssue>,
    last_resync_reason: Option<DesktopRuntimeError>,
    counters: DesktopProjectionCounters,
}

impl DesktopProjection {
    pub fn new(initial: DesktopRuntimeHydratedSnapshot) -> Result<Self, DesktopProjectionIssue> {
        let DesktopRuntimeHydratedSnapshot {
            project,
            session,
            transcript,
            pending_recoveries,
        } = initial;
        let transcript_items = transcript.items.len() as u64;
        let authoritative_recovery_ids = pending_recoveries
            .iter()
            .map(|recovery| recovery.recovery_id.clone())
            .collect();
        let conversation = ConversationProjection::hydrate(transcript.clone());
        let conversation_blocks = conversation.blocks().len() as u64;
        let product = CodingAgentClientProjection::from_bootstrap(CodingAgentClientBootstrap {
            snapshot: session,
            transcript,
            pending_recoveries,
        })
        .map_err(DesktopProjectionIssue::from)?;
        let mut projection = Self {
            project,
            product,
            conversation,
            lifecycle: DesktopProjectionLifecycle::Running,
            recent_events: VecDeque::new(),
            messages: VecDeque::new(),
            tools: VecDeque::new(),
            diagnostics: VecDeque::new(),
            recoveries: VecDeque::new(),
            authoritative_recovery_ids,
            issues: VecDeque::new(),
            last_resync_reason: None,
            counters: DesktopProjectionCounters {
                full_transcript_hydrations: 1,
                transcript_items_hydrated: transcript_items,
                conversation_blocks_allocated: conversation_blocks,
                ..DesktopProjectionCounters::default()
            },
        };
        projection.rebuild_product_views();
        Ok(projection)
    }

    pub fn project(&self) -> &CodingAgentEmbeddingSnapshot {
        &self.project
    }

    pub fn snapshot(&self) -> &CodingAgentSnapshot {
        self.product.snapshot()
    }

    #[cfg(test)]
    pub(crate) fn product_for_tests(&self) -> &CodingAgentClientProjection {
        &self.product
    }

    pub fn cursor(&self) -> &CodingAgentSnapshotCursor {
        &self.product.snapshot().cursor
    }

    pub fn conversation(&self) -> &ConversationProjection {
        &self.conversation
    }

    pub const fn lifecycle(&self) -> DesktopProjectionLifecycle {
        self.lifecycle
    }

    pub fn recent_events(&self) -> &VecDeque<DesktopEventMarker> {
        &self.recent_events
    }

    pub fn messages(&self) -> &VecDeque<DesktopMessageOverlay> {
        &self.messages
    }

    pub fn tools(&self) -> &VecDeque<DesktopToolOverlay> {
        &self.tools
    }

    pub fn diagnostics(&self) -> &VecDeque<DesktopDiagnostic> {
        &self.diagnostics
    }

    pub fn recoveries(&self) -> &VecDeque<DesktopRecoveryProjection> {
        &self.recoveries
    }

    pub fn issues(&self) -> &VecDeque<DesktopProjectionIssue> {
        &self.issues
    }

    pub(crate) fn require_command_resync(&mut self, code: &str, message: &str) {
        self.require_resync(DesktopProjectionIssue::new(code, message));
    }

    #[cfg(test)]
    pub(crate) const fn counters(&self) -> DesktopProjectionCounters {
        self.counters
    }

    #[cfg(test)]
    pub fn last_resync_reason(&self) -> Option<&DesktopRuntimeError> {
        self.last_resync_reason.as_ref()
    }

    pub fn apply(&mut self, update: DesktopRuntimeUpdate) -> DesktopProjectionApply {
        let _span = tracing::trace_span!(
            "desktop.projection.apply",
            update_kind = update.kind_label(),
            cursor = self.cursor().last_event_sequence
        )
        .entered();
        match update {
            DesktopRuntimeUpdate::Reloaded { metadata, .. }
            | DesktopRuntimeUpdate::SelectionChanged { metadata, .. } => {
                self.replace_metadata_snapshot(metadata)
            }
            DesktopRuntimeUpdate::RecoveryChanged { recovery, .. } => {
                self.replace_recovery_snapshot(recovery)
            }
            DesktopRuntimeUpdate::Resynced { replacement, .. } => match replacement {
                DesktopRuntimeResyncSnapshot::Metadata(metadata) => {
                    self.replace_metadata_snapshot(metadata)
                }
                DesktopRuntimeResyncSnapshot::Hydrated(snapshot) => {
                    self.replace_runtime_snapshot(snapshot, false, None)
                }
            },
            DesktopRuntimeUpdate::SessionChanged { snapshot, .. } => {
                self.replace_runtime_snapshot(snapshot, true, None)
            }
            DesktopRuntimeUpdate::PromptAcceptedWithSession { snapshot, .. } => {
                self.replace_runtime_snapshot(snapshot, true, None)
            }
            DesktopRuntimeUpdate::PromptRejectedWithSession {
                snapshot, error, ..
            } => {
                let outcome = self.replace_runtime_snapshot(snapshot, true, None);
                self.push_issue(DesktopProjectionIssue::new(error.code, error.message));
                outcome
            }
            DesktopRuntimeUpdate::PromptStarted {
                operation_id,
                metadata,
                ..
            } => {
                if metadata
                    .session
                    .as_ref()
                    .and_then(|session| session.submitted_operation.as_ref())
                    .is_some_and(|submitted| submitted.operation_id != operation_id)
                {
                    return self.require_resync(DesktopProjectionIssue::new(
                        "prompt_operation_mismatch",
                        "the prompt operation does not match its replacement snapshot",
                    ));
                }
                self.replace_metadata_snapshot(metadata)
            }
            DesktopRuntimeUpdate::PromptFinished { snapshot, .. } => {
                self.replace_runtime_snapshot(snapshot, false, None)
            }
            DesktopRuntimeUpdate::ProductEvent { event, .. } => self.apply_product_event(event),
            DesktopRuntimeUpdate::ResyncRequired { reason, snapshot } => {
                self.replace_product_snapshot(snapshot, Some(reason))
            }
            DesktopRuntimeUpdate::CommandRejected { code, message, .. } => {
                self.push_issue(DesktopProjectionIssue::new(code, message));
                DesktopProjectionApply::NoChange
            }
            DesktopRuntimeUpdate::RuntimeFailed { error } => {
                self.lifecycle = DesktopProjectionLifecycle::Failed;
                self.push_issue(DesktopProjectionIssue::new(
                    error.code.clone(),
                    error.message.clone(),
                ));
                DesktopProjectionApply::NoChange
            }
            DesktopRuntimeUpdate::Stopped => {
                self.lifecycle = DesktopProjectionLifecycle::Stopped;
                DesktopProjectionApply::NoChange
            }
            DesktopRuntimeUpdate::SessionClosed {
                command_id,
                session_id,
            } => {
                tracing::trace!(command_id, session_id, "desktop.session.closed");
                DesktopProjectionApply::NoChange
            }
            DesktopRuntimeUpdate::PromptAccepted { .. }
            | DesktopRuntimeUpdate::SessionsListed { .. }
            | DesktopRuntimeUpdate::SessionRenamed { .. }
            | DesktopRuntimeUpdate::SessionNameObserved { .. }
            | DesktopRuntimeUpdate::ControlAccepted { .. }
            | DesktopRuntimeUpdate::AuthorizationDecisionAccepted { .. }
            | DesktopRuntimeUpdate::FileReviewed { .. }
            | DesktopRuntimeUpdate::ExternalEditorOpened { .. } => DesktopProjectionApply::NoChange,
        }
    }

    fn apply_product_event(&mut self, event: CodingAgentProductEvent) -> DesktopProjectionApply {
        if self.lifecycle != DesktopProjectionLifecycle::Running {
            return DesktopProjectionApply::NeedsResync;
        }
        let applied = self.product.apply(&event);
        match applied {
            CodingAgentClientProjectionApply::Applied(changes) => {
                if let Some(recovery_id) = event_recovery_id(&event) {
                    self.authoritative_recovery_ids.remove(recovery_id);
                }
                self.push_event_marker(&event);
                let delta = DesktopProjectionDelta::from_client_changes(
                    &changes,
                    event.terminal_operation().is_some(),
                );
                self.sync_product_delta(&delta, event.sequence());
                DesktopProjectionApply::Applied(delta)
            }
            CodingAgentClientProjectionApply::IgnoredDuplicate => {
                DesktopProjectionApply::IgnoredDuplicate
            }
            CodingAgentClientProjectionApply::NeedsResync(issue) => {
                self.require_resync(issue.into())
            }
        }
    }

    fn replace_runtime_snapshot(
        &mut self,
        replacement: DesktopRuntimeHydratedSnapshot,
        allow_session_change: bool,
        resync_reason: Option<DesktopRuntimeError>,
    ) -> DesktopProjectionApply {
        let DesktopRuntimeHydratedSnapshot {
            project,
            session,
            transcript,
            pending_recoveries,
        } = replacement;
        let transcript_items = transcript.items.len() as u64;
        let candidate = if allow_session_change {
            match CodingAgentClientProjection::from_bootstrap(CodingAgentClientBootstrap {
                snapshot: session,
                transcript: transcript.clone(),
                pending_recoveries: pending_recoveries.clone(),
            }) {
                Ok(candidate) => candidate,
                Err(issue) => return self.require_resync(issue.into()),
            }
        } else {
            let mut candidate = self.product.clone();
            if let Err(issue) = candidate.replace_snapshot(session) {
                return self.require_resync(issue.into());
            }
            if let Err(issue) = candidate.replace_transcript(transcript.clone()) {
                return self.require_resync(issue.into());
            }
            if let Err(issue) = candidate.replace_pending_recoveries(pending_recoveries.clone()) {
                return self.require_resync(issue.into());
            }
            candidate
        };

        if allow_session_change {
            self.authoritative_recovery_ids = pending_recoveries
                .iter()
                .map(|recovery| recovery.recovery_id.clone())
                .collect();
        } else {
            self.authoritative_recovery_ids = pending_recoveries
                .into_iter()
                .map(|recovery| recovery.recovery_id)
                .collect();
        }
        self.authoritative_recovery_ids.retain(|recovery_id| {
            candidate
                .recoveries()
                .iter()
                .any(|recovery| recovery.recovery_id == *recovery_id)
        });
        self.product = candidate;
        self.project = project;
        self.conversation = ConversationProjection::hydrate(transcript);
        self.counters.full_transcript_hydrations += 1;
        self.counters.transcript_items_hydrated += transcript_items;
        self.counters.conversation_blocks_allocated += self.conversation.blocks().len() as u64;
        self.lifecycle = DesktopProjectionLifecycle::Running;
        self.recent_events.clear();
        self.last_resync_reason = resync_reason;
        self.rebuild_product_views();
        DesktopProjectionApply::Replaced(DesktopProjectionDelta::full_replace())
    }

    fn replace_product_snapshot(
        &mut self,
        snapshot: CodingAgentSnapshot,
        resync_reason: Option<DesktopRuntimeError>,
    ) -> DesktopProjectionApply {
        let mut candidate = self.product.clone();
        if let Err(issue) = candidate.replace_snapshot(snapshot) {
            return self.require_resync(issue.into());
        }
        self.product = candidate;
        self.counters.product_snapshot_replacements += 1;
        self.lifecycle = DesktopProjectionLifecycle::Running;
        self.recent_events.clear();
        self.last_resync_reason = resync_reason;
        self.rebuild_product_views();
        DesktopProjectionApply::Replaced(DesktopProjectionDelta::full_replace())
    }

    fn replace_metadata_snapshot(
        &mut self,
        replacement: DesktopRuntimeMetadataSnapshot,
    ) -> DesktopProjectionApply {
        let DesktopRuntimeMetadataSnapshot { project, session } = replacement;
        let Some(session) = session else {
            self.project = project;
            self.counters.metadata_replacements += 1;
            self.lifecycle = DesktopProjectionLifecycle::Running;
            self.last_resync_reason = None;
            return DesktopProjectionApply::Replaced(DesktopProjectionDelta {
                profiles: true,
                capabilities: true,
                ..DesktopProjectionDelta::default()
            });
        };
        let mut candidate = self.product.clone();
        if let Err(issue) = candidate.replace_metadata_snapshot(session) {
            return self.require_resync(issue.into());
        }
        self.product = candidate;
        self.project = project;
        self.counters.metadata_replacements += 1;
        self.lifecycle = DesktopProjectionLifecycle::Running;
        self.recent_events.clear();
        self.last_resync_reason = None;
        self.rebuild_product_views();
        DesktopProjectionApply::Replaced(DesktopProjectionDelta::full_replace())
    }

    fn replace_recovery_snapshot(
        &mut self,
        replacement: DesktopRuntimeRecoverySnapshot,
    ) -> DesktopProjectionApply {
        let DesktopRuntimeRecoverySnapshot {
            project,
            session,
            pending_recoveries,
        } = replacement;
        let mut candidate = self.product.clone();
        if let Err(issue) = candidate.replace_metadata_snapshot(session) {
            return self.require_resync(issue.into());
        }
        if let Err(issue) = candidate.replace_pending_recoveries(pending_recoveries.clone()) {
            return self.require_resync(issue.into());
        }
        self.authoritative_recovery_ids = pending_recoveries
            .into_iter()
            .map(|recovery| recovery.recovery_id)
            .collect();
        self.authoritative_recovery_ids.retain(|recovery_id| {
            candidate
                .recoveries()
                .iter()
                .any(|recovery| recovery.recovery_id == *recovery_id)
        });
        self.product = candidate;
        self.project = project;
        self.counters.recovery_replacements += 1;
        self.lifecycle = DesktopProjectionLifecycle::Running;
        self.recent_events.clear();
        self.last_resync_reason = None;
        self.rebuild_product_views();
        DesktopProjectionApply::Replaced(DesktopProjectionDelta::full_replace())
    }

    fn require_resync(&mut self, issue: DesktopProjectionIssue) -> DesktopProjectionApply {
        self.lifecycle = DesktopProjectionLifecycle::NeedsResync;
        self.push_issue(issue);
        DesktopProjectionApply::NeedsResync
    }

    fn push_event_marker(&mut self, event: &CodingAgentProductEvent) {
        self.recent_events.push_back(DesktopEventMarker {
            sequence: event.sequence(),
            family: event.family().as_str().to_owned(),
            kind: event.kind_name().to_owned(),
            operation_id: event.operation_id().map(ToOwned::to_owned),
            terminal: event.terminal_operation().is_some(),
        });
        while self.recent_events.len() > MAX_DESKTOP_EVENT_MARKERS {
            self.recent_events.pop_front();
        }
    }

    fn sync_product_delta(&mut self, delta: &DesktopProjectionDelta, sequence: u64) {
        if delta.conversation {
            self.sync_message_overlay(sequence);
        }
        if delta.tools {
            self.sync_tool_overlay(sequence);
        }
        if delta.diagnostics {
            self.sync_diagnostic_overlay(sequence);
        }
        if delta.recoveries {
            self.sync_recovery_overlay(sequence);
        }
        if delta.lifecycle {
            self.sync_lifecycle();
        }
    }

    fn sync_message_overlay(&mut self, sequence: u64) {
        let product_len = self.product.messages().len();
        let Some(updated) = self
            .product
            .messages()
            .iter()
            .find(|message| message.updated_sequence == sequence)
            .map(DesktopMessageOverlay::from)
        else {
            return;
        };
        if let Some(current) = self.messages.iter_mut().find(|current| {
            current.operation_id == updated.operation_id
                && current.turn_id == updated.turn_id
                && current.message_id == updated.message_id
        }) {
            *current = updated;
        } else {
            self.messages.push_back(updated);
        }
        while self.messages.len() > product_len {
            self.messages.pop_front();
        }
        self.counters.incremental_message_updates += 1;
    }

    fn sync_tool_overlay(&mut self, sequence: u64) {
        let product_len = self.product.tools().len();
        let Some(updated) = self
            .product
            .tools()
            .iter()
            .find(|tool| tool.updated_sequence == sequence)
            .map(DesktopToolOverlay::from)
        else {
            return;
        };
        if let Some(current) = self
            .tools
            .iter_mut()
            .find(|current| current.tool_call_id == updated.tool_call_id)
        {
            *current = updated;
        } else {
            self.tools.push_back(updated);
        }
        while self.tools.len() > product_len {
            self.tools.pop_front();
        }
        self.counters.incremental_tool_updates += 1;
    }

    fn sync_diagnostic_overlay(&mut self, sequence: u64) {
        let product_len = self.product.diagnostics().len();
        let Some(updated) = self
            .product
            .diagnostics()
            .back()
            .filter(|diagnostic| diagnostic.sequence == sequence)
            .map(DesktopDiagnostic::from)
        else {
            return;
        };
        self.diagnostics.push_back(updated);
        while self.diagnostics.len() > product_len {
            self.diagnostics.pop_front();
        }
        self.counters.incremental_diagnostic_updates += 1;
    }

    fn sync_recovery_overlay(&mut self, sequence: u64) {
        let product_len = self.product.recoveries().len();
        let Some(updated) = self
            .product
            .recoveries()
            .iter()
            .find(|recovery| recovery.updated_sequence == sequence)
            .map(|recovery| {
                desktop_recovery(
                    recovery,
                    self.authoritative_recovery_ids
                        .contains(&recovery.recovery_id),
                )
            })
        else {
            return;
        };
        self.recoveries
            .retain(|current| current.recovery_id != updated.recovery_id);
        self.recoveries.push_front(updated);
        self.recoveries.truncate(product_len);
        self.counters.incremental_recovery_updates += 1;
    }

    fn rebuild_product_views(&mut self) {
        self.messages = self
            .product
            .messages()
            .iter()
            .map(DesktopMessageOverlay::from)
            .collect();
        self.tools = self
            .product
            .tools()
            .iter()
            .map(DesktopToolOverlay::from)
            .collect();
        self.diagnostics = self
            .product
            .diagnostics()
            .iter()
            .map(DesktopDiagnostic::from)
            .collect();
        self.recoveries = self
            .product
            .recoveries()
            .iter()
            .map(|recovery| {
                desktop_recovery(
                    recovery,
                    self.authoritative_recovery_ids
                        .contains(&recovery.recovery_id),
                )
            })
            .collect();
        self.sync_lifecycle();
        self.counters.product_view_rebuilds += 1;
    }

    fn sync_lifecycle(&mut self) {
        self.lifecycle = match self.product.lifecycle() {
            CodingAgentClientProjectionLifecycle::Running => DesktopProjectionLifecycle::Running,
            CodingAgentClientProjectionLifecycle::NeedsResync => {
                DesktopProjectionLifecycle::NeedsResync
            }
            CodingAgentClientProjectionLifecycle::Stopped => DesktopProjectionLifecycle::Stopped,
        };
    }

    fn push_issue(&mut self, issue: DesktopProjectionIssue) {
        self.issues.push_back(issue);
        while self.issues.len() > MAX_DESKTOP_PROJECTION_ISSUES {
            self.issues.pop_front();
        }
    }
}

fn event_recovery_id(event: &CodingAgentProductEvent) -> Option<&str> {
    match event.event() {
        CodingAgentProductEventKind::Workflow(
            CodingAgentWorkflowProductEvent::OperationRecoveryPending { recovery_id, .. }
            | CodingAgentWorkflowProductEvent::OperationRecoveryResolved { recovery_id, .. }
            | CodingAgentWorkflowProductEvent::OperationRecovered { recovery_id, .. },
        ) => Some(recovery_id),
        _ => None,
    }
}

fn desktop_recovery(
    recovery: &CodingAgentClientRecovery,
    authoritative: bool,
) -> DesktopRecoveryProjection {
    let identity = recovery
        .record_version
        .zip(recovery.descriptor_revision)
        .map(
            |(record_version, descriptor_revision)| DesktopRecoveryIdentity {
                operation_id: recovery.operation_id.clone(),
                recovery_id: recovery.recovery_id.clone(),
                record_version,
                descriptor_revision,
                capability_generation: recovery.capability_generation,
                attempt_count: recovery.attempt_count,
            },
        );
    DesktopRecoveryProjection {
        operation_id: recovery.operation_id.clone(),
        recovery_id: recovery.recovery_id.clone(),
        status: match recovery.status {
            CodingAgentClientRecoveryStatus::Pending => DesktopRecoveryStatus::Pending,
            CodingAgentClientRecoveryStatus::Resolved => DesktopRecoveryStatus::Resolved,
            CodingAgentClientRecoveryStatus::Recovered => DesktopRecoveryStatus::Recovered,
        },
        reason: recovery.reason.clone(),
        updated_sequence: recovery.updated_sequence,
        identity,
        attempt_count: recovery.attempt_count,
        authoritative,
    }
}
