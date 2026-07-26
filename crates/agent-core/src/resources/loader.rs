use std::io::Read;
use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use crate::agent::types::{DiagnosticSeverity, ResourceDiagnostic};

pub const MAX_RESOURCE_ROOTS: usize = 64;
pub const MAX_RESOURCE_ENTRIES: usize = 16_384;
pub const MAX_RESOURCE_FILES: usize = 1_024;
pub const MAX_RESOURCE_FILE_BYTES: usize = 512 * 1024;
pub const MAX_RESOURCE_TOTAL_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_RESOURCE_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLoadPolicy {
    pub max_roots: usize,
    pub max_entries: usize,
    pub max_files: usize,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
    pub max_depth: usize,
}

impl Default for ResourceLoadPolicy {
    fn default() -> Self {
        Self {
            max_roots: MAX_RESOURCE_ROOTS,
            max_entries: MAX_RESOURCE_ENTRIES,
            max_files: MAX_RESOURCE_FILES,
            max_file_bytes: MAX_RESOURCE_FILE_BYTES,
            max_total_bytes: MAX_RESOURCE_TOTAL_BYTES,
            max_depth: MAX_RESOURCE_DEPTH,
        }
    }
}

impl ResourceLoadPolicy {
    fn validate(self) -> Result<(), ResourceLoadError> {
        for (field, value) in [
            ("max_roots", self.max_roots),
            ("max_entries", self.max_entries),
            ("max_files", self.max_files),
            ("max_file_bytes", self.max_file_bytes),
            ("max_total_bytes", self.max_total_bytes),
            ("max_depth", self.max_depth),
        ] {
            if value == 0 {
                return Err(ResourceLoadError::InvalidPolicy { field });
            }
        }
        if self.max_file_bytes > self.max_total_bytes {
            return Err(ResourceLoadError::InvalidPolicy {
                field: "max_file_bytes",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceLoadLimit {
    Roots,
    Entries,
    Files,
    FileBytes,
    TotalBytes,
}

impl std::fmt::Display for ResourceLoadLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Roots => "roots",
            Self::Entries => "traversal entries",
            Self::Files => "resource files",
            Self::FileBytes => "bytes per resource file",
            Self::TotalBytes => "aggregate resource bytes",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourceLoadError {
    #[error("resource loading was cancelled")]
    Cancelled,
    #[error("invalid resource load policy field: {field}")]
    InvalidPolicy { field: &'static str },
    #[error("resource load exceeded {limit} limit of {max} at {path}")]
    Limit {
        limit: ResourceLoadLimit,
        max: usize,
        path: PathBuf,
    },
    #[error("resource loader worker failed")]
    Worker,
}

pub(crate) struct ResourceLoadBudget<'a> {
    policy: ResourceLoadPolicy,
    cancellation: Option<&'a CancellationToken>,
    roots: usize,
    entries: usize,
    files: usize,
    bytes: usize,
}

impl<'a> ResourceLoadBudget<'a> {
    pub(crate) fn new(
        policy: ResourceLoadPolicy,
        cancellation: Option<&'a CancellationToken>,
    ) -> Result<Self, ResourceLoadError> {
        policy.validate()?;
        Ok(Self {
            policy,
            cancellation,
            roots: 0,
            entries: 0,
            files: 0,
            bytes: 0,
        })
    }

    pub(crate) fn policy(&self) -> ResourceLoadPolicy {
        self.policy
    }

    pub(crate) fn check_cancelled(&self) -> Result<(), ResourceLoadError> {
        if self
            .cancellation
            .is_some_and(CancellationToken::is_cancelled)
        {
            Err(ResourceLoadError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub(crate) fn visit_root(&mut self, path: &Path) -> Result<(), ResourceLoadError> {
        self.check_cancelled()?;
        self.roots = self.roots.saturating_add(1);
        self.enforce(
            ResourceLoadLimit::Roots,
            self.roots,
            self.policy.max_roots,
            path,
        )
    }

    pub(crate) fn visit_entry(&mut self, path: &Path) -> Result<(), ResourceLoadError> {
        self.check_cancelled()?;
        self.entries = self.entries.saturating_add(1);
        self.enforce(
            ResourceLoadLimit::Entries,
            self.entries,
            self.policy.max_entries,
            path,
        )
    }

    pub(crate) fn read_text(
        &mut self,
        path: &Path,
        error_code: &str,
        diagnostics: &mut Vec<ResourceDiagnostic>,
    ) -> Result<Option<String>, ResourceLoadError> {
        self.check_cancelled()?;
        self.files = self.files.saturating_add(1);
        self.enforce(
            ResourceLoadLimit::Files,
            self.files,
            self.policy.max_files,
            path,
        )?;

        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => {
                push_io_diagnostic(path, error_code, error, diagnostics);
                return Ok(None);
            }
        };
        if metadata.file_type().is_symlink() {
            diagnostics.push(ResourceDiagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "resource_symlink_rejected".into(),
                message: format!("resource symlink is not followed: {}", path.display()),
                path: path.to_path_buf(),
            });
            return Ok(None);
        }
        if !metadata.is_file() {
            return Ok(None);
        }

        let metadata_bytes = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
        self.enforce(
            ResourceLoadLimit::FileBytes,
            metadata_bytes,
            self.policy.max_file_bytes,
            path,
        )?;
        let remaining_total = self.policy.max_total_bytes.saturating_sub(self.bytes);
        if metadata_bytes > remaining_total {
            return Err(self.limit(
                ResourceLoadLimit::TotalBytes,
                self.policy.max_total_bytes,
                path,
            ));
        }

        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) => {
                push_io_diagnostic(path, error_code, error, diagnostics);
                return Ok(None);
            }
        };
        let read_limit = self.policy.max_file_bytes.min(remaining_total);
        let mut bytes = Vec::with_capacity(metadata_bytes.min(read_limit));
        if let Err(error) = file
            .take(
                u64::try_from(read_limit)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
            )
            .read_to_end(&mut bytes)
        {
            push_io_diagnostic(path, error_code, error, diagnostics);
            return Ok(None);
        }
        self.check_cancelled()?;
        if bytes.len() > read_limit {
            let (limit, max) = if remaining_total < self.policy.max_file_bytes {
                (ResourceLoadLimit::TotalBytes, self.policy.max_total_bytes)
            } else {
                (ResourceLoadLimit::FileBytes, self.policy.max_file_bytes)
            };
            return Err(self.limit(limit, max, path));
        }
        self.bytes = self.bytes.checked_add(bytes.len()).ok_or_else(|| {
            self.limit(
                ResourceLoadLimit::TotalBytes,
                self.policy.max_total_bytes,
                path,
            )
        })?;
        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(error) => {
                diagnostics.push(ResourceDiagnostic {
                    severity: DiagnosticSeverity::Warning,
                    code: error_code.into(),
                    message: format!("resource is not valid UTF-8: {}", path.display()),
                    path: path.to_path_buf(),
                });
                let _ = error;
                return Ok(None);
            }
        };
        Ok(Some(content))
    }

    fn enforce(
        &self,
        limit: ResourceLoadLimit,
        actual: usize,
        max: usize,
        path: &Path,
    ) -> Result<(), ResourceLoadError> {
        if actual > max {
            Err(self.limit(limit, max, path))
        } else {
            Ok(())
        }
    }

    fn limit(&self, limit: ResourceLoadLimit, max: usize, path: &Path) -> ResourceLoadError {
        ResourceLoadError::Limit {
            limit,
            max,
            path: path.to_path_buf(),
        }
    }
}

pub(crate) fn path_metadata(
    path: &Path,
    diagnostics: &mut Vec<ResourceDiagnostic>,
) -> Option<std::fs::Metadata> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            diagnostics.push(ResourceDiagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "resource_symlink_rejected".into(),
                message: format!("resource symlink is not followed: {}", path.display()),
                path: path.to_path_buf(),
            });
            None
        }
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            push_io_diagnostic(path, "resource_metadata_error", error, diagnostics);
            None
        }
    }
}

pub(crate) fn error_diagnostic(error: &ResourceLoadError) -> ResourceDiagnostic {
    let path = match error {
        ResourceLoadError::Limit { path, .. } => path.clone(),
        _ => PathBuf::new(),
    };
    ResourceDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: match error {
            ResourceLoadError::Cancelled => "resource_cancelled",
            ResourceLoadError::InvalidPolicy { .. } => "resource_invalid_policy",
            ResourceLoadError::Limit { .. } => "resource_limit",
            ResourceLoadError::Worker => "resource_worker_error",
        }
        .into(),
        message: error.to_string(),
        path,
    }
}

fn push_io_diagnostic(
    path: &Path,
    code: &str,
    error: std::io::Error,
    diagnostics: &mut Vec<ResourceDiagnostic>,
) {
    diagnostics.push(ResourceDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: code.into(),
        message: format!("failed to read {}: {}", path.display(), error),
        path: path.to_path_buf(),
    });
}

pub(crate) struct BlockingLoadGuard(CancellationToken);

impl BlockingLoadGuard {
    pub(crate) fn new() -> Self {
        Self(CancellationToken::new())
    }

    pub(crate) fn token(&self) -> CancellationToken {
        self.0.clone()
    }

    pub(crate) fn cancel(&self) {
        self.0.cancel();
    }
}

impl Drop for BlockingLoadGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
