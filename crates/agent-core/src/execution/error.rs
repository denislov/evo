use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileErrorCode {
    NotFound,
    AlreadyExists,
    PermissionDenied,
    InvalidPath,
    NotADirectory,
    IsDirectory,
    Io,
    Unknown,
}

impl FileErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            FileErrorCode::NotFound => "not_found",
            FileErrorCode::AlreadyExists => "already_exists",
            FileErrorCode::PermissionDenied => "permission_denied",
            FileErrorCode::InvalidPath => "invalid_path",
            FileErrorCode::NotADirectory => "not_a_directory",
            FileErrorCode::IsDirectory => "is_directory",
            FileErrorCode::Io => "io",
            FileErrorCode::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum FileError {
    #[error("{message}")]
    NotFound {
        message: String,
        path: Option<PathBuf>,
    },
    #[error("{message}")]
    AlreadyExists {
        message: String,
        path: Option<PathBuf>,
    },
    #[error("{message}")]
    PermissionDenied {
        message: String,
        path: Option<PathBuf>,
    },
    #[error("{message}")]
    InvalidPath {
        message: String,
        path: Option<PathBuf>,
    },
    #[error("{message}")]
    NotADirectory {
        message: String,
        path: Option<PathBuf>,
    },
    #[error("{message}")]
    IsDirectory {
        message: String,
        path: Option<PathBuf>,
    },
    #[error("{message}")]
    Io {
        message: String,
        path: Option<PathBuf>,
    },
    #[error("{message}")]
    Unknown {
        message: String,
        path: Option<PathBuf>,
    },
}

impl FileError {
    pub fn code(&self) -> FileErrorCode {
        match self {
            FileError::NotFound { .. } => FileErrorCode::NotFound,
            FileError::AlreadyExists { .. } => FileErrorCode::AlreadyExists,
            FileError::PermissionDenied { .. } => FileErrorCode::PermissionDenied,
            FileError::InvalidPath { .. } => FileErrorCode::InvalidPath,
            FileError::NotADirectory { .. } => FileErrorCode::NotADirectory,
            FileError::IsDirectory { .. } => FileErrorCode::IsDirectory,
            FileError::Io { .. } => FileErrorCode::Io,
            FileError::Unknown { .. } => FileErrorCode::Unknown,
        }
    }

    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            FileError::NotFound { path, .. }
            | FileError::AlreadyExists { path, .. }
            | FileError::PermissionDenied { path, .. }
            | FileError::InvalidPath { path, .. }
            | FileError::NotADirectory { path, .. }
            | FileError::IsDirectory { path, .. }
            | FileError::Io { path, .. }
            | FileError::Unknown { path, .. } => path.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionErrorCode {
    Aborted,
    Timeout,
    ShellUnavailable,
    SpawnError,
    CallbackError,
    OutputLimit,
    Protocol,
    Unknown,
}

impl ExecutionErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionErrorCode::Aborted => "aborted",
            ExecutionErrorCode::Timeout => "timeout",
            ExecutionErrorCode::ShellUnavailable => "shell_unavailable",
            ExecutionErrorCode::SpawnError => "spawn_error",
            ExecutionErrorCode::CallbackError => "callback_error",
            ExecutionErrorCode::OutputLimit => "output_limit",
            ExecutionErrorCode::Protocol => "protocol",
            ExecutionErrorCode::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ExecutionError {
    #[error("{message}")]
    Aborted { message: String },
    #[error("{message}")]
    Timeout { message: String },
    #[error("{message}")]
    ShellUnavailable { message: String },
    #[error("{message}")]
    SpawnError { message: String },
    #[error("{message}")]
    CallbackError { message: String },
    #[error("{message}")]
    OutputLimit { message: String },
    #[error("{message}")]
    Protocol { message: String },
    #[error("{message}")]
    Unknown { message: String },
}

impl ExecutionError {
    pub fn code(&self) -> ExecutionErrorCode {
        match self {
            ExecutionError::Aborted { .. } => ExecutionErrorCode::Aborted,
            ExecutionError::Timeout { .. } => ExecutionErrorCode::Timeout,
            ExecutionError::ShellUnavailable { .. } => ExecutionErrorCode::ShellUnavailable,
            ExecutionError::SpawnError { .. } => ExecutionErrorCode::SpawnError,
            ExecutionError::CallbackError { .. } => ExecutionErrorCode::CallbackError,
            ExecutionError::OutputLimit { .. } => ExecutionErrorCode::OutputLimit,
            ExecutionError::Protocol { .. } => ExecutionErrorCode::Protocol,
            ExecutionError::Unknown { .. } => ExecutionErrorCode::Unknown,
        }
    }
}
