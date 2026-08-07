//! Rust grammar / query 配置。
//!
//! `.scm` 查询文本与 namespaces 直接移植 Grok（数据 / 契约层）。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (languages/rust.rs); query text and namespaces copied verbatim, language ids
// normalized to Evo's lowercase ids.
use crate::languages::LanguageConfig;

pub fn rust_lang() -> LanguageConfig {
    LanguageConfig::with_grammar(
        vec!["rust".to_owned()],
        vec!["rs".to_owned()],
        vec![vec![
            "const".to_owned(),
            "function".to_owned(),
            "variable".to_owned(),
            "struct".to_owned(),
            "enum".to_owned(),
            "union".to_owned(),
            "typedef".to_owned(),
            "interface".to_owned(),
            "field".to_owned(),
            "enumerator".to_owned(),
            "module".to_owned(),
            "label".to_owned(),
            "lifetime".to_owned(),
        ]],
        RUST_QUERY.to_owned(),
        || tree_sitter_rust::LANGUAGE.into(),
    )
}

/// Rust definitions query（移植 Grok；覆盖 ADT / 方法 / 函数 / trait /
/// module / macro / 常量定义 + 调用 / impl / use / 类型引用 + alias 对）。
const RUST_QUERY: &str = r#"; ADT definitions

        (struct_item
            name: (type_identifier) @name.definition.class) @definition.class
        
        (enum_item
            name: (type_identifier) @name.definition.class) @definition.class
        
        (union_item
            name: (type_identifier) @name.definition.class) @definition.class
        
        ; type aliases
        
        (type_item
            name: (type_identifier) @name.definition.class) @definition.class
        
        ; method definitions
        
        (declaration_list
            (function_item
                name: (identifier) @name.definition.method)) @definition.method
        
        ; function definitions
        
        (function_item
            name: (identifier) @name.definition.function) @definition.function
        
        ; trait definitions
        (trait_item
            name: (type_identifier) @name.definition.interface) @definition.interface
        
        ; module definitions
        (mod_item
            name: (identifier) @name.definition.module) @definition.module
        
        ; macro definitions
        
        (macro_definition
            name: (identifier) @name.definition.macro) @definition.macro
        
        ; const and static definitions
        (const_item
            name: (identifier) @name.definition.variable) @definition.variable
        
        (static_item
            name: (identifier) @name.definition.variable) @definition.variable
        
        ; ============ REFERENCES ============
        
        ; Function and method calls
        (call_expression
            function: (identifier) @name.reference.call) @reference.call
        
        (call_expression
            function: (field_expression
                field: (field_identifier) @name.reference.call)) @reference.call
        
        (macro_invocation
            macro: (identifier) @name.reference.call) @reference.call
        
        ; implementations
        
        (impl_item
            trait: (type_identifier) @name.reference.implementation) @reference.implementation
        
        (impl_item
            type: (type_identifier) @name.reference.implementation
            !trait) @reference.implementation
        
        ; ============ USE/IMPORT REFERENCES ============
        
        ; Simple use: use Foo;
        (use_declaration
            argument: (identifier) @name.reference.import) @reference.import
        
        ; Scoped use: use foo::Bar;
        (use_declaration
            argument: (scoped_identifier
                name: (identifier) @name.reference.import)) @reference.import
        
        ; Use list: use foo::{Bar, Baz};
        (use_declaration
            argument: (scoped_use_list
                list: (use_list
                    (identifier) @name.reference.import)))
        
        ; Nested scoped use: use foo::bar::{Baz, Qux};
        (use_declaration
            argument: (scoped_use_list
                list: (use_list
                    (scoped_identifier
                        name: (identifier) @name.reference.import))))
        
        ; Use with alias: use Foo as Bar;
        (use_declaration
            argument: (use_as_clause
                path: (identifier) @name.reference.import))
        
        (use_declaration
            argument: (use_as_clause
                path: (scoped_identifier
                    name: (identifier) @name.reference.import)))
        
        ; ============ ALIAS TRACKING ============
        ; These patterns capture alias relationships for unified lookups
        
        ; use Foo as Bar - captures original and alias
        (use_declaration
            argument: (use_as_clause
                path: (identifier) @alias.original
                alias: (identifier) @alias.name))
        
        ; use foo::Bar as Baz - scoped version
        (use_declaration
            argument: (use_as_clause
                path: (scoped_identifier
                    name: (identifier) @alias.original)
                alias: (identifier) @alias.name))
        
        ; ============ TYPE REFERENCES ============
        
        ; Type identifiers in function parameters
        (parameter
            type: (type_identifier) @name.reference.type)
        
        ; Return types
        (function_item
            return_type: (type_identifier) @name.reference.type)
        
        ; Struct fields
        (field_declaration
            type: (type_identifier) @name.reference.type)
        
        ; Let bindings with type annotation
        (let_declaration
            type: (type_identifier) @name.reference.type)
        
        ; Generic type arguments: Vec<Foo>
        (type_arguments
            (type_identifier) @name.reference.type)
        
        ; Scoped type identifier: foo::Bar
        (scoped_type_identifier
            name: (type_identifier) @name.reference.type)
        
        ; Reference types: &Foo
        (reference_type
            type: (type_identifier) @name.reference.type)
        
        ; Tuple struct patterns
        (tuple_struct_pattern
            type: (identifier) @name.reference.type)
        
        ; Struct expressions: Foo { ... }
        (struct_expression
            name: (type_identifier) @name.reference.type)
        
        ; Tuple struct expressions: Foo(...)
        (call_expression
            function: (scoped_identifier
                name: (identifier) @name.reference.call))
        
        ; Path segments in scoped identifiers: foo::Bar::baz()
        ; This captures types used in paths like SomeType::method()
        (scoped_identifier
            path: (scoped_identifier
                name: (identifier) @name.reference.type))
        
        ; Direct scoped calls with type in path: Foo::bar()
        (scoped_identifier
            path: (identifier) @name.reference.type
            name: (identifier))
        "#;
