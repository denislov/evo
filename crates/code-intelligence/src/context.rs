//! 按需 context 注入入口（ARC-830）：**不把完整符号图塞入 system prompt**。
//!
//! 本模块定义「给定当前符号 → 返回有限的 symbol 摘要片段」的查询入口：
//!
//! - [`search_symbols`]（`graph/query.rs`）：按名称片段搜索 + 相关度排序 +
//!   条数截断（`limit`）；
//! - [`render_symbol_context`]：对搜索结果施加**字节预算**（`max_bytes`），
//!   超限截断并显式标记（`truncated`），不静默截断；
//! - [`render_context_text`]：把摘要渲染为有界的 `<code_context>` 文本块，
//!   供 coding-agent 的 context 组装 seam 使用。
//!
//! 注入点（`crates/coding-agent/src/app/code_context.rs`）只依赖本模块的
//! 公开面；无 graph 配置时该 seam 不参与任何 context 组装，产品行为不变。

// Evo 独立设计（Grok 无对应概念：它直接给模型全量符号图缓存；Evo 按
// master plan ARC-830 决策采用按需查询 + 预算内摘要）。
use serde::{Deserialize, Serialize};

use crate::graph::index::CodebaseIndex;
use crate::graph::query::{BoundedSymbolSearch, SymbolHit, search_symbols};

/// 默认符号摘要预算：最多 20 条、8 KiB 文本。
pub const DEFAULT_SYMBOL_CONTEXT_MAX_RESULTS: usize = 20;
pub const DEFAULT_SYMBOL_CONTEXT_MAX_BYTES: usize = 8 * 1024;

/// 符号摘要的预算（条数 + 字节双重上限）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolContextBudget {
    /// 结果条数上限（`0` = 不限）。
    pub max_results: usize,
    /// 渲染文本的字节上限（`0` = 不限）。
    pub max_bytes: usize,
}

impl Default for SymbolContextBudget {
    fn default() -> Self {
        Self {
            max_results: DEFAULT_SYMBOL_CONTEXT_MAX_RESULTS,
            max_bytes: DEFAULT_SYMBOL_CONTEXT_MAX_BYTES,
        }
    }
}

/// 摘要条目（按需查询返回的有限符号摘要片段）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolContextEntry {
    pub name: String,
    pub symbol_type: String,
    /// 相对 workspace root 的路径（正斜杠分隔）。
    pub path: String,
    /// 1-indexed 定义行号。
    pub line: usize,
    /// 全局引用数。
    pub references: usize,
}

/// 有界的符号摘要：预算内条目 + 显式截断标记。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolContextSnippet {
    pub symbol: String,
    /// 搜索命中的候选总数（预算截断前）。
    pub total: usize,
    /// 预算内保留的条目数。
    pub kept: usize,
    /// 条目被截断（`kept < total`）。
    pub truncated: bool,
    pub entries: Vec<SymbolContextEntry>,
}

/// 按需查询：给定符号名称片段，返回预算内的符号摘要。
///
/// `max_results` 由搜索层截断（相关度排序后取前 N）；`max_bytes` 由
/// 渲染层截断（条目按序加入直到字节预算）。两种截断都在结果中显式
/// 标记，不静默丢弃。
pub fn query_symbol_context(
    index: &CodebaseIndex,
    symbol: &str,
    budget: &SymbolContextBudget,
) -> SymbolContextSnippet {
    let search = search_symbols(index, symbol, budget.max_results);
    render_symbol_context(&search, budget)
}

/// 对搜索结果施加字节预算，返回摘要片段。
pub fn render_symbol_context(
    search: &BoundedSymbolSearch,
    budget: &SymbolContextBudget,
) -> SymbolContextSnippet {
    let mut entries = Vec::new();
    let mut bytes = 0_usize;
    for hit in &search.results {
        let entry = entry_from_hit(hit);
        let entry_bytes = entry_bytes(&entry);
        if budget.max_bytes != 0 && bytes + entry_bytes > budget.max_bytes {
            break;
        }
        bytes += entry_bytes;
        entries.push(entry);
    }
    let kept = entries.len();
    SymbolContextSnippet {
        symbol: search.query.clone(),
        total: search.total,
        kept,
        truncated: search.truncated || kept < search.total,
        entries,
    }
}

/// 把摘要渲染为有界的 `<code_context>` 文本块（空摘要返回 `None`）。
pub fn render_context_text(snippet: &SymbolContextSnippet) -> Option<String> {
    if snippet.entries.is_empty() {
        return None;
    }
    let mut out = String::from("<code_context>\n");
    for (index, entry) in snippet.entries.iter().enumerate() {
        out.push_str(&format!(
            "{index}. {symbol_type} {name} — {path}:{line} ({references} refs)\n",
            symbol_type = entry.symbol_type,
            name = entry.name,
            path = entry.path,
            line = entry.line,
            references = entry.references,
        ));
    }
    if snippet.truncated {
        out.push_str(&format!(
            "… {} more match(es) truncated\n",
            snippet.total.saturating_sub(snippet.kept)
        ));
    }
    out.push_str("</code_context>");
    Some(out)
}

fn entry_from_hit(hit: &SymbolHit) -> SymbolContextEntry {
    SymbolContextEntry {
        name: hit.name.clone(),
        symbol_type: hit.symbol_type.clone(),
        path: hit.path.clone(),
        line: hit.line,
        references: hit.references,
    }
}

fn entry_bytes(entry: &SymbolContextEntry) -> usize {
    entry.name.len() + entry.symbol_type.len() + entry.path.len() + 32 // 固定开销（行号 / 数字 / 分隔符的估算上限）。
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::index::CodebaseIndex;
    use crate::graph::persist::FileMeta;
    use crate::graph::test_support::{builtin, extract};

    fn index_with(files: &[(&str, &str)]) -> CodebaseIndex {
        let registry = builtin();
        let mut index = CodebaseIndex::new(1);
        for (path, src) in files {
            let config = registry
                .for_file_path(std::path::Path::new(path))
                .expect("language configured");
            let extracted = extract(&config, src).expect("extract");
            index.add_file(
                path,
                FileMeta {
                    size: src.len() as u64,
                    mtime_secs: 0,
                    mtime_nanos: 0,
                },
                extracted.graph,
                &extracted.aliases,
                &extracted.exports,
            );
        }
        index
    }

    fn budget(results: usize, bytes: usize) -> SymbolContextBudget {
        SymbolContextBudget {
            max_results: results,
            max_bytes: bytes,
        }
    }

    #[test]
    fn no_matches_yields_empty_snippet() {
        let index = index_with(&[("a.rs", "pub fn alpha() {}\n")]);
        let snippet = query_symbol_context(&index, "nonexistent", &budget(10, 4096));
        assert_eq!(snippet.total, 0);
        assert_eq!(snippet.kept, 0);
        assert!(!snippet.truncated);
        assert!(snippet.entries.is_empty());
        assert_eq!(render_context_text(&snippet), None);
    }

    #[test]
    fn empty_query_yields_no_results() {
        let index = index_with(&[("a.rs", "pub fn alpha() {}\n")]);
        let snippet = query_symbol_context(&index, "", &budget(10, 4096));
        assert_eq!(snippet.total, 0);
    }

    #[test]
    fn result_count_budget_truncates_with_marker() {
        let index = index_with(&[(
            "a.rs",
            "pub fn target_alpha() {}\npub fn target_beta() {}\npub fn target_gamma() {}\n",
        )]);
        let snippet = query_symbol_context(&index, "target_", &budget(2, 4096));
        assert_eq!(snippet.total, 3);
        assert_eq!(snippet.kept, 2);
        assert!(snippet.truncated);
        assert_eq!(snippet.entries.len(), 2);
        let text = render_context_text(&snippet).expect("non-empty snippet");
        assert!(text.contains("1 more match(es) truncated"));
        assert!(text.starts_with("<code_context>"));
        assert!(text.ends_with("</code_context>"));
    }

    #[test]
    fn byte_budget_truncates_with_marker() {
        let index = index_with(&[(
            "a.rs",
            "pub fn another_long_symbol_name_beta() {}\npub fn long_symbol_name_alpha() {}\n",
        )]);
        // 字节预算只够容纳相关度最高的第一条。
        let snippet = query_symbol_context(&index, "long", &budget(10, 80));
        assert_eq!(snippet.total, 2);
        assert!(snippet.truncated, "byte truncation must be marked");
        assert_eq!(snippet.kept, 1);
        assert_eq!(snippet.entries[0].name, "another_long_symbol_name_beta");
        let text = render_context_text(&snippet).expect("non-empty snippet");
        assert!(text.contains("1 more match(es) truncated"));
    }

    #[test]
    fn results_are_ranked_relevance_first() {
        let index = index_with(&[(
            "a.rs",
            "pub fn target_helper() {}\npub fn target() {}\npub fn target_extra() {}\n",
        )]);
        let snippet = query_symbol_context(&index, "target", &budget(10, 4096));
        assert_eq!(snippet.entries[0].name, "target");
        // 精确名在相关度排序中最前。
        assert_eq!(snippet.entries[0].path, "a.rs");
    }

    #[test]
    fn reference_count_is_reported() {
        let index = index_with(&[("a.rs", "pub fn shared() {}\nfn uses() { shared(); }\n")]);
        let snippet = query_symbol_context(&index, "shared", &budget(10, 4096));
        assert_eq!(snippet.entries[0].references, 1);
    }
}
