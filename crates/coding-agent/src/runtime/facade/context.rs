use crate::application::operation::control::OperationActivity;
use crate::kernel::operation::OperationKind;

pub use crate::session::view::{
    CodingAgentRecoveryPending, CodingAgentRecoveryResolutionRequest,
    CodingAgentRecoveryResolutionResult, CodingAgentRecoveryRetryRequest,
    CodingAgentRecoveryRetryResult, CodingAgentSessionNameUpdate,
    CodingAgentSessionNameUpdateReceiver, CodingAgentSessionOpenTarget, CodingAgentSessionOptions,
    CodingAgentSessionOverview, CodingAgentSessionSummary, CodingAgentSessionTranscriptItem,
    CodingAgentSessionView, CodingAgentTranscriptContinuation, CodingAgentTranscriptSnapshot,
    SessionStorageHandle,
};
pub(crate) use crate::session::view::{CodingAgentSessionHydration, CodingAgentSessionTree};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentCapabilities {
    pub prompt: CapabilityStatus,
    pub abort: CapabilityStatus,
    pub steer: CapabilityStatus,
    pub follow_up: CapabilityStatus,
    pub compact: CapabilityStatus,
    pub fork: CapabilityStatus,
    pub clone_session: CapabilityStatus,
    pub branch_summary: CapabilityStatus,
    pub export: CapabilityStatus,
    pub self_healing_edit: CapabilityStatus,
    pub agent_profiles: CapabilityStatus,
    pub team_profiles: CapabilityStatus,
    pub delegation: CapabilityStatus,
    pub tools: CapabilityStatus,
    pub shell: CapabilityStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityStatus {
    Available,
    Disabled { reason: String },
    Unsupported { reason: String },
    Busy { operation: String },
}

impl CodingAgentCapabilities {
    pub fn idle(persistent_session: bool) -> Self {
        Self::for_session_write_operation(None, persistent_session)
    }

    pub(crate) fn for_session_write_operation(
        operation: Option<OperationKind>,
        persistent_session: bool,
    ) -> Self {
        Self::from_runtime_state(
            &OperationActivity::from_session_write(operation),
            persistent_session,
        )
    }

    pub(crate) fn from_runtime_state(
        activity: &OperationActivity,
        persistent_session: bool,
    ) -> Self {
        let session_write_capability = match activity.session_write_blocker() {
            Some(operation) => CapabilityStatus::Busy {
                operation: operation.as_str().into(),
            },
            None => CapabilityStatus::Available,
        };

        let persistent_session_write_capability =
            match (persistent_session, activity.session_write_blocker()) {
                (false, _) => CapabilityStatus::Disabled {
                    reason: "requires persistent Rust-native session".into(),
                },
                (true, Some(operation)) => CapabilityStatus::Busy {
                    operation: operation.as_str().into(),
                },
                (true, None) => CapabilityStatus::Available,
            };
        let persistent_read_capability = if persistent_session {
            CapabilityStatus::Available
        } else {
            CapabilityStatus::Disabled {
                reason: "requires persistent Rust-native session".into(),
            }
        };
        let prompt_control_capability = match activity.session_write() {
            Some(OperationKind::Prompt) => CapabilityStatus::Available,
            _ => CapabilityStatus::Disabled {
                reason: "no prompt is running".into(),
            },
        };
        let abort_capability = match activity.primary() {
            Some(_) => CapabilityStatus::Available,
            None => CapabilityStatus::Disabled {
                reason: "no cancellable operation is running".into(),
            },
        };
        let non_session_root_capability = match activity.non_session_root_blocker() {
            Some(operation) => CapabilityStatus::Busy {
                operation: operation.as_str().into(),
            },
            None => CapabilityStatus::Available,
        };

        Self {
            prompt: session_write_capability,
            abort: abort_capability,
            steer: prompt_control_capability.clone(),
            follow_up: prompt_control_capability,
            compact: persistent_session_write_capability.clone(),
            fork: persistent_session_write_capability.clone(),
            clone_session: persistent_read_capability.clone(),
            branch_summary: persistent_session_write_capability.clone(),
            export: persistent_read_capability,
            self_healing_edit: persistent_session_write_capability,
            agent_profiles: CapabilityStatus::Available,
            team_profiles: CapabilityStatus::Available,
            delegation: non_session_root_capability,
            tools: CapabilityStatus::Available,
            shell: CapabilityStatus::Available,
        }
    }
}
