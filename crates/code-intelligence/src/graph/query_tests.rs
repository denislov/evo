//! `GraphNavigator` 查询测试：definitions / references / containment
//! 与边界（无符号位置、越界位置、不支持语言、未索引文件）。

use std::path::Path;

use tempfile::tempdir;

use crate::budget::IndexBudget;
use crate::graph::build::IndexBuilder;
use crate::graph::query::GraphNavigator;
use crate::graph::test_support::{builtin, write_workspace};

fn budget() -> IndexBudget {
    IndexBudget::default()
}

fn build(root: &Path) -> crate::graph::index::CodebaseIndex {
    IndexBuilder::new(root, &builtin(), budget())
        .build(7)
        .expect("build must succeed")
        .0
}

/// 标准 fixture workspace。
fn fixture_workspace() -> (tempfile::TempDir, crate::graph::index::CodebaseIndex) {
    let dir = tempdir().unwrap();
    write_workspace(
        dir.path(),
        &[
            (
                "src/point.rs",
                r#"
pub struct Point {
    pub x: i32,
}

impl Point {
    pub fn new() -> Point {
        Point { x: 0 }
    }
}
"#,
            ),
            (
                "src/main.rs",
                r#"
use crate::point::Point as P;

fn main() {
    let p = P::new();
}
"#,
            ),
            (
                "web/ui.ts",
                r#"
export class Button {
  onClick() {
    return 1;
  }
}

export function render() {
  const b = new Button();
  return b;
}
"#,
            ),
        ],
    );
    let index = build(dir.path());
    (dir, index)
}

#[test]
fn file_symbols_containment_tree() {
    let (dir, index) = fixture_workspace();
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    let symbols = navigator
        .file_symbols("web/ui.ts")
        .expect("file must be indexed");
    assert_eq!(symbols.len(), 2, "Button + render: {symbols:#?}");
    let button = symbols
        .iter()
        .find(|symbol| symbol.name == "Button")
        .expect("Button symbol");
    assert_eq!(button.symbol_type, "class");
    assert_eq!(button.children.len(), 1);
    assert_eq!(button.children[0].name, "onClick");
    assert_eq!(button.children[0].symbol_type, "method");
    // 位置 1-indexed。
    assert_eq!(button.line, 2);
}

#[test]
fn file_symbols_sorted_by_position() {
    let (dir, index) = fixture_workspace();
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    let symbols = navigator
        .file_symbols("src/point.rs")
        .expect("file must be indexed");
    // Point 在前，impl 中的 new 在后。
    let names: Vec<&str> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();
    assert_eq!(names, vec!["Point", "new"]);
}

#[test]
fn file_symbols_not_indexed_errors() {
    let (dir, index) = fixture_workspace();
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    let error = navigator
        .file_symbols("never/indexed.rs")
        .expect_err("unindexed file must error");
    assert!(matches!(
        error,
        crate::graph::query::GraphQueryError::FileNotIndexed(_)
    ));
}

#[test]
fn definition_by_name_across_files() {
    let (dir, index) = fixture_workspace();
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    let result = navigator.definition_by_name("Point", None);
    assert_eq!(result.symbol, "Point");
    assert!(
        result
            .locations
            .iter()
            .any(|location| location.path == "src/point.rs" && location.line == 2),
        "Point definition must be at src/point.rs:3: {:?}",
        result.locations
    );
}

#[test]
fn definition_by_name_resolves_alias_to_original() {
    let (dir, index) = fixture_workspace();
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    // `use crate::point::Point as P;` —— 查询别名 P 应解析到原符号 Point。
    let result = navigator.definition_by_name("P", None);
    assert!(
        result
            .locations
            .iter()
            .any(|location| location.path == "src/point.rs" && location.line == 2),
        "alias P must resolve to Point definition: {:?}",
        result.locations
    );
}

#[test]
fn goto_definition_by_position() {
    let (dir, index) = fixture_workspace();
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    // src/main.rs 第 5 行 `let p = P::new();` 中 P 位于列 13。
    let result = navigator.goto_definition("src/main.rs", 5, 13);
    match result {
        Ok(navigation) => {
            assert_eq!(navigation.symbol, "P");
            assert!(
                navigation
                    .locations
                    .iter()
                    .any(|location| location.path == "src/point.rs"),
                "P must resolve into src/point.rs: {:?}",
                navigation.locations
            );
        }
        Err(error) => panic!("goto_definition failed: {error}"),
    }
}

#[test]
fn references_by_name_and_include_definition() {
    let (dir, index) = fixture_workspace();
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    // 不含定义位置：Point 的引用在 src/point.rs（impl）与 main.rs。
    let without = navigator.references_by_name("Point", false, None);
    assert!(!without.locations.is_empty());
    assert!(
        !without
            .locations
            .iter()
            .any(|location| location.path == "src/point.rs" && location.line == 2),
        "definition line must be excluded when include_definition=false: {:?}",
        without.locations
    );
    // 含定义位置：定义行出现在最前。
    let with = navigator.references_by_name("Point", true, None);
    assert!(
        with.locations
            .iter()
            .any(|location| location.path == "src/point.rs" && location.line == 2),
        "definition line must be included: {:?}",
        with.locations
    );
}

#[test]
fn references_resolve_through_alias() {
    let (dir, index) = fixture_workspace();
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    // main.rs 里 `P::new()` 是对 Point 的别名引用；查 Point 应包含该行。
    let result = navigator.references_by_name("Point", false, None);
    assert!(
        result
            .locations
            .iter()
            .any(|location| location.path == "src/main.rs" && location.line == 5),
        "alias reference on src/main.rs:7 must be found: {:?}",
        result.locations
    );
}

#[test]
fn position_out_of_bounds_rejected() {
    let (dir, index) = fixture_workspace();
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    for (row, col) in [(0, 1), (1, 0), (0, 0)] {
        let error = navigator
            .get_symbol_at_position("src/point.rs", row, col)
            .expect_err("zero-indexed positions must be rejected");
        assert!(
            matches!(
                error,
                crate::graph::query::GraphQueryError::PositionOutOfBounds { .. }
            ),
            "expected PositionOutOfBounds for ({row}, {col}): {error}"
        );
    }
}

#[test]
fn position_past_end_of_file_reports_no_symbol() {
    let (dir, index) = fixture_workspace();
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    let error = navigator
        .get_symbol_at_position("src/point.rs", 99, 1)
        .expect_err("position past EOF must error");
    assert!(matches!(
        error,
        crate::graph::query::GraphQueryError::NoSymbolAtPosition { .. }
    ));
}

#[test]
fn unsupported_language_position_query_errors() {
    let (dir, index) = fixture_workspace();
    std::fs::write(dir.path().join("notes.txt"), "hello").unwrap();
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    let error = navigator
        .get_symbol_at_position("notes.txt", 1, 1)
        .expect_err("unsupported language must error");
    assert!(matches!(
        error,
        crate::graph::query::GraphQueryError::UnsupportedLanguage(_)
    ));
}

#[test]
fn missing_file_position_query_errors() {
    let (dir, index) = fixture_workspace();
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    let error = navigator
        .get_symbol_at_position("no/such.rs", 1, 1)
        .expect_err("missing file must error");
    assert!(matches!(
        error,
        crate::graph::query::GraphQueryError::FileNotFound(_)
    ));
}

#[test]
fn smart_sort_ranks_same_language_first() {
    // 同名符号分布在 rust 与 ts 文件；从 rust 上下文查询时 rust 优先。
    let dir = tempdir().unwrap();
    write_workspace(
        dir.path(),
        &[
            ("a.rs", "pub fn target() {}\n"),
            ("b.ts", "export function target() {}\n"),
            ("c.rs", "pub fn target() {}\n"),
        ],
    );
    let index = build(dir.path());
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    let result = navigator.definition_by_name("target", Some("ctx.rs"));
    let locations = result.locations;
    assert_eq!(locations.len(), 3);
    // 前两个必须是 .rs。
    assert!(locations[0].path.ends_with(".rs"));
    assert!(locations[1].path.ends_with(".rs"));
    // 路径排序：a.rs 在 c.rs 前。
    assert!(locations[0].path == "a.rs");
}

#[test]
fn empty_workspace_queries_return_empty() {
    let dir = tempdir().unwrap();
    let index = build(dir.path());
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    let result = navigator.definition_by_name("anything", None);
    assert!(result.locations.is_empty());
    let references = navigator.references_by_name("anything", false, None);
    assert!(references.locations.is_empty());
}
