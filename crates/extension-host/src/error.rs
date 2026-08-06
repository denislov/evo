//! 结构化错误类型。
//!
//! 每个错误携带可展示的结构化信息（路径 / 名称 / 详情），供产品层投影为
//! 用户可见诊断，不丢失定位线索。

// Adapted from xai-grok-hooks, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// rewritten for Evo semantics (not a verbatim copy).
use std::path::PathBuf;

use crate::budget::BudgetKind;

/// Extension host 的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    #[error("failed to read extension file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to parse extension file {path}: {detail}")]
    ParseFile { path: PathBuf, detail: String },

    #[error("invalid extension configuration {name} in {path}: {detail}")]
    InvalidConfig {
        name: String,
        path: PathBuf,
        detail: String,
    },

    #[error("extension '{extension}' resides in untrusted folder {folder}")]
    Untrusted { extension: String, folder: PathBuf },

    #[error("extension budget exceeded ({kind}): limit {limit}, observed {observed}")]
    BudgetExceeded {
        kind: BudgetKind,
        limit: u64,
        observed: u64,
    },

    #[error("unsupported extension event version {version}; supported: {supported}")]
    UnsupportedVersion { version: u32, supported: u32 },

    #[error("extension host is not running")]
    NotRunning,

    #[error("extension host is shutting down: {reason}")]
    ShuttingDown { reason: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
