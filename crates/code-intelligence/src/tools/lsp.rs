//! `code_lsp`：LSP 的 read-only 查询工具（ARC-830）。
//!
//! 经 [`crate::lsp::server::LspHandle::query`] 提交 hover / definition /
//! references 查询（位置为 LSP 契约：**0-indexed** 行 / 列，`character`
//! 是 UTF-16 code unit）。服务未就绪 / 已关闭 / 重启中 → 结构化
//! `Unavailable`；取消令牌经 `tokio::select!` 贯通（取消 → `Cancelled`）。
//!
//! 工具划分决策（与 `code_graph` 分开）：LSP 是子进程服务——生命周期
//! （start/restart/backoff）与延迟特征（async 网络往返）都和内存索引
//! 不同，独立 ToolCapabilities 与失败模式；模型按需求选择调用。
//!
//! 结果语义映射（ARC-820 债务偿还）：hover 提取 markdown 文本、
//! definition/references 归一化为位置数组；输出预算截断显式标记。

// Evo 独立设计（Grok 的 LSP 工具直接调 async-lsp client；Evo 的
// 查询面 + 语义映射为自研，见 docs/refactor/phase8-lsp.md）。
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::json;
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind,
};
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind, ToolOutput};
use tool_runtime::api::{DynamicTool, ToolCallContext, ToolFuture};

use crate::lsp::query::{LspQuery, LspQueryKind, LspQueryResult};
use crate::lsp::server::LspError;
use crate::lsp::server::LspHandle;

use super::budget::QueryOutputBudget;

/// 静态工具 id。
pub const CODE_LSP_TOOL_ID: &str = "code_lsp";

/// `code_lsp` 参数。
#[derive(Debug, Clone, Deserialize)]
pub struct CodeLspArgs {
    /// 查询类型：`hover` / `definition` / `references`。
    pub query: String,
    /// workspace-relative 路径（相对 `workspace_root`）。
    pub path: String,
    /// 0-indexed 行号（LSP 契约）。
    pub line: u64,
    /// 0-indexed 列号（UTF-16 code unit，LSP 契约）。
    pub character: u64,
    /// 输出条数 / 字节上限覆盖。
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `code_lsp` 工具。
pub struct CodeLspTool {
    definition: ToolDefinition,
    handle: LspHandle,
    workspace_root: PathBuf,
    budget: QueryOutputBudget,
}

/// 构造 `code_lsp` 工具（默认输出预算）。
pub fn code_lsp_tool(handle: LspHandle, workspace_root: PathBuf) -> Arc<dyn DynamicTool> {
    code_lsp_tool_with_budget(handle, workspace_root, QueryOutputBudget::default())
}

/// 构造 `code_lsp` 工具（自定义输出预算）。
pub fn code_lsp_tool_with_budget(
    handle: LspHandle,
    workspace_root: PathBuf,
    budget: QueryOutputBudget,
) -> Arc<dyn DynamicTool> {
    Arc::new(CodeLspTool {
        definition: lsp_definition(),
        handle,
        workspace_root,
        budget,
    })
}

fn lsp_definition() -> ToolDefinition {
    ToolDefinition {
        id: ToolId::new(CODE_LSP_TOOL_ID).expect("static tool id is valid"),
        kind: ToolKind::Function,
        description: "Query the language server for the current workspace (read-only): \
                      hover documentation, go-to-definition, or find references. Positions \
                      are 0-indexed (line, character in UTF-16 code units) per the LSP \
                      protocol; paths are workspace-relative."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "enum": ["hover", "definition", "references"],
                          "description": "Query type."},
                "path": {"type": "string", "description": "Workspace-relative file path."},
                "line": {"type": "integer", "description": "0-indexed line."},
                "character": {"type": "integer", "description": "0-indexed column (UTF-16 code units)."},
                "limit": {"type": "integer", "description": "Result count cap override."}
            },
            "required": ["query", "path", "line", "character"]
        }),
        capabilities: ToolCapabilities {
            read_only: true,
            execution: ToolExecutionMode::Parallel,
            cancel: true,
            timeout: true,
            streaming: false,
            provider_executed: false,
        },
        behavior: ToolBehaviorVersion::V1,
        authorization_risk: AuthorizationRisk::WorkspaceLocalReadOnly,
        requirements: Vec::new(),
    }
}

impl DynamicTool for CodeLspTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn execute(&self, context: ToolCallContext, arguments: serde_json::Value) -> ToolFuture {
        let handle = self.handle.clone();
        let root = self.workspace_root.clone();
        let budget = self.budget;
        Box::pin(run_lsp_query(
            handle,
            root,
            budget,
            context,
            parse_args(arguments),
        ))
    }
}

fn parse_args(arguments: serde_json::Value) -> Result<CodeLspArgs, ToolError> {
    serde_json::from_value(arguments).map_err(|error| {
        ToolError::new(
            ToolErrorKind::InvalidArguments,
            format!("invalid tool arguments: {error}"),
        )
    })
}

async fn run_lsp_query(
    handle: LspHandle,
    workspace_root: PathBuf,
    budget: QueryOutputBudget,
    context: ToolCallContext,
    args: Result<CodeLspArgs, ToolError>,
) -> Result<ToolOutput, ToolError> {
    let args = args?;
    let effective_limit = args.limit.unwrap_or(budget.max_items);
    let query = build_query(&args, &workspace_root)?;
    let result = tokio::select! {
        biased;
        _ = context.cancel.cancelled() => {
            return Err(ToolError::new(ToolErrorKind::Cancelled, "code_lsp query cancelled"));
        }
        result = handle.query(query) => result,
    }
    .map_err(|error| lsp_error_to_tool(&error))?;
    let payload = result_to_payload(&result, effective_limit, budget.max_bytes)?;
    Ok(ToolOutput {
        content: vec![ToolContent::Json { value: payload }],
        ..Default::default()
    })
}

/// 参数 → LSP 查询（校验失败返回 `InvalidArguments`）。
fn build_query(
    args: &CodeLspArgs,
    workspace_root: &std::path::Path,
) -> Result<LspQuery, ToolError> {
    let invalid = |message: String| {
        ToolError::new(
            ToolErrorKind::InvalidArguments,
            format!("code_lsp: {message}"),
        )
    };
    let kind = match args.query.as_str() {
        "hover" => LspQueryKind::Hover,
        "definition" => LspQueryKind::Definition,
        "references" => LspQueryKind::References,
        other => return Err(invalid(format!("unknown query type '{other}'"))),
    };
    if args.path.trim().is_empty() || args.path.contains("..") {
        return Err(invalid(
            "'path' must be a non-empty workspace-relative path".into(),
        ));
    }
    Ok(LspQuery {
        kind,
        uri: file_uri(workspace_root, &args.path),
        position: crate::lsp::documents::Position {
            line: args.line as u32,
            character: args.character as u32,
        },
    })
}

/// workspace-relative 路径 → `file://` URI（不透明字符百分号编码）。
pub fn file_uri(workspace_root: &std::path::Path, rel_path: &str) -> String {
    let joined = workspace_root.join(rel_path);
    let raw = joined.to_string_lossy().replace('\\', "/");
    let mut encoded = String::new();
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("file://{encoded}")
}

/// 查询结果 → 预算内 JSON 载荷。
fn result_to_payload(
    result: &LspQueryResult,
    limit: usize,
    max_bytes: usize,
) -> Result<serde_json::Value, ToolError> {
    let mut payload = serde_json::Map::new();
    payload.insert("query".into(), json!(result.kind.as_str()));
    payload.insert("path".into(), json!(result.uri));
    let truncated = match result.kind {
        LspQueryKind::Hover => {
            let text = hover_text(&result.result);
            let (kept, cut) = truncate_text(text.as_deref(), max_bytes);
            payload.insert("result".into(), json!(kept));
            cut
        }
        LspQueryKind::Definition | LspQueryKind::References => {
            let locations = normalize_locations(&result.result);
            let total = locations.len();
            let (items, cut) = if limit == 0 || locations.len() <= limit {
                (locations, false)
            } else {
                (locations[..limit].to_vec(), true)
            };
            payload.insert("results".into(), serde_json::Value::Array(items));
            payload.insert("count".into(), json!(total));
            if cut {
                payload.insert("truncated_to".into(), json!(limit));
            }
            cut
        }
    };
    if truncated {
        payload.insert("truncated".into(), json!(true));
    }
    Ok(serde_json::Value::Object(payload))
}

/// hover 结果 → 纯文本（markdown / 纯字符串 / 多段拼接；无内容 → `None`）。
pub fn hover_text(result: &serde_json::Value) -> Option<String> {
    if result.is_null() {
        return None;
    }
    let contents = result.get("contents")?;
    let mut parts = Vec::new();
    match contents {
        serde_json::Value::String(text) => parts.push(text.clone()),
        serde_json::Value::Object(_) => {
            if let Some(value) = contents.get("value").and_then(serde_json::Value::as_str) {
                parts.push(value.to_string());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(value) = item.get("value").and_then(serde_json::Value::as_str) {
                    parts.push(value.to_string());
                } else if let Some(value) = item.as_str() {
                    parts.push(value.to_string());
                }
            }
        }
        _ => {}
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\n"))
}

/// definition / references 结果 → 位置数组（`null` → 空数组）。
pub fn normalize_locations(result: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut locations = Vec::new();
    match result {
        serde_json::Value::Null => {}
        serde_json::Value::Array(items) => {
            for item in items {
                if item.is_object() {
                    locations.push(item.clone());
                }
            }
        }
        serde_json::Value::Object(location) => {
            locations.push(serde_json::Value::Object(location.clone()))
        }
        _ => {}
    }
    locations
}

/// 文本字节截断：超限截断并在尾部显式标记。
pub fn truncate_text(text: Option<&str>, max_bytes: usize) -> (Option<String>, bool) {
    let Some(text) = text else {
        return (None, false);
    };
    if max_bytes == 0 || text.len() <= max_bytes {
        return (Some(text.to_string()), false);
    }
    const MARKER: &str = "\n…[truncated]";
    let budget = max_bytes.saturating_sub(MARKER.len());
    let mut end = budget.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (Some(format!("{}{MARKER}", &text[..end])), true)
}

/// LSP 错误 → 工具错误（结构化类别）。
fn lsp_error_to_tool(error: &LspError) -> ToolError {
    match error {
        LspError::NotRunning | LspError::ShuttingDown { .. } => {
            ToolError::new(ToolErrorKind::Unavailable, "lsp service is not running")
        }
        LspError::NotReady { state } => ToolError::new(
            ToolErrorKind::Unavailable,
            format!("lsp server is not ready (state: {state})"),
        ),
        LspError::AlreadyRunning => {
            ToolError::new(ToolErrorKind::Unavailable, "lsp service is already running")
        }
        LspError::QueryPanicked => {
            ToolError::new(ToolErrorKind::Execution, "lsp query processing panicked")
        }
        other => ToolError::new(
            ToolErrorKind::Execution,
            format!("lsp query failed: {other}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(
        query: &str,
        path: &str,
        line: u64,
        character: u64,
        limit: Option<usize>,
    ) -> CodeLspArgs {
        CodeLspArgs {
            query: query.into(),
            path: path.into(),
            line,
            character,
            limit,
        }
    }

    #[test]
    fn definition_is_valid_and_read_only() {
        let definition = lsp_definition();
        definition.validate().expect("valid definition");
        assert_eq!(definition.id.as_str(), CODE_LSP_TOOL_ID);
        assert!(definition.capabilities.read_only);
        assert_eq!(
            definition.authorization_risk,
            AuthorizationRisk::WorkspaceLocalReadOnly
        );
    }

    #[test]
    fn build_query_rejects_unknown_type_and_empty_path() {
        let root = PathBuf::from("/ws");
        let err = build_query(&args("bogus", "a.rs", 0, 0, None), &root).unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::InvalidArguments);
        let err = build_query(&args("hover", "  ", 0, 0, None), &root).unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::InvalidArguments);
    }

    #[test]
    fn build_query_encodes_uri_and_positions() {
        let root = PathBuf::from("/ws");
        let query = build_query(&args("references", "src/a b.rs", 3, 7, None), &root).unwrap();
        assert_eq!(query.kind, LspQueryKind::References);
        assert_eq!(query.uri, "file:///ws/src/a%20b.rs");
        assert_eq!(query.position.line, 3);
        assert_eq!(query.position.character, 7);
    }

    #[test]
    fn file_uri_encodes_opaque_characters() {
        assert_eq!(file_uri(&PathBuf::from("/ws"), "a.rs"), "file:///ws/a.rs");
        assert_eq!(
            file_uri(&PathBuf::from("/ws"), "dir/a b#c.rs"),
            "file:///ws/dir/a%20b%23c.rs"
        );
    }

    #[test]
    fn hover_text_extracts_markdown_and_strings() {
        assert_eq!(hover_text(&serde_json::Value::Null), None);
        assert_eq!(
            hover_text(&json!({"contents": {"kind": "markdown", "value": "docs"}})),
            Some("docs".into())
        );
        assert_eq!(
            hover_text(&json!({"contents": "plain"})),
            Some("plain".into())
        );
        assert_eq!(
            hover_text(&json!({"contents": [{"value": "a"}, {"value": "b"}]})),
            Some("a\nb".into())
        );
        assert_eq!(hover_text(&json!({"contents": 42})), None);
    }

    #[test]
    fn normalize_locations_handles_null_single_and_array() {
        assert!(normalize_locations(&serde_json::Value::Null).is_empty());
        let single = normalize_locations(&json!({"uri": "file:///ws/a.rs", "range": {}}));
        assert_eq!(single.len(), 1);
        let many =
            normalize_locations(&json!([{"uri": "file:///ws/a.rs"}, {"uri": "file:///ws/b.rs"}]));
        assert_eq!(many.len(), 2);
        assert!(normalize_locations(&json!([1, 2])).is_empty());
    }

    #[test]
    fn truncate_text_marks_explicitly() {
        let long = "x".repeat(100);
        let (kept, cut) = truncate_text(Some(&long), 32);
        assert!(cut);
        let kept = kept.unwrap();
        assert!(kept.len() <= 32);
        assert!(kept.ends_with("[truncated]"));
        let (kept, cut) = truncate_text(Some("short"), 32);
        assert_eq!(kept, Some("short".into()));
        assert!(!cut);
        let (kept, cut) = truncate_text(None, 32);
        assert_eq!(kept, None);
        assert!(!cut);
    }

    #[test]
    fn truncate_text_respects_char_boundaries() {
        let long = "汉".repeat(50);
        let (kept, _) = truncate_text(Some(&long), 20);
        let kept = kept.unwrap();
        assert!(kept.len() <= 20);
        assert!(kept.is_char_boundary(kept.len()));
    }

    #[test]
    fn payload_marks_truncation_and_counts() {
        let result = LspQueryResult {
            kind: LspQueryKind::References,
            uri: "file:///ws/a.rs".into(),
            result: json!([
                {"uri": "file:///ws/a.rs", "range": {}},
                {"uri": "file:///ws/b.rs", "range": {}},
                {"uri": "file:///ws/c.rs", "range": {}}
            ]),
        };
        let payload = result_to_payload(&result, 2, 0).unwrap();
        assert_eq!(payload["count"], 3);
        assert_eq!(payload["results"].as_array().unwrap().len(), 2);
        assert_eq!(payload["truncated"], true);
        assert_eq!(payload["truncated_to"], 2);
    }

    #[test]
    fn payload_within_budget_is_not_truncated() {
        let result = LspQueryResult {
            kind: LspQueryKind::Hover,
            uri: "file:///ws/a.rs".into(),
            result: json!({"contents": {"kind": "markdown", "value": "hello"}}),
        };
        let payload = result_to_payload(&result, 0, 0).unwrap();
        assert_eq!(payload["result"], "hello");
        assert!(payload.get("truncated").is_none());
    }

    #[test]
    fn lsp_errors_map_to_structured_categories() {
        assert_eq!(
            lsp_error_to_tool(&LspError::NotRunning).kind,
            ToolErrorKind::Unavailable
        );
        assert_eq!(
            lsp_error_to_tool(&LspError::NotReady {
                state: "failed".into()
            })
            .kind,
            ToolErrorKind::Unavailable
        );
        assert_eq!(
            lsp_error_to_tool(&LspError::QueryPanicked).kind,
            ToolErrorKind::Execution
        );
    }
}
