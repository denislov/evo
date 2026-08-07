//! LSP edit 转换层：`workspace/applyEdit` 的 `WorkspaceEdit` →
//! 校验 → mutation 计划（`EditPlan`），经注入的 [`EditApplicator`] 受限
//! 应用，生成 `ChangeReceipt`。
//!
//! **本模块绝不直接写磁盘**（LSP 编辑必须经 workspace edit / ChangeReceipt
//! 路径进入 review，见 Phase 8 需求与 `docs/refactor/phase8-lsp.md`）：
//!
//! 1. 校验（fail closed，任一失败整个 edit 拒绝）：
//!    - uri 必须 `file://` + workspace 内（词法逃逸拒绝，同
//!      [`DocumentStore::parse_uri`]）；
//!    - 目标文档必须已打开（`WorkspaceEdit.changes` 形态不带版本，
//!      版本校验只能针对打开文档；未打开文档的编辑拒绝）；
//!    - `documentChanges` 形态携带的版本必须等于文档当前版本
//!      （不匹配 → [`EditError::VersionMismatch`]）；
//!    - range 越界拒绝。
//! 2. 生成 [`EditPlan`]：每文件一行替换（LSP 的 edits 已是数组，按
//!    LSP 语义**不重叠**；多 edit 按逆序应用或顺序应用由 applicator
//!    决定——计划层保持 LSP 原始数组）。
//! 3. 应用：经注入的 [`EditApplicator`] 执行并返回
//!    `Vec<ChangeReceipt>`（ARC-830 接线 coding-agent 的授权 / review
//!    流程时替换为真实实现）；**没有 applicator 时拒绝请求**（结构化
//!    错误，绝不静默吞掉 edit），但计划仍记录供调用方查询。
//!
//! 多文件事务：`EditPlan` 是计划列表，原子性由 applicator 决定（本任务
//! 的受限 applicator 逐文件应用、失败即中止，见债务登记）。

// Evo 独立设计：Grok 的 LSP 工具把 applyEdit 直接转发给编辑基础设施
// （无校验层）；Evo 的「校验 → 计划 → 注入 applicator → ChangeReceipt」
// 转换层为自研（授权边界以 ARC-830 收口）。
use std::collections::BTreeMap;
use std::path::Path;

use change_tracker::ChangeReceipt;
use serde::{Deserialize, Serialize};

use crate::lsp::documents::{DocumentStore, Range};

/// 一个文本编辑（LSP `TextEdit`；`range: None` = 全量替换）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Option<Range>,
    pub new_text: String,
}

/// `documentChanges` 形态的单文件编辑（携带可选版本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextDocumentEdit {
    pub uri: String,
    pub version: Option<i64>,
    pub edits: Vec<TextEdit>,
}

/// `WorkspaceEdit`（只支持 `changes` 与 `documentChanges` 两种形态；
/// `workspaceEdit.operations` 未实现，见债务登记）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkspaceEdit {
    /// uri → edits（LSP `changes` 形态，不带版本）。
    pub changes: BTreeMap<String, Vec<TextEdit>>,
    /// 版本化形态（`documentChanges`，优先用于版本校验）。
    pub document_changes: Vec<TextDocumentEdit>,
}

/// 计划中的一次替换（校验通过后的产物）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedChange {
    /// 完整 uri（`file://…`）。
    pub uri: String,
    /// workspace 相对路径（正斜杠；诊断 / 展示用）。
    pub rel_path: String,
    /// `None` = 全量替换。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
    pub new_text: String,
}

/// 校验通过的 mutation 计划（多文件）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EditPlan {
    pub changes: Vec<PlannedChange>,
}

impl EditPlan {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// edit 校验 / 应用的错误分类。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("workspace edit failed: {detail}")]
    Invalid { detail: String },
    #[error("edit targets document {uri} which is not open")]
    DocumentNotOpen { uri: String },
    #[error("edit version {given} does not match document version {current} for {uri}")]
    VersionMismatch {
        uri: String,
        given: i64,
        current: i64,
    },
    #[error("edit range {range:?} is out of bounds for document {uri}")]
    RangeOutOfBounds { uri: String, range: Range },
    #[error("edit uri {uri} is outside the workspace")]
    OutsideWorkspace { uri: String },
    #[error("no edit applicator configured; edit was rejected (plan recorded)")]
    NoApplicator,
    #[error("edit application failed: {detail}")]
    Apply { detail: String },
}

/// 受限的 edit 应用器：执行 [`EditPlan`] 并返回 `ChangeReceipt` 列表。
///
/// 本 trait 的注入点即授权边界：ARC-830 在此接线 coding-agent 的完整
/// authorization / review 流程。本任务提供测试用受限实现（临时目录内
/// 应用，生成 receipt 语义）。
pub trait EditApplicator: Send + Sync {
    /// 应用计划。实现负责：路径内校验、事务语义、生成 `ChangeReceipt`
    /// （before/after revision、byte/line delta、unified diff）。
    fn apply(&self, plan: &EditPlan) -> Result<Vec<ChangeReceipt>, EditError>;
}

/// 校验 `WorkspaceEdit` 并生成 mutation 计划。
///
/// - `document_changes` 与 `changes` 同时提供时，两者都要校验（LSP 不
///   允许同时出现，但 fail closed：都校验不合并）。
/// - 版本校验：`documentChanges` 携带版本时必须等于文档当前版本；
///   `changes` 形态以文档当前版本为准（不拒绝）。
pub fn validate_edit(
    edit: &WorkspaceEdit,
    documents: &DocumentStore,
) -> Result<EditPlan, EditError> {
    let mut plan = EditPlan::default();
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    for (uri, edits) in &edit.changes {
        if seen.insert(uri.clone(), ()).is_some() {
            return Err(EditError::Invalid {
                detail: format!("duplicate target {uri}"),
            });
        }
        let parsed = parse_target(uri, documents)?;
        plan.changes.extend(plan_changes(parsed, edits, documents)?);
    }
    for document_change in &edit.document_changes {
        let uri = &document_change.uri;
        if seen.insert(uri.clone(), ()).is_some() {
            return Err(EditError::Invalid {
                detail: format!("duplicate target {uri}"),
            });
        }
        let parsed = parse_target(uri, documents)?;
        if let Some(given) = document_change.version {
            let document = documents
                .get(uri)
                .map_err(|_| EditError::DocumentNotOpen { uri: uri.clone() })?;
            if given != document.version {
                return Err(EditError::VersionMismatch {
                    uri: uri.clone(),
                    given,
                    current: document.version,
                });
            }
        }
        plan.changes
            .extend(plan_changes(parsed, &document_change.edits, documents)?);
    }
    Ok(plan)
}

/// 校验并打开目标文档的 uri（workspace 内 + 已打开）。
fn parse_target<'a>(
    uri: &str,
    documents: &'a DocumentStore,
) -> Result<&'a crate::lsp::documents::OpenDocument, EditError> {
    let parsed = documents.parse_uri(uri).map_err(|error| match error {
        crate::lsp::documents::DocumentError::OutsideWorkspace { .. } => {
            EditError::OutsideWorkspace {
                uri: uri.to_string(),
            }
        }
        other => EditError::Invalid {
            detail: other.to_string(),
        },
    })?;
    documents
        .get(uri)
        .map_err(|_| EditError::DocumentNotOpen {
            uri: uri.to_string(),
        })
        .map(|_| parsed)
        .map(|_| documents.get(uri).expect("get after parse ok"))
}

/// 把 edit 数组转成计划（range 越界拒绝，与文档当前文本比对）。
fn plan_changes(
    document: &crate::lsp::documents::OpenDocument,
    edits: &[TextEdit],
    documents: &DocumentStore,
) -> Result<Vec<PlannedChange>, EditError> {
    let mut planned = Vec::with_capacity(edits.len());
    let mut cursor = 0usize;
    for edit in edits {
        // LSP 保证 edits 按位置升序且不重叠；校验顺序递增（同一位置允许
        // 插入）。range 越界拒绝。
        if let Some(range) = edit.range {
            let start = position_index(document, range.start, documents)?;
            let end = position_index(document, range.end, documents)?;
            if start > end || start < cursor {
                return Err(EditError::RangeOutOfBounds {
                    uri: document.uri.as_str().to_string(),
                    range,
                });
            }
            cursor = start;
        }
        planned.push(PlannedChange {
            uri: document.uri.as_str().to_string(),
            rel_path: relative_path(document.uri.abs_path(), documents.workspace_root()),
            range: edit.range,
            new_text: edit.new_text.clone(),
        });
    }
    Ok(planned)
}

fn position_index(
    document: &crate::lsp::documents::OpenDocument,
    position: crate::lsp::documents::Position,
    _documents: &DocumentStore,
) -> Result<usize, EditError> {
    let text = &document.text;
    let mut current_line = 0u32;
    let mut offset = 0usize;
    for (char_index, character) in text.char_indices() {
        if current_line == position.line {
            let line_text = &text[offset..];
            let target =
                crate::lsp::documents::utf16_to_char_index(line_text, position.character as usize);
            if target > line_text.chars().count() {
                return Err(EditError::RangeOutOfBounds {
                    uri: document.uri.as_str().to_string(),
                    range: crate::lsp::documents::Range {
                        start: position,
                        end: position,
                    },
                });
            }
            // char 索引 → 行内字节偏移（UTF-8 变长编码）。
            let byte_offset = line_text
                .char_indices()
                .nth(target)
                .map(|(byte, _)| byte)
                .unwrap_or(line_text.len());
            return Ok(char_index + byte_offset);
        }
        if character == '\n' {
            current_line += 1;
            offset = char_index + 1;
        }
    }
    if current_line == position.line {
        let line_text = &text[offset..];
        let target =
            crate::lsp::documents::utf16_to_char_index(line_text, position.character as usize);
        if target <= line_text.chars().count() {
            let byte_offset = line_text
                .char_indices()
                .nth(target)
                .map(|(byte, _)| byte)
                .unwrap_or(line_text.len());
            return Ok(offset + byte_offset);
        }
    }
    Err(EditError::RangeOutOfBounds {
        uri: document.uri.as_str().to_string(),
        range: crate::lsp::documents::Range {
            start: position,
            end: position,
        },
    })
}

/// workspace 相对路径（正斜杠）。
pub fn relative_path(abs_path: &Path, workspace_root: &Path) -> String {
    abs_path
        .strip_prefix(workspace_root)
        .unwrap_or(abs_path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// 解析 `workspace/applyEdit` 请求 params 为 [`WorkspaceEdit`]。
///
/// 支持 `changes`（BTreeMap<String uri, Vec<TextEdit>>）与
/// `documentChanges`（数组，每项 `{textDocument: {uri, version?}, edits}`）。
pub fn parse_apply_edit_params(params: &serde_json::Value) -> Result<WorkspaceEdit, EditError> {
    let mut edit = WorkspaceEdit::default();
    if let Some(changes) = params.get("changes").and_then(serde_json::Value::as_object) {
        for (uri, edits_value) in changes {
            edit.changes.insert(
                uri.clone(),
                parse_edits(edits_value).map_err(|detail| EditError::Invalid {
                    detail: format!("changes[{uri}]: {detail}"),
                })?,
            );
        }
    }
    if let Some(document_changes) = params
        .get("documentChanges")
        .and_then(serde_json::Value::as_array)
    {
        for (index, entry) in document_changes.iter().enumerate() {
            let text_document = entry
                .get("textDocument")
                .ok_or_else(|| EditError::Invalid {
                    detail: format!("documentChanges[{index}] missing textDocument"),
                })?;
            let uri = text_document
                .get("uri")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| EditError::Invalid {
                    detail: format!("documentChanges[{index}] missing uri"),
                })?
                .to_string();
            let version = text_document
                .get("version")
                .and_then(serde_json::Value::as_i64);
            let edits = parse_edits(entry.get("edits").ok_or_else(|| EditError::Invalid {
                detail: format!("documentChanges[{index}] missing edits"),
            })?)
            .map_err(|detail| EditError::Invalid {
                detail: format!("documentChanges[{index}]: {detail}"),
            })?;
            edit.document_changes.push(TextDocumentEdit {
                uri,
                version,
                edits,
            });
        }
    }
    if edit.changes.is_empty() && edit.document_changes.is_empty() {
        return Err(EditError::Invalid {
            detail: "empty workspace edit".into(),
        });
    }
    Ok(edit)
}

fn parse_edits(value: &serde_json::Value) -> Result<Vec<TextEdit>, String> {
    let array = value
        .as_array()
        .ok_or_else(|| "edits must be an array".to_string())?;
    let mut edits = Vec::with_capacity(array.len());
    for item in array {
        let new_text = item
            .get("newText")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "edit missing newText".to_string())?
            .to_string();
        let range = match item.get("range") {
            // `null`（全量替换）与缺失等价；非 null 必须可解析。
            Some(range) if !range.is_null() => Some(
                serde_json::from_value(range.clone())
                    .map_err(|error| format!("invalid range: {error}"))?,
            ),
            _ => None,
        };
        edits.push(TextEdit { range, new_text });
    }
    Ok(edits)
}

/// 把 [`ContentChange`] 组装成 LSP `didChange` 通知 params
/// （**全量**同步：`range: null` + 最新文本，见 documents.rs 模块注释）。
pub fn did_change_params(uri: &str, version: i64, text: &str) -> serde_json::Value {
    serde_json::json!({
        "textDocument": {"uri": uri, "version": version},
        "contentChanges": [{"text": text}],
    })
}

/// 组装 `didOpen` 通知 params。
pub fn did_open_params(
    uri: &str,
    language_id: &str,
    version: i64,
    text: &str,
) -> serde_json::Value {
    serde_json::json!({
        "textDocument": {
            "uri": uri,
            "languageId": language_id,
            "version": version,
            "text": text,
        }
    })
}

/// 组装 `didClose` 通知 params。
pub fn did_close_params(uri: &str) -> serde_json::Value {
    serde_json::json!({"textDocument": {"uri": uri}})
}

/// 受限应用器的最小产物（供测试断言；真实 applicator 由 ARC-830 注入）。
pub mod restricted {
    use sha2::{Digest, Sha256};

    /// 计算 revision（sha256 内容哈希，ChangeReceipt 语义，
    /// `format!("{:x}", Sha256::digest(bytes))`，与 change-tracker /
    /// workspace-runtime 的 revision 约定一致）。
    pub fn revision_of(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
}
