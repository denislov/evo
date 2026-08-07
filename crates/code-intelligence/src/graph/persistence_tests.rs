//! 持久化测试：`GraphCacheData` round-trip / golden、identity mismatch
//! 重建、corruption recovery、crash-reopen。

use std::path::{Path, PathBuf};

use tempfile::tempdir;

use crate::budget::IndexBudget;
use crate::graph::backend::{GraphBackendOptions, GraphQueryBackend};
use crate::graph::build::IndexBuilder;
use crate::graph::index::CodebaseIndex;
use crate::graph::persist::{GRAPH_SCHEMA_VERSION, PersistedGraph};
use crate::graph::query::GraphNavigator;
use crate::graph::test_support::{builtin, test_identity, write_workspace};
use crate::identity::{CacheIdentity, ParserVersion, RevisionId};
use crate::{IndexCache, IndexCacheData};
use workspace_runtime::api::{WorkspaceId, WorkspaceKind};

fn budget() -> IndexBudget {
    IndexBudget::default()
}

fn build(root: &Path) -> CodebaseIndex {
    IndexBuilder::new(root, &builtin(), budget())
        .build(21)
        .expect("build must succeed")
        .0
}

fn fixture_workspace() -> (tempfile::TempDir, CodebaseIndex) {
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
                "web/ui.ts",
                r#"
export class Button {
  onClick() {
    return 1;
  }
}
"#,
            ),
        ],
    );
    let index = build(dir.path());
    (dir, index)
}

fn backend_options(
    root: PathBuf,
    cache_path: Option<PathBuf>,
    identity: CacheIdentity,
) -> GraphBackendOptions {
    GraphBackendOptions {
        root,
        cache_path,
        identity,
        registry: builtin(),
        budget: budget(),
    }
}

#[test]
fn graph_cache_data_round_trip_preserves_index() {
    let (dir, index) = fixture_workspace();
    let persisted = index.to_persisted();
    assert_eq!(persisted.schema_version, GRAPH_SCHEMA_VERSION);
    assert_eq!(persisted.files.len(), 2);

    let rebuilt = CodebaseIndex::from_persisted(&persisted).expect("rebuild must succeed");
    assert_eq!(rebuilt.stats(), index.stats());
    assert_eq!(rebuilt.paths(), index.paths());
    assert_eq!(
        rebuilt.find_definitions("Point"),
        index.find_definitions("Point")
    );
    assert_eq!(
        rebuilt.find_references("Point"),
        index.find_references("Point")
    );
    assert_eq!(rebuilt.query_version(), index.query_version());
    assert_eq!(rebuilt.alias_count(), index.alias_count());
    // containment 语义保留：符号树一致。
    let registry = builtin();
    let before = GraphNavigator::new(&index, &registry, dir.path())
        .file_symbols("web/ui.ts")
        .expect("file symbols");
    let after = GraphNavigator::new(&rebuilt, &registry, dir.path())
        .file_symbols("web/ui.ts")
        .expect("file symbols");
    assert_eq!(before, after);
}

#[test]
fn persisted_graph_json_golden() {
    let graph = PersistedGraph {
        definitions: vec![crate::graph::persist::PersistedDef {
            name: "alpha".into(),
            symbol_type: "function".into(),
            range: crate::graph::range::Range::new(
                crate::graph::range::Position::new(0, 0, 0),
                crate::graph::range::Position::new(0, 5, 5),
            ),
            symbol_id: None,
        }],
        references: vec![],
        imports: vec![],
        containment: vec![],
    };
    let json = serde_json::to_string(&graph).unwrap();
    assert!(
        json.contains(r#""name":"alpha""#),
        "golden name field missing: {json}"
    );
    assert!(
        json.contains(r#""symbol_type":"function""#),
        "golden symbol_type field missing: {json}"
    );
    // 反序列化 round-trip。
    let decoded: PersistedGraph = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, graph);
}

#[test]
fn schema_mismatch_rejected_by_from_persisted() {
    let (_, index) = fixture_workspace();
    let mut persisted = index.to_persisted();
    persisted.schema_version = GRAPH_SCHEMA_VERSION + 1;
    assert!(
        CodebaseIndex::from_persisted(&persisted).is_none(),
        "未知 schema 必须拒绝（调用方重建）"
    );
}

#[test]
fn backend_persist_and_reopen_restores_index() {
    let (dir, index) = fixture_workspace();
    let cache_path = dir.path().join(".evo_index.bin");
    let identity = test_identity(1);
    let backend = GraphQueryBackend::new(backend_options(
        dir.path().to_path_buf(),
        Some(cache_path.clone()),
        identity.clone(),
    ))
    .expect("backend construction");
    assert_eq!(backend.stats(), index.stats());
    backend.persist().expect("persist must succeed");

    // crash-reopen：重新构造 backend（同 identity）→ 命中缓存，无需重建。
    let reopened = GraphQueryBackend::new(backend_options(
        dir.path().to_path_buf(),
        Some(cache_path.clone()),
        identity,
    ))
    .expect("reopen must succeed");
    assert_eq!(reopened.stats(), index.stats());
    let defs = reopened.snapshot().find_definitions("Point");
    assert!(!defs.is_empty());
}

#[test]
fn backend_identity_mismatch_rebuilds() {
    let dir = tempdir().unwrap();
    write_workspace(dir.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let cache_path = dir.path().join("cache.bin");
    let identity_a = test_identity(1);
    let backend = GraphQueryBackend::new(backend_options(
        dir.path().to_path_buf(),
        Some(cache_path.clone()),
        identity_a,
    ))
    .expect("backend construction");
    backend.persist().expect("persist");
    let stats_before = backend.stats();

    // revision 不同 → identity mismatch → 重建（索引仍可用，且与磁盘一致）。
    let identity_b = CacheIdentity {
        workspace: WorkspaceId::user_supplied(WorkspaceKind::Source, "graph-test").unwrap(),
        revision: RevisionId::parse("rev-2").unwrap(),
        parser_version: ParserVersion::Version(1),
    };
    let reopened = GraphQueryBackend::new(backend_options(
        dir.path().to_path_buf(),
        Some(cache_path.clone()),
        identity_b,
    ))
    .expect("rebuild must succeed");
    assert_eq!(reopened.stats(), stats_before);
    assert!(reopened.snapshot().has_definition("alpha"));
}

#[test]
fn backend_corrupted_cache_recovers_by_rebuild() {
    let dir = tempdir().unwrap();
    write_workspace(dir.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let cache_path = dir.path().join("cache.bin");
    let identity = test_identity(1);
    let backend = GraphQueryBackend::new(backend_options(
        dir.path().to_path_buf(),
        Some(cache_path.clone()),
        identity.clone(),
    ))
    .expect("backend construction");
    backend.persist().expect("persist");

    // 截断缓存文件。
    let bytes = std::fs::read(&cache_path).unwrap();
    std::fs::write(&cache_path, &bytes[..bytes.len() / 2]).unwrap();

    // 重建：backend 构造不 panic，索引可用。
    let recovered = GraphQueryBackend::new(backend_options(
        dir.path().to_path_buf(),
        Some(cache_path.clone()),
        identity,
    ))
    .expect("corruption recovery must not fail");
    assert!(recovered.snapshot().has_definition("alpha"));
}

#[test]
fn backend_legacy_cache_without_graph_rebuilds() {
    let dir = tempdir().unwrap();
    write_workspace(dir.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let cache_path = dir.path().join("cache.bin");
    let identity = test_identity(1);
    // 手工写入一个无 graph 字段的 ARC-800 形态缓存。
    let mut cache = IndexCache::new(Some(cache_path.clone()), identity.clone());
    cache
        .save(IndexCacheData {
            schema_version: crate::INDEX_SCHEMA_VERSION,
            built_at_unix_secs: 0,
            files: vec![],
            graph: None,
        })
        .expect("save legacy cache");
    drop(cache);

    let backend = GraphQueryBackend::new(backend_options(
        dir.path().to_path_buf(),
        Some(cache_path.clone()),
        identity,
    ))
    .expect("backend must rebuild from legacy cache");
    assert!(backend.snapshot().has_definition("alpha"));
    assert_eq!(backend.snapshot().file_count(), 1);
}

#[test]
fn backend_shutdown_persists_cache() {
    let dir = tempdir().unwrap();
    write_workspace(dir.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let cache_path = dir.path().join("cache.bin");
    let identity = test_identity(1);
    let backend = GraphQueryBackend::new(backend_options(
        dir.path().to_path_buf(),
        Some(cache_path.clone()),
        identity.clone(),
    ))
    .expect("backend construction");
    crate::service::QueryBackend::shutdown(&backend);
    assert!(cache_path.exists(), "shutdown 必须持久化缓存");

    // 重新打开命中。
    let reopened = GraphQueryBackend::new(backend_options(
        dir.path().to_path_buf(),
        Some(cache_path),
        identity,
    ))
    .expect("reopen");
    assert_eq!(reopened.stats(), backend.stats());
}

#[test]
fn cached_index_meta_enables_stale_detection() {
    let (dir, index) = fixture_workspace();
    let persisted = index.to_persisted();
    let file = persisted
        .files
        .iter()
        .find(|file| file.rel_path == "src/point.rs")
        .expect("point.rs persisted");
    assert!(file.meta.size > 0);
    // 未变化 → 不 stale。
    assert!(!file.meta.is_stale(&dir.path().join("src/point.rs")));
    // 修改后 → stale。
    std::fs::write(dir.path().join("src/point.rs"), "pub fn changed() {}\n").unwrap();
    assert!(file.meta.is_stale(&dir.path().join("src/point.rs")));
}
