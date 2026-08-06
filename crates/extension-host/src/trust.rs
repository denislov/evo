//! Folder trust：扩展启用前的信任判定。
//!
//! 与 Grok 一致：项目级扩展复用「folder trust」单一权威，不建第二套信任
//! 数据库。骨架提供判定入口（[`decide_trust`]）、存储抽象（[`TrustStore`]）
//! 与内存实现（[`InMemoryTrustStore`]），以及首次启用时向用户展示来源与
//! 能力所需的 DTO（[`EnableRequest`] / [`CapabilityClaim`]）。ARC-710 由
//! 产品信任存储实现 [`TrustStore`]。

// Adapted from xai-grok-hooks, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// rewritten for Evo semantics (not a verbatim copy).
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::budget::ExtensionBudget;
use crate::config::ExtensionSource;
use crate::discovery::ExtensionRecord;

/// 单个文件夹的信任状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustStatus {
    Trusted,
    Untrusted,
    /// 从未决定过（首次启用场景）：由产品向用户展示来源与能力后放行。
    NotDecided,
}

/// folder trust 存储抽象。产品在 ARC-710 接入真实信任存储。
pub trait TrustStore: std::fmt::Debug + Send + Sync {
    fn trust_status(&self, folder: &Path) -> TrustStatus;
}

/// 进程内信任存储（测试与骨架阶段使用）。
#[derive(Debug, Clone, Default)]
pub struct InMemoryTrustStore {
    trusted: Arc<Mutex<HashSet<PathBuf>>>,
    decided: Arc<Mutex<HashSet<PathBuf>>>,
}

impl InMemoryTrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 将文件夹标记为已信任。
    pub fn trust(&self, folder: PathBuf) {
        let folder = canonical(&folder);
        self.trusted.lock().unwrap().insert(folder.clone());
        self.decided.lock().unwrap().insert(folder);
    }

    /// 将文件夹标记为明确不信任。
    pub fn distrust(&self, folder: PathBuf) {
        let folder = canonical(&folder);
        self.trusted.lock().unwrap().remove(&folder);
        self.decided.lock().unwrap().insert(folder);
    }
}

fn canonical(folder: &Path) -> PathBuf {
    folder
        .canonicalize()
        .unwrap_or_else(|_| folder.to_path_buf())
}

impl TrustStore for InMemoryTrustStore {
    fn trust_status(&self, folder: &Path) -> TrustStatus {
        let folder = canonical(folder);
        // 信任语义：精确匹配或 folder 位于某信任目录之下（信任祖先目录）。
        if self
            .trusted
            .lock()
            .unwrap()
            .iter()
            .any(|t| folder.starts_with(t))
        {
            return TrustStatus::Trusted;
        }
        // 明确决定过（且不信任）的目录（或其祖先被决定过）。
        if self
            .decided
            .lock()
            .unwrap()
            .iter()
            .any(|t| folder.starts_with(t))
        {
            return TrustStatus::Untrusted;
        }
        TrustStatus::NotDecided
    }
}

/// 一次信任判定的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustDecision {
    pub folder: PathBuf,
    pub status: TrustStatus,
}

/// 判定一个扩展目录的信任状态。
pub fn decide_trust(folder: &Path, store: &dyn TrustStore) -> TrustDecision {
    TrustDecision {
        folder: canonical(folder),
        status: store.trust_status(folder),
    }
}

/// 扩展声明的一项能力，首次启用时向用户展示。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityClaim {
    pub name: String,
    pub description: String,
    pub risk: CapabilityRisk,
}

/// 能力带来的风险级别（展示与授权参考，非最终授权）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityRisk {
    None,
    ReadOnly,
    Mutating,
    ProcessExecution,
}

/// 首次启用请求：向用户展示扩展来源与能力，由产品决定是否放行
/// （ARC-710 提供确认路径；骨架只提供 DTO 形状与构造入口）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnableRequest {
    pub extension_id: String,
    pub name: String,
    pub version: String,
    pub source: ExtensionSource,
    pub source_dir: PathBuf,
    pub capabilities: Vec<CapabilityClaim>,
    pub budget: ExtensionBudget,
}

/// 从发现记录构造首次启用 DTO。
pub fn build_enable_request(record: &ExtensionRecord, budget: ExtensionBudget) -> EnableRequest {
    EnableRequest {
        extension_id: record.id.clone(),
        name: record.manifest.name.clone(),
        version: record.manifest.version.clone(),
        source: record.source,
        source_dir: record.dir.clone(),
        capabilities: record.manifest.capabilities.clone(),
        budget,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(path: &str) -> PathBuf {
        // canonicalize 需要真实目录；测试统一用已创建的 tempdir。
        PathBuf::from(path)
    }

    #[test]
    fn decide_trust_boundaries() {
        let store = InMemoryTrustStore::new();
        let folder = dir("/nonexistent/project");
        assert_eq!(store.trust_status(&folder), TrustStatus::NotDecided);
        store.trust(folder.clone());
        assert_eq!(store.trust_status(&folder), TrustStatus::Trusted);
    }

    #[test]
    fn untrusted_after_distrust() {
        let store = InMemoryTrustStore::new();
        let folder = dir("/nonexistent/project");
        store.distrust(folder.clone());
        assert_eq!(store.trust_status(&folder), TrustStatus::Untrusted);
    }

    #[test]
    fn trust_is_inherited_by_children() {
        let store = InMemoryTrustStore::new();
        store.trust(dir("/home/u/projects"));
        assert_eq!(
            store.trust_status(&dir("/home/u/projects/evo")),
            TrustStatus::Trusted,
            "subdirectory of a trusted folder is trusted"
        );
    }

    #[test]
    fn decided_untrusted_ancestor_covers_children() {
        let store = InMemoryTrustStore::new();
        store.distrust(dir("/home/u/scratch"));
        assert_eq!(
            store.trust_status(&dir("/home/u/scratch/sub")),
            TrustStatus::Untrusted
        );
    }

    #[test]
    fn trust_requires_explicit_decision() {
        let store = InMemoryTrustStore::new();
        assert_eq!(
            store.trust_status(&dir("/elsewhere")),
            TrustStatus::NotDecided
        );
    }

    #[test]
    fn trust_of_exact_folder_does_not_leak_to_sibling() {
        let store = InMemoryTrustStore::new();
        store.trust(dir("/home/u/projects/evo"));
        assert_eq!(
            store.trust_status(&dir("/home/u/projects/other")),
            TrustStatus::NotDecided,
            "siblings of a trusted folder are NOT trusted"
        );
    }

    #[test]
    fn decide_trust_returns_canonical_folder() {
        let temp = tempfile::tempdir().unwrap();
        let store = InMemoryTrustStore::new();
        store.trust(temp.path().to_path_buf());
        let decision = decide_trust(temp.path(), &store);
        assert_eq!(decision.status, TrustStatus::Trusted);
        assert_eq!(decision.folder, canonical(temp.path()));
    }

    #[test]
    fn enable_request_dto_carries_source_and_capabilities() {
        let record = ExtensionRecord {
            id: "lint-tools".into(),
            manifest: crate::discovery::ExtensionManifest {
                name: "Lint Tools".into(),
                version: "0.2.0".into(),
                description: Some("extra linting".into()),
                capabilities: vec![CapabilityClaim {
                    name: "lint".into(),
                    description: "run linters".into(),
                    risk: CapabilityRisk::ProcessExecution,
                }],
                config: None,
                hooks: None,
            },
            source: ExtensionSource::Project,
            dir: dir("/project/ext/lint-tools"),
        };
        let request = build_enable_request(&record, ExtensionBudget::default());
        assert_eq!(request.extension_id, "lint-tools");
        assert_eq!(request.source, ExtensionSource::Project);
        assert_eq!(
            request.capabilities[0].risk,
            CapabilityRisk::ProcessExecution
        );
        assert_eq!(request.budget, ExtensionBudget::default());

        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["source"], "project");
        assert_eq!(value["capabilities"][0]["risk"], "process_execution");
        let back: EnableRequest = serde_json::from_value(value).unwrap();
        assert_eq!(back, request);
    }
}
