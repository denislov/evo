//! `code_graph`：codebase graph 的 read-only 查询工具（ARC-830）。
//!
//! 经 [`crate::service::CodeIntelligenceHandle`] 提交查询（服务未启动 /
//! 关闭中 → 结构化 `Unavailable`；图查询错误按类别映射：
//! 参数问题 → `InvalidArguments`，文件/索引缺失 → `Unavailable`）。
//! 取消令牌经 `tokio::select!` 贯通到查询（取消 → `Cancelled`）。
//!
//! 查询模式（`query` 字段）：
//!
//! - `symbols`：按文件列符号（containment 树），`path` 必填；
//! - `definitions`：`symbol` 或 `path`+`line`+`column`（1-indexed）；
//! - `references`：同上，可选 `include_definition`；
//! - `search`：按名称片段搜索符号（相关度排序），`symbol` 必填。
//!
//! 输出预算（[`QueryOutputBudget`]）：条数 + 字节双层截断，任何截断都在
//! 输出 JSON 中显式标记（`truncated`），不静默截断。
//!
//! 工具划分决策（graph 一个工具 + LSP 一个工具，见
//! `docs/refactor/phase8-tools-context.md`）：graph 与 LSP 的查询面
//! 生命周期 / 延迟特征 / 失败模式都不同（内存索引同步查询 vs 子进程
//! async 网络往返），独立声明 ToolCapabilities 让模型按需求选择。

// Evo 独立设计（Grok 无 graph 查询工具形态；read-only query tool 是
// master plan ARC-810/830 的产品决策）。
use std::sync::Arc;

use serde::Deserialize;
use serde_json::json;
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind,
};
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind, ToolOutput};
use tool_runtime::api::{DynamicTool, ToolCallContext, ToolFuture};

use crate::graph::backend::{GraphQueryResult, context_field};
use crate::graph::query::NavigationResult;
use crate::service::{CodeIntelligenceHandle, QueryKind, QueryRequest};

use super::budget::{QueryOutputBudget, truncate_by_bytes, truncate_items};

/// 静态工具 id。
pub const CODE_GRAPH_TOOL_ID: &str = "code_graph";

/// `code_graph` 参数。
#[derive(Debug, Clone, Deserialize)]
pub struct CodeGraphArgs {
    /// 查询模式：`symbols` / `definitions` / `references` / `search`。
    pub query: String,
    /// workspace-relative 路径（`symbols` 与位置查询必填）。
    #[serde(default)]
    pub path: Option<String>,
    /// 符号名（`definitions` / `references` / `search` 按名查询）。
    #[serde(default)]
    pub symbol: Option<String>,
    /// 1-indexed 行号（位置查询，与 `path` + `column` 配合）。
    #[serde(default)]
    pub line: Option<u64>,
    /// 1-indexed 列号（位置查询，与 `path` + `line` 配合）。
    #[serde(default)]
    pub column: Option<u64>,
    /// `references` 是否并入定义位置。
    #[serde(default)]
    pub include_definition: Option<bool>,
    /// 输出条数上限覆盖（默认来自输出预算）。
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `code_graph` 工具。
pub struct CodeGraphTool {
    definition: ToolDefinition,
    handle: CodeIntelligenceHandle,
    budget: QueryOutputBudget,
}

/// 构造 `code_graph` 工具（默认输出预算）。
pub fn code_graph_tool(handle: CodeIntelligenceHandle) -> Arc<dyn DynamicTool> {
    code_graph_tool_with_budget(handle, QueryOutputBudget::default())
}

/// 构造 `code_graph` 工具（自定义输出预算）。
pub fn code_graph_tool_with_budget(
    handle: CodeIntelligenceHandle,
    budget: QueryOutputBudget,
) -> Arc<dyn DynamicTool> {
    Arc::new(CodeGraphTool {
        definition: graph_definition(),
        handle,
        budget,
    })
}

fn graph_definition() -> ToolDefinition {
    ToolDefinition {
        id: ToolId::new(CODE_GRAPH_TOOL_ID).expect("static tool id is valid"),
        kind: ToolKind::Function,
        description: "Query the local codebase graph (read-only): list symbols in a file \
                      (symbols), find definitions (definitions), find references (references), \
                      or search symbols by name fragment (search). Paths are workspace-relative; \
                      lines and columns are 1-indexed."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "enum": ["symbols", "definitions", "references", "search"],
                          "description": "Query mode."},
                "path": {"type": "string", "description": "Workspace-relative file path."},
                "symbol": {"type": "string", "description": "Symbol name (definitions/references/search)."},
                "line": {"type": "integer", "description": "1-indexed line (position queries)."},
                "column": {"type": "integer", "description": "1-indexed column (position queries)."},
                "include_definition": {"type": "boolean", "description": "Merge definition locations into references."},
                "limit": {"type": "integer", "description": "Result count cap override."}
            },
            "required": ["query"]
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

impl DynamicTool for CodeGraphTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn execute(&self, context: ToolCallContext, arguments: serde_json::Value) -> ToolFuture {
        let handle = self.handle.clone();
        let budget = self.budget;
        Box::pin(run_graph_query(
            handle,
            budget,
            context,
            parse_args(arguments),
        ))
    }
}

fn parse_args(arguments: serde_json::Value) -> Result<CodeGraphArgs, ToolError> {
    serde_json::from_value(arguments).map_err(|error| {
        ToolError::new(
            ToolErrorKind::InvalidArguments,
            format!("invalid tool arguments: {error}"),
        )
    })
}

async fn run_graph_query(
    handle: CodeIntelligenceHandle,
    budget: QueryOutputBudget,
    context: ToolCallContext,
    args: Result<CodeGraphArgs, ToolError>,
) -> Result<ToolOutput, ToolError> {
    let args = args?;
    let effective_limit = args.limit.unwrap_or(budget.max_items);
    let request = build_request(&args, effective_limit)?;
    let response = tokio::select! {
        biased;
        _ = context.cancel.cancelled() => {
            return Err(ToolError::new(ToolErrorKind::Cancelled, "code_graph query cancelled"));
        }
        result = handle.submit(request) => result,
    }
    .map_err(|error| service_error_to_tool(&error))?;
    let payload = result_to_payload(response.graph, &args, effective_limit, budget)?;
    Ok(ToolOutput {
        content: vec![ToolContent::Json { value: payload }],
        ..Default::default()
    })
}

/// 参数 → 服务查询请求（参数校验失败返回 `InvalidArguments`）。
fn build_request(args: &CodeGraphArgs, limit: usize) -> Result<QueryRequest, ToolError> {
    let invalid = |message: String| {
        ToolError::new(
            ToolErrorKind::InvalidArguments,
            format!("code_graph: {message}"),
        )
    };
    match args.query.as_str() {
        "symbols" => {
            let path = args
                .path
                .clone()
                .ok_or_else(|| invalid("'path' is required for query 'symbols'".into()))?;
            Ok(QueryRequest::new(
                QueryKind::FileSymbols,
                json!({ context_field::PATH: path }),
            ))
        }
        "definitions" | "references" => {
            let mut context = serde_json::Map::new();
            match (&args.symbol, &args.path) {
                (Some(symbol), _) => {
                    context.insert(context_field::SYMBOL.into(), json!(symbol));
                }
                (None, Some(path)) => {
                    let line = args
                        .line
                        .ok_or_else(|| invalid("'line' is required for position queries".into()))?;
                    let column = args.column.ok_or_else(|| {
                        invalid("'column' is required for position queries".into())
                    })?;
                    context.insert(context_field::PATH.into(), json!(path));
                    context.insert(context_field::LINE.into(), json!(line));
                    context.insert(context_field::COLUMN.into(), json!(column));
                }
                (None, None) => {
                    return Err(invalid(
                        "either 'symbol' or 'path'+'line'+'column' is required".into(),
                    ));
                }
            }
            if args.query == "references" && args.include_definition.unwrap_or(false) {
                context.insert(context_field::INCLUDE_DEFINITION.into(), json!(true));
            }
            Ok(QueryRequest::new(
                if args.query == "definitions" {
                    QueryKind::Definition
                } else {
                    QueryKind::Reference
                },
                serde_json::Value::Object(context),
            ))
        }
        "search" => {
            let symbol = args
                .symbol
                .clone()
                .ok_or_else(|| invalid("'symbol' is required for query 'search'".into()))?;
            Ok(QueryRequest::new(
                QueryKind::SymbolSearch,
                json!({ context_field::SYMBOL: symbol, context_field::LIMIT: limit }),
            ))
        }
        other => Err(invalid(format!("unknown query mode '{other}'"))),
    }
}

/// 服务响应 → 预算内 JSON 载荷（截断显式标记）。
fn result_to_payload(
    graph: Option<GraphQueryResult>,
    args: &CodeGraphArgs,
    limit: usize,
    budget: QueryOutputBudget,
) -> Result<serde_json::Value, ToolError> {
    let graph = graph.ok_or_else(|| {
        ToolError::new(
            ToolErrorKind::Execution,
            "code_graph: service returned no graph result",
        )
    })?;
    let mut payload = serde_json::Map::new();
    payload.insert("query".into(), json!(args.query.as_str()));
    let (mut items, total, mut truncated) = match graph {
        GraphQueryResult::Symbols { symbols } => {
            let total = symbols.len();
            let (items, cut) = truncate_items(symbols, limit);
            (serde_items(items), total, cut)
        }
        GraphQueryResult::Definitions(result) | GraphQueryResult::References(result) => {
            navigation_to_items(result, limit)
        }
        GraphQueryResult::Search(search) => {
            let total = search.total;
            payload.insert("symbol".into(), json!(search.query));
            let (items, item_cut) = truncate_items(search.results, limit);
            (serde_items(items), total, search.truncated || item_cut)
        }
    };
    truncated |= apply_byte_budget(&mut items, budget.max_bytes);
    payload.insert("results".into(), serde_json::Value::Array(items));
    payload.insert("count".into(), json!(total));
    if truncated {
        payload.insert("truncated".into(), json!(true));
    }
    Ok(serde_json::Value::Object(payload))
}

fn navigation_to_items(
    result: NavigationResult,
    limit: usize,
) -> (Vec<serde_json::Value>, usize, bool) {
    let total = result.locations.len();
    let (items, truncated) = truncate_items(result.locations, limit);
    (serde_items(items), total, truncated)
}

fn serde_items<T: serde::Serialize>(items: Vec<T>) -> Vec<serde_json::Value> {
    items
        .into_iter()
        .map(|item| serde_json::to_value(item).unwrap_or(serde_json::Value::Null))
        .collect()
}

/// 字节预算截断：返回是否发生截断。
fn apply_byte_budget(items: &mut Vec<serde_json::Value>, max_bytes: usize) -> bool {
    if max_bytes == 0 {
        return false;
    }
    let items_vec = std::mem::take(items);
    let (kept, truncated, _) =
        truncate_by_bytes(items_vec, max_bytes, |item| item.to_string().len());
    *items = kept;
    truncated
}

/// 服务错误 → 工具错误（结构化类别）。
fn service_error_to_tool(error: &crate::error::CodeIntelligenceError) -> ToolError {
    use crate::error::CodeIntelligenceError;
    match error {
        CodeIntelligenceError::NotRunning | CodeIntelligenceError::ShuttingDown { .. } => {
            ToolError::new(
                ToolErrorKind::Unavailable,
                "code intelligence service is not running",
            )
        }
        CodeIntelligenceError::QueryPanicked => ToolError::new(
            ToolErrorKind::Execution,
            "code intelligence query processing panicked",
        ),
        CodeIntelligenceError::GraphQuery { detail } => classify_graph_error(detail),
        CodeIntelligenceError::Unimplemented { kind, .. } => ToolError::new(
            ToolErrorKind::Unavailable,
            format!("query kind '{kind}' is not implemented"),
        ),
        other => ToolError::new(
            ToolErrorKind::Execution,
            format!("code intelligence query failed: {other}"),
        ),
    }
}

/// 图查询错误分类（detail 是 `GraphQueryError` 的 Display，契约稳定：
/// 见 `crates/code-intelligence/src/graph/query.rs`）。
pub fn classify_graph_error(detail: &str) -> ToolError {
    let kind = if detail.starts_with("parse error") {
        ToolErrorKind::InvalidArguments
    } else if detail.starts_with("file not indexed")
        || detail.starts_with("file not found")
        || detail.starts_with("unsupported language")
    {
        ToolErrorKind::Unavailable
    } else if detail.starts_with("position out of bounds") {
        ToolErrorKind::InvalidArguments
    } else {
        ToolErrorKind::Execution
    };
    ToolError::new(kind, format!("code_graph: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::query::BoundedSymbolSearch;

    #[test]
    fn definition_is_valid_and_read_only() {
        let definition = graph_definition();
        definition.validate().expect("valid definition");
        assert_eq!(definition.id.as_str(), CODE_GRAPH_TOOL_ID);
        assert!(definition.capabilities.read_only);
        assert_eq!(
            definition.authorization_risk,
            AuthorizationRisk::WorkspaceLocalReadOnly
        );
        assert!(definition.capabilities.cancel);
        assert!(!definition.capabilities.streaming);
    }

    #[test]
    fn args_defaults_are_absent() {
        let args: CodeGraphArgs = serde_json::from_value(json!({"query": "symbols"})).unwrap();
        assert!(args.path.is_none());
        assert!(args.symbol.is_none());
        assert!(args.limit.is_none());
    }

    #[test]
    fn build_request_validates_modes() {
        // symbols 缺 path 拒绝。
        let err = build_request(
            &CodeGraphArgs {
                query: "symbols".into(),
                path: None,
                symbol: None,
                line: None,
                column: None,
                include_definition: None,
                limit: None,
            },
            10,
        )
        .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::InvalidArguments);

        // definitions 需要 symbol 或位置。
        let err = build_request(
            &CodeGraphArgs {
                query: "definitions".into(),
                path: None,
                symbol: None,
                line: None,
                column: None,
                include_definition: None,
                limit: None,
            },
            10,
        )
        .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::InvalidArguments);

        // 未知模式拒绝。
        let err = build_request(
            &CodeGraphArgs {
                query: "bogus".into(),
                path: None,
                symbol: None,
                line: None,
                column: None,
                include_definition: None,
                limit: None,
            },
            10,
        )
        .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::InvalidArguments);

        // search 缺 symbol 拒绝。
        let err = build_request(
            &CodeGraphArgs {
                query: "search".into(),
                path: None,
                symbol: None,
                line: None,
                column: None,
                include_definition: None,
                limit: None,
            },
            10,
        )
        .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::InvalidArguments);
    }

    #[test]
    fn build_request_encodes_contexts() {
        let request = build_request(
            &CodeGraphArgs {
                query: "symbols".into(),
                path: Some("src/a.rs".into()),
                symbol: None,
                line: None,
                column: None,
                include_definition: None,
                limit: None,
            },
            10,
        )
        .unwrap();
        assert_eq!(request.kind, QueryKind::FileSymbols);
        assert_eq!(request.context["path"], "src/a.rs");

        let request = build_request(
            &CodeGraphArgs {
                query: "references".into(),
                path: Some("src/a.rs".into()),
                symbol: None,
                line: Some(5),
                column: Some(3),
                include_definition: Some(true),
                limit: None,
            },
            10,
        )
        .unwrap();
        assert_eq!(request.kind, QueryKind::Reference);
        assert_eq!(request.context["line"], 5);
        assert_eq!(request.context["column"], 3);
        assert_eq!(request.context["include_definition"], true);

        let request = build_request(
            &CodeGraphArgs {
                query: "search".into(),
                path: None,
                symbol: Some("parse".into()),
                line: None,
                column: None,
                include_definition: None,
                limit: Some(7),
            },
            10,
        )
        .unwrap();
        assert_eq!(request.kind, QueryKind::SymbolSearch);
        assert_eq!(request.context["symbol"], "parse");
        // 搜索条数上限来自 build_request 的 limit 参数。
        assert_eq!(request.context["limit"], 10);
    }

    #[test]
    fn classify_graph_error_maps_structured_categories() {
        assert_eq!(
            classify_graph_error("parse error: missing path").kind,
            ToolErrorKind::InvalidArguments
        );
        assert_eq!(
            classify_graph_error("file not indexed: src/a.rs").kind,
            ToolErrorKind::Unavailable
        );
        assert_eq!(
            classify_graph_error("file not found: src/a.rs").kind,
            ToolErrorKind::Unavailable
        );
        assert_eq!(
            classify_graph_error("unsupported language: py3").kind,
            ToolErrorKind::Unavailable
        );
        assert_eq!(
            classify_graph_error("position out of bounds: 0:1").kind,
            ToolErrorKind::InvalidArguments
        );
        assert_eq!(
            classify_graph_error("no symbol found at position 3:3").kind,
            ToolErrorKind::Execution
        );
    }

    #[test]
    fn payload_marks_item_truncation() {
        use crate::graph::query::FileSymbol;
        let symbols = vec![
            FileSymbol {
                name: "a".into(),
                symbol_type: "function".into(),
                line: 1,
                column: 1,
                children: Vec::new(),
            };
            3
        ];
        let payload = result_to_payload(
            Some(GraphQueryResult::Symbols { symbols }),
            &CodeGraphArgs {
                query: "symbols".into(),
                path: None,
                symbol: None,
                line: None,
                column: None,
                include_definition: None,
                limit: Some(2),
            },
            2,
            QueryOutputBudget {
                max_items: 2,
                max_bytes: 0,
            },
        )
        .unwrap();
        assert_eq!(payload["count"], 3);
        assert_eq!(payload["results"].as_array().unwrap().len(), 2);
        assert_eq!(payload["truncated"], true);
    }

    #[test]
    fn payload_marks_byte_truncation() {
        use crate::graph::query::FileSymbol;
        let symbols = vec![
            FileSymbol {
                name: "long_symbol_name".into(),
                symbol_type: "function".into(),
                line: 1,
                column: 1,
                children: Vec::new(),
            };
            4
        ];
        let payload = result_to_payload(
            Some(GraphQueryResult::Symbols { symbols }),
            &CodeGraphArgs {
                query: "symbols".into(),
                path: None,
                symbol: None,
                line: None,
                column: None,
                include_definition: None,
                limit: Some(10),
            },
            10,
            QueryOutputBudget {
                max_items: 10,
                max_bytes: 64,
            },
        )
        .unwrap();
        assert_eq!(payload["count"], 4);
        let kept = payload["results"].as_array().unwrap().len();
        assert!(kept < 4, "byte budget must drop items");
        assert_eq!(payload["truncated"], true);
    }

    #[test]
    fn search_payload_carries_symbol_and_bounds() {
        let search = BoundedSymbolSearch {
            query: "parse".into(),
            total: 3,
            truncated: false,
            results: Vec::new(),
        };
        let payload = result_to_payload(
            Some(GraphQueryResult::Search(search)),
            &CodeGraphArgs {
                query: "search".into(),
                path: None,
                symbol: Some("parse".into()),
                line: None,
                column: None,
                include_definition: None,
                limit: None,
            },
            10,
            QueryOutputBudget::default(),
        )
        .unwrap();
        assert_eq!(payload["symbol"], "parse");
        assert_eq!(payload["count"], 3);
        assert!(payload.get("truncated").is_none());
    }
}
