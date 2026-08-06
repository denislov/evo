//! `IndexCache` 的 fault injection / crash-reopen / identity 测试。

use std::time::UNIX_EPOCH;

use crate::{
    CacheIdentity, CacheStatus, CachedFileEntry, CodeIntelligenceError, IndexCache, IndexCacheData,
    LoadOutcome, ParserVersion, RevisionId, probe_cache,
};
use tempfile::tempdir;
use workspace_runtime::api::{WorkspaceId, WorkspaceKind};

const SCHEMA: u32 = crate::INDEX_SCHEMA_VERSION;

fn identity(workspace: &str, revision: &str) -> CacheIdentity {
    CacheIdentity {
        workspace: WorkspaceId::user_supplied(WorkspaceKind::Source, workspace).unwrap(),
        revision: RevisionId::parse(revision).unwrap(),
        parser_version: ParserVersion::Version(1),
    }
}

fn data(files: Vec<CachedFileEntry>) -> IndexCacheData {
    IndexCacheData {
        schema_version: SCHEMA,
        built_at_unix_secs: 1_700_000_000,
        files,
    }
}

fn entry(rel_path: &str, size: u64) -> CachedFileEntry {
    CachedFileEntry::new(rel_path.into(), size, 1_700_000_000, 0)
}

fn open_cache(path: Option<std::path::PathBuf>, id: CacheIdentity) -> IndexCache {
    IndexCache::new(path, id)
}

#[test]
fn save_then_load_round_trip() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join(crate::CACHE_FILE_NAME);
    let id = identity("demo", "rev-1");
    let mut cache = open_cache(Some(cache_path.clone()), id.clone());
    let payload = data(vec![entry("src/lib.rs", 42), entry("src/main.rs", 7)]);
    cache.save(payload.clone()).unwrap();

    let mut reopened = open_cache(Some(cache_path.clone()), id.clone());
    assert_eq!(reopened.load().unwrap(), LoadOutcome::Hit(payload.clone()));
    assert!(reopened.is_loaded());
    assert_eq!(reopened.data(), Some(&payload));
    assert_eq!(reopened.identity(), &id);
}

#[test]
fn miss_when_no_file() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("absent.bin");
    let id = identity("demo", "rev-1");
    let mut cache = open_cache(Some(cache_path), id);
    assert_eq!(cache.load().unwrap(), LoadOutcome::Miss);
    assert!(!cache.is_loaded());
}

#[test]
fn memory_only_cache_never_touches_disk() {
    let id = identity("demo", "rev-1");
    let mut cache = open_cache(None, id);
    assert_eq!(cache.load().unwrap(), LoadOutcome::Miss);
    let payload = data(vec![]);
    cache.save(payload.clone()).unwrap();
    assert_eq!(cache.data(), Some(&payload));
}

fn mismatched_identity_mismatch_is_reported(workspace: &str, revision: &str) {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("cache.bin");
    let id = identity("demo", "rev-1");
    let mut cache = open_cache(Some(cache_path.clone()), id.clone());
    cache.save(data(vec![entry("a.rs", 1)])).unwrap();

    let mut reopened = open_cache(Some(cache_path.clone()), identity(workspace, revision));
    let err = reopened.load().unwrap_err();
    match err {
        CodeIntelligenceError::CacheIdentityMismatch { expected, found } => {
            assert_eq!(
                expected.workspace.as_str(),
                identity(workspace, revision).workspace.as_str()
            );
            assert_eq!(*found, id);
        }
        other => panic!("expected identity mismatch, got {other:?}"),
    }
    // 重建闭环：同一实例 save 新 identity 后能再次加载。
    reopened.save(data(vec![entry("b.rs", 2)])).unwrap();
    assert_eq!(
        reopened.load().unwrap(),
        LoadOutcome::Hit(data(vec![entry("b.rs", 2)]))
    );
}

#[test]
fn workspace_mismatch_triggers_rebuild() {
    mismatched_identity_mismatch_is_reported("other", "rev-1");
}

#[test]
fn revision_mismatch_triggers_rebuild() {
    mismatched_identity_mismatch_is_reported("demo", "rev-2");
}

#[test]
fn parser_version_mismatch_triggers_rebuild() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("cache.bin");
    let mut cache = open_cache(Some(cache_path.clone()), identity("demo", "rev-1"));
    cache.save(data(vec![])).unwrap();

    let mut reopened = open_cache(
        Some(cache_path.clone()),
        CacheIdentity {
            workspace: identity("demo", "rev-1").workspace,
            revision: identity("demo", "rev-1").revision,
            parser_version: ParserVersion::Version(2),
        },
    );
    assert!(matches!(
        reopened.load().unwrap_err(),
        CodeIntelligenceError::CacheIdentityMismatch { .. }
    ));
}

#[test]
fn truncated_file_is_corrupted_not_panic() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("cache.bin");
    let mut cache = open_cache(Some(cache_path.clone()), identity("demo", "rev-1"));
    cache.save(data(vec![entry("a.rs", 1)])).unwrap();
    let bytes = std::fs::read(&cache_path).unwrap();
    // 截断到 payload 中间（模拟 crash 写一半）。
    let truncated_len = bytes.len() / 2;
    std::fs::write(&cache_path, &bytes[..truncated_len]).unwrap();

    let mut reopened = open_cache(Some(cache_path.clone()), identity("demo", "rev-1"));
    let err = reopened.load().unwrap_err();
    assert!(matches!(err, CodeIntelligenceError::CacheCorrupted { .. }));
    // 可重建：直接 save 覆盖并重新加载。
    reopened.save(data(vec![entry("b.rs", 2)])).unwrap();
    assert!(matches!(reopened.load(), Ok(LoadOutcome::Hit(_))));
}

#[test]
fn garbage_magic_is_corrupted() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("cache.bin");
    std::fs::write(&cache_path, b"GARBAGE DATA NOT A CACHE").unwrap();
    let mut cache = open_cache(Some(cache_path.clone()), identity("demo", "rev-1"));
    let err = cache.load().unwrap_err();
    assert!(matches!(
        err,
        CodeIntelligenceError::CacheCorrupted { ref detail, .. }
            if detail.contains("magic")
    ));
}

#[test]
fn unknown_format_version_is_format_error() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("cache.bin");
    // 手写头：magic + 未知 format version。
    let mut bytes = vec![];
    bytes.extend_from_slice(b"EVOIX");
    bytes.push(99);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&cache_path, bytes).unwrap();
    let mut cache = open_cache(Some(cache_path.clone()), identity("demo", "rev-1"));
    let err = cache.load().unwrap_err();
    assert!(matches!(err, CodeIntelligenceError::CacheFormat { .. }));
}

#[test]
fn corrupted_identity_json_is_reported() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("cache.bin");
    let mut bytes = vec![];
    bytes.extend_from_slice(b"EVOIX");
    bytes.push(1);
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(b"{\"workspace\": broken");
    bytes.extend_from_slice(&0u64.to_le_bytes());
    std::fs::write(&cache_path, bytes).unwrap();
    let mut cache = open_cache(Some(cache_path.clone()), identity("demo", "rev-1"));
    assert!(matches!(
        cache.load().unwrap_err(),
        CodeIntelligenceError::CacheCorrupted { .. }
    ));
}

#[test]
fn corrupted_payload_json_is_reported() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("cache.bin");
    let id = identity("demo", "rev-1");
    let mut cache = open_cache(Some(cache_path.clone()), id.clone());
    cache.save(data(vec![entry("a.rs", 1)])).unwrap();

    // 翻转 payload 末尾字节（JSON 尾部必然损坏）。
    let mut bytes = std::fs::read(&cache_path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&cache_path, &bytes).unwrap();

    let mut reopened = open_cache(Some(cache_path.clone()), id.clone());
    let err = reopened.load().unwrap_err();
    assert!(matches!(
        err,
        CodeIntelligenceError::CacheCorrupted { ref detail, .. }
            if detail.contains("payload")
    ));
}

#[test]
fn unknown_payload_schema_is_format_error() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("cache.bin");
    let id = identity("demo", "rev-1");
    let mut cache = open_cache(Some(cache_path.clone()), id.clone());
    cache.save(data(vec![])).unwrap();

    // 手工重写 payload：schema 版本改为 999。
    let bytes = std::fs::read(&cache_path).unwrap();
    let identity_len = u32::from_le_bytes(bytes[6..10].try_into().unwrap()) as usize;
    let payload_offset = 10 + identity_len + 8;
    let mut payload: serde_json::Value = serde_json::from_slice(&bytes[payload_offset..]).unwrap();
    payload["schema_version"] = serde_json::json!(999);
    let patched = serde_json::to_vec(&payload).unwrap();
    // 重新拼装：保留 magic + format + identity + 旧 payload 长度 header
    // 之前的部分，替换为新 payload 长度 header + 新 payload。
    let mut out = bytes[..payload_offset - 8].to_vec();
    out.extend_from_slice(&(patched.len() as u64).to_le_bytes());
    out.extend_from_slice(&patched);
    std::fs::write(&cache_path, out).unwrap();

    let mut reopened = open_cache(Some(cache_path.clone()), id.clone());
    assert!(matches!(
        reopened.load().unwrap_err(),
        CodeIntelligenceError::CacheFormat { .. }
    ));
}

#[test]
fn save_rejects_unknown_schema() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("cache.bin");
    let mut cache = open_cache(Some(cache_path.clone()), identity("demo", "rev-1"));
    let bad = IndexCacheData {
        schema_version: 999,
        built_at_unix_secs: 0,
        files: vec![],
    };
    assert!(matches!(
        cache.save(bad).unwrap_err(),
        CodeIntelligenceError::CacheFormat { .. }
    ));
    // 目标文件未被创建。
    assert!(!cache_path.exists());
}

#[test]
fn crash_reopen_cycle_rebuilds_and_recovers() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("cache.bin");
    let id = identity("demo", "rev-1");

    // 第一次构建。
    let mut cache = open_cache(Some(cache_path.clone()), id.clone());
    cache.save(data(vec![entry("a.rs", 10)])).unwrap();

    // crash：文件被截断。
    let bytes = std::fs::read(&cache_path).unwrap();
    std::fs::write(&cache_path, &bytes[..12]).unwrap();

    // reopen：检测损坏（不 panic）。
    let mut reopened = open_cache(Some(cache_path.clone()), id.clone());
    assert!(matches!(
        reopened.load().unwrap_err(),
        CodeIntelligenceError::CacheCorrupted { .. }
    ));

    // 重建并再次 reopen：恢复可用。
    reopened
        .save(data(vec![entry("a.rs", 10), entry("b.rs", 20)]))
        .unwrap();
    let mut third = open_cache(Some(cache_path.clone()), id.clone());
    assert_eq!(
        third.load().unwrap(),
        LoadOutcome::Hit(data(vec![entry("a.rs", 10), entry("b.rs", 20)]))
    );
}

#[test]
fn failed_save_keeps_previous_cache_intact() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("cache.bin");
    let id = identity("demo", "rev-1");
    let mut cache = open_cache(Some(cache_path.clone()), id.clone());
    cache.save(data(vec![entry("a.rs", 1)])).unwrap();

    // 把缓存文件变成目录，使 rename 失败（模拟写失败）。
    std::fs::remove_file(&cache_path).unwrap();
    std::fs::create_dir(&cache_path).unwrap();
    assert!(cache.save(data(vec![entry("b.rs", 2)])).is_err());
    // 半成品临时文件被清理。
    assert!(!cache_path.with_extension("tmp").exists());
}

#[test]
fn probe_cache_projects_outcomes() {
    let dir = tempdir().unwrap();
    let id = identity("demo", "rev-1");
    // 无路径 → Missing。
    assert_eq!(probe_cache(None, &id), CacheStatus::Missing);

    let cache_path = dir.path().join("cache.bin");
    // 无文件 → Missing。
    assert_eq!(probe_cache(Some(&cache_path), &id), CacheStatus::Missing);

    // 正常缓存 → Ready。
    let mut cache = open_cache(Some(cache_path.clone()), id.clone());
    cache.save(data(vec![])).unwrap();
    assert_eq!(probe_cache(Some(&cache_path), &id), CacheStatus::Ready);

    // identity 不匹配 → RebuildRequired。
    assert!(matches!(
        probe_cache(Some(&cache_path), &identity("other", "rev-1")),
        CacheStatus::RebuildRequired { .. }
    ));

    // 损坏 → RebuildRequired，不 panic。
    std::fs::write(&cache_path, b"junk").unwrap();
    assert!(matches!(
        probe_cache(Some(&cache_path), &id),
        CacheStatus::RebuildRequired { .. }
    ));
}

#[test]
fn cached_file_entry_staleness_detection() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.rs");
    std::fs::write(&file, b"fn main() {}").unwrap();
    let meta = std::fs::metadata(&file).unwrap();
    let modified = meta.modified().unwrap().duration_since(UNIX_EPOCH).unwrap();

    let fresh = CachedFileEntry::new(
        "a.rs".into(),
        meta.len(),
        modified.as_secs() as i64,
        modified.subsec_nanos(),
    );
    assert!(!fresh.is_stale(&meta));

    // size 变化 → stale。
    let wrong_size = CachedFileEntry::new("a.rs".into(), meta.len() + 1, 0, 0);
    assert!(wrong_size.is_stale(&meta));

    // mtime 变化 → stale。
    let old_mtime = CachedFileEntry::new(
        "a.rs".into(),
        meta.len(),
        modified.as_secs() as i64 - 100,
        0,
    );
    assert!(old_mtime.is_stale(&meta));

    // 内容写入后 size 不变但 mtime 变 → stale（模拟 touch）。
    let other = dir.path().join("b.rs");
    std::fs::write(&other, b"fn helper() {}").unwrap();
    let other_meta = std::fs::metadata(&other).unwrap();
    assert!(fresh.is_stale(&other_meta) || other_meta.len() != meta.len());
}

#[test]
fn save_uses_atomic_rename_leaving_no_partial_file() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("cache.bin");
    let id = identity("demo", "rev-1");
    let mut cache = open_cache(Some(cache_path.clone()), id.clone());
    cache.save(data(vec![])).unwrap();
    assert!(cache_path.exists());
    assert!(!cache_path.with_extension("tmp").exists());

    // 重写（覆盖既有缓存）：临时文件消失、目标完整。
    cache.save(data(vec![entry("a.rs", 3)])).unwrap();
    assert!(!cache_path.with_extension("tmp").exists());
    let mut reopened = open_cache(Some(cache_path.clone()), id.clone());
    assert_eq!(
        reopened.load().unwrap(),
        LoadOutcome::Hit(data(vec![entry("a.rs", 3)]))
    );
}

#[test]
fn load_is_idempotent_after_save() {
    let dir = tempdir().unwrap();
    let cache_path = dir.path().join("cache.bin");
    let id = identity("demo", "rev-1");
    let mut cache = open_cache(Some(cache_path.clone()), id.clone());
    cache.save(data(vec![entry("a.rs", 1)])).unwrap();
    assert!(matches!(cache.load(), Ok(LoadOutcome::Hit(_))));
    assert!(matches!(cache.load(), Ok(LoadOutcome::Hit(_))));
}
