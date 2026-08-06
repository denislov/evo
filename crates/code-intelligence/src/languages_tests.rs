//! `LanguageRegistry` 的查询与哈希测试。

use crate::{LanguageConfig, LanguageRegistry};

#[test]
fn builtin_covers_arc810_launch_languages() {
    let registry = LanguageRegistry::builtin();
    let ids: Vec<&str> = registry
        .all_configs()
        .iter()
        .map(|config| config.primary_language_id())
        .collect();
    for expected in ["rust", "typescript", "javascript", "python", "go"] {
        assert!(ids.contains(&expected), "missing {expected}");
    }
    assert_eq!(registry.all_configs().len(), 5);
}

#[test]
fn lookup_by_id_extension_and_path() {
    let registry = LanguageRegistry::default();
    assert_eq!(
        registry.for_id("rust").unwrap().primary_language_id(),
        "rust"
    );
    assert_eq!(
        registry.for_extension("rs").unwrap().primary_language_id(),
        "rust"
    );
    assert_eq!(
        registry
            .for_file_path("/src/main.rs")
            .unwrap()
            .primary_language_id(),
        "rust"
    );
    assert!(registry.for_id("cobol").is_none());
    assert!(registry.for_extension("txt").is_none());
    assert!(registry.for_file_path("README").is_none());
}

#[test]
fn language_ids_include_aliases() {
    let registry = LanguageRegistry::builtin();
    assert_eq!(
        registry.for_id("ts").unwrap().primary_language_id(),
        "typescript"
    );
    assert_eq!(
        registry.for_id("js").unwrap().primary_language_id(),
        "javascript"
    );
}

#[test]
fn is_supported_and_supported_extensions() {
    let registry = LanguageRegistry::builtin();
    assert!(registry.is_supported("a.py"));
    assert!(registry.is_supported("x/main.go"));
    assert!(!registry.is_supported("a.md"));
    let extensions = registry.supported_extensions();
    for expected in [
        "rs", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "pyi", "go",
    ] {
        assert!(
            extensions.contains(&expected),
            "missing extension {expected}"
        );
    }
}

#[test]
fn extensions_same_language_family() {
    let registry = LanguageRegistry::builtin();
    assert!(registry.extensions_same_language("ts", "tsx"));
    assert!(registry.extensions_same_language("ts", "ts"));
    assert!(registry.extensions_same_language("py", "pyi"));
    assert!(!registry.extensions_same_language("ts", "js"));
    assert!(!registry.extensions_same_language("rs", "py"));
    assert!(!registry.extensions_same_language("rs", "unknown"));
}

#[test]
fn query_hash_is_deterministic() {
    let registry = LanguageRegistry::builtin();
    assert_eq!(registry.query_hash(), registry.query_hash());
    assert_eq!(
        LanguageRegistry::builtin().query_hash(),
        registry.query_hash()
    );
}

#[test]
fn query_hash_changes_with_registry_content() {
    let empty = LanguageRegistry {
        configs: vec![],
        by_extension: std::collections::HashMap::new(),
        by_id: std::collections::HashMap::new(),
    };
    let mut partial = empty.clone();
    partial
        .configs
        .push(std::sync::Arc::new(LanguageConfig::new(
            vec!["rust".into()],
            vec!["rs".into()],
        )));
    partial.rebuild_index();
    assert_ne!(empty.query_hash(), partial.query_hash());
    assert_ne!(
        partial.query_hash(),
        LanguageRegistry::builtin().query_hash()
    );
}

#[test]
fn query_hash_is_order_independent() {
    let registry = LanguageRegistry::builtin();
    // 反转注册顺序（保持索引一致）后哈希不变。
    let mut configs = registry.configs.clone();
    configs.reverse();
    let mut reversed = LanguageRegistry {
        configs,
        by_extension: std::collections::HashMap::new(),
        by_id: std::collections::HashMap::new(),
    };
    reversed.rebuild_index();
    assert_eq!(registry.query_hash(), reversed.query_hash());
}

#[test]
fn language_config_shape() {
    let config = LanguageConfig::new(vec!["ts".into(), "typescript".into()], vec!["ts".into()]);
    assert_eq!(config.language_ids(), &["ts", "typescript"]);
    assert_eq!(config.primary_language_id(), "ts");
    assert_eq!(config.file_extensions(), &["ts"]);
    assert_eq!(
        LanguageConfig::new(vec![], vec![]).primary_language_id(),
        "unknown"
    );
}
