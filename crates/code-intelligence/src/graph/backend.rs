//! `GraphQueryBackend`：实现 [`crate::service::QueryBackend`] 的图查询后端。
//!
//! 生命周期：
//!
//! - [`GraphQueryBackend::new`]（同步）：probe 缓存 → 命中则重建索引，
//!   否则全量构建（`IndexBudget` 强制）；
//! - [`GraphQueryBackend::start_incremental`]：接入 change-tracker 事件流
//!   （可选）；
//! - `QueryBackend::query`：只读回答 `FileSymbols` / `Definition` /
//!   `Reference`（懒构建兜底）；
//! - `QueryBackend::shutdown`（async）：停止增量消费 → 等待在途 →
//!   持久化 → 关闭（服务 actor 退出前调用，骨架已接线）。

// Evo 原创模块（ARC-800 骨架预留的 `QueryBackend` 的 graph 实现）。
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use change_tracker::FsEvent;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::budget::IndexBudget;
use crate::cache::{IndexCache, IndexCacheData, LoadOutcome};
use crate::error::CodeIntelligenceError;
use crate::identity::CacheIdentity;
use crate::languages::LanguageRegistry;
use crate::service::{QueryBackend, QueryKind, QueryRequest, QueryResponse};

use super::build::{BuildReport, IndexBuilder, IndexSkip};
use super::incremental::{IncrementalIndexer, spawn_incremental_indexer};
use super::index::CodebaseIndex;
use super::query::{GraphNavigator, GraphQueryError, NavigationResult};

/// graph backend 的启动参数。
#[derive(Debug, Clone)]
pub struct GraphBackendOptions {
    /// workspace 根目录（索引与查询的路径基准）。
    pub root: PathBuf,
    /// 缓存文件路径；`None` = 纯内存。
    pub cache_path: Option<PathBuf>,
    /// 缓存 identity（workspace / revision / parser-version）。
    pub identity: CacheIdentity,
    /// 语言注册表（grammar / query 已填充）。
    pub registry: LanguageRegistry,
    /// 索引预算。
    pub budget: IndexBudget,
}

/// 图查询结果（`QueryResponse.graph` 载荷）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphQueryResult {
    /// `FileSymbols`：文件符号树（containment 内嵌）。
    Symbols {
        symbols: Vec<super::query::FileSymbol>,
    },
    /// `Definition`：符号定义位置。
    Definitions(NavigationResult),
    /// `Reference`：符号引用位置。
    References(NavigationResult),
}

/// 查询上下文 JSON 的字段名（ARC-810 契约）。
pub mod context_field {
    pub const PATH: &str = "path";
    pub const LINE: &str = "line";
    pub const COLUMN: &str = "column";
    pub const SYMBOL: &str = "symbol";
    pub const INCLUDE_DEFINITION: &str = "include_definition";
}

/// 共享内部状态。
struct GraphInner {
    index: Arc<RwLock<CodebaseIndex>>,
    root: PathBuf,
    registry: LanguageRegistry,
    budget: IndexBudget,
    cache: Mutex<IndexCache>,
    incremental: Mutex<Option<IncrementalIndexer>>,
    built: AtomicBool,
    /// 增量期间的跳过记录（构建期报告之外的诊断）。
    skipped: Arc<Mutex<Vec<IndexSkip>>>,
}

/// 图查询后端。
#[derive(Clone)]
pub struct GraphQueryBackend {
    inner: Arc<GraphInner>,
}

impl GraphQueryBackend {
    /// 构造并同步就绪：probe 缓存 → 命中重建 / 未命中全量构建。
    pub fn new(options: GraphBackendOptions) -> Result<Self, CodeIntelligenceError> {
        let query_version = options.registry.query_hash();
        let mut cache = IndexCache::new(options.cache_path.clone(), options.identity);
        let mut index = None;
        match cache.load() {
            Ok(LoadOutcome::Hit(data)) => {
                if let Some(graph_data) = data.graph.as_ref()
                    && let Some(rebuilt) = CodebaseIndex::from_persisted(graph_data)
                {
                    index = Some(rebuilt);
                }
            }
            Ok(LoadOutcome::Miss) => {}
            Err(_) => {
                // 损坏 / identity 不匹配：走全量重建（corruption recovery）。
                index = None;
            }
        }
        let (index, _) = match index {
            Some(index) => (index, None),
            None => {
                let builder = IndexBuilder::new(&options.root, &options.registry, options.budget);
                let (built, report) = builder.build(query_version)?;
                (built, Some(report))
            }
        };
        let inner = Arc::new(GraphInner {
            index: Arc::new(RwLock::new(index)),
            root: options.root,
            registry: options.registry,
            budget: options.budget,
            cache: Mutex::new(cache),
            incremental: Mutex::new(None),
            built: AtomicBool::new(true),
            skipped: Arc::new(Mutex::new(Vec::new())),
        });
        Ok(Self { inner })
    }

    /// 接入 change-tracker 事件流并启动增量 reindex actor。
    ///
    /// 调用方负责事件流的生命周期（`FsEventService` 由上层持有）。
    pub fn start_incremental(&self, events: broadcast::Receiver<FsEvent>) {
        let mut slot = self.inner.incremental.lock().unwrap();
        if slot.is_some() {
            return;
        }
        *slot = Some(spawn_incremental_indexer(
            self.inner.index.clone(),
            self.inner.root.clone(),
            self.inner.registry.clone(),
            events,
            self.inner.skipped.clone(),
        ));
    }
    /// 只读访问索引（测试与 ARC-830 使用）。
    pub fn snapshot(&self) -> std::sync::RwLockReadGuard<'_, CodebaseIndex> {
        self.inner.index.read().unwrap()
    }

    /// 当前索引统计。
    pub fn stats(&self) -> (usize, usize, usize) {
        self.snapshot().stats()
    }

    /// 增量期间的跳过记录（诊断）。
    pub fn skipped_records(&self) -> Vec<IndexSkip> {
        self.inner.skipped.lock().unwrap().clone()
    }

    /// 同步触发一次全量 rebuild（构建 / 崩溃恢复路径）。
    pub fn rebuild(&self) -> Result<BuildReport, CodeIntelligenceError> {
        let query_version = self.inner.registry.query_hash();
        let builder = IndexBuilder::new(&self.inner.root, &self.inner.registry, self.inner.budget);
        let (index, report) = builder.build(query_version)?;
        *self.inner.index.write().unwrap() = index;
        self.inner.built.store(true, Ordering::SeqCst);
        Ok(report)
    }

    /// 持久化当前索引（shutdown 路径与手动触发共用）。
    pub fn persist(&self) -> Result<(), CodeIntelligenceError> {
        let graph_data = self.snapshot().to_persisted();
        let data = IndexCacheData {
            schema_version: crate::cache::INDEX_SCHEMA_VERSION,
            built_at_unix_secs: chrono_now_secs(),
            files: Vec::new(),
            graph: Some(graph_data),
        };
        self.inner.cache.lock().unwrap().save(data)
    }

    fn query_graph(&self, request: &QueryRequest) -> Result<GraphQueryResult, GraphQueryError> {
        let navigator = GraphNavigator {
            index: &self.inner.index.read().unwrap(),
            registry: &self.inner.registry,
            root: &self.inner.root,
        };
        match request.kind {
            QueryKind::FileSymbols => {
                let path = context_string(&request.context, context_field::PATH)
                    .ok_or_else(|| GraphQueryError::ParseError("missing path".into()))?;
                Ok(GraphQueryResult::Symbols {
                    symbols: navigator.file_symbols(&path)?,
                })
            }
            QueryKind::Definition => {
                let result = match symbol_from_context(&request.context) {
                    Some(symbol) => navigator.definition_by_name(&symbol, None),
                    None => {
                        let (path, line, column) = position_from_context(&request.context)?;
                        navigator.goto_definition(&path, line, column)?
                    }
                };
                Ok(GraphQueryResult::Definitions(result))
            }
            QueryKind::Reference => {
                let include_definition = request
                    .context
                    .get(context_field::INCLUDE_DEFINITION)
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                let result = match symbol_from_context(&request.context) {
                    Some(symbol) => navigator.references_by_name(&symbol, include_definition, None),
                    None => {
                        let (path, line, column) = position_from_context(&request.context)?;
                        navigator.goto_references(&path, line, column, include_definition)?
                    }
                };
                Ok(GraphQueryResult::References(result))
            }
            QueryKind::Status | QueryKind::Diagnostics => Err(GraphQueryError::ParseError(
                "kind not handled by graph backend".into(),
            )),
        }
    }

    /// 停止增量消费 → 等待在途 → 持久化（同步；服务 actor 退出前调用）。
    fn shutdown_inner(&self) {
        if let Some(mut indexer) = self.inner.incremental.lock().unwrap().take() {
            indexer.stop();
        }
        let _ = self.persist();
        self.inner.built.store(false, Ordering::SeqCst);
    }
}

impl QueryBackend for GraphQueryBackend {
    fn query(&self, request: &QueryRequest) -> Result<QueryResponse, CodeIntelligenceError> {
        // 懒构建兜底：rebuild 失败则走错误路径（fail closed）。
        if !self.inner.built.load(Ordering::SeqCst) {
            self.rebuild()?;
        }
        let result =
            self.query_graph(request)
                .map_err(|error| CodeIntelligenceError::GraphQuery {
                    detail: error.to_string(),
                })?;
        // status 为占位：dispatch_loop 会用 actor 的真实状态覆盖。
        Ok(QueryResponse {
            kind: request.kind,
            status: crate::service::IndexStatus {
                state: crate::service::ServiceState::Running,
                identity: self.inner.cache.lock().unwrap().identity().clone(),
                cache: crate::cache::CacheStatus::Missing,
                budget: crate::budget::BudgetSnapshot::default(),
            },
            graph: Some(result),
        })
    }

    fn shutdown(&self) {
        self.shutdown_inner();
    }
}

/// 从 context JSON 读取字符串字段。
fn context_string(context: &serde_json::Value, field: &str) -> Option<String> {
    context
        .get(field)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// `Definition` / `Reference` 的「按符号名」上下文。
fn symbol_from_context(context: &serde_json::Value) -> Option<String> {
    context_string(context, context_field::SYMBOL)
}

/// 位置上下文 → `(path, line, column)`（1-indexed）。
fn position_from_context(
    context: &serde_json::Value,
) -> Result<(String, usize, usize), GraphQueryError> {
    let path = context_string(context, context_field::PATH)
        .ok_or_else(|| GraphQueryError::ParseError("missing path".into()))?;
    let line = context
        .get(context_field::LINE)
        .and_then(|value| value.as_u64())
        .ok_or_else(|| GraphQueryError::ParseError("missing line".into()))? as usize;
    let column = context
        .get(context_field::COLUMN)
        .and_then(|value| value.as_u64())
        .ok_or_else(|| GraphQueryError::ParseError("missing column".into()))?
        as usize;
    Ok((path, line, column))
}

/// 当前 UNIX 秒（无 chrono 依赖的轻量实现）。
fn chrono_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
