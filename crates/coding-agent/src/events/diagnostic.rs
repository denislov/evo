use super::emission::ProductEventDraft;
use super::{
    CodingAgentDiagnosticProductEvent, CodingAgentProductEventDurability,
    CodingAgentProductEventKind,
};
use crate::runtime::public_error::{
    CodingAgentPublicDiagnostic, CodingAgentPublicDiagnosticOrigin,
    CodingAgentPublicDiagnosticSeverity,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiagnosticEvent {
    Diagnostic {
        operation_id: Option<String>,
        message: String,
    },
}

impl DiagnosticEvent {
    pub(crate) fn into_product_draft(self) -> ProductEventDraft {
        match self {
            Self::Diagnostic {
                operation_id,
                message,
            } => ProductEventDraft {
                event: CodingAgentProductEventKind::Diagnostic(
                    CodingAgentDiagnosticProductEvent::Diagnostic {
                        diagnostic: CodingAgentPublicDiagnostic::new(
                            CodingAgentPublicDiagnosticSeverity::Warning,
                            "runtime_diagnostic",
                            &message,
                            CodingAgentPublicDiagnosticOrigin::Runtime,
                            operation_id.as_deref(),
                        ),
                    },
                ),
                operation_id,
                session_id: None,
                terminal_status: None,
                durability: CodingAgentProductEventDurability::LiveOnly,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_diagnostic_redacts_secret_shaped_values_before_publication() {
        const SECRET: &str = "diagnostic-secret-canary";
        let draft = DiagnosticEvent::Diagnostic {
            operation_id: Some("operation-1".into()),
            message: format!("provider failed with token={SECRET}, Authorization: Bearer {SECRET}"),
        }
        .into_product_draft();
        let serialized = serde_json::to_string(&draft.event).unwrap();

        assert!(!serialized.contains(SECRET), "{serialized}");
        assert!(serialized.contains("<redacted>"), "{serialized}");
    }
}
