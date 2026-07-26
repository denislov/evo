use crate::protocol::{AssistantMessage, AssistantMessageDiagnostic, DiagnosticErrorInfo};

pub fn create(
    diagnostic_type: &str,
    error_message: impl Into<String>,
    details: Option<serde_json::Value>,
) -> AssistantMessageDiagnostic {
    AssistantMessageDiagnostic {
        diagnostic_type: diagnostic_type.into(),
        timestamp: now_secs(),
        error: Some(DiagnosticErrorInfo {
            name: Some("Error".into()),
            message: error_message.into(),
            stack: None,
            code: None,
        }),
        details,
    }
}

pub fn append(message: &mut AssistantMessage, diagnostic: AssistantMessageDiagnostic) {
    message
        .diagnostics
        .get_or_insert_with(Vec::new)
        .push(diagnostic);
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
