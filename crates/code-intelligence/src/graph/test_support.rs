//! 图模块测试共享辅助：fixture 解析、临时 workspace 构建、身份构造。

use std::path::Path;

use workspace_runtime::api::{WorkspaceId, WorkspaceKind};

use crate::identity::{CacheIdentity, ParserVersion, RevisionId};
use crate::languages::{LanguageConfig, LanguageRegistry};

use super::extract::{ExtractedFile, build_scope_graph};

/// 解析一段源码为提取结果（语法错误时返回 `None`）。
pub fn extract(config: &LanguageConfig, src: &str) -> Option<ExtractedFile> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&config.language()?).ok()?;
    let tree = parser.parse(src.as_bytes(), None)?;
    let query = config.compile_query()?;
    Some(build_scope_graph(
        &query,
        tree.root_node(),
        src.as_bytes(),
        config,
    ))
}

/// 写入一组 `(相对路径, 内容)` 文件到临时 workspace。
pub fn write_workspace(root: &Path, files: &[(&str, &str)]) {
    for (rel, content) in files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }
}

/// 内建语言注册表。
pub fn builtin() -> LanguageRegistry {
    LanguageRegistry::builtin()
}

/// 测试身份（workspace / revision / parser-version）。
pub fn test_identity(parser_version: u64) -> CacheIdentity {
    CacheIdentity {
        workspace: WorkspaceId::user_supplied(WorkspaceKind::Source, "graph-test").unwrap(),
        revision: RevisionId::parse("rev-1").unwrap(),
        parser_version: ParserVersion::Version(parser_version),
    }
}

/// 定义的 `(名字, 类型)` 集合（提取顺序）。
pub fn def_pairs(extracted: &ExtractedFile) -> Vec<(String, String)> {
    extracted
        .graph
        .definitions()
        .iter()
        .map(|def| (def.name.clone(), def.symbol_type.clone()))
        .collect()
}

/// 引用的名字集合。
pub fn ref_names(extracted: &ExtractedFile) -> Vec<String> {
    extracted
        .graph
        .references()
        .iter()
        .map(|reference| reference.name.clone())
        .collect()
}

/// containment 边映射为 `(child, parent)` 名字对。
pub fn containment_names(extracted: &ExtractedFile) -> Vec<(String, String)> {
    let defs = extracted.graph.definition_nodes();
    extracted
        .graph
        .containment()
        .iter()
        .filter_map(|(child, parent)| {
            let child_name = defs
                .iter()
                .find(|(idx, _)| idx == child)
                .map(|(_, def)| def.name.clone())?;
            let parent_name = defs
                .iter()
                .find(|(idx, _)| idx == parent)
                .map(|(_, def)| def.name.clone())?;
            Some((child_name, parent_name))
        })
        .collect()
}

/// 断言定义存在（按名字）。
pub fn assert_has_def(extracted: &ExtractedFile, name: &str, symbol_type: &str) {
    let found = extracted
        .graph
        .definitions()
        .into_iter()
        .find(|def| def.name == name);
    assert!(
        found.is_some(),
        "missing definition {name} (type {symbol_type}); defs: {:?}",
        def_pairs(extracted)
    );
    assert_eq!(
        found.unwrap().symbol_type,
        symbol_type,
        "definition {name} has wrong type"
    );
}

/// 断言引用存在。
pub fn assert_has_ref(extracted: &ExtractedFile, name: &str) {
    assert!(
        ref_names(extracted).contains(&name.to_string()),
        "missing reference {name}; refs: {:?}",
        ref_names(extracted)
    );
}
