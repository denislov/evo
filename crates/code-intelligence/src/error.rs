//! 结构化错误类型。
//!
//! 每个错误携带可展示的结构化信息（路径 / 期望与实际的 identity / 预算
//! 维度），供产品层投影为诊断；缓存损坏与 identity 不匹配都带有
//! "rebuild required" 语义，调用方捕获后走重建路径，不 panic。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (manager/cache.rs CacheError classification); rewritten for Evo semantics
// (identity mismatch is a first-class variant, not a legacy-format marker).
use std::path::PathBuf;

use crate::budget::BudgetKind;
use crate::identity::CacheIdentity;

/// `code-intelligence` 的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum CodeIntelligenceError {
    /// 缓存文件损坏 / 截断 / 内容不可解析。重建即可恢复，不 panic。
    #[error("index cache corrupted at {path}: {detail} (rebuild required)")]
    CacheCorrupted { path: PathBuf, detail: String },

    /// 缓存文件格式不受支持（未知 magic 或 format/payload schema 版本）。
    /// 重建即可恢复。
    #[error("index cache format unsupported at {path}: {detail} (rebuild required)")]
    CacheFormat { path: PathBuf, detail: String },

    /// 缓存 identity 与期望不一致（workspace / revision / parser-version
    /// 任一要素不匹配）。重建即可恢复。
    #[error("index cache identity mismatch: expected {expected}, found {found} (rebuild required)")]
    CacheIdentityMismatch {
        expected: Box<CacheIdentity>,
        found: Box<CacheIdentity>,
    },

    /// revision id 不合法（为空 / 超长 / 含不可打印字符）。
    #[error("invalid revision id: {detail}")]
    InvalidRevision { detail: String },

    /// 预算被超出。
    #[error("index budget exceeded ({kind}): limit {limit}, observed {observed}")]
    BudgetExceeded {
        kind: BudgetKind,
        limit: u64,
        observed: u64,
    },

    /// 服务未启动。
    #[error("code intelligence service is not running")]
    NotRunning,

    /// 服务正在关闭，新请求被拒。
    #[error("code intelligence service is shutting down: {reason}")]
    ShuttingDown { reason: String },

    /// 服务已启动，不能再次 start。
    #[error("code intelligence service is already running")]
    AlreadyRunning,

    /// 查询类型尚未实现（骨架预留给后续 ARC）。
    #[error("query kind '{kind}' is not implemented yet ({phase})")]
    Unimplemented { kind: String, phase: &'static str },

    /// 查询处理 task 发生 panic（fail closed），响应丢失。
    #[error("query processing failed: backend task panicked")]
    QueryPanicked,

    /// 图查询失败（文件未索引 / 位置越界 / 无符号 / 语言不支持 / 解析
    /// 失败等，detail 携带结构化原因）。
    #[error("graph query failed: {detail}")]
    GraphQuery { detail: String },

    /// 透明包装的 IO 错误。
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
