//! 结构化诊断输出。
//!
//! host 的错误 / 预算 / 生命周期信号统一落成 [`DiagnosticRecord`]
//! （severity + 稳定 code + 上下文 map），经有界环形缓冲保留快照，
//! 同时转发到可选 [`DiagnosticSink`]（ARC-710 接产品事件 / 日志）。

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// 诊断级别。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    #[default]
    Debug,
    Info,
    Warning,
    Error,
}

/// 一条结构化诊断记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticRecord {
    pub level: DiagnosticLevel,
    /// 稳定机器可读 code（例如 `budget_exceeded`、`manifest_invalid`）。
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    #[serde(default)]
    pub context: BTreeMap<String, String>,
}

/// 诊断输出目标抽象。ARC-710 由产品实现（事件 / 日志通道）。
pub trait DiagnosticSink: std::fmt::Debug + Send + Sync {
    fn emit(&self, record: DiagnosticRecord);
}

/// 空 sink：丢弃所有记录。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopDiagnosticSink;

impl DiagnosticSink for NoopDiagnosticSink {
    fn emit(&self, _record: DiagnosticRecord) {}
}

/// 有界环形缓冲 + 可选 sink 的诊断收集器。非线程安全，由持有方（host 的
/// 共享状态）加锁访问。
#[derive(Debug, Clone)]
pub struct DiagnosticsCollector {
    sink: Option<Arc<dyn DiagnosticSink>>,
    buffer: VecDeque<DiagnosticRecord>,
    capacity: usize,
}

impl DiagnosticsCollector {
    pub fn new(sink: Option<Arc<dyn DiagnosticSink>>, capacity: usize) -> Self {
        Self {
            sink,
            buffer: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    /// 记录一条诊断；超过容量时丢弃最老的记录。
    pub fn record(&mut self, record: DiagnosticRecord) {
        if let Some(sink) = &self.sink {
            sink.emit(record.clone());
        }
        self.buffer.push_back(record);
        while self.buffer.len() > self.capacity {
            self.buffer.pop_front();
        }
    }

    /// 快照全部保留记录（最老在前）。
    pub fn snapshot(&self) -> Vec<DiagnosticRecord> {
        self.buffer.iter().cloned().collect()
    }

    /// 取走全部记录并清空。
    pub fn drain(&mut self) -> Vec<DiagnosticRecord> {
        self.buffer.drain(..).collect()
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(level: DiagnosticLevel, code: &str) -> DiagnosticRecord {
        DiagnosticRecord {
            level,
            code: code.into(),
            message: format!("{code} happened"),
            extension_id: None,
            context: BTreeMap::new(),
        }
    }

    #[test]
    fn records_are_kept_in_order() {
        let mut collector = DiagnosticsCollector::new(None, 4);
        collector.record(record(DiagnosticLevel::Info, "one"));
        collector.record(record(DiagnosticLevel::Error, "two"));
        let snapshot = collector.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].code, "one");
        assert_eq!(snapshot[1].code, "two");
    }

    #[test]
    fn ring_buffer_drops_oldest_over_capacity() {
        let mut collector = DiagnosticsCollector::new(None, 2);
        collector.record(record(DiagnosticLevel::Debug, "a"));
        collector.record(record(DiagnosticLevel::Debug, "b"));
        collector.record(record(DiagnosticLevel::Error, "c"));
        let snapshot = collector.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].code, "b");
        assert_eq!(snapshot[1].code, "c");
    }

    #[test]
    fn drain_clears_buffer() {
        let mut collector = DiagnosticsCollector::new(None, 4);
        collector.record(record(DiagnosticLevel::Info, "a"));
        let drained = collector.drain();
        assert_eq!(drained.len(), 1);
        assert!(collector.is_empty());
    }

    #[test]
    fn sink_receives_copies() {
        let sink = Arc::new(CollectingSink::default());
        let mut collector = DiagnosticsCollector::new(Some(sink.clone()), 4);
        collector.record(record(DiagnosticLevel::Warning, "w"));
        assert_eq!(sink.records.lock().unwrap().len(), 1);
        assert_eq!(collector.len(), 1);
    }

    #[test]
    fn zero_capacity_clamps_to_one() {
        let mut collector = DiagnosticsCollector::new(None, 0);
        collector.record(record(DiagnosticLevel::Info, "a"));
        collector.record(record(DiagnosticLevel::Error, "b"));
        assert_eq!(collector.len(), 1);
        assert_eq!(collector.snapshot()[0].code, "b");
    }

    #[test]
    fn record_round_trips_via_json() {
        let r = record(DiagnosticLevel::Error, "budget_exceeded");
        let value = serde_json::to_value(&r).unwrap();
        assert_eq!(value["level"], "error");
        let back: DiagnosticRecord = serde_json::from_value(value).unwrap();
        assert_eq!(back, r);
    }

    #[derive(Default)]
    struct CollectingSink {
        records: std::sync::Mutex<Vec<DiagnosticRecord>>,
    }

    impl std::fmt::Debug for CollectingSink {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CollectingSink").finish_non_exhaustive()
        }
    }

    impl DiagnosticSink for CollectingSink {
        fn emit(&self, record: DiagnosticRecord) {
            self.records.lock().unwrap().push(record);
        }
    }
}
