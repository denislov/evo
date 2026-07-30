use crate::authorization::ToolAuthorizationRequest;
use crate::events::{ProductEvent, ProductEventSequence};
use crate::runtime::capability::CapabilityGeneration;
use crate::runtime::client::context::UiContextProjection;
use crate::runtime::control::OperationKind;
use crate::runtime::facade::context::{CodingAgentCapabilities, CodingAgentSessionView};
use crate::runtime::version::ProtocolFamilyVersion;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UiSnapshotCursor {
    pub(crate) stream_id: String,
    pub(crate) last_event_sequence: ProductEventSequence,
    pub(crate) last_session_sequence: u64,
    pub(crate) capability_generation: CapabilityGeneration,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UiSnapshot {
    pub(crate) cursor: UiSnapshotCursor,
    pub(crate) version: ProtocolFamilyVersion,
    pub(crate) session: CodingAgentSessionView,
    pub(crate) capabilities: CodingAgentCapabilities,
    pub(crate) active_operation: Option<OperationKind>,
    pub(crate) client_drafts: Vec<ClientDraft>,
    pub(crate) pending_authorizations: Vec<ToolAuthorizationRequest>,
    pub(crate) context: UiContextProjection,
    pub(crate) recent_child_events: Vec<ProductEvent>,
}

impl UiSnapshot {
    pub(crate) fn new(
        cursor: UiSnapshotCursor,
        version: ProtocolFamilyVersion,
        session: CodingAgentSessionView,
        capabilities: CodingAgentCapabilities,
        active_operation: Option<OperationKind>,
        client_drafts: Vec<ClientDraft>,
        pending_authorizations: Vec<ToolAuthorizationRequest>,
    ) -> Self {
        Self {
            cursor,
            version,
            session,
            capabilities,
            active_operation,
            client_drafts,
            pending_authorizations,
            context: UiContextProjection::default(),
            recent_child_events: Vec::new(),
        }
    }

    pub(crate) fn with_context(mut self, context: UiContextProjection) -> Self {
        self.context = context;
        self
    }

    pub(crate) fn with_recent_child_events(mut self, events: Vec<ProductEvent>) -> Self {
        self.recent_child_events = events;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ClientConnectionId(String);

impl ClientConnectionId {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientDraftKind {
    Prompt,
    Steer,
    FollowUp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientDraft {
    pub(crate) kind: ClientDraftKind,
    pub(crate) text: String,
}

impl ClientDraft {
    pub(crate) fn new(kind: ClientDraftKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }
}
