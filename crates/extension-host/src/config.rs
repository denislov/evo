//! 扩展配置与分层合并。
//!
//! 配置来源优先级（高 → 低）：`Managed > Project > Global`。合并规则
//! （见 [`merge_config_layers`]）：
//!
//! - `enabled`：**AND** 合并 —— 任意一层禁用则整体禁用（任何层可关掉
//!   扩展，安全优先）。
//! - `budget` / `diagnostic_level`：最高优先级层的值覆盖。
//! - `permissions`：所有层的**并集**（去重）。
//! - 无法解析的层记录错误并跳过，其余层照常合并（与 Grok 的
//!   TOML layer 容错一致）。

// Adapted from xai-grok-hooks, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// rewritten for Evo semantics (not a verbatim copy).
use serde::{Deserialize, Serialize};

use crate::budget::ExtensionBudget;
use crate::diagnostic::DiagnosticLevel;
use crate::error::ExtensionError;

/// 配置来源（同时用于 discovery 记录的来源与 config layer 的优先级）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionSource {
    Global,
    Project,
    Managed,
}

impl ExtensionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
            Self::Managed => "managed",
        }
    }
}

impl std::fmt::Display for ExtensionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 合并后的扩展配置。字段全部带默认值，便于 `#[serde(default)]` 容错
/// （配置层允许只声明部分字段）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub budget: ExtensionBudget,
    #[serde(default = "default_diagnostic_level")]
    pub diagnostic_level: DiagnosticLevel,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
}

fn default_enabled() -> bool {
    true
}

fn default_diagnostic_level() -> DiagnosticLevel {
    DiagnosticLevel::Info
}

impl Default for ExtensionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            budget: ExtensionBudget::default(),
            diagnostic_level: DiagnosticLevel::Info,
            permissions: Vec::new(),
        }
    }
}

impl ExtensionConfig {
    /// 与高层配置合并：`self` 是**低**优先级，`higher` 覆盖。
    ///
    /// 规则：`enabled` 取逻辑与；`budget` / `diagnostic_level` 取 `higher`；
    /// `permissions` 取并集（保持 `self` 顺序在前）。
    pub fn merged_with(&self, higher: &Self) -> Self {
        let mut permissions = self.permissions.clone();
        for perm in &higher.permissions {
            if !permissions.contains(perm) {
                permissions.push(perm.clone());
            }
        }
        Self {
            enabled: self.enabled && higher.enabled,
            budget: higher.budget,
            diagnostic_level: higher.diagnostic_level,
            permissions,
        }
    }
}

/// 一个配置层：来源 + 展示名 + 已解析的配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionConfigLayer {
    pub source: ExtensionSource,
    pub source_name: String,
    pub config: ExtensionConfig,
}

impl ExtensionConfigLayer {
    pub fn new(
        source: ExtensionSource,
        source_name: impl Into<String>,
        config: ExtensionConfig,
    ) -> Self {
        Self {
            source,
            source_name: source_name.into(),
            config,
        }
    }

    /// 从 TOML 值解析一个配置层。`value` 应为扩展配置表
    /// （例如 `[extensions.<name>]` 下的内容）。
    pub fn from_toml(
        source: ExtensionSource,
        source_name: impl Into<String>,
        value: &toml::Value,
    ) -> Result<Self, String> {
        // toml::Value 是 owned Deserializer；先归一化到 serde_json 再解析，
        // 使 TOML / JSON 两条路径共享同一结构解析。
        let json =
            serde_json::to_value(value).map_err(|e| format!("invalid extension config: {e}"))?;
        let config = ExtensionConfig::deserialize(json)
            .map_err(|e| format!("invalid extension config: {e}"))?;
        Ok(Self::new(source, source_name, config))
    }

    /// 从 JSON 值解析一个配置层。
    pub fn from_json(
        source: ExtensionSource,
        source_name: impl Into<String>,
        value: &serde_json::Value,
    ) -> Result<Self, String> {
        let config = ExtensionConfig::deserialize(value)
            .map_err(|e| format!("invalid extension config: {e}"))?;
        Ok(Self::new(source, source_name, config))
    }
}

/// 合并配置层为最终配置。`layers` 按**高优先级在前**排列；合并本身不会
/// 失败（坏层在解析阶段已被过滤），返回错误列表保持 API 对称。
///
/// 实现：从低到高 fold，`merged_with(self = 低, higher)` 中 higher 覆盖
/// scalar 字段，`permissions` 并集（低优先级层在前）。
pub fn merge_config_layers(
    layers: &[ExtensionConfigLayer],
) -> (ExtensionConfig, Vec<ExtensionError>) {
    let mut config = ExtensionConfig::default();
    for layer in layers.iter().rev() {
        // rev 后 layer 从低到高：当前 config 是更低方，layer 是 higher。
        config = config.merged_with(&layer.config);
    }
    (config, Vec::new())
}

/// 从原始 TOML 值序列合并配置层（容错：坏层跳过并返回错误列表）。
pub fn merge_toml_layers(
    layers: &[(ExtensionSource, String, toml::Value)],
) -> (ExtensionConfig, Vec<ExtensionError>) {
    let mut parsed = Vec::new();
    let mut errors = Vec::new();
    for (source, name, value) in layers {
        match ExtensionConfigLayer::from_toml(*source, name, value) {
            Ok(layer) => parsed.push(layer),
            Err(detail) => errors.push(ExtensionError::InvalidConfig {
                name: name.clone(),
                path: std::path::PathBuf::from(name),
                detail,
            }),
        }
    }
    let (config, mut merge_errors) = merge_config_layers(&parsed);
    errors.append(&mut merge_errors);
    (config, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(source: ExtensionSource, name: &str, config: ExtensionConfig) -> ExtensionConfigLayer {
        ExtensionConfigLayer::new(source, name, config)
    }

    fn config(
        enabled: bool,
        budget: Option<ExtensionBudget>,
        permissions: &[&str],
    ) -> ExtensionConfig {
        ExtensionConfig {
            enabled,
            budget: budget.unwrap_or_default(),
            diagnostic_level: DiagnosticLevel::Info,
            permissions: permissions.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn higher_priority_overrides_scalars() {
        let low = config(true, None, &["read"]);
        let high = ExtensionConfig {
            enabled: true,
            budget: ExtensionBudget {
                max_calls_per_session: 7,
                ..Default::default()
            },
            diagnostic_level: DiagnosticLevel::Debug,
            permissions: vec!["write".into()],
        };
        let merged = low.merged_with(&high);
        assert_eq!(merged.budget.max_calls_per_session, 7);
        assert_eq!(merged.diagnostic_level, DiagnosticLevel::Debug);
    }

    #[test]
    fn any_layer_can_disable() {
        let low = config(true, None, &[]);
        let high = config(false, None, &[]);
        let merged = low.merged_with(&high);
        assert!(!merged.enabled, "disabled higher layer wins");
        // AND 语义：任何层禁用（无论优先级）→ 整体禁用。
        let disabled_low = config(false, None, &[]);
        assert!(
            !high.merged_with(&disabled_low).enabled,
            "AND: lower disabled wins too"
        );
    }

    #[test]
    fn permissions_union_dedupes() {
        let low = config(true, None, &["read", "write"]);
        let high = config(true, None, &["write", "network"]);
        let merged = low.merged_with(&high);
        assert_eq!(merged.permissions, vec!["read", "write", "network"]);
    }

    #[test]
    fn merge_config_layers_applies_priority_order() {
        let global = layer(
            ExtensionSource::Global,
            "global.toml",
            config(false, None, &["read"]),
        );
        let managed = layer(
            ExtensionSource::Managed,
            "managed.toml",
            config(true, None, &["write"]),
        );
        let (merged, errors) = merge_config_layers(&[managed.clone(), global.clone()]);
        assert!(errors.is_empty());
        // enabled：AND（global 禁用 → 整体禁用）
        assert!(!merged.enabled);
        // permissions：并集，低优先级层在前（global 先声明 read）。
        assert_eq!(merged.permissions, vec!["read", "write"]);
        // scalar 取最高优先级（managed）。
        assert_eq!(merged.budget, managed.config.budget);
    }

    #[test]
    fn empty_layers_yield_default_config() {
        let (merged, errors) = merge_config_layers(&[]);
        assert!(errors.is_empty());
        assert_eq!(merged, ExtensionConfig::default());
    }

    #[test]
    fn layer_parses_from_toml_with_defaults() {
        let value: toml::Value =
            toml::from_str("enabled = true\nbudget = { maxCallsPerSession = 5 }").unwrap();
        let layer =
            ExtensionConfigLayer::from_toml(ExtensionSource::Project, "p.toml", &value).unwrap();
        assert!(layer.config.enabled);
        assert_eq!(layer.config.budget.max_calls_per_session, 5);
        assert_eq!(layer.config.budget.max_run_secs, 3_600);
        assert!(layer.config.permissions.is_empty());
    }

    #[test]
    fn bad_layer_is_skipped_others_merge() {
        let bad: toml::Value = toml::from_str("enabled = \"not-a-bool\"").unwrap();
        let (merged, errors) = merge_toml_layers(&[
            (ExtensionSource::Managed, "bad.toml".to_string(), bad),
            (
                ExtensionSource::Project,
                "good.toml".to_string(),
                toml::Value::Table(Default::default()),
            ),
        ]);
        assert_eq!(errors.len(), 1, "bad layer surfaces one error");
        assert!(matches!(&errors[0], ExtensionError::InvalidConfig { .. }));
        // 好层照常合并（默认配置）。
        assert_eq!(merged, ExtensionConfig::default());
    }

    #[test]
    fn layer_round_trips_via_json() {
        let layer = layer(
            ExtensionSource::Managed,
            "m",
            ExtensionConfig {
                enabled: true,
                budget: ExtensionBudget {
                    max_calls_per_session: 3,
                    ..Default::default()
                },
                diagnostic_level: DiagnosticLevel::Warning,
                permissions: vec!["read".into()],
            },
        );
        let value = serde_json::to_value(&layer.config).unwrap();
        assert_eq!(value["enabled"], true);
        assert_eq!(value["budget"]["maxCallsPerSession"], 3);
        let back: ExtensionConfig = serde_json::from_value(value).unwrap();
        assert_eq!(back, layer.config);
    }
}
