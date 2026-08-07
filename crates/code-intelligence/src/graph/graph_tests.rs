//! ScopeGraph / 提取语义与语言查询契约测试。
//!
//! 语言查询契约：每个语言一个 fixture，断言 definitions / references /
//! aliases / exports / containment 的提取 golden。fixture 与 `.scm` 查询
//! （`src/languages/*.rs`，移植自 Grok）一一对应；这里固化的是「查询文本
//! + 提取逻辑」的契约，任何一侧的变化都会在这里暴露。

use crate::languages::{LanguageConfig, LanguageRegistry};
use crate::{QueryKind, QueryResponse};

use super::test_support::{assert_has_def, assert_has_ref, containment_names, extract};

fn rust_config() -> LanguageConfig {
    LanguageRegistry::builtin()
        .for_id("rust")
        .unwrap()
        .as_ref()
        .clone()
}

fn ts_config() -> LanguageConfig {
    LanguageRegistry::builtin()
        .for_id("typescript")
        .unwrap()
        .as_ref()
        .clone()
}

fn js_config() -> LanguageConfig {
    LanguageRegistry::builtin()
        .for_id("javascript")
        .unwrap()
        .as_ref()
        .clone()
}

fn python_config() -> LanguageConfig {
    LanguageRegistry::builtin()
        .for_id("python")
        .unwrap()
        .as_ref()
        .clone()
}

fn go_config() -> LanguageConfig {
    LanguageRegistry::builtin()
        .for_id("go")
        .unwrap()
        .as_ref()
        .clone()
}

// ======================================================================
// Rust 契约
// ======================================================================

const RUST_FIXTURE: &str = r#"
use std::collections::HashMap;

pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Point {
        Point { x, y }
    }
}

fn distance(a: &Point, b: &Point) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    (dx * dx + dy * dy) as f64
}
"#;

#[test]
fn rust_definitions_golden() {
    let extracted = extract(&rust_config(), RUST_FIXTURE).expect("fixture must parse");
    assert_has_def(&extracted, "Point", "class");
    assert_has_def(&extracted, "new", "method");
    assert_has_def(&extracted, "distance", "function");
    // `x` / `y` 字段不是 def capture（Rust 查询只捕获字段类型引用）。
    let names: Vec<&str> = extracted
        .graph
        .definitions()
        .iter()
        .map(|def| def.name.as_str())
        .collect();
    assert!(
        !names.contains(&"x"),
        "field x must not be a definition: {names:?}"
    );
}

#[test]
fn rust_references_golden() {
    let extracted = extract(&rust_config(), RUST_FIXTURE).expect("fixture must parse");
    // use 的 import 引用：`use std::collections::HashMap` → HashMap。
    assert_has_ref(&extracted, "HashMap");
    // 类型引用：impl Point、参数 &Point、struct 表达式 Point { ... }。
    assert_has_ref(&extracted, "Point");
}

#[test]
fn rust_line_numbers_are_one_indexed() {
    let extracted = extract(&rust_config(), RUST_FIXTURE).expect("fixture must parse");
    let distance = extracted
        .graph
        .definitions()
        .into_iter()
        .find(|def| def.name == "distance")
        .expect("distance definition");
    assert_eq!(distance.range.start_line_1indexed(), 15);
    let point = extracted
        .graph
        .definitions()
        .into_iter()
        .find(|def| def.name == "Point")
        .expect("Point definition");
    assert_eq!(point.range.start_line_1indexed(), 4);
}

#[test]
fn rust_alias_pair_from_use_as() {
    let source = r#"
use crate::widgets::Button as FancyButton;
"#;
    let extracted = extract(&rust_config(), source).expect("fixture must parse");
    assert_eq!(
        extracted.aliases,
        vec![("FancyButton".to_string(), "Button".to_string())]
    );
}

// ======================================================================
// TypeScript 契约
// ======================================================================

const TS_FIXTURE: &str = r#"
import { useState as useAppState } from "react";

export interface User {
  id: number;
  name: string;
}

export class UserService {
  private users: User[] = [];

  fetchAll(): User[] {
    const app = useAppState();
    return this.users;
  }
}

export const DEFAULT_USER: User = { id: 0, name: "anonymous" };
export { UserService as UserServiceAlias };
"#;

#[test]
fn typescript_definitions_golden() {
    let extracted = extract(&ts_config(), TS_FIXTURE).expect("fixture must parse");
    assert_has_def(&extracted, "User", "interface");
    assert_has_def(&extracted, "UserService", "class");
    assert_has_def(&extracted, "fetchAll", "method");
    assert_has_def(&extracted, "DEFAULT_USER", "variable");
}

#[test]
fn typescript_references_and_exports_golden() {
    let extracted = extract(&ts_config(), TS_FIXTURE).expect("fixture must parse");
    // User 类型引用（字段类型 / 数组泛型 / const 注解）。
    assert_has_ref(&extracted, "User");
    // 导出符号：`export { UserService as UserServiceAlias }`。
    assert!(
        extracted.exports.contains(&"UserService".to_string()),
        "exports missing UserService: {:?}",
        extracted.exports
    );
}

#[test]
fn typescript_alias_pair_from_named_import() {
    let extracted = extract(&ts_config(), TS_FIXTURE).expect("fixture must parse");
    assert_eq!(
        extracted.aliases,
        vec![("useAppState".to_string(), "useState".to_string())]
    );
}

#[test]
fn typescript_containment_class_method() {
    let extracted = extract(&ts_config(), TS_FIXTURE).expect("fixture must parse");
    let containment = containment_names(&extracted);
    assert!(
        containment
            .iter()
            .any(|(child, parent)| child == "fetchAll" && parent == "UserService"),
        "expected containment fetchAll -> UserService, got {containment:?}"
    );
}

#[test]
fn typescript_nested_function_containment() {
    let source = r#"
function outer() {
  function inner() {
    return 1;
  }
  return inner();
}
"#;
    let extracted = extract(&ts_config(), source).expect("fixture must parse");
    let containment = containment_names(&extracted);
    assert!(
        containment
            .iter()
            .any(|(child, parent)| child == "inner" && parent == "outer"),
        "expected containment inner -> outer, got {containment:?}"
    );
    // inner 也必须存在于 defs（嵌套函数仍被提取）。
    assert_has_def(&extracted, "inner", "function");
}

// ======================================================================
// JavaScript 契约
// ======================================================================

const JS_FIXTURE: &str = r#"
import { format } from "./format";
import helper from "./helper";

export class Formatter {
  format(value) {
    return helper(value);
  }
}

function convert(value) {
  const result = format(value);
  return result;
}
"#;

#[test]
fn javascript_definitions_golden() {
    let extracted = extract(&js_config(), JS_FIXTURE).expect("fixture must parse");
    assert_has_def(&extracted, "Formatter", "class");
    assert_has_def(&extracted, "format", "method");
    assert_has_def(&extracted, "convert", "function");
    assert_has_def(&extracted, "result", "variable");
}

#[test]
fn javascript_references_and_imports_golden() {
    let extracted = extract(&js_config(), JS_FIXTURE).expect("fixture must parse");
    // 命名 import：format。
    assert_has_ref(&extracted, "format");
    // 默认 import：helper。
    assert_has_ref(&extracted, "helper");
    // 方法调用：helper(value)。
    assert_has_ref(&extracted, "helper");
    // 导出符号：`export class Formatter` 无 export capture（JS 查询只有
    // export_specifier）；`export class` 关键字不产生 exports 条目——
    // 本 fixture 没有 export { ... }，因此 exports 为空。
    assert!(
        extracted.exports.is_empty(),
        "unexpected exports: {:?}",
        extracted.exports
    );
}

#[test]
fn javascript_alias_pair_from_import() {
    let source = r#"
import { readFile as read } from "fs";
"#;
    let extracted = extract(&js_config(), source).expect("fixture must parse");
    assert_eq!(
        extracted.aliases,
        vec![("read".to_string(), "readFile".to_string())]
    );
}

// ======================================================================
// Python 契约
// ======================================================================

const PYTHON_FIXTURE: &str = r#"
class Greeter:
    def __init__(self, name):
        self.name = name

    def greet(self):
        return f"hello {self.name}"


def make_greeter(name):
    greeter = Greeter(name)
    return greeter.greet()
"#;

#[test]
fn python_definitions_golden() {
    let extracted = extract(&python_config(), PYTHON_FIXTURE).expect("fixture must parse");
    assert_has_def(&extracted, "Greeter", "class");
    assert_has_def(&extracted, "__init__", "function");
    assert_has_def(&extracted, "greet", "function");
    assert_has_def(&extracted, "make_greeter", "function");
}

#[test]
fn python_references_golden() {
    let extracted = extract(&python_config(), PYTHON_FIXTURE).expect("fixture must parse");
    // 直接调用：Greeter(name)；方法调用：greeter.greet() 的 greet。
    assert_has_ref(&extracted, "Greeter");
    assert_has_ref(&extracted, "greet");
}

// ======================================================================
// Go 契约
// ======================================================================

const GO_FIXTURE: &str = r#"
package main

import (
	"fmt"
	"strings"
)

type Greeter struct {
	prefix string
}

func (g *Greeter) Greet(name string) string {
	return fmt.Sprintf("%s %s", g.prefix, name)
}

func NewGreeter() *Greeter {
	return &Greeter{prefix: "hello"}
}

func main() {
	g := NewGreeter()
	parts := strings.Split("a,b", ",")
	fmt.Println(g.Greet(parts[0]))
}
"#;

#[test]
fn go_definitions_golden() {
    let extracted = extract(&go_config(), GO_FIXTURE).expect("fixture must parse");
    assert_has_def(&extracted, "Greeter", "type");
    assert_has_def(&extracted, "Greet", "method");
    assert_has_def(&extracted, "NewGreeter", "function");
    assert_has_def(&extracted, "main", "function");
}

#[test]
fn go_references_golden() {
    let extracted = extract(&go_config(), GO_FIXTURE).expect("fixture must parse");
    // 调用引用。
    assert_has_ref(&extracted, "NewGreeter");
    assert_has_ref(&extracted, "Greet");
    // 类型引用（type_identifier 全覆盖）。
    assert_has_ref(&extracted, "Greeter");
    // import 引用（interpreted_string_literal 文本）。
    assert_has_ref(&extracted, "\"fmt\"");
    assert_has_ref(&extracted, "\"strings\"");
}

// ======================================================================
// ScopeGraph 结构语义
// ======================================================================

#[test]
fn scope_graph_insert_and_find() {
    use crate::graph::nodes::{LocalDef, LocalScope, Reference};
    use crate::graph::range::{Position, Range};
    use crate::graph::scope::ScopeGraph;

    let mut graph = ScopeGraph::new(Range::default(), "rust".into());
    let def = LocalDef::new(
        "alpha".into(),
        "function".into(),
        Range::new(Position::new(1, 0, 4), Position::new(1, 9, 13)),
        None,
        LocalScope::new(Range::default()),
    );
    graph.insert_global_def(def);
    let reference = Reference::new(
        "alpha".into(),
        Range::new(Position::new(5, 0, 40), Position::new(5, 5, 45)),
        None,
    );
    graph.insert_ref_unconditional(reference);

    assert_eq!(graph.find_definition("alpha").unwrap().start_line(), 1);
    assert_eq!(graph.find_references("alpha").len(), 1);
    assert!(graph.find_definition("beta").is_none());
    assert_eq!(graph.definitions().len(), 1);
    assert_eq!(graph.references().len(), 1);
}

#[test]
fn scope_graph_def_identifier_ranges() {
    let extracted = extract(&rust_config(), RUST_FIXTURE).expect("fixture must parse");
    let graph = &extracted.graph;
    let nodes = graph.definition_nodes();
    assert!(!nodes.is_empty());
    for (idx, def) in &nodes {
        // 节点 identifier 范围 = def 标识符范围（跳转定位语义）；
        // NodeKind::range() 对 def 返回作用域范围（Grok 语义）。
        let identifier = graph.graph[*idx].identifier_range();
        assert_eq!(identifier, def.range);
    }
}

#[test]
fn scope_graph_persisted_round_trip_preserves_semantics() {
    let extracted = extract(&ts_config(), TS_FIXTURE).expect("fixture must parse");
    let persisted = extracted.graph.to_persisted();
    let rebuilt = crate::graph::scope::ScopeGraph::from_persisted(
        extracted.graph.lang().to_string(),
        &persisted,
    );
    assert_eq!(
        rebuilt.definitions().len(),
        extracted.graph.definitions().len()
    );
    assert_eq!(
        rebuilt.references().len(),
        extracted.graph.references().len()
    );
    assert_eq!(
        rebuilt.containment().len(),
        extracted.graph.containment().len()
    );
    // 名字 / 类型逐项一致。
    for (original, rebuilt_def) in extracted
        .graph
        .definitions()
        .iter()
        .zip(rebuilt.definitions())
    {
        assert_eq!(original.name, rebuilt_def.name);
        assert_eq!(original.symbol_type, rebuilt_def.symbol_type);
        assert_eq!(original.range, rebuilt_def.range);
    }
    // 查询语义一致。
    assert_eq!(
        rebuilt.find_definition("UserService"),
        extracted.graph.find_definition("UserService")
    );
    assert_eq!(
        rebuilt.find_references("User").len(),
        extracted.graph.find_references("User").len()
    );
}

#[test]
fn all_builtin_queries_compile() {
    let registry = LanguageRegistry::builtin();
    for config in registry.all_configs() {
        assert!(
            config.language().is_some(),
            "{} must have a grammar",
            config.primary_language_id()
        );
        assert!(
            config.compile_query().is_some(),
            "{} query must compile",
            config.primary_language_id()
        );
    }
}

#[test]
fn query_hash_uses_query_text() {
    // grammar / 扩展名相同、query 不同的两个配置，哈希必须不同。
    let original = LanguageRegistry::builtin();
    let mut modified = LanguageRegistry::builtin();
    let base = modified.for_id("rust").unwrap();
    let tweaked = LanguageConfig::with_grammar(
        base.language_ids().to_vec(),
        base.file_extensions().to_vec(),
        base.namespaces().to_vec(),
        format!(
            "{}; (comment) @name.definition.variable",
            base.file_definition_queries()
        ),
        || tree_sitter_rust::LANGUAGE.into(),
    );
    modified.configs = modified
        .configs
        .iter()
        .map(|config| {
            if config.primary_language_id() == "rust" {
                std::sync::Arc::new(tweaked.clone())
            } else {
                std::sync::Arc::clone(config)
            }
        })
        .collect();
    modified.rebuild_index();
    assert_ne!(original.query_hash(), modified.query_hash());
}

// ======================================================================
// 契约回归：QueryRequest / QueryResponse 的图载荷形状
// ======================================================================

#[test]
fn query_response_graph_field_serde_default() {
    // 旧格式（无 graph 字段）的响应 JSON 反序列化后 graph 为 None。
    let json = r#"{"kind":"status","status":{"state":"running","identity":{"workspace":"source-demo","revision":"rev-1","parser_version":{"Version":42}},"cache":"missing","budget":{"files":0,"total_bytes":0,"active_parses":0}}}"#;
    let response: QueryResponse = serde_json::from_str(json).expect("old format must deserialize");
    assert_eq!(response.kind, QueryKind::Status);
    assert!(response.graph.is_none());
}
