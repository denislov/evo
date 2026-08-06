//! 扩展预算：输出 / 时长 / 调用次数上限类型与记账。
//!
//! 骨架阶段落地「每 session 调用次数」与「每 session 输出字节」两维记账
//! （dispatch 事件时校验）；`max_run_secs` 与 `max_concurrent_extensions`
//! 是 ARC-710 runner / ARC-720 MCP adapter 的输入维度，本骨架只提供类型
//! 与默认值，不做强制。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::ExtensionError;

/// 预算被超出的维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetKind {
    /// 每个 session 允许的扩展事件/调用次数。
    CallCount,
    /// 每个 session 允许的扩展输出字节数。
    OutputBytes,
    /// 单次扩展运行的最大时长（秒）。ARC-710 runner 使用。
    RunDurationSecs,
    /// 同时启用的扩展数量上限。ARC-720 使用。
    ConcurrentExtensions,
}

impl BudgetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CallCount => "call_count",
            Self::OutputBytes => "output_bytes",
            Self::RunDurationSecs => "run_duration_secs",
            Self::ConcurrentExtensions => "concurrent_extensions",
        }
    }
}

impl std::fmt::Display for BudgetKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 扩展预算配置。全部字段在骨架阶段给出保守默认值，产品可覆盖；
/// serde 上允许部分指定（缺失字段走默认值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionBudget {
    /// 每个 session 最多处理的扩展事件/调用次数。
    #[serde(default = "default_calls_per_session")]
    pub max_calls_per_session: u32,
    /// 每个 session 最多记录的扩展输出字节数。
    #[serde(default = "default_output_bytes_per_session")]
    pub max_output_bytes_per_session: usize,
    /// 单次扩展运行的最大时长（秒）；`0` 表示不限制。
    #[serde(default = "default_run_secs")]
    pub max_run_secs: u64,
    /// 同时启用的扩展数量上限；`0` 表示不限制。
    #[serde(default = "default_concurrent_extensions")]
    pub max_concurrent_extensions: u32,
}

fn default_calls_per_session() -> u32 {
    100_000
}

fn default_output_bytes_per_session() -> usize {
    64 * 1024 * 1024
}

fn default_run_secs() -> u64 {
    3_600
}

fn default_concurrent_extensions() -> u32 {
    32
}

impl Default for ExtensionBudget {
    fn default() -> Self {
        Self {
            max_calls_per_session: 100_000,
            max_output_bytes_per_session: 64 * 1024 * 1024,
            max_run_secs: 3_600,
            max_concurrent_extensions: 32,
        }
    }
}

/// 记账的当前用量快照。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BudgetSnapshot {
    /// session 的事件/调用次数累计。
    pub calls: u64,
    /// session 的输出字节累计。
    pub output_bytes: u64,
}

/// 每 session 预算记账器。dispatch 路径持有（通过 host 共享状态），
/// `session_end` 事件后可调用 [`BudgetTracker::reset_session`] 清零。
#[derive(Debug, Clone, Default)]
pub struct BudgetTracker {
    limits: ExtensionBudget,
    calls: HashMap<String, u32>,
    output_bytes: HashMap<String, usize>,
}

impl BudgetTracker {
    pub fn new(limits: ExtensionBudget) -> Self {
        Self {
            limits,
            calls: HashMap::new(),
            output_bytes: HashMap::new(),
        }
    }

    /// 当前预算上限（ARC-710 runner 读取 `max_run_secs`）。
    pub fn limits(&self) -> ExtensionBudget {
        self.limits
    }

    /// 记录一次事件处理；超出调用次数上限返回 [`ExtensionError::BudgetExceeded`]。
    pub fn record_call(&mut self, session_id: &str) -> Result<(), ExtensionError> {
        let next = self.calls.get(session_id).copied().unwrap_or(0) + 1;
        let limit = self.limits.max_calls_per_session;
        if limit > 0 && next > limit {
            return Err(ExtensionError::BudgetExceeded {
                kind: BudgetKind::CallCount,
                limit: u64::from(limit),
                observed: u64::from(next),
            });
        }
        self.calls.insert(session_id.to_string(), next);
        Ok(())
    }

    /// 记录输出字节；超出上限返回 [`ExtensionError::BudgetExceeded`]。
    pub fn record_output_bytes(
        &mut self,
        session_id: &str,
        bytes: usize,
    ) -> Result<(), ExtensionError> {
        let next = self.output_bytes.get(session_id).copied().unwrap_or(0) + bytes;
        let limit = self.limits.max_output_bytes_per_session;
        if limit > 0 && next > limit {
            return Err(ExtensionError::BudgetExceeded {
                kind: BudgetKind::OutputBytes,
                limit: limit as u64,
                observed: next as u64,
            });
        }
        self.output_bytes.insert(session_id.to_string(), next);
        Ok(())
    }

    /// 清零一个 session 的记账（例如收到 `session_end` 后）。
    pub fn reset_session(&mut self, session_id: &str) {
        self.calls.remove(session_id);
        self.output_bytes.remove(session_id);
    }

    /// 当前 session 的用量快照；无记录时为零。
    pub fn snapshot(&self, session_id: &str) -> BudgetSnapshot {
        BudgetSnapshot {
            calls: u64::from(self.calls.get(session_id).copied().unwrap_or(0)),
            output_bytes: self.output_bytes.get(session_id).copied().unwrap_or(0) as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_budget() -> ExtensionBudget {
        ExtensionBudget {
            max_calls_per_session: 3,
            max_output_bytes_per_session: 10,
            ..Default::default()
        }
    }

    #[test]
    fn default_budget_is_sane_and_serializable() {
        let budget = ExtensionBudget::default();
        assert!(budget.max_calls_per_session > 0);
        assert!(budget.max_output_bytes_per_session > 0);
        let json = serde_json::to_value(budget).unwrap();
        assert_eq!(json["maxCallsPerSession"], 100_000);
        let back: ExtensionBudget = serde_json::from_value(json).unwrap();
        assert_eq!(back, budget);
    }

    #[test]
    fn records_calls_within_limit() {
        let mut tracker = BudgetTracker::new(small_budget());
        for _ in 0..3 {
            tracker.record_call("s1").unwrap();
        }
        assert_eq!(tracker.snapshot("s1").calls, 3);
    }

    #[test]
    fn call_count_exceeded_is_reported() {
        let mut tracker = BudgetTracker::new(small_budget());
        for _ in 0..3 {
            tracker.record_call("s1").unwrap();
        }
        let err = tracker.record_call("s1").unwrap_err();
        assert!(matches!(
            err,
            ExtensionError::BudgetExceeded {
                kind: BudgetKind::CallCount,
                limit: 3,
                observed: 4
            }
        ));
    }

    #[test]
    fn output_bytes_exceeded_is_reported() {
        let mut tracker = BudgetTracker::new(small_budget());
        tracker.record_output_bytes("s1", 6).unwrap();
        let err = tracker.record_output_bytes("s1", 5).unwrap_err();
        assert!(matches!(
            err,
            ExtensionError::BudgetExceeded {
                kind: BudgetKind::OutputBytes,
                ..
            }
        ));
    }

    #[test]
    fn sessions_are_independent() {
        let mut tracker = BudgetTracker::new(small_budget());
        tracker.record_call("s1").unwrap();
        tracker.record_call("s1").unwrap();
        tracker.record_call("s2").unwrap();
        assert_eq!(tracker.snapshot("s1").calls, 2);
        assert_eq!(tracker.snapshot("s2").calls, 1);
        // s1 已超限失败，s2 仍可用。
        assert!(tracker.record_call("s2").is_ok());
    }

    #[test]
    fn reset_session_clears_usage() {
        let mut tracker = BudgetTracker::new(small_budget());
        for _ in 0..3 {
            tracker.record_call("s1").unwrap();
        }
        tracker.reset_session("s1");
        assert_eq!(tracker.snapshot("s1").calls, 0);
        assert!(tracker.record_call("s1").is_ok());
    }

    #[test]
    fn zero_limit_means_unlimited() {
        let budget = ExtensionBudget {
            max_calls_per_session: 0,
            max_output_bytes_per_session: 0,
            ..Default::default()
        };
        let mut tracker = BudgetTracker::new(budget);
        for _ in 0..10_000 {
            tracker.record_call("s1").unwrap();
            tracker.record_output_bytes("s1", 1).unwrap();
        }
        assert_eq!(tracker.snapshot("s1").calls, 10_000);
    }
}
