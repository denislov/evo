//! 符号搜索（`search_symbols` / `QueryKind::SymbolSearch`）测试：
//! 相关度排序契约（共用 `tool-contract` 排序接口的 graph 侧）、预算内
//! 截断与显式标记、服务端到端 round-trip。

use std::sync::Arc;

use serde_json::json;

use crate::budget::IndexBudget;
use crate::graph::backend::{GraphBackendOptions, GraphQueryResult, context_field};
use crate::graph::query::{BoundedSymbolSearch, MAX_SYMBOL_SEARCH_CANDIDATES, search_symbols};
use crate::graph::test_support::{builtin, test_identity, write_workspace};
use crate::service::{
    CodeIntelligenceService, CodeIntelligenceServiceOptions, QueryKind, QueryRequest,
};

fn index_with(files: &[(&str, &str)]) -> (tempfile::TempDir, crate::graph::index::CodebaseIndex) {
    let dir = tempfile::tempdir().unwrap();
    write_workspace(dir.path(), files);
    let registry = builtin();
    let backend = crate::graph::backend::GraphQueryBackend::new(GraphBackendOptions {
        root: dir.path().to_path_buf(),
        cache_path: None,
        identity: test_identity(1),
        registry,
        budget: IndexBudget::default(),
    })
    .expect("backend builds");
    let index = backend.snapshot().clone();
    (dir, index)
}

#[test]
fn search_is_case_insensitive_substring() {
    let (_dir, index) = index_with(&[("a.rs", "pub fn AlphaBeta() {}\npub fn gamma() {}\n")]);
    let search = search_symbols(&index, "alphabeta", 0);
    assert_eq!(search.total, 1);
    assert_eq!(search.results[0].name, "AlphaBeta");
    assert!(!search.truncated);
}

#[test]
fn exact_name_ranks_before_partial_matches() {
    let (_dir, index) = index_with(&[(
        "a.rs",
        "pub fn target_helper() {}\npub fn target() {}\npub fn target_extra() {}\n",
    )]);
    let search = search_symbols(&index, "target", 0);
    assert_eq!(search.results[0].name, "target");
    // 同名余项按文件内位置稳定排序。
    assert_eq!(search.results[1].name, "target_helper");
    assert_eq!(search.results[2].name, "target_extra");
}

#[test]
fn cross_file_hits_are_deterministic() {
    let (_dir, index) = index_with(&[
        ("z.rs", "pub fn shared() {}\n"),
        ("a.rs", "pub fn shared() {}\n"),
    ]);
    let search = search_symbols(&index, "shared", 0);
    assert_eq!(search.total, 2);
    // 同分稳定：保持路径排序（a.rs 在 z.rs 前）。
    assert_eq!(search.results[0].path, "a.rs");
    assert_eq!(search.results[1].path, "z.rs");
}

#[test]
fn limit_truncates_and_marks() {
    let (_dir, index) = index_with(&[(
        "a.rs",
        "pub fn alpha() {}\npub fn alpha_two() {}\npub fn alpha_three() {}\n",
    )]);
    let search = search_symbols(&index, "alpha", 2);
    assert_eq!(search.total, 3);
    assert_eq!(search.results.len(), 2);
    assert!(search.truncated);
}

#[test]
fn no_matches_and_empty_query() {
    let (_dir, index) = index_with(&[("a.rs", "pub fn alpha() {}\n")]);
    let search = search_symbols(&index, "nope", 0);
    assert_eq!(search.total, 0);
    assert!(search.results.is_empty());
    assert!(!search.truncated);
    let search = search_symbols(&index, "", 0);
    assert_eq!(search.total, 0);
}

#[test]
fn reference_counts_are_reported() {
    let (_dir, index) = index_with(&[(
        "a.rs",
        "pub fn shared() {}\nfn uses() {\n    shared();\n    shared();\n}\n",
    )]);
    let search = search_symbols(&index, "shared", 0);
    assert_eq!(search.results[0].references, 2);
}

#[test]
fn collection_cap_marks_truncation() {
    // 构造超过收集上限的符号集（用同一文件重复符号不现实，改用大量
    // 唯一符号名的文件内容）。
    let mut src = String::new();
    for i in 0..(MAX_SYMBOL_SEARCH_CANDIDATES + 16) {
        src.push_str(&format!("pub fn sym_{i}() {{}}\n"));
    }
    let (_dir, index) = index_with(&[("a.rs", &src)]);
    let search = search_symbols(&index, "sym_", 0);
    assert_eq!(search.total, MAX_SYMBOL_SEARCH_CANDIDATES);
    assert!(search.truncated, "collection cap must be marked");
}

#[test]
fn serde_round_trip_of_search_result() {
    let search = BoundedSymbolSearch {
        query: "x".into(),
        total: 1,
        truncated: true,
        results: Vec::new(),
    };
    let value = serde_json::to_value(&search).unwrap();
    let back: BoundedSymbolSearch = serde_json::from_value(value).unwrap();
    assert_eq!(back, search);
}

#[tokio::test]
async fn symbol_search_round_trips_through_service() {
    let dir = tempfile::tempdir().unwrap();
    write_workspace(
        dir.path(),
        &[(
            "a.rs",
            "pub fn target_alpha() {}\npub fn target_beta() {}\n",
        )],
    );
    let backend = crate::graph::backend::GraphQueryBackend::new(GraphBackendOptions {
        root: dir.path().to_path_buf(),
        cache_path: None,
        identity: test_identity(1),
        registry: builtin(),
        budget: IndexBudget::default(),
    })
    .expect("backend builds");
    let service = CodeIntelligenceService::new(CodeIntelligenceServiceOptions {
        identity: test_identity(1),
        cache_path: None,
        budget: IndexBudget::default(),
        languages: builtin(),
        backend: Some(Arc::new(backend)),
    });
    let (handle, task) = service.start().expect("service starts");
    let response = handle
        .submit(QueryRequest::new(
            QueryKind::SymbolSearch,
            json!({context_field::SYMBOL: "target_", context_field::LIMIT: 1}),
        ))
        .await
        .expect("query accepted");
    let GraphQueryResult::Search(search) = response.graph.expect("search result") else {
        panic!("expected search result");
    };
    assert_eq!(search.total, 2);
    assert_eq!(search.results.len(), 1);
    assert!(search.truncated);
    handle.shutdown("test");
    task.join().await;
}
