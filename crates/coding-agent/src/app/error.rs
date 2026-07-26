#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplicationError {
    #[error("unsupported mode: {0}")]
    UnsupportedMode(String),
    #[error("missing prompt")]
    MissingPrompt,
    #[error("unknown model: {0}")]
    UnknownModel(String),
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    SessionFailure(String),
    #[error("{0}")]
    Product(crate::api::error::CodingAgentPublicError),
    #[error("partial commit uncertainty for operation {operation_id}: {message}")]
    PartialCommit {
        operation_id: String,
        message: String,
    },
}

impl From<crate::api::error::CodingAgentPublicError> for ApplicationError {
    fn from(error: crate::api::error::CodingAgentPublicError) -> Self {
        Self::Product(error)
    }
}
