use super::*;
use crate::runtime::public_error::safe_public_summary;
use crate::session::id::{Clock, SystemClock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentAgentProfileSummary {
    pub id: ProfileId,
    pub display_name: String,
    pub description: Option<String>,
    pub source: ProfileSource,
    pub model_id: Option<String>,
    pub tools: Vec<String>,
    pub skills: Vec<String>,
    pub supervision: SupervisionPolicy,
    pub delegation: DelegationPolicy,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentTeamProfileSummary {
    pub id: ProfileId,
    pub display_name: String,
    pub description: Option<String>,
    pub source: ProfileSource,
    pub supervisor: TeamSupervisor,
    pub strategy: TeamStrategy,
    pub members: Vec<ProfileId>,
    pub delegation: DelegationPolicy,
}

impl CodingAgentSession {
    pub fn pending_tool_authorizations(
        &self,
    ) -> Vec<crate::authorization::ToolAuthorizationRequest> {
        self.runtime_host.authorization_service.pending()
    }

    pub fn decide_tool_authorization(
        &self,
        identity: &crate::authorization::ToolAuthorizationIdentity,
        decision: crate::authorization::ToolAuthorizationDecision,
    ) -> Result<(), CodingAgentPublicError> {
        self.decide_tool_authorization_internal(identity, decision)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn decide_tool_authorization_internal(
        &self,
        identity: &crate::authorization::ToolAuthorizationIdentity,
        decision: crate::authorization::ToolAuthorizationDecision,
    ) -> Result<(), CodingSessionError> {
        self.runtime_host
            .authorization_service
            .decide(identity, decision)
    }

    pub(in crate::runtime) fn default_agent_profile_id(&self) -> ProfileId {
        crate::operations::prompt::default_agent_profile_id(
            &self.runtime_host.session_coordinator.persistence,
        )
    }

    pub fn capabilities(&self) -> CodingAgentCapabilities {
        IntentRouter::admit_query(
            &self.runtime_host.operation_supervisor.control,
            QueryIntent::Capabilities,
        );
        let persistent = matches!(
            self.runtime_host.session_coordinator.persistence,
            SessionPersistence::Persistent(_)
        );
        CodingAgentCapabilities::from_runtime_state(
            &self.runtime_host.operation_supervisor.control.activity(),
            persistent,
        )
    }

    pub fn view(&self) -> CodingAgentSessionView {
        IntentRouter::admit_query(
            &self.runtime_host.operation_supervisor.control,
            QueryIntent::SessionView,
        );
        let _ = &self.runtime_host.runtime_service;
        match &self.runtime_host.session_coordinator.persistence {
            SessionPersistence::Persistent(session_service) => session_service.view(),
            SessionPersistence::NonPersistent(state) => CodingAgentSessionView {
                session_id: state.runtime_id.clone(),
                name: None,
                default_agent_profile_id: state.default_agent_profile_id.clone(),
            },
        }
    }

    pub fn recovery_pending(
        &self,
    ) -> Result<Vec<CodingAgentRecoveryPending>, CodingAgentPublicError> {
        self.recovery_pending_internal()
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn recovery_pending_internal(
        &self,
    ) -> Result<Vec<CodingAgentRecoveryPending>, CodingSessionError> {
        IntentRouter::admit_query(
            &self.runtime_host.operation_supervisor.control,
            QueryIntent::SessionView,
        );
        let SessionPersistence::Persistent(service) =
            &self.runtime_host.session_coordinator.persistence
        else {
            return Ok(Vec::new());
        };
        Ok(service
            .inspect_recovery_pending()?
            .into_iter()
            .map(|pending| CodingAgentRecoveryPending {
                operation_id: pending.operation_id,
                recovery_id: pending.recovery_id,
                operation_kind: pending.operation_kind,
                record_version: pending.record_version,
                descriptor_revision: pending.descriptor_revision,
                capability_generation: pending.capability_generation,
                attempt_count: pending.attempt_count,
                last_attempt_at: pending.last_attempt_at,
                next_attempt_at: pending.next_attempt_at,
            })
            .collect())
    }

    pub fn agent_profiles(&self) -> Vec<CodingAgentAgentProfileSummary> {
        IntentRouter::admit_query(
            &self.runtime_host.operation_supervisor.control,
            QueryIntent::AgentProfiles,
        );
        let default_profile_id = self.default_agent_profile_id();
        self.runtime_host
            .profile_registry
            .agents()
            .map(|profile| CodingAgentAgentProfileSummary {
                id: profile.id.clone(),
                display_name: safe_public_summary(&profile.display_name),
                description: profile.description.as_deref().map(safe_public_summary),
                source: profile.source,
                model_id: profile.model.clone(),
                tools: profile.tools.clone(),
                skills: profile.skills.clone(),
                supervision: profile.supervision.clone(),
                delegation: profile.delegation.clone(),
                is_default: profile.id == default_profile_id,
            })
            .collect()
    }

    pub fn team_profiles(&self) -> Vec<CodingAgentTeamProfileSummary> {
        IntentRouter::admit_query(
            &self.runtime_host.operation_supervisor.control,
            QueryIntent::TeamProfiles,
        );
        self.runtime_host
            .profile_registry
            .teams()
            .map(|profile| CodingAgentTeamProfileSummary {
                id: profile.id.clone(),
                display_name: safe_public_summary(&profile.display_name),
                description: profile.description.as_deref().map(safe_public_summary),
                source: profile.source,
                supervisor: profile.supervisor.clone(),
                strategy: profile.strategy.clone(),
                members: profile.members.clone(),
                delegation: profile.delegation.clone(),
            })
            .collect()
    }

    pub fn profile_diagnostics(&self) -> Vec<CodingAgentPublicDiagnostic> {
        IntentRouter::admit_query(
            &self.runtime_host.operation_supervisor.control,
            QueryIntent::ProfileDiagnostics,
        );
        CodingAgentPublicDiagnostic::from_profile_diagnostics(
            self.runtime_host.profile_registry.diagnostics(),
        )
    }

    pub fn pending_delegation_confirmations(&self) -> Vec<PendingDelegationConfirmation> {
        IntentRouter::admit_query(
            &self.runtime_host.operation_supervisor.control,
            QueryIntent::PendingDelegationConfirmations,
        );
        let now = SystemClock.now_rfc3339();
        crate::operations::delegation::confirmation::active_views(
            &self
                .runtime_host
                .session_coordinator
                .pending_delegation_confirmations,
            &now,
        )
    }
}
