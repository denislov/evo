use agent_core::api::tool::{AgentTool, ToolExecutionContext};
use std::collections::BTreeSet;
use std::path::PathBuf;

pub use crate::runtime::capability::FilesystemTarget;
pub use crate::runtime::facade::{FilesystemCapability, ShellCapability};

pub(crate) mod filesystem;
pub(crate) mod mutation_queue;
pub(crate) mod output;
pub(crate) mod shell;

pub(crate) const PRODUCT_TOOL_NAMES: [&str; 9] = [
    "read",
    "write",
    "edit",
    "bash",
    "grep",
    "find",
    "ls",
    "delegate_agent",
    "delegate_team",
];

pub fn builtin_tools(
    cwd: PathBuf,
) -> Result<Vec<AgentTool>, crate::runtime::facade::CodingSessionError> {
    let filesystem = FilesystemCapability::new(cwd.clone())?;
    let shell = ShellCapability::new(cwd);
    Ok(builtin_tools_with_capabilities(&filesystem, &shell))
}

pub(crate) async fn filesystem_target_for_execution(
    filesystem: &FilesystemCapability,
    context: &ToolExecutionContext,
    tool_name: &str,
    path: &str,
) -> Result<FilesystemTarget, String> {
    match context.scope_id() {
        Some(operation_id) => filesystem
            .take_bound_tool_target(operation_id, context.tool_call_id(), tool_name, path)
            .map_err(|error| error.to_string()),
        None => filesystem
            .prepare_target_for_tool(tool_name, path)
            .await
            .map_err(|error| error.to_string()),
    }
}

fn builtin_tools_with_capabilities(
    filesystem: &FilesystemCapability,
    shell: &ShellCapability,
) -> Vec<AgentTool> {
    vec![
        filesystem::read::read_tool(filesystem.clone()),
        filesystem::write::write_tool(filesystem.clone()),
        filesystem::edit::edit_tool(filesystem.clone()),
        shell::bash_tool(shell.clone()),
        filesystem::grep::grep_tool(filesystem.clone()),
        filesystem::find::find_tool(filesystem.clone()),
        filesystem::ls::ls_tool(filesystem.clone()),
    ]
}

pub(crate) fn bind_builtin_tool_to_capabilities(
    tool: AgentTool,
    filesystem: Option<&FilesystemCapability>,
    shell: Option<&ShellCapability>,
) -> Option<AgentTool> {
    match tool.name.as_str() {
        "read" => filesystem.cloned().map(filesystem::read::read_tool),
        "write" => filesystem.cloned().map(filesystem::write::write_tool),
        "edit" => filesystem.cloned().map(filesystem::edit::edit_tool),
        "grep" => filesystem.cloned().map(filesystem::grep::grep_tool),
        "find" => filesystem.cloned().map(filesystem::find::find_tool),
        "ls" => filesystem.cloned().map(filesystem::ls::ls_tool),
        "bash" => shell.cloned().map(shell::bash_tool),
        _ => Some(tool),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolFilter {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub no_tools: bool,
    pub no_builtin_tools: bool,
}

pub fn filter_tools(tools: Vec<AgentTool>, filter: &ToolFilter) -> Vec<AgentTool> {
    if filter.no_tools {
        return Vec::new();
    }
    let allow: BTreeSet<_> = filter.allow.iter().map(String::as_str).collect();
    let deny: BTreeSet<_> = filter.deny.iter().map(String::as_str).collect();
    let builtins = BTreeSet::from(["read", "write", "edit", "bash", "grep", "find", "ls"]);
    tools
        .into_iter()
        .filter(|tool| !filter.no_builtin_tools || !builtins.contains(tool.name.as_str()))
        .filter(|tool| allow.is_empty() || allow.contains(tool.name.as_str()))
        .filter(|tool| !deny.contains(tool.name.as_str()))
        .collect()
}
