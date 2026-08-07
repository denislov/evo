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
pub use crate::graph::backend::{
    GraphBackendOptions, GraphQueryBackend, GraphQueryResult, context_field,
};
pub use crate::graph::build::{
    BuildReport, IndexBuilder, IndexSkip, IndexSkipReason, MAX_INDEXABLE_FILE_SIZE,
    ReconcileReport, reconcile, reindex_file,
};
pub use crate::graph::incremental::{IncrementalIndexer, spawn_incremental_indexer};
pub use crate::graph::index::CodebaseIndex;
pub use crate::graph::persist::{FileMeta, GRAPH_SCHEMA_VERSION, GraphCacheData, PersistedGraph};
pub use crate::graph::query::{
    FileSymbol, GraphNavigator, GraphQueryError, Location, NavigationResult,
};
pub use crate::graph::range::{Position, Range};
pub use crate::graph::scope::ScopeGraph;
pub use crate::identity::{CacheIdentity, IdentityDiff, ParserVersion, RevisionId};
pub use crate::languages::{GrammarFn, LanguageConfig, LanguageRegistry};
pub use crate::service::{
    CodeIntelligenceHandle, CodeIntelligenceService, CodeIntelligenceServiceOptions,
    CodeIntelligenceTask, IndexStatus, QueryBackend, QueryKind, QueryRequest, QueryResponse,
    ServiceExit, ServiceShutdownReason, ServiceState, SkeletonQueryBackend,
};
