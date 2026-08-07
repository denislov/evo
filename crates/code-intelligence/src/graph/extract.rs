//! 用 tree-sitter query 从语法树提取符号（definitions / references /
//! aliases / exports / containment 边）。
//!
//! 移植自 Grok `scope_graph_from_definitions_query` 与
//! `extract_symbols_fast` 的合并形态：
//!
//! - capture 名约定 `name.definition.{sym}` / `name.reference.{sym}` /
//!   `alias.original` / `alias.name`（Grok 与各语言 `.scm` 查询的契约，
//!   直接复用）；
//! - def 插入挂全局 scope（Grok fast 路径语义；作用域树仍构建，
//!   供文件内 ref 解析与后续 ARC 使用）；
//! - ref 无条件插入（跨文件引用跟踪）；
//! - **containment 推导为 Evo 扩展**：def 声明体被另一个 def 声明体
//!   严格包含时建立 `(child, parent)` 边（O(n²) 双循环；单文件 def 数量
//!   有限，简单性优先）；
//! - `name.reference.export` 类 capture 收集为文件级 exports 列表。
//! - 名字在提取阶段直接落节点（Grok 依赖调用方持 src 再切片；Evo 查询
//!   面不携带 src，见 `nodes.rs`）。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (scope_graph/graph.rs: scope_graph_from_definitions_query +
// extract_symbols_fast); Evo extension: containment derivation + exports
// collection, name-carrying nodes.
use tree_sitter::{Query, QueryCursor, StreamingIterator};

use crate::languages::LanguageConfig;

use super::nodes::{LocalDef, Reference, SymbolId};
use super::range::Range;
use super::scope::ScopeGraph;

/// 一个文件的提取结果。
#[derive(Debug)]
pub struct ExtractedFile {
    pub graph: ScopeGraph,
    /// alias 对：`(alias_name, original_name)`。
    pub aliases: Vec<(String, String)>,
    /// 文件级导出符号名。
    pub exports: Vec<String>,
}

/// 从语法树构建单文件符号图。
///
/// `query` 必须是语言配置编译后的 definitions query；`src` 为文件字节。
pub fn build_scope_graph(
    query: &Query,
    root_node: tree_sitter::Node<'_>,
    src: &[u8],
    config: &LanguageConfig,
) -> ExtractedFile {
    let mut scope_graph = ScopeGraph::new(
        Range::for_tree_node(&root_node),
        config.primary_language_id().to_string(),
    );

    let mut cursor = QueryCursor::new();

    // 收集 capture：def 记录「名字节点 + 声明体范围 + 类型」（containment
    // 推导需要声明体范围，而非名字标识符的单 token 范围）。
    let mut def_captures: Vec<(tree_sitter::Node<'_>, Range, Option<SymbolId>, String)> =
        Vec::new();
    let mut ref_captures: Vec<(Range, Option<SymbolId>)> = Vec::new();
    let mut alias_pairs: Vec<(String, String)> = Vec::new();
    let mut exports: Vec<String> = Vec::new();

    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, root_node, src);
    while let Some(match_) = matches.next() {
        let mut alias_original: Option<String> = None;
        let mut alias_name: Option<String> = None;
        // 当前 match 的 def 声明节点（`@definition.{sym}` capture）。
        let mut def_range: Option<Range> = None;
        let mut name_capture: Option<(tree_sitter::Node<'_>, Range, Option<SymbolId>, String)> =
            None;

        for capture in match_.captures {
            let range = Range::for_tree_node(&capture.node);
            let capture_name = &capture_names[capture.index as usize];
            let text = String::from_utf8_lossy(&src[capture.node.byte_range()]).to_string();
            let parts: Vec<&str> = capture_name.split('.').collect();

            match parts.as_slice() {
                ["name", "definition", sym] => {
                    name_capture = Some((
                        capture.node,
                        range,
                        config.symbol_id_of(sym),
                        sym.to_string(),
                    ));
                }
                ["definition", sym] => {
                    let _ = sym;
                    def_range = Some(range);
                }
                ["name", "reference", sym] => {
                    let symbol_id = config.symbol_id_of(sym);
                    if *sym == "export" {
                        exports.push(text.clone());
                    }
                    ref_captures.push((range, symbol_id));
                }
                ["alias", "original"] => alias_original = Some(text),
                ["alias", "name"] => alias_name = Some(text),
                _ => {}
            }
        }

        if let (Some(original), Some(alias)) = (alias_original, alias_name) {
            alias_pairs.push((alias, original));
        }
        if let Some((name_node, name_range, symbol_id, symbol_type)) = name_capture {
            // 多个 pattern 可能匹配同一声明（如 TS 的 variable 与
            // function 两个 pattern 都命中 `const x = fn()`）：去重
            // 同名同范围，保留首个（Grok 不去重，Evo 收敛为单定义）。
            if def_captures
                .iter()
                .any(|(existing, _, _, _)| existing.byte_range() == name_node.byte_range())
            {
                continue;
            }
            // 无 `@definition.{sym}` capture 的 pattern 退化为名字范围。
            let def_range = def_range.unwrap_or(name_range);
            def_captures.push((name_node, def_range, symbol_id, symbol_type));
        }
    }

    // 定义挂全局 scope（Grok fast 路径）；名字直接落节点。
    for (node, _, symbol_id, symbol_type) in &def_captures {
        let name = String::from_utf8_lossy(&src[node.byte_range()]).to_string();
        let name_range = Range::for_tree_node(node);
        let local_scope = scope_graph.find_tightest_local_scope(&name_range);
        let local_def = LocalDef::new(
            name,
            symbol_type.clone(),
            name_range,
            *symbol_id,
            local_scope,
        );
        scope_graph.insert_global_def(local_def);
    }

    // 引用无条件插入（跨文件跟踪）。
    for (range, symbol_id) in &ref_captures {
        let name = String::from_utf8_lossy(&src[range.start_byte()..range.end_byte()]).to_string();
        let reference = Reference::new(name, *range, *symbol_id);
        scope_graph.insert_ref_unconditional(reference);
    }

    // containment 推导：对每个 def 找「声明体严格包含它且跨度最小」的 def。
    for (child_idx, (_, child_def_range, _, _)) in def_captures.iter().enumerate() {
        let mut best_parent: Option<usize> = None;
        for (parent_idx, (_, parent_def_range, _, _)) in def_captures.iter().enumerate() {
            if child_idx == parent_idx {
                continue;
            }
            if parent_def_range.strictly_contains(child_def_range)
                && best_parent.is_none_or(|best| {
                    def_captures[best].1.byte_size() > parent_def_range.byte_size()
                })
            {
                best_parent = Some(parent_idx);
            }
        }
        if let Some(parent_idx) = best_parent {
            let child = def_captures[child_idx].0;
            let parent = def_captures[parent_idx].0;
            if let (Some(child_idx), Some(parent_idx)) = (
                locate_def_node(&scope_graph, &child),
                locate_def_node(&scope_graph, &parent),
            ) && child_idx != parent_idx
            {
                scope_graph.add_containment(child_idx, parent_idx);
            }
        }
    }

    ExtractedFile {
        graph: scope_graph,
        aliases: alias_pairs,
        exports,
    }
}

/// 在图中定位 def 节点：按标识符字节范围精确匹配（def 节点的
/// `NodeKind::range()` 返回其作用域范围，无法用来区分同级 def，因此
/// 必须用标识符范围定位）；精确失败退化为最紧匹配。
fn locate_def_node(
    scope_graph: &ScopeGraph,
    node: &tree_sitter::Node<'_>,
) -> Option<super::scope::NodeIndex> {
    let (start, end) = (node.byte_range().start, node.byte_range().end);
    scope_graph
        .definition_nodes()
        .into_iter()
        .find(|(_, def)| def.range.start_byte() == start && def.range.end_byte() == end)
        .map(|(idx, _)| idx)
        .or_else(|| scope_graph.tightest_node_for_range(start, end))
}
