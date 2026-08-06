//! 索引缓存 identity：workspace / revision / parser-version 三要素。
//!
//! 缓存必须能回答「这份缓存属于谁、基于哪个基线、用什么解析器构建」；
//! 任一要素与期望不一致都必须重建（不能混用增量）。与 Grok 的差异：
//! Grok 只有 `QueryVersion` 一维（见下），Evo 按 master plan 扩展为
//! workspace（`WorkspaceId`）+ revision（索引基线）+ parser-version 三维。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (scope_graph/graph.rs `QueryVersion`: Legacy 强制重建 + Version(u64) 比对);
// extended to a three-part identity for Evo semantics.
use std::fmt;

use serde::{Deserialize, Serialize};
use workspace_runtime::api::WorkspaceId;

use crate::error::CodeIntelligenceError;

/// revision id 的长度上限（与 `WorkspaceId` 的 128 字节上限对齐）。
const MAX_REVISION_BYTES: usize = 128;

/// grammar / query 集合的版本（继承 Grok `QueryVersion` 的设计：grammar 或
/// query 变化会触发索引重建，即使文件内容没变）。
///
/// ARC-810 落地 tree-sitter query 后，当前版本号由
/// [`crate::languages::LanguageRegistry::query_hash`] 提供；本骨架中该值由
/// 调用方构造 [`ParserVersion::Version`] 时给出。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ParserVersion {
    /// 未知版本（旧缓存或未记录）——无法判断一致性，强制重建。
    #[default]
    Legacy,
    /// grammar + query 集合的确定性哈希。
    Version(u64),
}

impl ParserVersion {
    /// 判断当前 parser 版本下是否必须重建。
    pub fn needs_rebuild(&self, current_version: u64) -> bool {
        match self {
            ParserVersion::Legacy => true,
            ParserVersion::Version(v) => *v != current_version,
        }
    }

    /// 转成可展示的字符串。
    pub fn as_str(&self) -> String {
        match self {
            ParserVersion::Legacy => "legacy".to_string(),
            ParserVersion::Version(v) => format!("version-{v}"),
        }
    }
}

impl fmt::Display for ParserVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str())
    }
}

/// 索引基线的标识：可以是 git HEAD、变更集快照或调用方提供的标签。
///
/// 语义：revision 不同意味着文件集合或内容基线不同，缓存必须重建。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RevisionId(String);

impl RevisionId {
    /// 解析一个 persisted revision id，应用与创建相同的约束：
    /// 非空、长度 ≤ 128、仅可打印 ASCII。
    pub fn parse(value: impl Into<String>) -> Result<Self, CodeIntelligenceError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_REVISION_BYTES
            && value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ');
        if !valid {
            return Err(CodeIntelligenceError::InvalidRevision {
                detail: format!("revision must be 1..={MAX_REVISION_BYTES} printable ASCII bytes"),
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RevisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for RevisionId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RevisionId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

fn serialize_workspace<S: serde::Serializer>(
    workspace: &WorkspaceId,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(workspace.as_str())
}

fn deserialize_workspace<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<WorkspaceId, D::Error> {
    let value = String::deserialize(deserializer)?;
    WorkspaceId::parse(value).map_err(serde::de::Error::custom)
}

/// 索引缓存的三要素 identity。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheIdentity {
    /// 所属 workspace。复用 `workspace-runtime` 的 [`WorkspaceId`]。
    #[serde(
        serialize_with = "serialize_workspace",
        deserialize_with = "deserialize_workspace"
    )]
    pub workspace: WorkspaceId,
    /// 索引基线。
    pub revision: RevisionId,
    /// 解析器（grammar / query）版本。
    pub parser_version: ParserVersion,
}

impl CacheIdentity {
    /// 与期望 identity 逐要素比对，返回差异报告（`self` 为缓存中的 identity，
    /// `expected` 为当前期望）。
    pub fn mismatch(&self, expected: &CacheIdentity) -> IdentityDiff {
        IdentityDiff {
            workspace: self.workspace != expected.workspace,
            revision: self.revision != expected.revision,
            parser_version: self.parser_version != expected.parser_version,
        }
    }
}

impl fmt::Display for CacheIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "workspace={} revision={} parser={}",
            self.workspace, self.revision, self.parser_version
        )
    }
}

/// 三要素逐项比对结果：`true` = 该要素不一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IdentityDiff {
    pub workspace: bool,
    pub revision: bool,
    pub parser_version: bool,
}

impl IdentityDiff {
    /// 任一要素不一致即需要重建。
    pub fn is_mismatch(&self) -> bool {
        self.workspace || self.revision || self.parser_version
    }
}
