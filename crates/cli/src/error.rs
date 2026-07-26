#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CliError {
    #[error("missing value for {0}")]
    MissingValue(String),
    #[error("unknown flag: {0}")]
    UnknownFlag(String),
    #[error("invalid max turns: {0}")]
    InvalidMaxTurns(String),
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    InvalidSessionFlags(String),
    #[error("agent failure: {0}")]
    AgentFailure(String),
    #[error("{0}")]
    SessionFailure(String),
    #[error("{0}")]
    Product(coding_agent::api::error::CodingAgentPublicError),
}

impl From<coding_agent::api::error::CodingAgentPublicError> for CliError {
    fn from(error: coding_agent::api::error::CodingAgentPublicError) -> Self {
        Self::Product(error)
    }
}
