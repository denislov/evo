//! 公开 API 清单。
//!
//! 服务 API 与 tool adapter 分离：本清单是 CLI / Desktop / agent tool 的
//! 唯一入口面；tool adapter（ARC-830）只依赖此面。

pub use crate::budget::{BudgetKind, BudgetSnapshot, IndexBudget, IndexBudgetTracker};
pub use crate::cache::{
    CACHE_FILE_NAME, CacheStatus, CachedFileEntry, INDEX_SCHEMA_VERSION, IndexCache,
    IndexCacheData, LoadOutcome, probe_cache,
};
pub use crate::context::{
    DEFAULT_SYMBOL_CONTEXT_MAX_BYTES, DEFAULT_SYMBOL_CONTEXT_MAX_RESULTS, SymbolContextBudget,
    SymbolContextEntry, SymbolContextSnippet, query_symbol_context, render_context_text,
    render_symbol_context,
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
    BoundedSymbolSearch, FileSymbol, GraphNavigator, GraphQueryError, Location,
    MAX_SYMBOL_SEARCH_CANDIDATES, NavigationResult, SymbolHit, search_symbols,
};
pub use crate::graph::range::{Position, Range};
pub use crate::graph::scope::ScopeGraph;
pub use crate::identity::{CacheIdentity, IdentityDiff, ParserVersion, RevisionId};
pub use crate::languages::{GrammarFn, LanguageConfig, LanguageRegistry};
pub use crate::lsp::diagnostics::{
    DiagnosticItem, DiagnosticSeverity, DiagnosticStaleness, DiagnosticStore, StalePolicy,
    StoredDiagnostics,
};
pub use crate::lsp::documents::{
    ContentChange, DocumentError, DocumentStore, DocumentUri, OpenDocument,
};
/// LSP 位置类型（与 graph 的 [`crate::graph::range::Position`] 语义不同：
/// 0-indexed + UTF-16 character；别名避免命名冲突）。
pub use crate::lsp::documents::{Position as LspPosition, Range as LspRange};
pub use crate::lsp::edit::{
    EditApplicator, EditError, EditPlan, PlannedChange, TextDocumentEdit, TextEdit, WorkspaceEdit,
};
pub use crate::lsp::query::{LspQuery, LspQueryKind, LspQueryResult};
pub use crate::lsp::server::{
    BackoffConfig, DEFAULT_BACKOFF_INITIAL, DEFAULT_BACKOFF_MAX, DEFAULT_MAX_FRAME_BYTES,
    DEFAULT_MAX_RESTART_ATTEMPTS, DEFAULT_PING_INTERVAL, DEFAULT_PING_TIMEOUT,
    DEFAULT_REQUEST_TIMEOUT, LivenessConfig, LspError, LspExit, LspHandle, LspServerConfig,
    LspService, LspShutdownReason, LspSnapshot, LspTask,
};
pub use crate::lsp::state::{LspEvent, LspLifecycleState};
pub use crate::service::{
    CodeIntelligenceHandle, CodeIntelligenceService, CodeIntelligenceServiceOptions,
    CodeIntelligenceTask, IndexStatus, QueryBackend, QueryKind, QueryRequest, QueryResponse,
    ServiceExit, ServiceShutdownReason, ServiceState, SkeletonQueryBackend,
};
