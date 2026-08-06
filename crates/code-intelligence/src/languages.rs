//! 语言注册表：language id ↔ 文件扩展名映射与查询接口。
//!
//! ARC-800 只落地 id / 扩展名映射与确定性 `query_hash()`（供
//! `ParserVersion::Version` 使用）；tree-sitter grammar 与 query 由 ARC-810
//! 填充（[`LanguageConfig`] 预留扩展点）。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (languages/mod.rs: registry lookup structure + compute_query_hash;
// languages/types.rs: TSLanguageConfig shape); rewritten for Evo skeleton
// semantics — no tree-sitter dependency, grammar/query fields deferred to ARC-810.
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;

/// 单个语言的配置。ARC-810 追加 `namespaces`、`file_definition_queries` 与
/// grammar 函数（参考 Grok `TSLanguageConfig`）。
#[derive(Debug, Clone)]
pub struct LanguageConfig {
    language_ids: Vec<String>,
    file_extensions: Vec<String>,
}

impl LanguageConfig {
    pub fn new(language_ids: Vec<String>, file_extensions: Vec<String>) -> Self {
        Self {
            language_ids,
            file_extensions,
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
    /// Python / Go），只含 id 与扩展名映射。
    pub fn builtin() -> Self {
        let configs: Vec<Arc<LanguageConfig>> = vec![
            Arc::new(LanguageConfig::new(vec!["rust".into()], vec!["rs".into()])),
            Arc::new(LanguageConfig::new(
                vec!["typescript".into(), "ts".into()],
                vec!["ts".into(), "tsx".into(), "mts".into(), "cts".into()],
            )),
            Arc::new(LanguageConfig::new(
                vec!["javascript".into(), "js".into()],
                vec!["js".into(), "jsx".into(), "mjs".into(), "cjs".into()],
            )),
            Arc::new(LanguageConfig::new(
                vec!["python".into()],
                vec!["py".into(), "pyi".into()],
            )),
            Arc::new(LanguageConfig::new(vec!["go".into()], vec!["go".into()])),
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
    /// 骨架阶段基于 language id + 扩展名计算（grammar 未落地，任何
    /// id/扩展名变化都会触发重建）；ARC-810 落地 query 后改为按 Grok
    /// `compute_query_hash` 的方式哈希 primary id + query 文本。
    pub fn query_hash(&self) -> u64 {
        let mut sorted: Vec<&Arc<LanguageConfig>> = self.configs.iter().collect();
        sorted.sort_by(|a, b| a.primary_language_id().cmp(b.primary_language_id()));
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for config in sorted {
            config.primary_language_id().hash(&mut hasher);
            for extension in config.file_extensions() {
                extension.hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

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
