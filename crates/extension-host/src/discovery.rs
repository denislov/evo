//! 扩展发现：从 global / project 目录扫描扩展 manifest。
//!
//! 与 Grok 的「目录内散落 JSON hook 文件」不同，Evo 的每个扩展是一个
//! **目录**，目录下放 `extension.json`（manifest，声明名称 / 版本 / 能力 /
//! 内联配置）。这一形状同时服务 ARC-710（runner 需要 per-extension 配置）
//! 与 ARC-720（MCP server 声明其 transport / tools）。
//!
//! 容错语义与 Grok discovery 一致：目录不存在是空结果（不是错误）；
//! 单个坏 manifest 记录错误并继续扫描其余扩展；结果按目录名排序保证稳定。

// Adapted from xai-grok-hooks, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// rewritten for Evo semantics (not a verbatim copy).
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{ExtensionConfig, ExtensionSource};
use crate::error::ExtensionError;
use crate::trust::CapabilityClaim;

/// 每个扩展目录内的 manifest 文件名。
pub const EXTENSION_MANIFEST_FILE: &str = "extension.json";

/// 发现的扩展记录：目录 + 解析后的 manifest + 来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionRecord {
    /// 目录名（稳定 id，用于 trust / enable / diagnostics）。
    pub id: String,
    pub manifest: ExtensionManifest,
    pub source: ExtensionSource,
    pub dir: PathBuf,
}

/// 扩展 manifest（wire 形状）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionManifest {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<CapabilityClaim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<ExtensionConfig>,
}

impl ExtensionManifest {
    /// 骨架校验：name / version 非空；capabilities 名称唯一。
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("manifest 'name' must not be empty".into());
        }
        if self.version.trim().is_empty() {
            return Err(format!("manifest '{}' has an empty 'version'", self.name));
        }
        let mut seen = std::collections::HashSet::new();
        for claim in &self.capabilities {
            if !seen.insert(claim.name.as_str()) {
                return Err(format!(
                    "manifest '{}' declares duplicate capability '{}'",
                    self.name, claim.name
                ));
            }
        }
        Ok(())
    }
}

/// 从字符串解析并校验 manifest。
pub fn parse_manifest(content: &str, path: &Path) -> Result<ExtensionManifest, ExtensionError> {
    let manifest: ExtensionManifest =
        serde_json::from_str(content).map_err(|e| ExtensionError::ParseFile {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;
    manifest
        .validate()
        .map_err(|detail| ExtensionError::InvalidConfig {
            name: manifest.name.clone(),
            path: path.to_path_buf(),
            detail,
        })?;
    Ok(manifest)
}

/// 扫描一组目录下的扩展（每个目录一个 `extension.json`）。
///
/// 目录不存在 → 空（不是错误）；其余读取 / 解析错误逐条记录并继续。
/// 结果按目录名排序，保证跨调用稳定。
pub fn discover_extensions(
    dirs: &[&Path],
    source: ExtensionSource,
) -> (Vec<ExtensionRecord>, Vec<ExtensionError>) {
    let mut records = Vec::new();
    let mut errors = Vec::new();

    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                errors.push(ExtensionError::ReadFile {
                    path: dir.to_path_buf(),
                    source: e,
                });
                continue;
            }
        };

        let mut candidates = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    errors.push(ExtensionError::ReadFile {
                        path: dir.to_path_buf(),
                        source: e,
                    });
                    continue;
                }
            };
            // 隐藏目录跳过。
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            candidates.push(path);
        }
        candidates.sort();

        for extension_dir in candidates {
            let manifest_path = extension_dir.join(EXTENSION_MANIFEST_FILE);
            let content = match std::fs::read_to_string(&manifest_path) {
                Ok(c) => c,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // 目录存在但没有 manifest：不是扩展，跳过。
                    continue;
                }
                Err(e) => {
                    errors.push(ExtensionError::ReadFile {
                        path: manifest_path,
                        source: e,
                    });
                    continue;
                }
            };
            let manifest = match parse_manifest(&content, &manifest_path) {
                Ok(m) => m,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
            records.push(ExtensionRecord {
                id: extension_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
                manifest,
                source,
                dir: extension_dir,
            });
        }
    }

    (records, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(EXTENSION_MANIFEST_FILE), content).unwrap();
    }

    fn valid_manifest(name: &str) -> String {
        serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "description": "test extension",
            "capabilities": [],
            "config": { "enabled": true }
        })
        .to_string()
    }

    #[test]
    fn discovers_nothing_in_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let (records, errors) = discover_extensions(&[dir.path()], ExtensionSource::Global);
        assert!(errors.is_empty());
        assert!(records.is_empty());
    }

    #[test]
    fn missing_dir_is_not_an_error() {
        let (records, errors) =
            discover_extensions(&[Path::new("/nonexistent/ext")], ExtensionSource::Global);
        assert!(errors.is_empty());
        assert!(records.is_empty());
    }

    #[test]
    fn discovers_one_extension_per_directory() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(&root.path().join("lint"), &valid_manifest("Lint"));
        write_manifest(&root.path().join("mcp-tools"), &valid_manifest("MCP Tools"));

        let (records, errors) = discover_extensions(&[root.path()], ExtensionSource::Project);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(records.len(), 2);
        let ids: Vec<_> = records.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["lint", "mcp-tools"], "stable sorted order");
        assert_eq!(records[0].manifest.name, "Lint");
        assert_eq!(records[0].source, ExtensionSource::Project);
    }

    #[test]
    fn skips_non_manifest_directories_and_files() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(&root.path().join("good"), &valid_manifest("Good"));
        std::fs::create_dir_all(root.path().join("no-manifest")).unwrap();
        std::fs::write(root.path().join("random.json"), "{}").unwrap();
        std::fs::create_dir_all(root.path().join(".hidden")).unwrap();
        write_manifest(&root.path().join(".hidden"), &valid_manifest("Hidden"));

        let (records, errors) = discover_extensions(&[root.path()], ExtensionSource::Global);
        assert!(errors.is_empty());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "good");
    }

    #[test]
    fn bad_manifest_is_recorded_others_still_load() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(&root.path().join("good"), &valid_manifest("Good"));
        std::fs::create_dir_all(root.path().join("bad")).unwrap();
        std::fs::write(
            root.path().join("bad").join(EXTENSION_MANIFEST_FILE),
            "{oops",
        )
        .unwrap();

        let (records, errors) = discover_extensions(&[root.path()], ExtensionSource::Global);
        assert_eq!(records.len(), 1);
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], ExtensionError::ParseFile { .. }));
    }

    #[test]
    fn invalid_manifest_semantics_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(
            &root.path().join("bad-name"),
            r#"{"name": "  ", "version": "1.0.0"}"#,
        );
        write_manifest(
            &root.path().join("bad-cap"),
            r#"{
                "name": "dup",
                "version": "1.0.0",
                "capabilities": [
                    {"name": "x", "description": "a", "risk": "none"},
                    {"name": "x", "description": "b", "risk": "none"}
                ]
            }"#,
        );
        let (records, errors) = discover_extensions(&[root.path()], ExtensionSource::Global);
        assert!(records.is_empty());
        assert_eq!(errors.len(), 2);
        for err in &errors {
            assert!(matches!(err, ExtensionError::InvalidConfig { .. }));
        }
    }

    #[test]
    fn manifest_round_trips_with_config_and_capabilities() {
        use crate::budget::ExtensionBudget;
        let manifest = ExtensionManifest {
            name: "Lint".into(),
            version: "0.2.0".into(),
            description: Some("linting".into()),
            capabilities: vec![CapabilityClaim {
                name: "lint".into(),
                description: "run linters".into(),
                risk: crate::trust::CapabilityRisk::ProcessExecution,
            }],
            config: Some(ExtensionConfig {
                enabled: true,
                budget: ExtensionBudget {
                    max_calls_per_session: 9,
                    ..Default::default()
                },
                ..Default::default()
            }),
        };
        assert!(manifest.validate().is_ok());
        let value = serde_json::to_value(&manifest).unwrap();
        assert_eq!(value["name"], "Lint");
        assert_eq!(value["capabilities"][0]["risk"], "process_execution");
        assert_eq!(value["config"]["budget"]["maxCallsPerSession"], 9);
        let back: ExtensionManifest = serde_json::from_value(value).unwrap();
        assert_eq!(back, manifest);
    }

    #[test]
    fn manifest_without_optional_fields_loads() {
        let manifest = parse_manifest(
            r#"{"name": "min", "version": "1"}"#,
            Path::new("e/extension.json"),
        )
        .unwrap();
        assert_eq!(manifest.description, None);
        assert!(manifest.capabilities.is_empty());
        assert_eq!(manifest.config, None);
    }
}
