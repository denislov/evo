//! Desktop projection DTOs: lifecycle, apply outcome, deltas, overlays, and
//! recovery/review projections over the product client projection.

use coding_agent::api::client::{
    CodingAgentClientDiagnostic, CodingAgentClientMessage, CodingAgentClientMessageStatus,
    CodingAgentClientProjectionArea, CodingAgentClientProjectionChanges,
    CodingAgentClientProjectionIssue, CodingAgentClientTool, CodingAgentClientToolStatus,
};
use coding_agent::api::event::CodingAgentProductEvent;

use crate::runtime::{
    DesktopRecoveryIdentity, DesktopRuntimeError, DesktopRuntimeHydratedSnapshot,
    DesktopRuntimeMetadataSnapshot, DesktopRuntimeRecoverySnapshot,
};
use coding_agent::api::client::CodingAgentSnapshot;

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

#[derive(Debug)]
pub(crate) enum ProjectionEvent {
    Metadata(DesktopRuntimeMetadataSnapshot),
    Recovery(DesktopRuntimeRecoverySnapshot),
    Hydrated {
        snapshot: DesktopRuntimeHydratedSnapshot,
        allow_session_change: bool,
        issue: Option<DesktopRuntimeError>,
    },
    PromptStarted {
        operation_id: String,
        metadata: DesktopRuntimeMetadataSnapshot,
    },
    Product(CodingAgentProductEvent),
    ProductSnapshot {
        reason: DesktopRuntimeError,
        snapshot: CodingAgentSnapshot,
    },
    Issue(DesktopRuntimeError),
    RuntimeFailed(DesktopRuntimeError),
    Stopped,
}

impl ProjectionEvent {
    pub(crate) const fn kind_label(&self) -> &'static str {
        match self {
            Self::Metadata(_) => "metadata",
            Self::Recovery(_) => "recovery",
            Self::Hydrated { .. } => "hydrated",
            Self::PromptStarted { .. } => "prompt_started",
            Self::Product(_) => "product_event",
            Self::ProductSnapshot { .. } => "product_snapshot",
            Self::Issue(_) => "issue",
            Self::RuntimeFailed(_) => "runtime_failed",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopProjectionApply {
    Applied(DesktopProjectionDelta),
    Replaced(DesktopProjectionDelta),
    IgnoredDuplicate,
    NoDelta,
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
            Self::IgnoredDuplicate | Self::NoDelta | Self::NeedsResync => None,
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

    pub(crate) fn insert(&mut self, flag: Self) {
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
    pub(crate) fn from_client_changes(
        changes: &CodingAgentClientProjectionChanges,
        terminal: bool,
    ) -> Self {
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

    pub(crate) fn full_replace() -> Self {
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
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
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
