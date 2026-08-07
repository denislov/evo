//! read-only 查询工具（ARC-830）：graph 与 LSP 的 tool adapter。
//!
//! 本模块是 `code-intelligence` 的服务 API 与产品工具装配之间的 adapter
//! （ARC-800 决策：服务 API 与 tool adapter 分离；tool adapter 只依赖
//! `api.rs` 公开面）。工具实现为 [`DynamicTool`]（无类型 JSON → JSON，
//! 参照 `extension-host` 的 MCP meta tools 先例），由 coding-agent 在
//! 配置了 code-intelligence 时装配注册。
//!
//! 工具划分（决策见 `docs/refactor/phase8-tools-context.md`）：
//!
//! - [`graph::code_graph_tool`]：内存索引查询（symbols / definitions /
//!   references / search），同步查询 + 有界队列；
//! - [`lsp::code_lsp_tool`]：子进程 LSP 查询（hover / definition /
//!   references），async 网络往返，生命周期（restart/backoff）独立。
//!
//! 每类查询有独立 [`QueryOutputBudget`]（条数 + 字节双层截断，超限显式
//! 标记）；取消经 `ToolCallContext::cancel` 贯通到查询。

// Evo 独立设计（无上游参考；形态参照 extension-host 的 MCP meta tools）。
use std::path::PathBuf;
use std::sync::Arc;

use tool_runtime::api::DynamicTool;

use crate::lsp::server::LspHandle;
use crate::service::CodeIntelligenceHandle;

pub mod budget;
pub mod graph;
pub mod lsp;

pub use budget::{QueryOutputBudget, truncate_by_bytes, truncate_items};
pub use graph::{
    CODE_GRAPH_TOOL_ID, CodeGraphArgs, CodeGraphTool, classify_graph_error, code_graph_tool,
    code_graph_tool_with_budget,
};
pub use lsp::{
    CODE_LSP_TOOL_ID, CodeLspArgs, CodeLspTool, code_lsp_tool, code_lsp_tool_with_budget, file_uri,
    hover_text, normalize_locations, truncate_text,
};

/// 装配入口：配置了 graph handle 时返回 `code_graph`；LSP handle 一并
/// 配置时追加 `code_lsp`。
pub fn code_tools(
    graph_handle: CodeIntelligenceHandle,
    lsp: Option<(LspHandle, PathBuf)>,
) -> Vec<Arc<dyn DynamicTool>> {
    let mut tools = vec![graph::code_graph_tool(graph_handle)];
    if let Some((lsp_handle, workspace_root)) = lsp {
        tools.push(lsp::code_lsp_tool(lsp_handle, workspace_root));
    }
    tools
}

#[cfg(test)]
mod graph_tests;
#[cfg(test)]
mod lsp_tests;
