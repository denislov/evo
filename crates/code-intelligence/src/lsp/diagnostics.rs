//! LSP diagnostics：push（`publishDiagnostics` 通知）存储 + pull
//! （`textDocument/pullDiagnostics` 请求）+ **stale policy**。
//!
//! 诊断与 document 版本的对应关系（状态机，转换表由测试钉死）：
//!
//! ```text
//!                         publish(version == doc)            doc change
//!   (uri, doc_version) ─────────────────────► Fresh(doc_version) ──► Stale
//!         │  publish(version != doc)                                  │
//!         ├──────────────────────────────► Stale{version}             │
//!         │  publish(no version)                                      │
//!         └──────────────────────────────► Unknown ───────────────────► Stale
//! ```
//!
//! - `Fresh { doc_version }`：服务器推送带的版本与当前 document 版本一致，
//!   内容可信。
//! - `Stale { reason }`：版本不匹配（服务器版本落后 / 超前）或文档在诊断
//!   之后被修改——诊断描述的不是当前内容。
//! - `Unknown`：服务器不携带版本；文档一旦变化自动转 `Stale`。
//!
//! [`StalePolicy::Discard`] 下查询只返回 `Fresh` 条目（stale/unknown
//! 丢弃）；[`StalePolicy::Mark`] 下返回全部并带标记。
//!
//! push 与 pull 共用同一个 store：pull 响应同样按版本入库（pull 的响应
//! 带 `version` 时走 Fresh/Stale 判定；不带时走 Unknown）。

// Evo 独立设计：Grok 的 LSP 诊断走 async-lsp 的 `publishDiagnostics`
// handler 直接转发给 UI，无版本状态机；stale policy 为 Evo 自研。
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::lsp::documents::{DocumentStore, DocumentUri};

/// 诊断严重程度（LSP `DiagnosticSeverity`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// 一条诊断（LSP `Diagnostic` 的简化投影：位置 + 严重度 + 消息 +
/// 来源/代码）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticItem {
    pub range: crate::lsp::documents::Range,
    pub severity: DiagnosticSeverity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// 诊断 staleness（与 document 版本的对应关系）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStaleness {
    /// 与当前 document 版本一致。
    Fresh { doc_version: i64 },
    /// 版本不匹配 / 文档已变化。
    Stale { reason: String },
    /// 服务器未携带版本；文档变化后自动转 Stale。
    Unknown,
}

impl DiagnosticStaleness {
    pub fn is_fresh(&self) -> bool {
        matches!(self, DiagnosticStaleness::Fresh { .. })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            DiagnosticStaleness::Fresh { .. } => "fresh",
            DiagnosticStaleness::Stale { .. } => "stale",
            DiagnosticStaleness::Unknown => "unknown",
        }
    }
}

/// 一个 uri 的诊断条目（一次 `publishDiagnostics` 的完整替换）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDiagnostics {
    pub uri: DocumentUri,
    /// 服务器声明版本（`publishDiagnostics.version`，可能缺失）。
    pub version: Option<i64>,
    pub staleness: DiagnosticStaleness,
    pub items: Vec<DiagnosticItem>,
}

/// stale 查询策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StalePolicy {
    /// 返回全部诊断 + 每个条目的 staleness 标记。
    Mark,
    /// 丢弃 stale/unknown，只返回与当前版本一致的诊断。
    Discard,
}

/// push/pull 诊断存储。
#[derive(Debug, Clone, Default)]
pub struct DiagnosticStore {
    entries: BTreeMap<DocumentUri, StoredDiagnostics>,
}

/// 纯函数：给定服务器推送版本与当前文档版本，判定 staleness。
pub fn staleness_of(published_version: Option<i64>, doc_version: i64) -> DiagnosticStaleness {
    match published_version {
        Some(version) if version == doc_version => DiagnosticStaleness::Fresh { doc_version },
        Some(version) if version < doc_version => DiagnosticStaleness::Stale {
            reason: format!("server version {version} is behind document version {doc_version}"),
        },
        Some(version) => DiagnosticStaleness::Stale {
            reason: format!("server version {version} is ahead of document version {doc_version}"),
        },
        None => DiagnosticStaleness::Unknown,
    }
}

/// 纯函数：文档版本变化后，旧的 staleness 如何转移（状态机转换）。
pub fn staleness_after_doc_change(
    before: &DiagnosticStaleness,
    new_doc_version: i64,
) -> DiagnosticStaleness {
    match before {
        DiagnosticStaleness::Fresh { doc_version } if *doc_version == new_doc_version => {
            DiagnosticStaleness::Fresh {
                doc_version: new_doc_version,
            }
        }
        DiagnosticStaleness::Fresh { doc_version } => DiagnosticStaleness::Stale {
            reason: format!(
                "document changed to version {new_doc_version} after diagnostics for version {doc_version}"
            ),
        },
        DiagnosticStaleness::Stale { .. } => before.clone(),
        DiagnosticStaleness::Unknown => DiagnosticStaleness::Stale {
            reason: "document changed but diagnostics carry no version".into(),
        },
    }
}

impl DiagnosticStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 入库一次推送（完整替换该 uri 的条目）。
    pub fn publish(
        &mut self,
        uri: DocumentUri,
        version: Option<i64>,
        items: Vec<DiagnosticItem>,
        doc_version: i64,
    ) {
        let staleness = staleness_of(version, doc_version);
        self.entries.insert(
            uri.clone(),
            StoredDiagnostics {
                uri,
                version,
                staleness,
                items,
            },
        );
    }

    /// 文档变化：更新该 uri（及全部 uri，保守起见）的 staleness。
    /// 只影响显式 uri 的条目（每文档独立状态机）。
    pub fn document_changed(&mut self, uri: &DocumentUri, new_version: i64) {
        if let Some(entry) = self.entries.get_mut(uri) {
            entry.staleness = staleness_after_doc_change(&entry.staleness, new_version);
        }
    }

    /// 文档关闭：删除其诊断（诊断描述的内容已不存在）。
    pub fn document_closed(&mut self, uri: &DocumentUri) -> Option<StoredDiagnostics> {
        self.entries.remove(uri)
    }

    /// 按 stale 策略查询一个 uri 的诊断。
    pub fn query(&self, uri: &DocumentUri, policy: StalePolicy) -> Option<&StoredDiagnostics> {
        let entry = self.entries.get(uri)?;
        match policy {
            StalePolicy::Mark => Some(entry),
            StalePolicy::Discard => entry.staleness.is_fresh().then_some(entry),
        }
    }

    /// 全部条目的 staleness 视图（调试 / 快照）。
    pub fn summary(&self) -> Vec<StoredDiagnostics> {
        self.entries.values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 从 `publishDiagnostics` 通知 params 解析条目。
pub fn parse_publish_params(
    params: &serde_json::Value,
) -> Option<(String, Option<i64>, Vec<DiagnosticItem>)> {
    let uri = params.get("uri")?.as_str()?.to_string();
    let version = params.get("version").and_then(serde_json::Value::as_i64);
    let items = parse_diagnostics(params.get("diagnostics")?)?;
    Some((uri, version, items))
}

fn parse_diagnostics(value: &serde_json::Value) -> Option<Vec<DiagnosticItem>> {
    let array = value.as_array()?;
    let mut items = Vec::with_capacity(array.len());
    for item in array {
        let severity = match item.get("severity").and_then(serde_json::Value::as_u64) {
            Some(1) => DiagnosticSeverity::Error,
            Some(2) => DiagnosticSeverity::Warning,
            Some(3) => DiagnosticSeverity::Information,
            Some(4) => DiagnosticSeverity::Hint,
            _ => DiagnosticSeverity::Error,
        };
        items.push(DiagnosticItem {
            range: serde_json::from_value(item.get("range")?.clone()).ok()?,
            severity,
            message: item.get("message")?.as_str()?.to_string(),
            source: item
                .get("source")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            code: item.get("code").and_then(|code| {
                code.as_str()
                    .map(str::to_string)
                    .or_else(|| code.as_i64().map(|n| n.to_string()))
            }),
        });
    }
    Some(items)
}

/// 构造 `textDocument/pullDiagnostics` 请求 params。
pub fn pull_params(uri: &str, previous_result_id: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "textDocument": {"uri": uri},
        "resultId": previous_result_id,
    })
}

/// 从 `textDocument/pullDiagnostics` 响应 result 解析条目
/// （`items` + 可选 `resultId`）。
pub fn parse_pull_result(
    result: &serde_json::Value,
) -> Option<(Vec<DiagnosticItem>, Option<String>)> {
    let items = parse_diagnostics(result.get("items")?)?;
    let result_id = result
        .get("resultId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    Some((items, result_id))
}

/// 用 `DocumentStore` 刷新全部条目的 staleness（启动 / 恢复时用）。
pub fn refresh_all(store: &mut DiagnosticStore, documents: &DocumentStore) {
    for entry in store.entries.values_mut() {
        if let Ok(document) = documents.get(entry.uri.as_str()) {
            entry.staleness = staleness_after_doc_change(&entry.staleness, document.version);
        }
    }
}
