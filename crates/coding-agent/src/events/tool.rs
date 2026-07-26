use super::emission::ProductEventDraft;
use super::{
    CodingAgentProductEventDurability, CodingAgentProductEventKind,
    CodingAgentProductEventTerminalStatus, CodingAgentToolProductEvent,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolEvent {
    AuthorizationRequired {
        request: crate::authorization::ToolAuthorizationRequest,
    },
    AuthorizationApproved {
        request: crate::authorization::ToolAuthorizationRequest,
        decision: crate::authorization::ToolAuthorizationDecision,
    },
    AuthorizationDenied {
        request: crate::authorization::ToolAuthorizationRequest,
        reason: String,
    },
    AuthorizationCancelled {
        request: crate::authorization::ToolAuthorizationRequest,
        reason: String,
    },
    Started {
        operation_id: String,
        turn_id: String,
        tool_call_id: String,
        name: String,
        arguments_json: String,
    },
    Updated {
        operation_id: String,
        turn_id: String,
        tool_call_id: String,
        name: String,
        message: String,
    },
    Completed {
        operation_id: String,
        turn_id: String,
        tool_call_id: String,
        name: String,
        summary: String,
    },
    Failed {
        operation_id: String,
        turn_id: String,
        tool_call_id: String,
        name: String,
        message: String,
    },
}

impl ToolEvent {
    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        let (event, operation_id, terminal_status) = match self {
            Self::AuthorizationRequired { request } => (
                CodingAgentToolProductEvent::AuthorizationRequired {
                    request: request.clone(),
                },
                request.operation_id,
                None,
            ),
            Self::AuthorizationApproved { request, decision } => (
                CodingAgentToolProductEvent::AuthorizationApproved {
                    authorization_id: request.authorization_id,
                    operation_id: request.operation_id.clone(),
                    tool_call_id: request.tool_call_id,
                    decision,
                },
                request.operation_id,
                None,
            ),
            Self::AuthorizationDenied { request, reason } => (
                CodingAgentToolProductEvent::AuthorizationDenied {
                    authorization_id: request.authorization_id,
                    operation_id: request.operation_id.clone(),
                    tool_call_id: request.tool_call_id,
                    reason,
                },
                request.operation_id,
                None,
            ),
            Self::AuthorizationCancelled { request, reason } => (
                CodingAgentToolProductEvent::AuthorizationCancelled {
                    authorization_id: request.authorization_id,
                    operation_id: request.operation_id.clone(),
                    tool_call_id: request.tool_call_id,
                    reason,
                },
                request.operation_id,
                None,
            ),
            Self::Started {
                operation_id,
                turn_id,
                tool_call_id,
                name,
                arguments_json,
            } => (
                CodingAgentToolProductEvent::Started {
                    operation_id: operation_id.clone(),
                    turn_id,
                    tool_call_id,
                    name,
                    arguments_json,
                },
                operation_id,
                None,
            ),
            Self::Updated {
                operation_id,
                turn_id,
                tool_call_id,
                name,
                message,
            } => (
                CodingAgentToolProductEvent::Updated {
                    operation_id: operation_id.clone(),
                    turn_id,
                    tool_call_id,
                    name,
                    message,
                },
                operation_id,
                None,
            ),
            Self::Completed {
                operation_id,
                turn_id,
                tool_call_id,
                name,
                summary,
            } => (
                CodingAgentToolProductEvent::Completed {
                    operation_id: operation_id.clone(),
                    turn_id,
                    tool_call_id,
                    name,
                    summary,
                },
                operation_id,
                Some(CodingAgentProductEventTerminalStatus::Completed),
            ),
            Self::Failed {
                operation_id,
                turn_id,
                tool_call_id,
                name,
                message,
            } => (
                CodingAgentToolProductEvent::Failed {
                    operation_id: operation_id.clone(),
                    turn_id,
                    tool_call_id,
                    name,
                    message,
                },
                operation_id,
                Some(CodingAgentProductEventTerminalStatus::Failed),
            ),
        };
        ProductEventDraft {
            event: CodingAgentProductEventKind::Tool(event),
            operation_id: Some(operation_id),
            session_id: None,
            terminal_status,
            durability: CodingAgentProductEventDurability::LiveOnly,
        }
    }
}
