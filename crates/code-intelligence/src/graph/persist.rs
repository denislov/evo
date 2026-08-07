//! 图持久化数据模型：`PersistedGraph`（单文件）+ `GraphCacheData`（跨文件）。
//!
//! 格式选择：JSON（与 ARC-800 的 `IndexCacheData` 载荷一致）。理由：
//!
//! - 与缓存层格式统一，加载/损坏诊断复用同一套结构化错误路径；
//! - 图数据只在保存/加载时全量序列化，单文件符号量级下 JSON 体积可接受
//!   （Grok 的二进制格式是内存优化，Evo 首批规模不构成瓶颈，见债务登记）；
//! - 反序列化失败可定位到具体字段，便于 corruption recovery 测试。
//!
//! 只序列化「查询需要的」结构：definitions / references / imports /
//! containment + 文件 meta；`RefToDef` / `RefToImport` 解析边不持久化
//! （跨文件查询由 `CodebaseIndex` 的二级索引回答，文件内解析边不参与
//! 查询路径——与 Grok 相同：Grok 也不序列化 per-file graphs）。

// Evo 原创模块（数据模型对应 Grok ScopeGraphIndex 序列化的子集：definitions /
// references / aliases / file_meta；Grok 用自定义二进制 "SGIX"，Evo 用 JSON）。
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::nodes::SymbolId;
use super::range::Range;

/// 单个定义的可持久化形态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedDef {
    pub name: String,
    pub symbol_type: String,
    pub range: Range,
    pub symbol_id: Option<SymbolId>,
}

/// 单个引用的可持久化形态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedRef {
    pub name: String,
    pub range: Range,
    pub symbol_id: Option<SymbolId>,
}

/// 单个 import 的可持久化形态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedImport {
    pub name: String,
    pub range: Range,
}

/// 单文件图的持久化形态。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedGraph {
    pub definitions: Vec<PersistedDef>,
    pub references: Vec<PersistedRef>,
    pub imports: Vec<PersistedImport>,
    /// containment 边：`(child_ordinal, parent_ordinal)`，指向
    /// `definitions` 的下标（与提取顺序一致）。
    pub containment: Vec<(u32, u32)>,
}

impl PersistedGraph {
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
            && self.references.is_empty()
            && self.imports.is_empty()
            && self.containment.is_empty()
    }
}

/// 文件 meta（staleness 检测）。与 `cache::CachedFileEntry` 同构：
/// 缓存层负责持久化基线，图内用它做增量时的 stale 判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    pub size: u64,
    pub mtime_secs: i64,
    pub mtime_nanos: u32,
}

impl FileMeta {
    pub fn from_metadata(meta: &std::fs::Metadata) -> Self {
        let size = meta.len();
        let (mtime_secs, mtime_nanos) = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| (d.as_secs() as i64, d.subsec_nanos()))
            .unwrap_or((0, 0));
        Self {
            size,
            mtime_secs,
            mtime_nanos,
        }
    }

    /// 与当前文件系统状态比对是否过期（缺失视为过期）。
    pub fn is_stale(&self, path: &std::path::Path) -> bool {
        match std::fs::metadata(path) {
            Ok(meta) => Self::from_metadata(&meta) != *self,
            Err(_) => true,
        }
    }
}

/// 单文件图 + meta 的完整持久化条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedFile {
    /// 相对 workspace root 的路径（正斜杠分隔）。
    pub rel_path: String,
    pub meta: FileMeta,
    pub graph: PersistedGraph,
    /// 文件级导出符号名（来自 `name.reference.export` 类 capture）。
    pub exports: Vec<String>,
    /// alias 对：`(alias, original)`。
    pub aliases: Vec<(String, String)>,
}

/// 跨文件索引的持久化载荷（挂在 `IndexCacheData.graph` 下）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphCacheData {
    /// 索引 schema 版本（图数据结构演进时递增）。
    pub schema_version: u32,
    /// 构建时的 parser-version 哈希（`ParserVersion::Version` 的一致性
    /// 已由缓存 identity 层校验；此处冗余记录便于诊断）。
    pub query_version: u64,
    pub files: Vec<PersistedFile>,
    /// 全局 alias 表：`(alias, original)`。
    pub aliases: Vec<(String, String)>,
}

/// 当前图数据 schema 版本。
pub const GRAPH_SCHEMA_VERSION: u32 = 1;

/// 拼接 rel_path 并归一化（Windows 分隔符 -> `/`；非法路径 -> None）。
pub fn normalize_rel_path(path: &std::path::Path) -> Option<String> {
    let rel = path.to_str()?;
    Some(rel.replace('\\', "/"))
}

/// 把归一化 rel path 还原为相对路径。
pub fn rel_path_from_string(rel: &str) -> PathBuf {
    PathBuf::from(rel.replace('/', std::path::MAIN_SEPARATOR_STR))
}
