//! Go grammar / query 配置。
//!
//! `.scm` 查询文本与 namespaces 直接移植 Grok（数据 / 契约层）。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (languages/golang.rs); query text and namespaces copied verbatim, language
// ids normalized to Evo's lowercase ids.
use crate::languages::LanguageConfig;

pub fn golang() -> LanguageConfig {
    LanguageConfig::with_grammar(
        vec!["go".to_owned()],
        vec!["go".to_owned()],
        vec![vec![
            "function".to_owned(),
            "type".to_owned(),
            "struct".to_owned(),
            "interface".to_owned(),
            "const".to_owned(),
            "var".to_owned(),
            "package".to_owned(),
        ]],
        GO_QUERY.to_owned(),
        || tree_sitter_go::LANGUAGE.into(),
    )
}

/// Go definitions query（移植 Grok）。
const GO_QUERY: &str = r#"
        ; Function definitions
        (function_declaration
            name: (identifier) @name.definition.function) @definition.function
        
        ; Method definitions
        (method_declaration
            name: (field_identifier) @name.definition.method) @definition.method
        
        ; Type definitions (struct, interface, etc.)
        (type_declaration
            (type_spec
                name: (type_identifier) @name.definition.type)) @definition.type
        
        ; Const declarations
        (const_declaration
            (const_spec
                name: (identifier) @name.definition.const)) @definition.const
        
        ; Var declarations
        (var_declaration
            (var_spec
                name: (identifier) @name.definition.var)) @definition.var
        
        ; ============ REFERENCES ============
        
        ; Function calls
        (call_expression
            function: (identifier) @name.reference.call) @reference.call
        
        ; Method calls
        (call_expression
            function: (selector_expression
                field: (field_identifier) @name.reference.call)) @reference.call
        
        ; Type references
        (type_identifier) @name.reference.type
        
        ; Package references in qualified names
        (qualified_type
            package: (package_identifier) @name.reference.package
            name: (type_identifier) @name.reference.type)
        
        ; ============ IMPORTS ============
        
        ; import "package"
        (import_spec
            path: (interpreted_string_literal) @name.reference.import)
        
        ; import alias "package"
        (import_spec
            name: (package_identifier) @alias.name
            path: (interpreted_string_literal) @alias.original)
        "#;
