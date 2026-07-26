use super::emission::ProductEventDraft;
use super::{
    CodingAgentDelegationEventContext, CodingAgentDelegationProductEvent,
    CodingAgentProductEventDurability, CodingAgentProductEventError, CodingAgentProductEventKind,
    CodingAgentProductEventProfileKind, CodingAgentProductEventTerminalStatus,
};
use crate::profiles::{ProfileId, ProfileKind};
use crate::runtime::facade::CodingSessionError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DelegationEventContext {
    pub(crate) operation_id: String,
    pub(crate) turn_id: String,
    pub(crate) tool_call_id: String,
    pub(crate) requesting_profile_id: ProfileId,
    pub(crate) target_kind: ProfileKind,
    pub(crate) target_id: ProfileId,
    pub(crate) task: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DelegationEvent {
    Requested {
        context: DelegationEventContext,
    },
    Rejected {
        context: DelegationEventContext,
        reason: String,
    },
    Approved {
        context: DelegationEventContext,
    },
    ConfirmationRequired {
        context: DelegationEventContext,
        reason: String,
    },
    Started {
        context: DelegationEventContext,
        child_operation_id: String,
    },
    Completed {
        context: DelegationEventContext,
        child_operation_id: String,
        final_text: String,
    },
    Failed {
        context: DelegationEventContext,
        child_operation_id: String,
        error: CodingSessionError,
    },
}

impl DelegationEvent {
    pub(crate) fn context(&self) -> &DelegationEventContext {
        match self {
            Self::Requested { context }
            | Self::Rejected { context, .. }
            | Self::Approved { context }
            | Self::ConfirmationRequired { context, .. }
            | Self::Started { context, .. }
            | Self::Completed { context, .. }
            | Self::Failed { context, .. } => context,
        }
    }

    pub(crate) fn is_requested(&self) -> bool {
        matches!(self, Self::Requested { .. })
    }

    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        let operation_id = self.context().operation_id.clone();
        let (event, terminal_status) = match self {
            Self::Requested { context } => (
                CodingAgentDelegationProductEvent::Requested {
                    context: public_context(context),
                },
                None,
            ),
            Self::Rejected { context, reason } => (
                CodingAgentDelegationProductEvent::Rejected {
                    context: public_context(context),
                    reason,
                },
                None,
            ),
            Self::Approved { context } => (
                CodingAgentDelegationProductEvent::Approved {
                    context: public_context(context),
                },
                None,
            ),
            Self::ConfirmationRequired { context, reason } => (
                CodingAgentDelegationProductEvent::ConfirmationRequired {
                    context: public_context(context),
                    reason,
                },
                None,
            ),
            Self::Started {
                context,
                child_operation_id,
            } => (
                CodingAgentDelegationProductEvent::Started {
                    context: public_context(context),
                    child_operation_id,
                },
                None,
            ),
            Self::Completed {
                context,
                child_operation_id,
                final_text,
            } => (
                CodingAgentDelegationProductEvent::Completed {
                    context: public_context(context),
                    child_operation_id,
                    final_text,
                },
                Some(CodingAgentProductEventTerminalStatus::Completed),
            ),
            Self::Failed {
                context,
                child_operation_id,
                error,
            } => (
                CodingAgentDelegationProductEvent::Failed {
                    context: public_context(context),
                    child_operation_id,
                    error: CodingAgentProductEventError::from(error),
                },
                Some(CodingAgentProductEventTerminalStatus::Failed),
            ),
        };
        ProductEventDraft {
            event: CodingAgentProductEventKind::Delegation(event),
            operation_id: Some(operation_id),
            session_id: None,
            terminal_status,
            durability: CodingAgentProductEventDurability::LiveOnly,
        }
    }
}

fn public_context(context: DelegationEventContext) -> CodingAgentDelegationEventContext {
    CodingAgentDelegationEventContext {
        operation_id: context.operation_id,
        turn_id: context.turn_id,
        tool_call_id: context.tool_call_id,
        requesting_profile_id: context.requesting_profile_id.as_str().to_owned(),
        target_kind: match context.target_kind {
            ProfileKind::Agent => CodingAgentProductEventProfileKind::Agent,
            ProfileKind::Team => CodingAgentProductEventProfileKind::Team,
        },
        target_id: context.target_id.as_str().to_owned(),
        task: context.task,
    }
}
