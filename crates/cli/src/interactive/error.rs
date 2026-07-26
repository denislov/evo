use coding_agent::api::error::CodingAgentPublicError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum CliError {
    #[error("unsupported mode: {0}")]
    UnsupportedMode(String),
    #[error("agent failure: {0}")]
    AgentFailure(String),
    #[error("{0}")]
    SessionFailure(String),
    #[error("{0}")]
    Product(CodingAgentPublicError),
}

impl From<CodingAgentPublicError> for CliError {
    fn from(error: CodingAgentPublicError) -> Self {
        Self::Product(error)
    }
}
