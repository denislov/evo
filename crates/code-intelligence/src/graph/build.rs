//! 索引构建：全量构建（walk + 预算强制 + 并发解析）与单文件增量解析。
//!
//! 移植自 Grok `manager/builder.rs`（collect_files / process_file /
//! extract_symbols_fast 形态）；Evo 改动：
//!
//! - `IndexBudget` 四维强制：文件数与总字节在收集阶段 `reserve_file`
//!   记账（超限 → 该文件与后续全部跳过并记录）；并发上限用 rayon 线程
//!   池大小强制（`max_concurrent_parses` 为 0 时用可用核心数）；单文件
//!   解析时长在解析阶段计时，超限跳过；
//! - 跳过原因结构化（[`IndexSkipReason`]），构建报告携带全部跳过记录；
//! - 二进制检测（前缀读 8 KiB 含 NUL）与 5 MiB 上限与 Grok 一致
//!   （[`MAX_INDEXABLE_FILE_SIZE`]）；
//! - 路径统一 workspace-relative（正斜杠）。
//!
//! 增量路径（[`reindex_file`]）不做预算强制（预算在构建期快照；增量
//! 事件量级小），保留语言 / 空 / 大小 / 二进制检查——见债务登记。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (manager/builder.rs + index_manager.rs: process_file_fast / reindex_file);
// Evo: IndexBudget enforcement, structured skip records, rayon pool sized by
// the concurrency budget.
use std::path::{Path, PathBuf};
use std::time::Instant;

use ignore::WalkBuilder;
use rayon::prelude::*;

use crate::budget::{BudgetKind, IndexBudget, IndexBudgetTracker};
use crate::error::CodeIntelligenceError;
use crate::languages::LanguageRegistry;

use super::extract::build_scope_graph;
use super::index::CodebaseIndex;
use super::persist::{FileMeta, normalize_rel_path};

/// 单文件可索引大小的上限（5 MiB，与 Grok 一致）。
pub const MAX_INDEXABLE_FILE_SIZE: u64 = 5 * 1024 * 1024;

/// 跳过索引的原因（结构化，供产品层投影为诊断）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexSkipReason {
    /// 语言不受支持（扩展名未注册）。
    UnsupportedLanguage,
    /// 空文件。
    Empty,
    /// 超过 [`MAX_INDEXABLE_FILE_SIZE`]。
    TooLarge(u64),
    /// 二进制内容（前缀含 NUL 字节）。
    Binary,
    /// 读取失败。
    ReadFailed,
    /// 解析超时（预算）。
    ParseTimeout,
    /// 预算超限（文件数 / 总字节），构建提前停止。
    BudgetExceeded(BudgetKind),
    /// 解析失败（语法树为空 / query 编译失败）。
    ParseFailed,
}

impl IndexSkipReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UnsupportedLanguage => "unsupported_language",
            Self::Empty => "empty",
            Self::TooLarge(_) => "too_large",
            Self::Binary => "binary",
            Self::ReadFailed => "read_failed",
            Self::ParseTimeout => "parse_timeout",
            Self::BudgetExceeded(kind) => kind.as_str(),
            Self::ParseFailed => "parse_failed",
        }
    }
}

/// 一条跳过记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSkip {
    pub rel_path: String,
    pub reason: IndexSkipReason,
}

/// 构建报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildReport {
    pub indexed_files: usize,
    pub definitions: usize,
    pub references: usize,
    pub skipped: Vec<IndexSkip>,
}

/// 全量构建入口。
pub struct IndexBuilder<'a> {
    root: &'a Path,
    registry: &'a LanguageRegistry,
    budget: IndexBudget,
}

impl<'a> IndexBuilder<'a> {
    pub fn new(root: &'a Path, registry: &'a LanguageRegistry, budget: IndexBudget) -> Self {
        Self {
            root,
            registry,
            budget,
        }
    }

    /// 全量构建：收集 → 预算记账 → 并发解析 → 合并。
    pub fn build(
        &self,
        query_version: u64,
    ) -> Result<(CodebaseIndex, BuildReport), CodeIntelligenceError> {
        let mut tracker = IndexBudgetTracker::new(self.budget);
        let mut skipped = Vec::new();
        let mut candidates: Vec<(String, PathBuf)> = Vec::new();

        let walker = WalkBuilder::new(self.root)
            .hidden(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .follow_links(false)
            .build();

        for entry in walker.flatten() {
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let absolute = entry.path();
            let Some(rel) =
                normalize_rel_path(absolute.strip_prefix(self.root).unwrap_or(absolute))
            else {
                continue;
            };
            if self.registry.for_file_path(absolute).is_none() {
                skipped.push(IndexSkip {
                    rel_path: rel,
                    reason: IndexSkipReason::UnsupportedLanguage,
                });
                continue;
            }
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            if let Err(error) = tracker.reserve_file(size) {
                // 文件数 / 字节预算超限：只增不减，剩余文件全部跳过并
                // 终止收集（预算在构建期快照）。
                let kind = match &error {
                    CodeIntelligenceError::BudgetExceeded { kind, .. } => *kind,
                    _ => unreachable!("reserve_file only fails with BudgetExceeded"),
                };
                skipped.push(IndexSkip {
                    rel_path: rel,
                    reason: IndexSkipReason::BudgetExceeded(kind),
                });
                break;
            }
            candidates.push((rel, absolute.to_path_buf()));
        }

        let pool_size = match self.budget.max_concurrent_parses {
            0 => std::thread::available_parallelism()
                .map(|count| count.get())
                .unwrap_or(4),
            limit => limit as usize,
        };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(pool_size.max(1))
            .build()
            .map_err(|error| CodeIntelligenceError::GraphQuery {
                detail: format!("cannot build parser pool: {error}"),
            })?;

        let timeout = tracker.parse_time_limit();
        let parsed: Vec<Option<ParseOutcome>> = pool.install(|| {
            candidates
                .par_iter()
                .map(|(_, absolute)| parse_file(absolute, self.registry, timeout))
                .collect()
        });

        let mut index = CodebaseIndex::new(query_version);
        let mut definitions = 0usize;
        let mut references = 0usize;
        for (candidate, outcome) in candidates.iter().zip(parsed) {
            match outcome {
                Some(ParseOutcome::Extracted(extracted)) => {
                    let (defs, refs) = extracted.graph.stats();
                    definitions += defs;
                    references += refs;
                    index.add_file(
                        &candidate.0,
                        file_meta_for(&candidate.1),
                        extracted.graph,
                        &extracted.aliases,
                        &extracted.exports,
                    );
                }
                Some(ParseOutcome::Skipped(reason)) => {
                    skipped.push(IndexSkip {
                        rel_path: candidate.0.clone(),
                        reason,
                    });
                }
                None => {}
            }
        }

        let report = BuildReport {
            indexed_files: index.file_count(),
            definitions,
            references,
            skipped,
        };
        Ok((index, report))
    }
}

/// 单文件解析产物（并发阶段）。
#[derive(Debug)]
pub(crate) enum ParseOutcome {
    Extracted(super::extract::ExtractedFile),
    Skipped(IndexSkipReason),
}

/// 单文件解析：读 → 大小 / 二进制检查 → 解析（计时）→ 提取。
pub(crate) fn parse_file(
    absolute: &Path,
    registry: &LanguageRegistry,
    timeout: Option<std::time::Duration>,
) -> Option<ParseOutcome> {
    let config = registry.for_file_path(absolute)?;
    let metadata = std::fs::metadata(absolute).ok()?;
    if metadata.len() == 0 {
        return Some(ParseOutcome::Skipped(IndexSkipReason::Empty));
    }
    if metadata.len() > MAX_INDEXABLE_FILE_SIZE {
        return Some(ParseOutcome::Skipped(IndexSkipReason::TooLarge(
            metadata.len(),
        )));
    }
    if is_binary_prefix(absolute) {
        return Some(ParseOutcome::Skipped(IndexSkipReason::Binary));
    }
    let content = match std::fs::read(absolute) {
        Ok(content) => content,
        Err(_) => return Some(ParseOutcome::Skipped(IndexSkipReason::ReadFailed)),
    };

    let Some(language) = config.language() else {
        return Some(ParseOutcome::Skipped(IndexSkipReason::ParseFailed));
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return Some(ParseOutcome::Skipped(IndexSkipReason::ParseFailed));
    }
    let start = Instant::now();
    let tree = parser.parse(&content, None);
    let elapsed = start.elapsed();
    if let Some(limit) = timeout
        && elapsed > limit
    {
        return Some(ParseOutcome::Skipped(IndexSkipReason::ParseTimeout));
    }
    let tree = match tree {
        Some(tree) => tree,
        None => return Some(ParseOutcome::Skipped(IndexSkipReason::ParseFailed)),
    };
    let Some(query) = config.compile_query() else {
        return Some(ParseOutcome::Skipped(IndexSkipReason::ParseFailed));
    };
    let extracted = build_scope_graph(&query, tree.root_node(), &content, &config);
    Some(ParseOutcome::Extracted(extracted))
}

/// 读取文件前缀 8 KiB 检查二进制内容（含 NUL 即视为二进制）。
fn is_binary_prefix(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return true;
    };
    let mut buffer = [0u8; 8192];
    let n = file.read(&mut buffer).unwrap_or(0);
    buffer[..n].contains(&0)
}

/// 解析成功后的文件 meta。
fn file_meta_for(path: &Path) -> FileMeta {
    std::fs::metadata(path)
        .ok()
        .map(|meta| FileMeta::from_metadata(&meta))
        .unwrap_or(FileMeta {
            size: 0,
            mtime_secs: 0,
            mtime_nanos: 0,
        })
}

/// 增量路径：重新解析单个文件并替换索引条目。
///
/// 不做预算强制（预算在构建期快照；增量事件量级小），保留语言 / 空 /
/// 5MB / 二进制检查。文件不可读或解析失败时条目保持缺席（与 Grok
/// `reindex_file` 语义一致）。
///
/// 返回 `true` 表示成功替换；`false` 表示跳过 / 失败（原因记录到
/// `skipped`）。
pub fn reindex_file(
    index: &mut CodebaseIndex,
    root: &Path,
    rel_path: &str,
    registry: &LanguageRegistry,
    skipped: &mut Vec<IndexSkip>,
) -> bool {
    let absolute = root.join(rel_path.replace('/', std::path::MAIN_SEPARATOR_STR));
    // 旧条目先行移除（重解析失败时保持缺席）。
    index.remove_file(rel_path);

    let Some(config) = registry.for_file_path(&absolute) else {
        skipped.push(IndexSkip {
            rel_path: rel_path.to_string(),
            reason: IndexSkipReason::UnsupportedLanguage,
        });
        return false;
    };
    let Some(metadata) = std::fs::metadata(&absolute).ok() else {
        skipped.push(IndexSkip {
            rel_path: rel_path.to_string(),
            reason: IndexSkipReason::ReadFailed,
        });
        return false;
    };
    if metadata.len() == 0 || metadata.len() > MAX_INDEXABLE_FILE_SIZE {
        let reason = if metadata.len() == 0 {
            IndexSkipReason::Empty
        } else {
            IndexSkipReason::TooLarge(metadata.len())
        };
        skipped.push(IndexSkip {
            rel_path: rel_path.to_string(),
            reason,
        });
        return false;
    }
    if is_binary_prefix(&absolute) {
        skipped.push(IndexSkip {
            rel_path: rel_path.to_string(),
            reason: IndexSkipReason::Binary,
        });
        return false;
    }
    let content = match std::fs::read(&absolute) {
        Ok(content) => content,
        Err(_) => {
            skipped.push(IndexSkip {
                rel_path: rel_path.to_string(),
                reason: IndexSkipReason::ReadFailed,
            });
            return false;
        }
    };
    let Some(language) = config.language() else {
        return false;
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return false;
    }
    let Some(tree) = parser.parse(&content, None) else {
        return false;
    };
    let Some(query) = config.compile_query() else {
        return false;
    };
    let extracted = build_scope_graph(&query, tree.root_node(), &content, &config);
    index.add_file(
        rel_path,
        FileMeta::from_metadata(&metadata),
        extracted.graph,
        &extracted.aliases,
        &extracted.exports,
    );
    true
}

/// 全量 reconcile：重新扫描 workspace，删除消失文件、重解析过期文件、
/// 添加新增文件（watcher gap 后的修正路径）。
pub fn reconcile(
    index: &mut CodebaseIndex,
    root: &Path,
    registry: &LanguageRegistry,
    skipped: &mut Vec<IndexSkip>,
) -> ReconcileReport {
    let mut report = ReconcileReport::default();
    let mut current: std::collections::BTreeMap<String, PathBuf> =
        std::collections::BTreeMap::new();
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false)
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if let Some(rel) =
            normalize_rel_path(entry.path().strip_prefix(root).unwrap_or(entry.path()))
        {
            current.insert(rel, entry.path().to_path_buf());
        }
    }

    // 1. 已索引但已消失的文件。
    let indexed_paths: Vec<String> = index.paths().iter().map(|path| path.to_string()).collect();
    for path in indexed_paths {
        if !current.contains_key(&path) {
            index.remove_file(&path);
            report.removed += 1;
        }
    }
    // 2. 过期 / 新增文件。
    for (rel, absolute) in &current {
        let stale = index
            .file_meta(rel)
            .is_some_and(|meta| meta.is_stale(absolute))
            || !index.is_indexed(rel);
        if stale {
            let was_indexed = index.is_indexed(rel);
            if reindex_file(index, root, rel, registry, skipped) {
                if was_indexed {
                    report.reindexed += 1;
                } else {
                    report.added += 1;
                }
            } else {
                report.skipped += 1;
            }
        }
    }
    report
}

/// reconcile 报告。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub removed: usize,
    pub reindexed: usize,
    pub added: usize,
    pub skipped: usize,
}
