//! 索引预算：文件数 / 总字节 / 单文件解析时长 / 并发解析数上限。
//!
//! 参照 `extension-host` 的 `ExtensionBudget` + `BudgetTracker` 模式
//! （Phase 7）：骨架落地类型、默认值与记账器；强制逻辑（索引构建路径
//! 校验）由 ARC-810 启用。

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::CodeIntelligenceError;

/// 预算被超出的维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetKind {
    /// 索引的文件数上限。
    Files,
    /// 索引文件的总字节上限。
    TotalBytes,
    /// 单文件解析时长上限（秒）。
    ParseDurationSecs,
    /// 并发解析数上限。
    ConcurrentParses,
}

impl BudgetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::TotalBytes => "total_bytes",
            Self::ParseDurationSecs => "parse_duration_secs",
            Self::ConcurrentParses => "concurrent_parses",
        }
    }
}

impl std::fmt::Display for BudgetKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 索引预算配置。全部字段在骨架阶段给出保守默认值，产品可覆盖；
/// serde 上允许部分指定（缺失字段走默认值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexBudget {
    /// 最多索引的文件数。
    #[serde(default = "default_max_files")]
    pub max_files: u64,
    /// 索引文件的总字节上限。
    #[serde(default = "default_max_total_bytes")]
    pub max_total_bytes: u64,
    /// 单文件解析的最大时长（秒）；`0` 表示不限制。
    #[serde(default = "default_max_parse_secs_per_file")]
    pub max_parse_secs_per_file: u64,
    /// 同时解析的文件数上限；`0` 表示不限制。
    #[serde(default = "default_max_concurrent_parses")]
    pub max_concurrent_parses: u32,
}

fn default_max_files() -> u64 {
    200_000
}

fn default_max_total_bytes() -> u64 {
    2 * 1024 * 1024 * 1024
}

fn default_max_parse_secs_per_file() -> u64 {
    30
}

fn default_max_concurrent_parses() -> u32 {
    8
}

impl Default for IndexBudget {
    fn default() -> Self {
        Self {
            max_files: 200_000,
            max_total_bytes: 2 * 1024 * 1024 * 1024,
            max_parse_secs_per_file: 30,
            max_concurrent_parses: 8,
        }
    }
}

/// 记账的当前用量快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BudgetSnapshot {
    /// 已登记的文件数。
    pub files: u64,
    /// 已登记文件的总字节。
    pub total_bytes: u64,
    /// 进行中的解析数。
    pub active_parses: u32,
}

/// 索引预算记账器。ARC-810 的索引构建路径持有；校验超限即返回
/// [`CodeIntelligenceError::BudgetExceeded`]。
#[derive(Debug, Clone)]
pub struct IndexBudgetTracker {
    limits: IndexBudget,
    files: u64,
    total_bytes: u64,
    active_parses: u32,
}

impl IndexBudgetTracker {
    pub fn new(limits: IndexBudget) -> Self {
        Self {
            limits,
            files: 0,
            total_bytes: 0,
            active_parses: 0,
        }
    }

    /// 当前预算上限（ARC-810 构建路径读取）。
    pub fn limits(&self) -> IndexBudget {
        self.limits
    }

    /// 登记一个待索引文件（大小为 `size` 字节）；超出文件数或总字节上限
    /// 返回 [`CodeIntelligenceError::BudgetExceeded`]，且不记账（失败
    /// 不留下半状态）。
    pub fn reserve_file(&mut self, size: u64) -> Result<(), CodeIntelligenceError> {
        let next_files = self.files + 1;
        let next_bytes = self.total_bytes.saturating_add(size);
        let limit_files = self.limits.max_files;
        if limit_files > 0 && next_files > limit_files {
            return Err(CodeIntelligenceError::BudgetExceeded {
                kind: BudgetKind::Files,
                limit: limit_files,
                observed: next_files,
            });
        }
        let limit_bytes = self.limits.max_total_bytes;
        if limit_bytes > 0 && next_bytes > limit_bytes {
            return Err(CodeIntelligenceError::BudgetExceeded {
                kind: BudgetKind::TotalBytes,
                limit: limit_bytes,
                observed: next_bytes,
            });
        }
        self.files = next_files;
        self.total_bytes = next_bytes;
        Ok(())
    }

    /// 开始一次解析；超出并发上限返回 [`CodeIntelligenceError::BudgetExceeded`]。
    /// 与 [`IndexBudgetTracker::parse_end`] 配对使用。
    pub fn parse_start(&mut self) -> Result<(), CodeIntelligenceError> {
        let limit = self.limits.max_concurrent_parses;
        let next = self.active_parses + 1;
        if limit > 0 && next > limit {
            return Err(CodeIntelligenceError::BudgetExceeded {
                kind: BudgetKind::ConcurrentParses,
                limit: u64::from(limit),
                observed: u64::from(next),
            });
        }
        self.active_parses = next;
        Ok(())
    }

    /// 结束一次解析（与 [`IndexBudgetTracker::parse_start`] 配对）。
    pub fn parse_end(&mut self) {
        self.active_parses = self.active_parses.saturating_sub(1);
    }

    /// 单文件解析时长上限；`0`（不限制）返回 `None`。
    pub fn parse_time_limit(&self) -> Option<Duration> {
        let secs = self.limits.max_parse_secs_per_file;
        (secs > 0).then(|| Duration::from_secs(secs))
    }

    /// 当前用量快照。
    pub fn snapshot(&self) -> BudgetSnapshot {
        BudgetSnapshot {
            files: self.files,
            total_bytes: self.total_bytes,
            active_parses: self.active_parses,
        }
    }
}
