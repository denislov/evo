use std::collections::BTreeSet;
use std::sync::Arc;
use tool_contract::api::definition::{
    AuthorizationRisk, ToolBehaviorVersion, ToolCapabilities, ToolDefinition, ToolExecutionMode,
    ToolId, ToolKind,
};
use tool_runtime::api::ToolCallContext;

pub use crate::platform::fs::capability::FilesystemCapability;
pub use crate::platform::fs::capability::FilesystemTarget;

pub(crate) mod filesystem;
#[cfg(test)]
mod server_tools_tests;
pub(crate) mod shell;

/// Tools the model provider executes rather than the harness.
///
/// They hold no local capability — no filesystem handle, no shell — so the
/// capability layer has nothing to withhold from them. Their only enforceable
/// gate is whether the declaration is sent at all, which
/// [`server_side_tools`] decides from the model.
pub(crate) fn product_tool_ids() -> Vec<ToolId> {
    [
        "read",
        "write",
        "edit",
        "bash",
        "grep",
        "find",
        "ls",
        "apply_patch",
        "hashline_edit",
        "delegate_agent",
        "delegate_team",
        "web_search",
    ]
    .into_iter()
    .map(|name| ToolId::new(name).expect("static tool id is valid"))
    .collect()
}

pub(crate) fn builtin_runtime_tool_ids() -> Vec<ToolId> {
    [
        "read",
        "write",
        "edit",
        "bash",
        "grep",
        "find",
        "ls",
        "apply_patch",
        "hashline_edit",
    ]
    .into_iter()
    .map(|name| ToolId::new(name).expect("static tool id is valid"))
    .collect()
}

pub(crate) fn server_tool_ids() -> Vec<ToolId> {
    vec![ToolId::new("web_search").expect("static tool id is valid")]
}

/// Grant the server-side tool names alongside an explicit tool list.
///
/// A profile that enumerates its local tools should not also have to opt into
/// provider-side search, so these names are added rather than intersected. An
/// explicitly empty list is left empty: that says "no tools", and silently
/// granting network reach would punch through a deliberate fail-closed
/// configuration.
pub(crate) fn grant_server_tools(ids: &mut Vec<ToolId>) {
    if ids.is_empty() {
        return;
    }
    for id in server_tool_ids() {
        if !ids.iter().any(|granted| granted == &id) {
            ids.push(id);
        }
    }
}

/// Declarations for every server-side tool this model can be sent.
///
/// Returns empty when the model's API cannot express the declaration or cannot
/// replay its result, so an unsupported model degrades to no web search rather
/// than to a rejected request.
pub(crate) fn server_side_tools(model: &ai_protocol::api::model::Model) -> Vec<ToolDefinition> {
    if ai::api::provider::model_supports_web_search(model) {
        vec![ToolDefinition {
            id: ToolId::new("web_search").expect("static tool id is valid"),
            kind: ToolKind::WebSearch,
            description: "Search the web. Executed by the model provider; results arrive with \
                          the assistant message."
                .into(),
            parameters: serde_json::Value::Null,
            capabilities: ToolCapabilities {
                read_only: true,
                execution: ToolExecutionMode::Parallel,
                cancel: false,
                timeout: false,
                streaming: true,
                provider_executed: true,
            },
            behavior: ToolBehaviorVersion::V1,
            authorization_risk: AuthorizationRisk::None,
            requirements: Vec::new(),
        }]
    } else {
        Vec::new()
    }
}

pub(crate) async fn filesystem_target_for_runtime_execution(
    filesystem: &FilesystemCapability,
    context: &ToolCallContext,
    tool_name: &str,
    path: &str,
) -> Result<FilesystemTarget, String> {
    // `apply_patch` carries a capability-bound batch of paths inside the
    // patch body. Authorization validates the whole batch before execution;
    // each path is then resolved through the same capability root here.
    if tool_name == "apply_patch" {
        return filesystem
            .prepare_target_for_tool(tool_name, path)
            .await
            .map_err(|error| error.to_string());
    }
    match context.operation_id.as_deref() {
        Some(operation_id) => filesystem
            .take_bound_tool_target(operation_id, &context.call_id, tool_name, path)
            .map_err(|error| error.to_string()),
        None => filesystem
            .prepare_target_for_tool(tool_name, path)
            .await
            .map_err(|error| error.to_string()),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolFilter {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub no_tools: bool,
    pub no_builtin_tools: bool,
}

pub fn filter_tools(
    tools: Vec<Arc<dyn tool_runtime::api::DynamicTool>>,
    filter: &ToolFilter,
) -> Vec<Arc<dyn tool_runtime::api::DynamicTool>> {
    if filter.no_tools {
        return Vec::new();
    }
    let allow: BTreeSet<_> = filter.allow.iter().map(String::as_str).collect();
    let deny: BTreeSet<_> = filter.deny.iter().map(String::as_str).collect();
    let builtins = BTreeSet::from([
        "read",
        "write",
        "edit",
        "bash",
        "grep",
        "find",
        "ls",
        "apply_patch",
        "hashline_edit",
    ]);
    tools
        .into_iter()
        .filter(|tool| {
            !filter.no_builtin_tools || !builtins.contains(tool.definition().id.as_str())
        })
        .filter(|tool| allow.is_empty() || allow.contains(tool.definition().id.as_str()))
        .filter(|tool| !deny.contains(tool.definition().id.as_str()))
        .collect()
}
