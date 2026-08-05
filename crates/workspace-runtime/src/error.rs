use thiserror::Error;

/// Failure of a platform-level workspace operation.
///
/// Product layers convert this into their own public error vocabulary; the
/// variants deliberately mirror the two failures a capability layer can
/// produce: a resource that could not be used, and an operation the granted
/// authority does not cover.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkspaceError {
    #[error("resource error: {message}")]
    Resource { message: String },
    #[error("unsupported capability: {capability}")]
    UnsupportedCapability { capability: String },
}
