//! 索引缓存：identity 三要素 + corruption recovery + 原子写。
//!
//! 文件格式（magic 校验 + 长度前置 + JSON 载荷），任何解析失败都产生
//! 结构化错误（重建信号），绝不 panic：
//!
//! ```text
//! [0..5)  magic "EVOIX"
//! [5]     format_version (u8)
//! [6..10) identity_len (u32 LE)
//! [10..)  identity JSON
//! [.. +8) payload_len (u64 LE)
//! [..]    payload JSON
//! ```
//!
//! 加载顺序：magic -> format 版本 -> identity（先于 payload 解析，identity
//! 不匹配时直接返回错误，不做无谓反序列化）-> payload（含 schema 版本）。
//! 保存走「同目录临时文件 + rename」原子写，避免 crash 留下半成品缓存。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (manager/cache.rs: cache file + format detection; types/mod.rs `FileMeta`:
// size/mtime staleness detection); rewritten for Evo semantics — identity
// header, atomic rename, structured corruption errors instead of a legacy
// bincode marker.
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CodeIntelligenceError;
use crate::identity::CacheIdentity;

/// 默认缓存文件名（位于 workspace root 下）。
pub const CACHE_FILE_NAME: &str = ".evo_index.bin";
/// 缓存文件 magic 字节。
const FILE_MAGIC: &[u8; 5] = b"EVOIX";
/// 文件格式版本（布局 / 字段编码）。
const FILE_FORMAT_VERSION: u8 = 1;
/// 载荷 schema 版本（`IndexCacheData` 的字段集）。
pub const INDEX_SCHEMA_VERSION: u32 = 1;

/// 单个文件的索引基线（staleness 检测用）。借鉴 Grok `FileMeta`
/// （size + mtime 秒 / 纳秒分量）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedFileEntry {
    /// 相对 workspace root 的路径（正斜杠分隔）。
    pub rel_path: String,
    /// 文件大小（字节）。
    pub size: u64,
    /// 修改时间（自 UNIX epoch 的秒）。
    pub mtime_secs: i64,
    /// 修改时间纳秒分量。
    pub mtime_nanos: u32,
}

impl CachedFileEntry {
    pub fn new(rel_path: String, size: u64, mtime_secs: i64, mtime_nanos: u32) -> Self {
        Self {
            rel_path,
            size,
            mtime_secs,
            mtime_nanos,
        }
    }

    /// 与当前文件系统状态比对，判断是否已过期（size 或 mtime 变化）。
    pub fn is_stale(&self, meta: &std::fs::Metadata) -> bool {
        let size = meta.len();
        let (mtime_secs, mtime_nanos) = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| (d.as_secs() as i64, d.subsec_nanos()))
            .unwrap_or((0, 0));
        self.size != size || self.mtime_secs != mtime_secs || self.mtime_nanos != mtime_nanos
    }
}

/// 索引载荷。骨架只携带文件基线元数据；ARC-810 追加 graph 序列化字段
/// （新字段必须带 `#[serde(default)]`，否则破坏向后兼容）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexCacheData {
    /// 载荷 schema 版本（必须等于 [`INDEX_SCHEMA_VERSION`]）。
    pub schema_version: u32,
    /// 构建时间（自 UNIX epoch 的秒）。
    pub built_at_unix_secs: i64,
    /// 已索引文件的基线元数据。
    pub files: Vec<CachedFileEntry>,
    /// ARC-810：codebase graph 持久化载荷；`None` = 无图数据（旧缓存，
    /// 触发全量重建）。
    #[serde(default)]
    pub graph: Option<crate::graph::persist::GraphCacheData>,
}

/// `load` 的返回结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    /// 无缓存（首次构建或路径未配置）。
    Miss,
    /// 缓存命中且 identity 完全匹配。
    Hit(IndexCacheData),
}

/// 缓存状态（服务 `Status` 查询的投影；探测失败不 panic，重建即可恢复）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheStatus {
    /// 无缓存路径或缓存文件不存在。
    Missing,
    /// 缓存命中且 identity 匹配。
    Ready,
    /// 缓存存在但损坏 / 格式不支持 / identity 不匹配，等待重建。
    RebuildRequired { reason: String },
}

/// 带 identity 的索引缓存。`path = None` 时退化为纯内存缓存（不读写磁盘）。
#[derive(Debug, Clone)]
pub struct IndexCache {
    path: Option<PathBuf>,
    identity: CacheIdentity,
    data: Option<IndexCacheData>,
}

impl IndexCache {
    pub fn new(path: Option<PathBuf>, identity: CacheIdentity) -> Self {
        Self {
            path,
            identity,
            data: None,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn identity(&self) -> &CacheIdentity {
        &self.identity
    }

    /// 当前会话是否加载过缓存（hit 后为 `true`）。
    pub fn is_loaded(&self) -> bool {
        self.data.is_some()
    }

    /// 最近一次成功加载的载荷（ARC-810 读取文件基线）。
    pub fn data(&self) -> Option<&IndexCacheData> {
        self.data.as_ref()
    }

    /// 从磁盘加载缓存。
    ///
    /// - 文件不存在 / 路径未配置 → [`LoadOutcome::Miss`]（不是错误）；
    /// - identity 不匹配 → [`CodeIntelligenceError::CacheIdentityMismatch`]；
    /// - magic / 长度 / JSON / schema 失败 → `CacheCorrupted` 或 `CacheFormat`。
    ///
    /// 所有失败都携带「重建即可恢复」语义，调用方捕获后重新
    /// [`IndexCache::save`] 即可；本方法不 panic。
    pub fn load(&mut self) -> Result<LoadOutcome, CodeIntelligenceError> {
        let Some(path) = self.path.as_ref() else {
            self.data = None;
            return Ok(LoadOutcome::Miss);
        };
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.data = None;
                return Ok(LoadOutcome::Miss);
            }
            Err(error) => return Err(error.into()),
        };

        const HEADER_LEN: usize = 10; // magic(5) + format_version(1) + identity_len(4)
        if bytes.len() < HEADER_LEN {
            return Err(CodeIntelligenceError::CacheCorrupted {
                path: path.clone(),
                detail: format!("file too short ({}) for header", bytes.len()),
            });
        }
        if &bytes[0..5] != FILE_MAGIC {
            return Err(CodeIntelligenceError::CacheCorrupted {
                path: path.clone(),
                detail: "bad magic bytes".into(),
            });
        }
        if bytes[5] != FILE_FORMAT_VERSION {
            return Err(CodeIntelligenceError::CacheFormat {
                path: path.clone(),
                detail: format!("format version {} unsupported", bytes[5]),
            });
        }
        let identity_len = u32::from_le_bytes(bytes[6..10].try_into().unwrap()) as usize;
        let identity_end = HEADER_LEN.saturating_add(identity_len);
        if identity_end > bytes.len() {
            return Err(CodeIntelligenceError::CacheCorrupted {
                path: path.clone(),
                detail: format!("identity length {identity_len} exceeds file size"),
            });
        }
        let found_identity: CacheIdentity =
            serde_json::from_slice(&bytes[HEADER_LEN..identity_end]).map_err(|error| {
                CodeIntelligenceError::CacheCorrupted {
                    path: path.clone(),
                    detail: format!("identity payload invalid: {error}"),
                }
            })?;
        if self.identity.mismatch(&found_identity).is_mismatch() {
            return Err(CodeIntelligenceError::CacheIdentityMismatch {
                expected: Box::new(self.identity.clone()),
                found: Box::new(found_identity),
            });
        }

        let payload_len_offset = identity_end.saturating_add(8);
        if payload_len_offset > bytes.len() {
            return Err(CodeIntelligenceError::CacheCorrupted {
                path: path.clone(),
                detail: "truncated payload length header".into(),
            });
        }
        let payload_len =
            u64::from_le_bytes(bytes[identity_end..payload_len_offset].try_into().unwrap())
                as usize;
        let payload_end = payload_len_offset.saturating_add(payload_len);
        if payload_end > bytes.len() {
            return Err(CodeIntelligenceError::CacheCorrupted {
                path: path.clone(),
                detail: format!("payload length {payload_len} exceeds file size"),
            });
        }
        let mut data: IndexCacheData =
            serde_json::from_slice(&bytes[payload_len_offset..payload_end]).map_err(|error| {
                CodeIntelligenceError::CacheCorrupted {
                    path: path.clone(),
                    detail: format!("payload invalid: {error}"),
                }
            })?;
        if data.schema_version != INDEX_SCHEMA_VERSION {
            return Err(CodeIntelligenceError::CacheFormat {
                path: path.clone(),
                detail: format!(
                    "payload schema {} unsupported (current {INDEX_SCHEMA_VERSION})",
                    data.schema_version
                ),
            });
        }
        data.built_at_unix_secs = sanitize_built_at(data.built_at_unix_secs);
        self.data = Some(data.clone());
        Ok(LoadOutcome::Hit(data))
    }

    /// 序列化并原子写入磁盘（同目录临时文件 + rename）；失败不会留下
    /// 半成品目标文件，已存在的旧缓存也不受影响。`path = None` 时只更新
    /// 内存状态。
    pub fn save(&mut self, data: IndexCacheData) -> Result<(), CodeIntelligenceError> {
        if data.schema_version != INDEX_SCHEMA_VERSION {
            let path = self
                .path
                .clone()
                .unwrap_or_else(|| PathBuf::from("<memory>"));
            return Err(CodeIntelligenceError::CacheFormat {
                path,
                detail: format!(
                    "payload schema {} unsupported (current {INDEX_SCHEMA_VERSION})",
                    data.schema_version
                ),
            });
        }
        let Some(path) = self.path.as_ref() else {
            self.data = Some(data);
            return Ok(());
        };
        let identity_json = serde_json::to_vec(&self.identity).map_err(|error| {
            CodeIntelligenceError::CacheCorrupted {
                path: path.clone(),
                detail: format!("identity serialization failed: {error}"),
            }
        })?;
        let payload_json =
            serde_json::to_vec(&data).map_err(|error| CodeIntelligenceError::CacheCorrupted {
                path: path.clone(),
                detail: format!("payload serialization failed: {error}"),
            })?;
        let mut bytes = Vec::with_capacity(10 + identity_json.len() + 8 + payload_json.len());
        bytes.extend_from_slice(FILE_MAGIC);
        bytes.push(FILE_FORMAT_VERSION);
        bytes.extend_from_slice(&(identity_json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&identity_json);
        bytes.extend_from_slice(&(payload_json.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&payload_json);

        let tmp = path.with_extension("tmp");
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&tmp, path)?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(error.into());
        }
        self.data = Some(data);
        Ok(())
    }
}

/// 负的 `built_at` 在序列化层没有意义；加载时规整为 0（防御性，
/// 不 panic、不拒绝）。
fn sanitize_built_at(built_at: i64) -> i64 {
    built_at.max(0)
}

/// 只读探测缓存状态（服务启动时调用；任何失败都投影为
/// [`CacheStatus::RebuildRequired`]，不 panic）。
pub fn probe_cache(path: Option<&Path>, identity: &CacheIdentity) -> CacheStatus {
    let Some(path) = path else {
        return CacheStatus::Missing;
    };
    match IndexCache::new(Some(path.to_path_buf()), identity.clone()).load() {
        Ok(LoadOutcome::Miss) => CacheStatus::Missing,
        Ok(LoadOutcome::Hit(_)) => CacheStatus::Ready,
        Err(error) => CacheStatus::RebuildRequired {
            reason: error.to_string(),
        },
    }
}
