//! MCP meta tools：`mcp_search` / `mcp_use`。
//!
//! 默认采用 search + use 两个 meta tools，**避免把大量 MCP inputSchema
//! 全塞进模型上下文**：模型只看到两个静态工具声明，运行时经
//! `mcp_search` 发现可用工具、`mcp_use` 转发 `tools/call`。
//!
//! - `mcp_search`：列出 / 搜索已发现工具（名称 / 描述 / 所属 server
//!   子串匹配），返回 JSON 数组。
//! - `mcp_use`：`tool = "<server>/<name>"` + `arguments` 对象，转发
//!   `tools/call`；per-tool 超时（server 配置）与取消（
//!   [`ToolCallContext::cancel`]）贯通，401 时按 lifecycle 的
//!   refresh/device-flow 语义自动恢复一次。
//!
//! 两个工具都是 [`DynamicTool`]（无类型 JSON → JSON），注册进产品的
//! 工具装配（`ApplicationRunOptions::tools` 或测试直接构造）。

// Evo 独立设计（xai-grok-mcp 无 meta tool 概念：它把每个 MCP 工具都注册
// 为 ToolBridge 条目；search/use 形态按 master plan 第六节决策采用）。
use std::sync::Arc;

use serde::Deserialize;
use serde_json::json;
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind,
};
use tool_contract::api::output::{ToolContent, ToolError, ToolErrorKind, ToolOutput};
use tool_contract::api::ranking::{DefaultResultRanker, ResultRanker};
use tool_runtime::api::{DynamicTool, ToolCallContext, ToolFuture};

use crate::mcp::lifecycle::McpHost;

/// meta tool 的静态 id。
pub const MCP_SEARCH_TOOL_ID: &str = "mcp_search";
pub const MCP_USE_TOOL_ID: &str = "mcp_use";

/// `mcp_search` 参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchArgs {
    /// 子串匹配（server 名 / 工具名 / 描述，不区分大小写）。
    #[serde(default)]
    pub query: Option<String>,
    /// 只列出该 server 的工具。
    #[serde(default)]
    pub server: Option<String>,
}

/// `mcp_use` 参数。
#[derive(Debug, Clone, Deserialize)]
pub struct UseArgs {
    /// 工具定位：`"<server>/<name>"`（来自 `mcp_search` 输出）。
    pub tool: String,
    /// 传给服务器的参数对象。
    #[serde(default = "default_arguments")]
    pub arguments: serde_json::Value,
}

fn default_arguments() -> serde_json::Value {
    json!({})
}

/// 构造 `mcp_search` 工具。
pub fn search_tool(host: Arc<McpHost>) -> Arc<dyn DynamicTool> {
    Arc::new(MetaTool {
        definition: meta_definition(MCP_SEARCH_TOOL_ID, search_definition()),
        kind: MetaToolKind::Search { host },
    })
}

/// 构造 `mcp_use` 工具。
pub fn use_tool(host: Arc<McpHost>) -> Arc<dyn DynamicTool> {
    Arc::new(MetaTool {
        definition: meta_definition(MCP_USE_TOOL_ID, use_definition()),
        kind: MetaToolKind::Use { host },
    })
}

/// 全部 meta tools（注册用）。
pub fn meta_tools(host: Arc<McpHost>) -> Vec<Arc<dyn DynamicTool>> {
    vec![search_tool(Arc::clone(&host)), use_tool(host)]
}

fn meta_definition(id: &str, parameters: serde_json::Value) -> ToolDefinition {
    ToolDefinition {
        id: ToolId::new(id).expect("static meta tool id is valid"),
        kind: ToolKind::Function,
        description: if id == MCP_SEARCH_TOOL_ID {
            "List or search MCP tools available from connected MCP servers. \
             Returns JSON array of {server, name, description}. Use mcp_use to call one."
                .into()
        } else {
            "Call an MCP tool discovered via mcp_search. Provide tool as \
             '<server>/<name>' and arguments as an object."
                .into()
        },
        parameters,
        capabilities: ToolCapabilities {
            read_only: id == MCP_SEARCH_TOOL_ID,
            execution: ToolExecutionMode::Parallel,
            cancel: true,
            timeout: true,
            streaming: false,
            provider_executed: false,
        },
        behavior: ToolBehaviorVersion::V1,
        authorization_risk: if id == MCP_USE_TOOL_ID {
            AuthorizationRisk::SideEffect
        } else {
            AuthorizationRisk::None
        },
        requirements: Vec::new(),
    }
}

fn search_definition() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "Optional substring to match against server name, tool name, or description."},
            "server": {"type": "string", "description": "Optional server name to filter to a single MCP server."}
        },
        "required": []
    })
}

fn use_definition() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "tool": {"type": "string", "description": "Tool to call, formatted as '<server>/<name>'."},
            "arguments": {"type": "object", "description": "Arguments object forwarded to the MCP server."}
        },
        "required": ["tool"]
    })
}

enum MetaToolKind {
    Search { host: Arc<McpHost> },
    Use { host: Arc<McpHost> },
}

struct MetaTool {
    definition: ToolDefinition,
    kind: MetaToolKind,
}

impl DynamicTool for MetaTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn execute(&self, context: ToolCallContext, arguments: serde_json::Value) -> ToolFuture {
        match &self.kind {
            MetaToolKind::Search { host } => Box::pin(run_search(
                Arc::clone(host),
                context,
                parse_args::<SearchArgs>(arguments),
            )),
            MetaToolKind::Use { host } => Box::pin(run_use(
                Arc::clone(host),
                context,
                parse_args::<UseArgs>(arguments),
            )),
        }
    }
}

fn parse_args<T: for<'de> Deserialize<'de>>(arguments: serde_json::Value) -> Result<T, ToolError> {
    serde_json::from_value(arguments).map_err(|error| {
        ToolError::new(
            ToolErrorKind::InvalidArguments,
            format!("invalid tool arguments: {error}"),
        )
    })
}

async fn run_search(
    host: Arc<McpHost>,
    context: ToolCallContext,
    args: Result<SearchArgs, ToolError>,
) -> Result<ToolOutput, ToolError> {
    let args = args?;
    let query = args
        .query
        .as_deref()
        .map(str::to_ascii_lowercase)
        .filter(|query| !query.is_empty());
    let server_filter = args.server.filter(|server| !server.is_empty());
    let tools = host.tools();
    let mut matches: Vec<serde_json::Value> = Vec::new();
    for (server, tools) in tools {
        if let Some(filter) = &server_filter
            && server != *filter
        {
            continue;
        }
        for tool in tools {
            let haystack =
                format!("{} {} {}", server, tool.name, tool.description).to_ascii_lowercase();
            if let Some(query) = &query
                && !haystack.contains(query.as_str())
            {
                continue;
            }
            matches.push(json!({
                "server": tool.server,
                "name": tool.name,
                "description": tool.description,
            }));
        }
    }
    // ARC-830：命中结果经共用排序接口（tool-contract::ranking）按与查询词
    // 的相关度排序；无查询词时保持发现顺序（列表语义）。
    let matches = rank_search_matches(matches, query.as_deref());
    drop(context);
    Ok(ToolOutput {
        content: vec![ToolContent::Json {
            value: json!(matches),
        }],
        ..Default::default()
    })
}

/// 排序接口的 MCP 侧接入：把命中结果按相关度降序稳定排序（同分保持
/// 输入顺序）。过滤（子串匹配）在调用方完成，本函数只排序不增删。
pub fn rank_search_matches(
    matches: Vec<serde_json::Value>,
    query: Option<&str>,
) -> Vec<serde_json::Value> {
    let Some(query) = query.filter(|query| !query.is_empty()) else {
        return matches;
    };
    let ranker = DefaultResultRanker::new();
    ranker
        .rank(
            query,
            matches,
            |value| {
                let server = value
                    .get("server")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let name = value
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let description = value
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                format!("{server} {name} {description}")
            },
            0,
        )
        .into_iter()
        .map(|result| result.item)
        .collect()
}

async fn run_use(
    host: Arc<McpHost>,
    context: ToolCallContext,
    args: Result<UseArgs, ToolError>,
) -> Result<ToolOutput, ToolError> {
    let args = args?;
    let (server, name) = split_tool(&args.tool).ok_or_else(|| {
        ToolError::new(
            ToolErrorKind::InvalidArguments,
            format!(
                "tool must be '<server>/<name>', got '{}'; use mcp_search to discover tools",
                args.tool
            ),
        )
    })?;
    let handle = host
        .servers()
        .iter()
        .find(|handle| handle.name() == server)
        .ok_or_else(|| {
            ToolError::new(
                ToolErrorKind::Unavailable,
                format!("unknown MCP server '{server}'"),
            )
        })?;
    let arguments = if args.arguments.is_object() {
        args.arguments
    } else {
        return Err(ToolError::new(
            ToolErrorKind::InvalidArguments,
            "'arguments' must be an object",
        ));
    };
    let result = handle
        .call_tool(name, arguments, &context.cancel)
        .await
        .map_err(|error| {
            ToolError::new(
                classify_error(&error),
                format!("mcp tool '{server}/{name}' failed: {error}"),
            )
        })?;
    let content = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![result.clone()]);
    let is_error = result.get("isError").and_then(serde_json::Value::as_bool);
    Ok(ToolOutput {
        content: content.into_iter().map(mcp_content_block).collect(),
        terminate: is_error.unwrap_or(false),
        ..Default::default()
    })
}

fn split_tool(tool: &str) -> Option<(&str, &str)> {
    let (server, name) = tool.split_once('/')?;
    if server.is_empty() || name.is_empty() {
        return None;
    }
    Some((server, name))
}

/// MCP content block → Evo 内容块（text 直通，其余 JSON 化）。
fn mcp_content_block(block: serde_json::Value) -> ToolContent {
    if let Some(text) = block.get("text").and_then(serde_json::Value::as_str) {
        return ToolContent::Text {
            text: text.to_string(),
        };
    }
    ToolContent::Json { value: block }
}

fn classify_error(error: &crate::mcp::transport::RpcError) -> ToolErrorKind {
    use crate::mcp::transport::RpcError;
    match error {
        RpcError::Timeout { .. } => ToolErrorKind::Timeout,
        RpcError::Cancelled => ToolErrorKind::Cancelled,
        RpcError::Unauthorized => ToolErrorKind::Unauthorized,
        _ => ToolErrorKind::Execution,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_tool_parses_server_and_name() {
        assert_eq!(split_tool("fs/read"), Some(("fs", "read")));
        assert_eq!(split_tool("fs/"), None);
        assert_eq!(split_tool("/read"), None);
        assert_eq!(split_tool("naked"), None);
    }

    #[test]
    fn meta_definitions_are_valid_and_stable() {
        for id in [MCP_SEARCH_TOOL_ID, MCP_USE_TOOL_ID] {
            let host = Arc::new(McpHost::new(
                vec![],
                Arc::new(crate::mcp::credentials::FileCredentialStore::new(
                    tempfile::tempdir().unwrap().path(),
                )),
            ));
            let tool = if id == MCP_SEARCH_TOOL_ID {
                search_tool(host)
            } else {
                use_tool(host)
            };
            tool.definition().validate().expect("definition valid");
            assert_eq!(tool.definition().id.as_str(), id);
        }
    }

    #[test]
    fn search_args_default_to_empty_query() {
        let args: SearchArgs = serde_json::from_value(json!({})).unwrap();
        assert_eq!(args.query, None);
        assert_eq!(args.server, None);
    }

    #[test]
    fn use_args_require_tool() {
        assert!(serde_json::from_value::<UseArgs>(json!({})).is_err());
        let args: UseArgs = serde_json::from_value(json!({"tool": "s/t"})).unwrap();
        assert_eq!(args.tool, "s/t");
        assert_eq!(args.arguments, json!({}));
    }

    #[test]
    fn mcp_content_block_maps_text_and_json() {
        assert_eq!(
            mcp_content_block(json!({"type": "text", "text": "hi"})),
            ToolContent::Text { text: "hi".into() }
        );
        assert_eq!(
            mcp_content_block(json!({"type": "image", "data": "x"})),
            ToolContent::Json {
                value: json!({"type": "image", "data": "x"})
            }
        );
    }

    fn match_item(server: &str, name: &str, description: &str) -> serde_json::Value {
        json!({"server": server, "name": name, "description": description})
    }

    fn names(matches: &[serde_json::Value]) -> Vec<&str> {
        matches
            .iter()
            .map(|value| value["name"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn rank_search_matches_orders_by_relevance() {
        let matches = vec![
            match_item("fs", "read", "Read file contents"),
            match_item("net", "fetch", "Fetch a URL"),
            match_item("fs", "read_dir", "List a directory"),
        ];
        let ranked = rank_search_matches(matches, Some("read"));
        // 精确名 `read` 最前；`read_dir` 前缀命中次之；`fetch` 无命中。
        assert_eq!(names(&ranked), ["read", "read_dir", "fetch"]);
    }

    #[test]
    fn rank_search_matches_without_query_keeps_input_order() {
        let matches = vec![
            match_item("z", "alpha", "first"),
            match_item("a", "beta", "second"),
        ];
        let ranked = rank_search_matches(matches.clone(), None);
        assert_eq!(ranked, matches);
        let ranked = rank_search_matches(matches.clone(), Some(""));
        assert_eq!(ranked, matches);
    }

    #[test]
    fn rank_search_matches_handles_empty_input() {
        let ranked = rank_search_matches(Vec::new(), Some("query"));
        assert!(ranked.is_empty());
    }

    #[test]
    fn rank_search_matches_ties_keep_input_order() {
        let matches = vec![
            match_item("fs", "list", "List entries"),
            match_item("db", "list", "List rows"),
        ];
        let ranked = rank_search_matches(matches, Some("list"));
        assert_eq!(names(&ranked), ["list", "list"]);
    }
}
