//! 语言注册表：language id ↔ 文件扩展名映射、tree-sitter grammar / 查询
//! 配置与 `query_hash()`。
//!
//! ARC-810 落地：`LanguageConfig` 追加 `namespaces` / `file_definition_queries`
//! / `grammar`（参考 Grok `TSLanguageConfig`）；`query_hash()` 切换为按
//! Grok `compute_query_hash` 方式哈希 primary id + query 文本；grammar 配置
//! 在 `languages/{rust,typescript,javascript,python,golang}.rs`。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (languages/mod.rs: registry lookup structure + compute_query_hash;
// languages/types.rs: TSLanguageConfig shape); extended with grammar/query
// fields for ARC-810.
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;

/// grammar 函数：返回 tree-sitter 语言。
pub type GrammarFn = fn() -> tree_sitter::Language;

/// 单个语言的配置：id / 扩展名 + ARC-810 的 namespaces / query / grammar。
#[derive(Debug, Clone)]
pub struct LanguageConfig {
    language_ids: Vec<String>,
    file_extensions: Vec<String>,
    /// 符号类型 namespace（`name.definition.{sym}` capture 的合法值集合，
    /// 供 `symbol_id_of` 解析）。
    namespaces: Vec<Vec<String>>,
    /// 文件级 definitions query（`.scm` 文本，Grok 各语言查询直接移植）。
    file_definition_queries: String,
    /// tree-sitter grammar；`None` = 只做映射不可解析（骨架形态）。
    grammar: Option<GrammarFn>,
}

impl LanguageConfig {
    /// 骨架形态：只有 id / 扩展名（无 grammar / query）。
    pub fn new(language_ids: Vec<String>, file_extensions: Vec<String>) -> Self {
        Self {
            language_ids,
            file_extensions,
            namespaces: Vec::new(),
            file_definition_queries: String::new(),
            grammar: None,
        }
    }

    /// 完整形态：带 namespaces / query / grammar。
    pub fn with_grammar(
        language_ids: Vec<String>,
        file_extensions: Vec<String>,
        namespaces: Vec<Vec<String>>,
        file_definition_queries: String,
        grammar: GrammarFn,
    ) -> Self {
        Self {
            language_ids,
            file_extensions,
            namespaces,
            file_definition_queries,
            grammar: Some(grammar),
        }
    }

    /// 该语言的全部 language id（如 typescript 的 `typescript` / `ts`）。
    pub fn language_ids(&self) -> &[String] {
        &self.language_ids
    }

    /// 主 language id（排序 / 哈希锚点）。
    pub fn primary_language_id(&self) -> &str {
        self.language_ids
            .first()
            .map(|s| s.as_str())
            .unwrap_or("unknown")
    }

    /// 该语言的全部文件扩展名（不含点）。
    pub fn file_extensions(&self) -> &[String] {
        &self.file_extensions
    }

    /// 符号类型 namespace。
    pub fn namespaces(&self) -> &[Vec<String>] {
        &self.namespaces
    }

    /// 文件级 definitions query 文本。
    pub fn file_definition_queries(&self) -> &str {
        &self.file_definition_queries
    }

    /// tree-sitter 语言；无 grammar 时返回 `None`。
    pub fn language(&self) -> Option<tree_sitter::Language> {
        self.grammar.map(|grammar| grammar())
    }

    /// 编译 definitions query；无 grammar / 编译失败返回 `None`。
    pub fn compile_query(&self) -> Option<tree_sitter::Query> {
        let language = self.language()?;
        if self.file_definition_queries.is_empty() {
            return None;
        }
        tree_sitter::Query::new(&language, &self.file_definition_queries).ok()
    }

    /// 按符号类型名查 `SymbolId`（namespace 内第一个匹配）。
    pub fn symbol_id_of(&self, symbol_type: &str) -> Option<crate::graph::nodes::SymbolId> {
        for (ns_idx, namespace) in self.namespaces.iter().enumerate() {
            for (sym_idx, sym) in namespace.iter().enumerate() {
                if sym == symbol_type {
                    return Some(crate::graph::nodes::SymbolId::new(ns_idx, sym_idx));
                }
            }
        }
        None
    }
}

/// 语言注册表：提供按扩展名 / language id / 文件路径的查询。
#[derive(Debug, Clone)]
pub struct LanguageRegistry {
    pub(crate) configs: Vec<Arc<LanguageConfig>>,
    pub(crate) by_extension: HashMap<String, Arc<LanguageConfig>>,
    pub(crate) by_id: HashMap<String, Arc<LanguageConfig>>,
}

impl LanguageRegistry {
    /// 内建注册表：ARC-810 首批语言（Rust / TypeScript / JavaScript /
    /// Python / Go），grammar / query / namespaces 见
    /// `languages/{rust,typescript,javascript,python,golang}.rs`。
    pub fn builtin() -> Self {
        let configs: Vec<Arc<LanguageConfig>> = vec![
            Arc::new(rust::rust_lang()),
            Arc::new(typescript::ts_lang()),
            Arc::new(javascript::js_lang()),
            Arc::new(python::python_lang()),
            Arc::new(golang::golang()),
        ];

        let mut by_extension = HashMap::new();
        let mut by_id = HashMap::new();
        for config in &configs {
            for extension in config.file_extensions() {
                by_extension.insert(extension.clone(), Arc::clone(config));
            }
            for id in config.language_ids() {
                by_id.insert(id.clone(), Arc::clone(config));
            }
        }
        Self {
            configs,
            by_extension,
            by_id,
        }
    }

    /// 按文件扩展名（不含点）查询语言配置。
    pub fn for_extension(&self, extension: &str) -> Option<Arc<LanguageConfig>> {
        self.by_extension.get(extension).cloned()
    }

    /// 按 language id 查询语言配置。
    pub fn for_id(&self, id: &str) -> Option<Arc<LanguageConfig>> {
        self.by_id.get(id).cloned()
    }

    /// 按文件路径查询语言配置（取路径的扩展名）。
    pub fn for_file_path(&self, path: impl AsRef<Path>) -> Option<Arc<LanguageConfig>> {
        let extension = path.as_ref().extension()?.to_str()?;
        self.for_extension(extension)
    }

    /// 文件路径是否受支持（扩展名已注册）。
    pub fn is_supported(&self, path: impl AsRef<Path>) -> bool {
        self.for_file_path(path).is_some()
    }

    /// 全部受支持的文件扩展名。
    pub fn supported_extensions(&self) -> Vec<&str> {
        self.by_extension.keys().map(|s| s.as_str()).collect()
    }

    /// 全部语言配置。
    pub fn all_configs(&self) -> &[Arc<LanguageConfig>] {
        &self.configs
    }

    /// 两个扩展名是否属于同一语言（同为已注册扩展且主 id 一致）。
    pub fn extensions_same_language(&self, first: &str, second: &str) -> bool {
        if first == second {
            return true;
        }
        match (self.by_extension.get(first), self.by_extension.get(second)) {
            (Some(first), Some(second)) => {
                first.primary_language_id() == second.primary_language_id()
            }
            _ => false,
        }
    }

    /// 解析器（grammar / query）集合的确定性哈希，供
    /// [`crate::identity::ParserVersion::Version`] 使用。
    ///
    /// 按 Grok `compute_query_hash` 的方式：按 primary id 排序后哈希
    /// primary id + query 文本（grammar 变化必然伴随 query 变化，因此
    /// 查询文本足以代表解析器版本）。
    pub fn query_hash(&self) -> u64 {
        let mut sorted: Vec<&Arc<LanguageConfig>> = self.configs.iter().collect();
        sorted.sort_by(|a, b| a.primary_language_id().cmp(b.primary_language_id()));
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for config in sorted {
            config.primary_language_id().hash(&mut hasher);
            config.file_definition_queries().hash(&mut hasher);
        }
        hasher.finish()
    }
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

mod golang;
mod javascript;
mod python;
mod rust;
mod typescript;

impl LanguageRegistry {
    /// 依据 `configs` 重建 id / 扩展名索引（测试与后续 ARC 注册自定义语言
    /// 时使用）。
    #[cfg(test)]
    pub(crate) fn rebuild_index(&mut self) {
        let mut by_extension = HashMap::new();
        let mut by_id = HashMap::new();
        for config in &self.configs {
            for extension in config.file_extensions() {
                by_extension.insert(extension.clone(), Arc::clone(config));
            }
            for id in config.language_ids() {
                by_id.insert(id.clone(), Arc::clone(config));
            }
        }
        self.by_extension = by_extension;
        self.by_id = by_id;
    }
}
