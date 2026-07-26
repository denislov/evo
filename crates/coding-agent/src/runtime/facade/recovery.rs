use super::{
    CodingAgentPublicError, CodingAgentRecoveryResolutionRequest,
    CodingAgentRecoveryResolutionResult, CodingAgentRecoveryRetryRequest,
    CodingAgentRecoveryRetryResult, CodingAgentSession, CodingSessionError,
};
use crate::session::service::SessionPersistence;

impl CodingAgentSession {
    pub fn resolve_recovery(
        &mut self,
        request: CodingAgentRecoveryResolutionRequest,
    ) -> Result<CodingAgentRecoveryResolutionResult, CodingAgentPublicError> {
        self.resolve_recovery_internal(request)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn resolve_recovery_internal(
        &mut self,
        request: CodingAgentRecoveryResolutionRequest,
    ) -> Result<CodingAgentRecoveryResolutionResult, CodingSessionError> {
        self.resolve_recovery_with_authority(request, "trusted_host")
    }

    pub(crate) fn resolve_recovery_with_authority(
        &mut self,
        request: CodingAgentRecoveryResolutionRequest,
        authorization_subject: &str,
    ) -> Result<CodingAgentRecoveryResolutionResult, CodingSessionError> {
        self.runtime_host
            .client_projection
            .coordinator
            .ensure_runtime_running()?;
        let SessionPersistence::Persistent(service) =
            &self.runtime_host.session_coordinator.persistence
        else {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: "recovery resolution requires a persistent session".into(),
            });
        };
        let commit = service.resolve_recovery_as(&request, authorization_subject)?;
        let operation_kind =
            super::connection::persisted_runtime_operation_kind(commit.operation_kind.clone())
                .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                    capability: "recovery resolution requires a durable root operation family"
                        .into(),
                })?;
        self.runtime_host
            .event_hub
            .service
            .emit_committed_terminal_draft(commit.draft, operation_kind);
        Ok(CodingAgentRecoveryResolutionResult {
            operation_id: commit.operation_id,
            recovery_id: commit.recovery_id,
            resolution: commit.resolution,
        })
    }
}

impl CodingAgentSession {
    pub fn retry_recovery(
        &mut self,
        request: CodingAgentRecoveryRetryRequest,
    ) -> Result<CodingAgentRecoveryRetryResult, CodingAgentPublicError> {
        self.retry_recovery_internal(request)
            .map_err(CodingAgentPublicError::from)
    }

    pub(crate) fn retry_recovery_internal(
        &mut self,
        request: CodingAgentRecoveryRetryRequest,
    ) -> Result<CodingAgentRecoveryRetryResult, CodingSessionError> {
        self.runtime_host
            .client_projection
            .coordinator
            .ensure_runtime_running()?;
        let SessionPersistence::Persistent(service) =
            &self.runtime_host.session_coordinator.persistence
        else {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: "recovery retry requires a persistent session".into(),
            });
        };
        let commit = service.retry_recovery(&request)?;
        let operation_kind =
            super::connection::persisted_runtime_operation_kind(commit.operation_kind);
        self.runtime_host
            .event_hub
            .service
            .emit_committed_recovery_pending_draft(
                commit.draft,
                operation_kind,
                commit.capability_generation,
            );
        Ok(CodingAgentRecoveryRetryResult {
            operation_id: commit.operation_id,
            recovery_id: commit.recovery_id,
            attempt_count: commit.attempt_count,
            last_attempt_at: commit.last_attempt_at,
            next_attempt_at: commit.next_attempt_at,
        })
    }
}
