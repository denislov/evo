use super::emission::ProductEventDraft;
use super::{
    CodingAgentProductEventDurability, CodingAgentProductEventKind, CodingAgentWorkflowProductEvent,
};

pub(crate) const RECOVERY_RECORD_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveryPendingEvent {
    pub(crate) operation_id: String,
    pub(crate) recovery_id: String,
    pub(crate) reason: String,
    pub(crate) session_id: String,
    pub(crate) record_version: u64,
    pub(crate) descriptor_revision: u16,
    pub(crate) capability_generation: Option<u64>,
    pub(crate) attempt_count: u32,
    pub(crate) last_attempt_at: Option<String>,
    pub(crate) next_attempt_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveryResolvedEvent {
    pub(crate) operation_id: String,
    pub(crate) recovery_id: String,
    pub(crate) resolution: super::CodingAgentRecoveryResolution,
    pub(crate) reason: String,
    pub(crate) session_id: String,
    pub(crate) record_version: u64,
    pub(crate) descriptor_revision: u16,
    pub(crate) capability_generation: Option<u64>,
}

impl RecoveryPendingEvent {
    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        ProductEventDraft {
            event: CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::OperationRecoveryPending {
                    operation_id: self.operation_id.clone(),
                    recovery_id: self.recovery_id.clone(),
                    reason: self.reason,
                    record_version: self.record_version,
                    descriptor_revision: self.descriptor_revision,
                    capability_generation: self.capability_generation,
                    attempt_count: self.attempt_count,
                    last_attempt_at: self.last_attempt_at,
                    next_attempt_at: self.next_attempt_at,
                },
            ),
            operation_id: Some(self.operation_id.clone()),
            session_id: Some(self.session_id.clone()),
            terminal_status: None,
            durability: CodingAgentProductEventDurability::DerivedFromSession {
                session_id: self.session_id,
                source_operation_id: self.operation_id,
                recovery_id: self.recovery_id,
            },
        }
    }
}

impl RecoveryResolvedEvent {
    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        let terminal_status = match self.resolution {
            super::CodingAgentRecoveryResolution::Failed => {
                super::CodingAgentProductEventTerminalStatus::Failed
            }
            super::CodingAgentRecoveryResolution::Aborted => {
                super::CodingAgentProductEventTerminalStatus::Aborted
            }
        };
        ProductEventDraft {
            event: CodingAgentProductEventKind::Workflow(
                CodingAgentWorkflowProductEvent::OperationRecoveryResolved {
                    operation_id: self.operation_id.clone(),
                    recovery_id: self.recovery_id.clone(),
                    resolution: self.resolution,
                    reason: self.reason,
                    record_version: self.record_version,
                    descriptor_revision: self.descriptor_revision,
                    capability_generation: self.capability_generation,
                },
            ),
            operation_id: Some(self.operation_id),
            session_id: Some(self.session_id.clone()),
            terminal_status: Some(terminal_status),
            durability: CodingAgentProductEventDurability::Durable {
                session_id: self.session_id,
            },
        }
    }
}
