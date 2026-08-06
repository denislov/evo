//! 公开 API 清单。
//!
//! 服务 API 与 tool adapter 分离：本清单是 CLI / Desktop / agent tool 的
//! 唯一入口面；tool adapter（ARC-830）只依赖此面。

pub use crate::budget::{BudgetKind, BudgetSnapshot, IndexBudget, IndexBudgetTracker};
pub use crate::cache::{
    CACHE_FILE_NAME, CacheStatus, CachedFileEntry, INDEX_SCHEMA_VERSION, IndexCache,
    IndexCacheData, LoadOutcome, probe_cache,
};
pub use crate::error::CodeIntelligenceError;
pub use crate::identity::{CacheIdentity, IdentityDiff, ParserVersion, RevisionId};
pub use crate::languages::{LanguageConfig, LanguageRegistry};
pub use crate::service::{
    CodeIntelligenceHandle, CodeIntelligenceService, CodeIntelligenceServiceOptions,
    CodeIntelligenceTask, IndexStatus, QueryBackend, QueryKind, QueryRequest, QueryResponse,
    ServiceExit, ServiceShutdownReason, ServiceState, SkeletonQueryBackend,
};
