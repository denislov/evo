//! `CodebaseIndex`：跨文件的符号索引。
//!
//! 移植自 Grok `ScopeGraphIndex`（graph.rs 后半）；Evo 裁剪：
//!
//! - 去掉 `StringInterner`（内存优化，Evo 首批规模不需要，见债务登记），
//!   直接用字符串键（`BTreeMap` 保证确定性排序）；
//! - 保留 reverse index（`file_to_defs` / `file_to_refs`），文件删除 /
//!   移动为 O(符号数) 而非 O(全量)；
//! - `definitions` / `references` 的值存 `(rel_path, line)`，line 为
//!   1-indexed，饱和到 u32（Grok 同款契约）；
//! - alias 映射为全局表（Grok 同款；不追踪 alias 的文件来源，删除文件
//!   后 alias 可能残留——见债务登记）；
//! - 新增文件级 `exports` 表与持久化入口（`from_persisted` /
//!   `to_persisted`）。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (scope_graph/graph.rs: ScopeGraphIndex definition/lookup/remove/rename/
// stats sections); Evo: plain String keys instead of the StringInterner,
// file-level exports, persistence entry points.
use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::persist::{FileMeta, GRAPH_SCHEMA_VERSION, GraphCacheData, PersistedFile};
use super::scope::ScopeGraph;

/// 跨文件符号索引。
#[derive(Debug, Clone, Default)]
pub struct CodebaseIndex {
    /// 相对路径（正斜杠）→ 单文件图。
    graphs: BTreeMap<String, ScopeGraph>,
    /// 相对路径 → 文件 meta（staleness 检测）。
    file_meta: BTreeMap<String, FileMeta>,
    /// 符号名 → `(rel_path, 1-indexed line)` 定义位置。
    definitions: BTreeMap<String, Vec<(String, u32)>>,
    /// 符号名 → `(rel_path, 1-indexed line)` 引用位置。
    references: BTreeMap<String, Vec<(String, u32)>>,
    /// alias → original（全局表）。
    aliases: HashMap<String, String>,
    /// original → alias 列表（反查）。
    reverse_aliases: HashMap<String, Vec<String>>,
    /// 相对路径 → 文件级导出符号。
    exports: BTreeMap<String, Vec<String>>,
    /// 文件 → 该文件含定义的符号名集合（O(符号数) 删除）。
    file_to_defs: HashMap<String, BTreeSet<String>>,
    /// 文件 → 该文件含引用的符号名集合。
    file_to_refs: HashMap<String, BTreeSet<String>>,
    /// 构建时的 query 哈希（诊断冗余；一致性由缓存 identity 层保证）。
    query_version: u64,
}

impl CodebaseIndex {
    pub fn new(query_version: u64) -> Self {
        Self {
            query_version,
            ..Self::default()
        }
    }

    pub fn query_version(&self) -> u64 {
        self.query_version
    }

    // ======================================================================
    // 文件操作
    // ======================================================================

    /// 加入 / 替换一个文件的图与符号位置。
    pub fn add_file(
        &mut self,
        rel_path: &str,
        meta: FileMeta,
        graph: ScopeGraph,
        aliases: &[(String, String)],
        exports: &[String],
    ) {
        // 替换语义：先清旧条目，避免残留。
        self.remove_file(rel_path);
        self.graphs.insert(rel_path.to_string(), graph);
        self.file_meta.insert(rel_path.to_string(), meta);
        if !exports.is_empty() {
            self.exports.insert(rel_path.to_string(), exports.to_vec());
        }
        let path_id = rel_path.to_string();

        let graph = &self.graphs[rel_path];
        for def in graph.definitions() {
            let line = def.range.start_line_1indexed().min(u32::MAX as usize) as u32;
            self.definitions
                .entry(def.name.clone())
                .or_default()
                .push((path_id.clone(), line));
            self.file_to_defs
                .entry(path_id.clone())
                .or_default()
                .insert(def.name.clone());
        }
        for reference in graph.references() {
            let line = reference.range.start_line_1indexed().min(u32::MAX as usize) as u32;
            self.references
                .entry(reference.name.clone())
                .or_default()
                .push((path_id.clone(), line));
            self.file_to_refs
                .entry(path_id.clone())
                .or_default()
                .insert(reference.name.clone());
        }
        for (alias, original) in aliases {
            self.add_alias(alias, original);
        }
    }

    /// 移除一个文件（O(符号数)，借助 reverse index）。
    pub fn remove_file(&mut self, rel_path: &str) {
        self.graphs.remove(rel_path);
        self.file_meta.remove(rel_path);
        self.exports.remove(rel_path);

        if let Some(symbol_ids) = self.file_to_defs.remove(rel_path) {
            for symbol in symbol_ids {
                if let Some(locs) = self.definitions.get_mut(&symbol) {
                    locs.retain(|(path, _)| path != rel_path);
                    if locs.is_empty() {
                        self.definitions.remove(&symbol);
                    }
                }
            }
        }
        if let Some(symbol_ids) = self.file_to_refs.remove(rel_path) {
            for symbol in symbol_ids {
                if let Some(locs) = self.references.get_mut(&symbol) {
                    locs.retain(|(path, _)| path != rel_path);
                    if locs.is_empty() {
                        self.references.remove(&symbol);
                    }
                }
            }
        }
    }

    /// 重命名一个文件（只改路径不重解析；内容可能变化时调用方应随后
    /// reindex 目标路径）。
    pub fn rename_file(&mut self, from: &str, to: &str) {
        if from == to {
            return;
        }
        let Some(graph) = self.graphs.remove(from) else {
            return;
        };
        self.graphs.insert(to.to_string(), graph);
        if let Some(meta) = self.file_meta.remove(from) {
            self.file_meta.insert(to.to_string(), meta);
        }
        if let Some(exported) = self.exports.remove(from) {
            self.exports.insert(to.to_string(), exported);
        }
        if let Some(symbol_ids) = self.file_to_defs.remove(from) {
            for symbol in &symbol_ids {
                if let Some(locs) = self.definitions.get_mut(symbol) {
                    for (path, _) in locs.iter_mut() {
                        if path == from {
                            *path = to.to_string();
                        }
                    }
                }
            }
            self.file_to_defs.insert(to.to_string(), symbol_ids);
        }
        if let Some(symbol_ids) = self.file_to_refs.remove(from) {
            for symbol in &symbol_ids {
                if let Some(locs) = self.references.get_mut(symbol) {
                    for (path, _) in locs.iter_mut() {
                        if path == from {
                            *path = to.to_string();
                        }
                    }
                }
            }
            self.file_to_refs.insert(to.to_string(), symbol_ids);
        }
    }

    pub fn is_indexed(&self, rel_path: &str) -> bool {
        self.graphs.contains_key(rel_path)
    }

    pub fn get_graph(&self, rel_path: &str) -> Option<&ScopeGraph> {
        self.graphs.get(rel_path)
    }

    pub fn file_meta(&self, rel_path: &str) -> Option<&FileMeta> {
        self.file_meta.get(rel_path)
    }

    pub fn exports_of(&self, rel_path: &str) -> &[String] {
        self.exports
            .get(rel_path)
            .map(|exports| exports.as_slice())
            .unwrap_or(&[])
    }

    /// 全部已索引路径（排序）。
    pub fn paths(&self) -> Vec<&str> {
        self.graphs.keys().map(|path| path.as_str()).collect()
    }

    pub fn file_count(&self) -> usize {
        self.graphs.len()
    }

    /// 统计：`(files, definitions, references)`。
    pub fn stats(&self) -> (usize, usize, usize) {
        (
            self.file_meta.len(),
            self.definitions.values().map(Vec::len).sum(),
            self.references.values().map(Vec::len).sum(),
        )
    }

    pub fn has_definition(&self, symbol: &str) -> bool {
        self.definitions.contains_key(symbol)
    }

    // ======================================================================
    // Alias 操作
    // ======================================================================

    /// 登记 alias 关系：`alias` 是 `original` 的别名。
    pub fn add_alias(&mut self, alias: &str, original: &str) {
        if alias == original {
            return;
        }
        self.aliases.insert(alias.to_string(), original.to_string());
        let reverse = self
            .reverse_aliases
            .entry(original.to_string())
            .or_default();
        if !reverse.iter().any(|existing| existing == alias) {
            reverse.push(alias.to_string());
        }
    }

    pub fn alias_count(&self) -> usize {
        self.aliases.len()
    }

    // ======================================================================
    // 查询
    // ======================================================================

    /// 符号的定义位置（直接 + alias 解析到 original）。
    pub fn find_definitions(&self, symbol: &str) -> Vec<(String, u32)> {
        let mut results = Vec::new();
        let mut seen = BTreeSet::new();
        let push = |results: &mut Vec<_>, symbol: &str, seen: &mut BTreeSet<_>| {
            if let Some(locs) = self.definitions.get(symbol) {
                for (path, line) in locs {
                    if seen.insert((path.clone(), *line)) {
                        results.push((path.clone(), *line));
                    }
                }
            }
        };
        push(&mut results, symbol, &mut seen);
        // symbol 是 alias：并入 original 的定义。
        if let Some(original) = self.aliases.get(symbol) {
            push(&mut results, original, &mut seen);
        }
        results
    }

    /// 符号的引用位置（直接 + alias original + 所有别名的引用）。
    pub fn find_references(&self, symbol: &str) -> Vec<(String, u32)> {
        let mut results = Vec::new();
        let mut seen = BTreeSet::new();
        let push = |results: &mut Vec<_>, symbol: &str, seen: &mut BTreeSet<_>| {
            if let Some(locs) = self.references.get(symbol) {
                for (path, line) in locs {
                    if seen.insert((path.clone(), *line)) {
                        results.push((path.clone(), *line));
                    }
                }
            }
        };
        push(&mut results, symbol, &mut seen);
        if let Some(original) = self.aliases.get(symbol) {
            push(&mut results, original, &mut seen);
        }
        if let Some(aliases) = self.reverse_aliases.get(symbol) {
            for alias in aliases {
                push(&mut results, alias, &mut seen);
            }
        }
        results
    }

    // ======================================================================
    // 持久化
    // ======================================================================

    pub fn to_persisted(&self) -> GraphCacheData {
        let files = self
            .graphs
            .iter()
            .filter_map(|(rel_path, graph)| {
                let meta = self.file_meta.get(rel_path)?;
                Some(PersistedFile {
                    rel_path: rel_path.clone(),
                    meta: *meta,
                    graph: graph.to_persisted(),
                    exports: self.exports_of(rel_path).to_vec(),
                    aliases: Vec::new(), // 全局 alias 表单独序列化。
                })
            })
            .collect();
        GraphCacheData {
            schema_version: GRAPH_SCHEMA_VERSION,
            query_version: self.query_version,
            files,
            aliases: self
                .aliases
                .iter()
                .map(|(alias, original)| (alias.clone(), original.clone()))
                .collect(),
        }
    }

    /// 从持久化载荷重建（schema 版本不匹配返回 `None`，调用方重建）。
    pub fn from_persisted(data: &GraphCacheData) -> Option<Self> {
        if data.schema_version != GRAPH_SCHEMA_VERSION {
            return None;
        }
        let mut index = Self::new(data.query_version);
        for file in &data.files {
            let graph = ScopeGraph::from_persisted(infer_lang(&file.rel_path), &file.graph);
            index.add_file(
                &file.rel_path,
                file.meta,
                graph,
                &file.aliases,
                &file.exports,
            );
        }
        for (alias, original) in &data.aliases {
            index.add_alias(alias, original);
        }
        Some(index)
    }
}

/// 从路径扩展名推断语言主 id（重建图的诊断标签用）。
fn infer_lang(rel_path: &str) -> String {
    let extension = rel_path.rsplit('.').next().unwrap_or("");
    match extension {
        "rs" => "rust",
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" | "pyi" => "python",
        "go" => "go",
        _ => "unknown",
    }
    .to_string()
}
