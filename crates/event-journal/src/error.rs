#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalErrorKind {
    InvalidInput,
    WriteRejected,
    LockBusy,
    Io,
    Corrupt,
    Unsupported,
    Codec,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct JournalError {
    kind: JournalErrorKind,
    message: String,
}

impl JournalError {
    pub fn new(kind: JournalErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn codec(message: impl Into<String>) -> Self {
        Self::new(JournalErrorKind::Codec, message)
    }

    pub const fn kind(&self) -> JournalErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}
