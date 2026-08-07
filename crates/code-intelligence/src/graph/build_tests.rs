//! `IndexBuilder` 全量构建测试：构建正确性（符号 / 引用 / containment
//! 边）与 `IndexBudget` 各维度强制、跳过记录。

use std::path::Path;
use std::time::Duration;

use tempfile::tempdir;

use crate::budget::{BudgetKind, IndexBudget};
use crate::graph::build::{IndexBuilder, IndexSkipReason, MAX_INDEXABLE_FILE_SIZE, parse_file};
use crate::graph::query::GraphNavigator;
use crate::graph::test_support::{builtin, write_workspace};

/// 默认预算（并发 4）。
fn budget() -> IndexBudget {
    IndexBudget {
        max_files: 200_000,
        max_total_bytes: 2 * 1024 * 1024 * 1024,
        max_parse_secs_per_file: 30,
        max_concurrent_parses: 4,
    }
}

/// 构建并返回 `(index, report)`。
fn build(
    root: &Path,
    budget: IndexBudget,
) -> (
    crate::graph::index::CodebaseIndex,
    crate::graph::build::BuildReport,
) {
    IndexBuilder::new(root, &builtin(), budget)
        .build(42)
        .expect("build must succeed")
}

#[test]
fn initial_build_extracts_symbols_across_files() {
    let dir = tempdir().unwrap();
    write_workspace(
        dir.path(),
        &[
            (
                "src/lib.rs",
                r#"
pub struct Point {
    pub x: i32,
}

impl Point {
    pub fn new() -> Point {
        Point { x: 0 }
    }
}

pub fn distance() -> f64 {
    0.0
}
"#,
            ),
            (
                "src/main.ts",
                r#"
import { Point } from "./lib";
const origin = Point.new();
const result = distance(origin);
"#,
            ),
            ("README.md", "# readme"),
        ],
    );
    let (index, report) = build(dir.path(), budget());
    assert_eq!(report.indexed_files, 2, "md 文件应被跳过");
    assert_eq!(report.definitions, 5); // Point + new + distance + origin + result
    assert!(report.skipped.iter().any(|skip| {
        skip.rel_path == "README.md" && skip.reason == IndexSkipReason::UnsupportedLanguage
    }));
    assert!(index.has_definition("Point"));
    assert!(index.has_definition("origin"));
    // 引用跨文件可查。
    let references = index.find_references("Point");
    assert!(
        references.iter().any(|(path, _)| path == "src/main.ts"),
        "src/main.ts must reference Point: {references:?}"
    );
    // 定义位置 1-indexed。
    let defs = index.find_definitions("Point");
    assert!(
        defs.iter()
            .any(|(path, line)| path == "src/lib.rs" && *line == 2)
    );
}

#[test]
fn initial_build_preserves_containment_edges() {
    let dir = tempdir().unwrap();
    write_workspace(
        dir.path(),
        &[(
            "component.tsx",
            r#"
export class Panel {
  render() {
    return null;
  }
}
"#,
        )],
    );
    let (index, _) = build(dir.path(), budget());
    let registry = builtin();
    let navigator = GraphNavigator::new(&index, &registry, dir.path());
    let symbols = navigator
        .file_symbols("component.tsx")
        .expect("file must be indexed");
    assert_eq!(symbols.len(), 1);
    let panel = &symbols[0];
    assert_eq!(panel.name, "Panel");
    assert_eq!(panel.symbol_type, "class");
    assert_eq!(panel.children.len(), 1);
    assert_eq!(panel.children[0].name, "render");
    assert_eq!(panel.children[0].symbol_type, "method");
}

#[test]
fn budget_max_files_exceeded_records_skip() {
    let dir = tempdir().unwrap();
    write_workspace(
        dir.path(),
        &[
            ("a.rs", "pub fn a() {}\n"),
            ("b.rs", "pub fn b() {}\n"),
            ("c.rs", "pub fn c() {}\n"),
        ],
    );
    let tight = IndexBudget {
        max_files: 2,
        ..budget()
    };
    let (index, report) = build(dir.path(), tight);
    assert_eq!(index.file_count(), 2);
    assert!(
        report
            .skipped
            .iter()
            .any(|skip| skip.reason == IndexSkipReason::BudgetExceeded(BudgetKind::Files)),
        "expected a BudgetExceeded(Files) skip: {:?}",
        report.skipped
    );
}

#[test]
fn budget_max_total_bytes_exceeded_records_skip() {
    let dir = tempdir().unwrap();
    write_workspace(
        dir.path(),
        &[
            ("a.rs", "pub fn a() {}\n"),
            ("b.rs", "pub fn b() {}\n"),
            ("c.rs", "pub fn c() {}\n"),
        ],
    );
    let tiny = IndexBudget {
        max_total_bytes: 32, // 三个文件远大于此
        ..budget()
    };
    let (index, report) = build(dir.path(), tiny);
    assert!(index.file_count() <= 2);
    assert!(
        report
            .skipped
            .iter()
            .any(|skip| skip.reason == IndexSkipReason::BudgetExceeded(BudgetKind::TotalBytes)),
        "expected a BudgetExceeded(TotalBytes) skip: {:?}",
        report.skipped
    );
}

#[test]
fn budget_parse_timeout_skips_file() {
    // 直接驱动 parse_file 的计时路径（1ns 的超时必然触发）。
    let dir = tempdir().unwrap();
    let file = dir.path().join("slow.rs");
    std::fs::write(&file, "pub fn slow() { let x = 1; }\n").unwrap();
    let outcome = parse_file(&file, &builtin(), Some(Duration::from_nanos(1)))
        .expect("parse_file returns Some outcome");
    match outcome {
        crate::graph::build::ParseOutcome::Skipped(IndexSkipReason::ParseTimeout) => {}
        other => panic!("expected ParseTimeout, got {other:?}"),
    }
}

#[test]
fn budget_concurrency_limited_build_still_correct() {
    let dir = tempdir().unwrap();
    let mut files: Vec<(String, &str)> = Vec::new();
    for i in 0..6 {
        files.push((format!("f{i}.rs"), "pub fn f() {}\npub fn g() {}\n"));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(rel, content)| (rel.as_str(), *content))
        .collect();
    write_workspace(dir.path(), &refs);
    let serial = IndexBudget {
        max_concurrent_parses: 1,
        ..budget()
    };
    let (index, report) = build(dir.path(), serial);
    assert_eq!(index.file_count(), 6);
    assert_eq!(report.definitions, 12);
    assert!(index.has_definition("f"));
}

#[test]
fn oversized_file_skipped() {
    let dir = tempdir().unwrap();
    let oversized = vec![b'a'; MAX_INDEXABLE_FILE_SIZE as usize + 1];
    std::fs::write(dir.path().join("huge.rs"), oversized).unwrap();
    std::fs::write(dir.path().join("ok.rs"), "pub fn ok() {}\n").unwrap();
    let (index, report) = build(dir.path(), budget());
    assert_eq!(index.file_count(), 1);
    assert!(
        report
            .skipped
            .iter()
            .any(|skip| skip.rel_path == "huge.rs"
                && matches!(skip.reason, IndexSkipReason::TooLarge(size) if size > MAX_INDEXABLE_FILE_SIZE)),
        "expected TooLarge skip: {:?}",
        report.skipped
    );
}

#[test]
fn unsupported_language_skipped() {
    let dir = tempdir().unwrap();
    write_workspace(
        dir.path(),
        &[
            ("a.rs", "pub fn a() {}\n"),
            ("notes.txt", "plain text"),
            ("data.json", "{}"),
        ],
    );
    let (index, report) = build(dir.path(), budget());
    assert_eq!(index.file_count(), 1);
    let reasons: Vec<&str> = report
        .skipped
        .iter()
        .map(|skip| skip.reason.as_str())
        .collect();
    assert!(
        reasons.contains(&"unsupported_language"),
        "expected unsupported_language skips: {reasons:?}"
    );
    assert_eq!(report.skipped.len(), 2);
}

#[test]
fn empty_and_binary_files_skipped() {
    let dir = tempdir().unwrap();
    write_workspace(
        dir.path(),
        &[
            ("empty.rs", ""),
            ("binary.rs", "\u{0}\u{1}\u{2}binary"),
            ("good.rs", "pub fn good() {}\n"),
        ],
    );
    let (index, report) = build(dir.path(), budget());
    assert_eq!(index.file_count(), 1);
    assert!(
        report
            .skipped
            .iter()
            .any(|skip| skip.rel_path == "empty.rs" && skip.reason == IndexSkipReason::Empty),
        "expected Empty skip: {:?}",
        report.skipped
    );
    assert!(
        report
            .skipped
            .iter()
            .any(|skip| skip.rel_path == "binary.rs" && skip.reason == IndexSkipReason::Binary),
        "expected Binary skip: {:?}",
        report.skipped
    );
}

#[test]
fn build_is_deterministic() {
    let dir = tempdir().unwrap();
    write_workspace(
        dir.path(),
        &[
            ("a.rs", "pub fn alpha() {}\npub fn beta() {}\n"),
            ("b.ts", "export class Widget {}\n"),
        ],
    );
    let (first, report_first) = build(dir.path(), budget());
    let (second, report_second) = build(dir.path(), budget());
    assert_eq!(first.stats(), second.stats());
    assert_eq!(first.paths(), second.paths());
    assert_eq!(report_first.skipped, report_second.skipped);
    assert_eq!(
        first.find_definitions("alpha"),
        second.find_definitions("alpha")
    );
}

#[test]
fn build_respects_gitignore_and_hidden() {
    let dir = tempdir().unwrap();
    // ignore crate 只在 git 仓库中应用 .gitignore：创建最小仓库标记。
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(dir.path().join(".gitignore"), "ignored.rs\n").unwrap();
    std::fs::write(dir.path().join("ignored.rs"), "pub fn nope() {}\n").unwrap();
    std::fs::write(dir.path().join(".hidden.rs"), "pub fn hidden() {}\n").unwrap();
    std::fs::write(dir.path().join("visible.rs"), "pub fn visible() {}\n").unwrap();
    let (index, _) = build(dir.path(), budget());
    let paths = index.paths();
    assert!(paths.contains(&"visible.rs"));
    assert!(
        !paths.contains(&"ignored.rs"),
        "gitignore 文件不应索引: {paths:?}"
    );
    assert!(
        !paths.contains(&".hidden.rs"),
        "隐藏文件不应索引: {paths:?}"
    );
}
