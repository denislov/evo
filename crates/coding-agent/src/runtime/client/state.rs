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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::ProductEventSequence;
    use crate::profiles::ProfileId;
    use crate::runtime::capability::CapabilityGeneration;
    use crate::runtime::control::OperationKind;
    use crate::runtime::facade::context::CodingAgentSessionView;
    use crate::runtime::facade::{CapabilityStatus, CodingAgentCapabilities};
    use crate::runtime::version::UI_SNAPSHOT_PROTOCOL_VERSION;

    fn capabilities() -> CodingAgentCapabilities {
        CodingAgentCapabilities {
            prompt: CapabilityStatus::Available,
            abort: CapabilityStatus::Disabled {
                reason: "no prompt is running".into(),
            },
            steer: CapabilityStatus::Disabled {
                reason: "no prompt is running".into(),
            },
            follow_up: CapabilityStatus::Disabled {
                reason: "no prompt is running".into(),
            },
            compact: CapabilityStatus::Available,
            fork: CapabilityStatus::Available,
            clone_session: CapabilityStatus::Available,
            branch_summary: CapabilityStatus::Available,
            switch_session: CapabilityStatus::Unsupported {
                reason: "session switching is not exposed on CodingAgentSession yet".into(),
            },
            export: CapabilityStatus::Available,
            agent_profiles: CapabilityStatus::Available,
            team_profiles: CapabilityStatus::Available,
            delegation: CapabilityStatus::Available,
            self_healing_edit: CapabilityStatus::Available,
            tools: CapabilityStatus::Available,
            shell: CapabilityStatus::Available,
        }
    }

    #[test]
    fn ui_snapshot_carries_cursor_session_and_runtime_state() {
        let snapshot = UiSnapshot::new(
            UiSnapshotCursor {
                stream_id: "stream_ui".into(),
                last_event_sequence: ProductEventSequence::new(7),
                last_session_sequence: 0,
                capability_generation: CapabilityGeneration::new(3),
            },
            UI_SNAPSHOT_PROTOCOL_VERSION,
            CodingAgentSessionView {
                session_id: "sess_ui".into(),
                default_agent_profile_id: ProfileId::from("reviewer"),
            },
            capabilities(),
            Some(OperationKind::Prompt),
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(snapshot.cursor.last_event_sequence.get(), 7);
        assert_eq!(snapshot.cursor.capability_generation.get(), 3);
        assert_eq!(snapshot.session.session_id, "sess_ui");
        assert_eq!(snapshot.active_operation, Some(OperationKind::Prompt));
        assert!(snapshot.client_drafts.is_empty());
    }

    #[test]
    fn ui_snapshot_carries_projection_version() {
        let snapshot = UiSnapshot::new(
            UiSnapshotCursor {
                stream_id: "stream_ui".into(),
                last_event_sequence: ProductEventSequence::new(7),
                last_session_sequence: 0,
                capability_generation: CapabilityGeneration::new(3),
            },
            UI_SNAPSHOT_PROTOCOL_VERSION,
            CodingAgentSessionView {
                session_id: "sess_version".into(),
                default_agent_profile_id: ProfileId::from("default"),
            },
            capabilities(),
            None,
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(snapshot.version.family, "ui_snapshot");
        assert_eq!(snapshot.version.major, 3);
        assert_eq!(snapshot.version.minor, 0);
        assert_eq!(snapshot.version, UI_SNAPSHOT_PROTOCOL_VERSION);
    }
}
