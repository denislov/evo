//! Python grammar / query 配置。
//!
//! `.scm` 查询文本与 namespaces 直接移植 Grok（数据 / 契约层）。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (languages/python.rs); query text and namespaces copied verbatim, language
// ids normalized to Evo's lowercase ids.
use crate::languages::LanguageConfig;

pub fn python_lang() -> LanguageConfig {
    LanguageConfig::with_grammar(
        vec!["python".to_owned()],
        vec!["py".to_owned(), "pyi".to_owned()],
        vec![vec![
            "function".to_owned(),
            "class".to_owned(),
            "variable".to_owned(),
            "module".to_owned(),
        ]],
        PYTHON_QUERY.to_owned(),
        || tree_sitter_python::LANGUAGE.into(),
    )
}

/// Python definitions query（移植 Grok）。
const PYTHON_QUERY: &str = r#"
        ; Class definitions
        (class_definition
            name: (identifier) @name.definition.class) @definition.class
        
        ; Function definitions
        (function_definition
            name: (identifier) @name.definition.function) @definition.function
        
        ; ============ REFERENCES ============
        
        ; Function calls (direct and method calls)
        (call
            function: [
                (identifier) @name.reference.call
                (attribute
                    attribute: (identifier) @name.reference.call)
            ]) @reference.call
        "#;
