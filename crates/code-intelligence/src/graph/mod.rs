//! Codebase graph：单文件符号图（`ScopeGraph`）+ 跨文件索引
//! （`CodebaseIndex`）+ 查询（`GraphNavigator`）+ 构建 / 增量 / 持久化。
//!
//! 移植自 Grok `xai-codebase-graph`（SOURCE_REV d6937fe...）：
//!
//! | Evo 模块 | Grok 来源 |
//! | --- | --- |
//! | `range` | `types/range.rs`（裁剪） |
//! | `nodes` / `edges` | `scope_graph/nodes.rs` / `edges.rs`（节点带名扩展） |
//! | `scope` | `scope_graph/graph.rs` 的 `ScopeGraph` 部分（+ containment 边） |
//! | `extract` | `scope_graph_from_definitions_query` + `extract_symbols_fast` |
//! | `index` | `ScopeGraphIndex` 部分（去掉 interner，+ exports） |
//! | `query` | `navigation.rs`（+ 文件符号树查询） |
//! | `build` | `manager/builder.rs` + `index_manager.rs`（+ 预算强制） |
//! | `incremental` | `index_manager.rs` 事件面（消费 change-tracker） |
//! | `persist` | `ScopeGraphIndex` 序列化子集（JSON 而非二进制） |
//! | `backend` | Evo 原创（`QueryBackend` 实现，接 ARC-800 服务骨架） |

pub mod backend;
pub mod build;
pub mod edges;
pub mod extract;
pub mod incremental;
pub mod index;
pub mod nodes;
pub mod persist;
pub mod query;
pub mod range;
pub mod scope;

pub use backend::{GraphBackendOptions, GraphQueryBackend, GraphQueryResult, context_field};
pub use build::{
    BuildReport, IndexBuilder, IndexSkip, IndexSkipReason, MAX_INDEXABLE_FILE_SIZE,
    ReconcileReport, reconcile, reindex_file,
};
pub use incremental::{IncrementalIndexer, spawn_incremental_indexer};
pub use index::CodebaseIndex;
pub use persist::{FileMeta, GRAPH_SCHEMA_VERSION, GraphCacheData, PersistedGraph};
pub use query::{FileSymbol, GraphNavigator, GraphQueryError, Location, NavigationResult};
pub use range::{Position, Range};
pub use scope::ScopeGraph;

#[cfg(test)]
mod backend_tests;
#[cfg(test)]
mod build_tests;
#[cfg(test)]
mod graph_tests;
#[cfg(test)]
mod incremental_tests;
#[cfg(test)]
mod persistence_tests;
#[cfg(test)]
mod query_tests;
#[cfg(test)]
mod test_support;
