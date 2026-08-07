//! 查询 / 导航 API：go-to-definition、go-to-references、文件符号树
//! （containment）、位置符号解析。
//!
//! 移植自 Grok `navigation.rs`（`Navigator` / `NavigationResult` /
//! `Location` 语义：1-indexed 行 / 列）；Evo 扩展：
//!
//! - `file_symbols`：从 per-file 图回答符号树（definitions + containment
//!   边），不读文件、不重解析；
//! - `Definition` / `Reference` 请求支持「按符号名」与「按位置」两种
//!   上下文；位置解析（`get_symbol_at_position`）与 Grok 相同——现读
//!   文件 + tree-sitter 解析 + 取最紧 identifier-like 节点。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (navigation.rs: Navigator / find_smallest_named_node_at_point /
// is_identifier_like / Location semantics); Evo extension: file_symbols
// containment tree, name-based queries, relative paths.
use std::path::Path;

use serde::{Deserialize, Serialize};
use tool_contract::api::ranking::ResultRanker;

use crate::languages::LanguageRegistry;

use super::index::CodebaseIndex;

/// 符号搜索候选收集上限（有界查询：防超大型符号集的无界遍历）。
pub const MAX_SYMBOL_SEARCH_CANDIDATES: usize = 4_096;

/// 查询结果中的一个位置（1-indexed 行 / 列）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// 相对 workspace root 的路径（正斜杠分隔）。
    pub path: String,
    /// 1-indexed 行号。
    pub line: usize,
    /// 1-indexed 列号。
    pub column: usize,
}

/// 导航查询结果：命中的符号名 + 位置列表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavigationResult {
    pub symbol: String,
    pub locations: Vec<Location>,
}

/// 文件符号树中的一个节点（containment 子节点内嵌）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSymbol {
    pub name: String,
    /// 符号类型（`function` / `class` / `struct` / `variable` …）。
    pub symbol_type: String,
    /// 1-indexed 起始行。
    pub line: usize,
    /// 1-indexed 起始列。
    pub column: usize,
    /// containment 子符号（按位置排序）。
    pub children: Vec<FileSymbol>,
}

/// 查询错误（映射为 `CodeIntelligenceError::GraphQuery`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphQueryError {
    FileNotFound(String),
    PositionOutOfBounds { row: usize, col: usize },
    NoSymbolAtPosition { row: usize, col: usize },
    UnsupportedLanguage(String),
    ParseError(String),
    FileNotIndexed(String),
}

impl std::fmt::Display for GraphQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileNotFound(path) => write!(f, "file not found: {path}"),
            Self::PositionOutOfBounds { row, col } => {
                write!(f, "position out of bounds: {row}:{col}")
            }
            Self::NoSymbolAtPosition { row, col } => {
                write!(f, "no symbol found at position {row}:{col}")
            }
            Self::UnsupportedLanguage(ext) => write!(f, "unsupported language: {ext}"),
            Self::ParseError(message) => write!(f, "parse error: {message}"),
            Self::FileNotIndexed(path) => write!(f, "file not indexed: {path}"),
        }
    }
}

/// 符号搜索结果条目（一个定义位置）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolHit {
    pub name: String,
    /// 符号类型（`function` / `class` / `struct` / `variable` …）。
    pub symbol_type: String,
    /// 相对 workspace root 的路径（正斜杠分隔）。
    pub path: String,
    /// 1-indexed 定义行号。
    pub line: usize,
    /// 该符号的全局引用数（含 alias 解析）。
    pub references: usize,
}

/// 有界符号搜索结果：相关度排序 + 显式截断标记（ARC-830）。
///
/// `total` 是匹配候选总数（受 [`MAX_SYMBOL_SEARCH_CANDIDATES`] 收集上限
/// 约束，命中上限时是低估）；`truncated` 表示结果被丢弃过——候选收集
/// 截断或 `limit` 截断任一发生即为 `true`，调用方必须显式展示该标记，
/// 不得静默截断。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedSymbolSearch {
    pub query: String,
    /// 匹配候选总数（受收集上限约束）。
    pub total: usize,
    /// 结果被截断（收集上限或 `limit` 截断）。
    pub truncated: bool,
    /// 相关度降序的定义位置（`limit` 截断后）。
    pub results: Vec<SymbolHit>,
}

/// 按名称片段搜索符号（大小写不敏感子串匹配），相关度降序（共用
/// `tool-contract` 的排序接口：精确名 / 整词 / 前缀 / 子串逐级降权），
/// `limit` 截断（`0` = 不限）。
pub fn search_symbols(index: &CodebaseIndex, query: &str, limit: usize) -> BoundedSymbolSearch {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return BoundedSymbolSearch {
            query: query.to_string(),
            total: 0,
            truncated: false,
            results: Vec::new(),
        };
    }
    let mut candidates: Vec<SymbolHit> = Vec::new();
    let mut capped = false;
    'outer: for path in index.paths() {
        let Some(graph) = index.get_graph(path) else {
            continue;
        };
        for (_, def) in graph.definition_nodes() {
            if !def.name.to_lowercase().contains(&needle) {
                continue;
            }
            candidates.push(SymbolHit {
                name: def.name.clone(),
                symbol_type: def.symbol_type.clone(),
                path: path.to_string(),
                line: def.range.start_line_1indexed(),
                references: 0,
            });
            if candidates.len() >= MAX_SYMBOL_SEARCH_CANDIDATES {
                capped = true;
                break 'outer;
            }
        }
    }
    let total = candidates.len();
    let ranker = tool_contract::api::ranking::DefaultResultRanker::new();
    // 相关度文本只取符号名：精确名 > 整词命中 > 前缀/子串；同分保持
    // 输入顺序（路径按 BTreeMap 排序，稳定排序保持确定性）。
    let mut ranked = ranker.rank(&needle, candidates, |hit| hit.name.clone(), limit);
    for result in &mut ranked {
        result.item.references = index.find_references(&result.item.name).len();
    }
    BoundedSymbolSearch {
        query: query.to_string(),
        total,
        truncated: capped || (limit != 0 && total > limit),
        results: ranked.into_iter().map(|result| result.item).collect(),
    }
}

/// 基于索引的查询入口。所有路径均为 workspace-relative。
pub struct GraphNavigator<'a> {
    pub index: &'a CodebaseIndex,
    pub registry: &'a LanguageRegistry,
    pub root: &'a Path,
}

impl<'a> GraphNavigator<'a> {
    pub fn new(index: &'a CodebaseIndex, registry: &'a LanguageRegistry, root: &'a Path) -> Self {
        Self {
            index,
            registry,
            root,
        }
    }

    fn absolute(&self, rel: &str) -> std::path::PathBuf {
        self.root
            .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR))
    }

    /// 文件符号树（containment）：未索引文件返回 `FileNotIndexed`。
    pub fn file_symbols(&self, rel_path: &str) -> Result<Vec<FileSymbol>, GraphQueryError> {
        let graph = self
            .index
            .get_graph(rel_path)
            .ok_or_else(|| GraphQueryError::FileNotIndexed(rel_path.to_string()))?;
        let defs = graph.definition_nodes();
        // containment 边：def 序号（提取顺序）→ 父 def 序号。
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); defs.len()];
        let mut has_parent = vec![false; defs.len()];
        let ordinal =
            |idx: super::scope::NodeIndex| defs.iter().position(|(node_idx, _)| *node_idx == idx);
        for (child, parent) in graph.containment() {
            if let (Some(child_ordinal), Some(parent_ordinal)) = (ordinal(*child), ordinal(*parent))
                && child_ordinal != parent_ordinal
            {
                children[parent_ordinal].push(child_ordinal);
                has_parent[child_ordinal] = true;
            }
        }
        let mut roots: Vec<FileSymbol> = (0..defs.len())
            .filter(|ordinal| !has_parent[*ordinal])
            .map(|ordinal| build_symbol_tree(&defs, &children, ordinal))
            .collect();
        roots.sort_by_key(|symbol| (symbol.line, symbol.column));
        Ok(roots)
    }

    /// 位置处的符号名（1-indexed row / col）。
    pub fn get_symbol_at_position(
        &self,
        rel_path: &str,
        row: usize,
        col: usize,
    ) -> Result<String, GraphQueryError> {
        if row == 0 || col == 0 {
            return Err(GraphQueryError::PositionOutOfBounds { row, col });
        }
        let absolute = self.absolute(rel_path);
        let content = std::fs::read(&absolute)
            .map_err(|_| GraphQueryError::FileNotFound(rel_path.to_string()))?;
        let lang_config = self.registry.for_file_path(&absolute).ok_or_else(|| {
            GraphQueryError::UnsupportedLanguage(
                absolute
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
            )
        })?;
        let Some(language) = lang_config.language() else {
            return Err(GraphQueryError::UnsupportedLanguage(
                absolute
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
            ));
        };
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .map_err(|error| GraphQueryError::ParseError(format!("language error: {error}")))?;
        let tree = parser
            .parse(&content, None)
            .ok_or_else(|| GraphQueryError::ParseError("failed to parse file".into()))?;
        let point = tree_sitter::Point::new(row - 1, col - 1);
        match find_smallest_named_node_at_point(tree.root_node(), point) {
            Some(node) => Ok(String::from_utf8_lossy(&content[node.byte_range()]).into_owned()),
            None => Err(GraphQueryError::NoSymbolAtPosition { row, col }),
        }
    }

    /// 按位置 go-to-definition。
    pub fn goto_definition(
        &self,
        rel_path: &str,
        row: usize,
        col: usize,
    ) -> Result<NavigationResult, GraphQueryError> {
        let symbol = self.get_symbol_at_position(rel_path, row, col)?;
        Ok(self.definition_by_name(&symbol, Some(rel_path)))
    }

    /// 按位置 go-to-references。
    pub fn goto_references(
        &self,
        rel_path: &str,
        row: usize,
        col: usize,
        include_definition: bool,
    ) -> Result<NavigationResult, GraphQueryError> {
        let symbol = self.get_symbol_at_position(rel_path, row, col)?;
        Ok(self.references_by_name(&symbol, include_definition, Some(rel_path)))
    }

    /// 按符号名查定义（alias 解析；`context_file` 给定时同语言优先排序，
    /// Grok `find_definitions_smart` 语义）。
    pub fn definition_by_name(&self, symbol: &str, context_file: Option<&str>) -> NavigationResult {
        let mut locations: Vec<Location> = self
            .index
            .find_definitions(symbol)
            .into_iter()
            .map(|(path, line)| Location {
                path,
                line: line as usize,
                column: 1,
            })
            .collect();
        sort_locations_smart(&mut locations, context_file, self.registry);
        NavigationResult {
            symbol: symbol.to_string(),
            locations,
        }
    }

    /// 按符号名查引用（alias 解析；可选并入定义位置）。
    pub fn references_by_name(
        &self,
        symbol: &str,
        include_definition: bool,
        context_file: Option<&str>,
    ) -> NavigationResult {
        let mut locations: Vec<Location> = self
            .index
            .find_references(symbol)
            .into_iter()
            .map(|(path, line)| Location {
                path,
                line: line as usize,
                column: 1,
            })
            .collect();
        sort_locations_smart(&mut locations, context_file, self.registry);
        if include_definition {
            let definitions = self.definition_by_name(symbol, context_file);
            let mut merged = definitions.locations;
            for location in locations {
                if !merged.contains(&location) {
                    merged.push(location);
                }
            }
            sort_locations_smart(&mut merged, context_file, self.registry);
            locations = merged;
        }
        NavigationResult {
            symbol: symbol.to_string(),
            locations,
        }
    }
}

/// 排序：与 `context_file` 同语言的路径优先，其次按路径 + 行。
fn sort_locations_smart(
    locations: &mut [Location],
    context_file: Option<&str>,
    registry: &LanguageRegistry,
) {
    let context_ext = context_file
        .and_then(|path| path.rsplit('.').next())
        .map(|ext| ext.to_string());
    locations.sort_by(|a, b| {
        let a_ext = a.path.rsplit('.').next().unwrap_or("").to_string();
        let b_ext = b.path.rsplit('.').next().unwrap_or("").to_string();
        let a_same = context_ext
            .as_ref()
            .is_some_and(|context| registry.extensions_same_language(context, &a_ext));
        let b_same = context_ext
            .as_ref()
            .is_some_and(|context| registry.extensions_same_language(context, &b_ext));
        match (a_same, b_same) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.path.cmp(&b.path).then(a.line.cmp(&b.line)),
        }
    });
}

/// 递归构建符号树（containment 子树按位置排序）。
fn build_symbol_tree(
    defs: &[(super::scope::NodeIndex, &crate::graph::nodes::LocalDef)],
    children: &[Vec<usize>],
    ordinal: usize,
) -> FileSymbol {
    let (_, def) = defs[ordinal];
    let mut sorted_children: Vec<FileSymbol> = children[ordinal]
        .iter()
        .map(|child| build_symbol_tree(defs, children, *child))
        .collect();
    sorted_children.sort_by_key(|symbol| (symbol.line, symbol.column));
    FileSymbol {
        name: def.name.clone(),
        symbol_type: def.symbol_type.clone(),
        line: def.range.start_line_1indexed(),
        column: def.range.start_column_1indexed(),
        children: sorted_children,
    }
}

/// 找到包含给定点的最小 named 节点（优先 identifier-like）。
fn find_smallest_named_node_at_point(
    node: tree_sitter::Node<'_>,
    point: tree_sitter::Point,
) -> Option<tree_sitter::Node<'_>> {
    if point < node.start_position() || point > node.end_position() {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_smallest_named_node_at_point(child, point) {
            if is_identifier_like(&found) {
                return Some(found);
            }
            if is_identifier_like(&child) {
                return Some(child);
            }
            return Some(found);
        }
    }
    if is_identifier_like(&node) {
        Some(node)
    } else {
        None
    }
}

/// 节点是否像标识符（各语言 identifier 类节点名）。
fn is_identifier_like(node: &tree_sitter::Node<'_>) -> bool {
    matches!(
        node.kind(),
        "identifier"
            | "type_identifier"
            | "property_identifier"
            | "field_identifier"
            | "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern"
            | "attribute" // Python
            | "package_identifier" // Go
    )
}
