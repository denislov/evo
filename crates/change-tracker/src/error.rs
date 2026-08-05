use thiserror::Error;

/// Errors returned by the filesystem event service.
#[derive(Debug, Error)]
pub enum ChangeTrackerError {
    #[error("change tracker io error: {message}")]
    Io { message: String },
    #[error("watch root is invalid: {message}")]
    InvalidRoot { message: String },
    #[error("cannot watch path: {message}")]
    WatchFailed { message: String },
    #[error("the change tracker has shut down")]
    Shutdown,
}
