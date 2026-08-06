//! MCP meta tools 的产品装配测试：`resolve_application_context` 在配置了
//! MCP server 时把 `mcp_search` / `mcp_use` 追加进工具列表；无 MCP
//! 配置时工具列表与现在完全一致。

use std::path::PathBuf;
use std::sync::Arc;

use crate::app::bootstrap::{ApplicationRunOptions, SessionRunOptions};
use crate::app::invocation::CodingAgentInvocationOptions;
use crate::app::startup::resolve_application_context;
use extension_host::api::{
    ExtensionHostOptions, FileCredentialStore, McpHost, McpServerConfig, StdioConfig,
    TransportConfig,
};
use tool_runtime::api::DynamicTool;

fn mcp_host() -> McpHost {
    let config = McpServerConfig::new(
        "fake",
        TransportConfig::Stdio(StdioConfig {
            command: "no-such-mcp-server".into(),
            args: vec![],
            env: workspace_runtime::api::EnvPolicy::AllowList(Default::default()),
            cwd: None,
            sandbox: None,
        }),
    );
    McpHost::new(
        vec![config],
        Arc::new(FileCredentialStore::new(
            tempfile::tempdir().unwrap().path(),
        )),
    )
}

fn run_options_with_mcp() -> ApplicationRunOptions {
    let session = SessionRunOptions::enabled(PathBuf::from("."));
    ApplicationRunOptions {
        session,
        extension_host_options: Some(ExtensionHostOptions {
            mcp: Some(mcp_host()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn tool_ids(tools: &[Arc<dyn DynamicTool>]) -> Vec<String> {
    tools
        .iter()
        .map(|tool| tool.definition().id.as_str().to_string())
        .collect()
}

#[test]
fn mcp_configured_appends_search_and_use_meta_tools() {
    let root = tempfile::tempdir().unwrap();
    let parsed = CodingAgentInvocationOptions::default();
    let resolved = resolve_application_context(
        parsed,
        run_options_with_mcp(),
        root.path().to_path_buf(),
        root.path().to_path_buf(),
    )
    .expect("application context resolves");
    let ids = tool_ids(&resolved.tools);
    assert!(
        ids.iter().any(|id| id == "mcp_search"),
        "mcp_search must be registered, got {ids:?}"
    );
    assert!(
        ids.iter().any(|id| id == "mcp_use"),
        "mcp_use must be registered, got {ids:?}"
    );
}

#[test]
fn without_mcp_tool_list_is_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let parsed = CodingAgentInvocationOptions::default();
    let session = SessionRunOptions::enabled(PathBuf::from("."));
    let resolved = resolve_application_context(
        parsed,
        ApplicationRunOptions {
            session,
            extension_host_options: None,
            ..Default::default()
        },
        root.path().to_path_buf(),
        root.path().to_path_buf(),
    )
    .expect("application context resolves");
    assert!(
        resolved.tools.is_empty(),
        "no MCP config must not add tools, got {:?}",
        tool_ids(&resolved.tools)
    );
}

#[test]
fn explicit_tools_survive_meta_tool_append() {
    let root = tempfile::tempdir().unwrap();
    let parsed = CodingAgentInvocationOptions::default();
    let session = SessionRunOptions::enabled(PathBuf::from("."));
    let tool = crate::tools::filesystem::ls::ls_runtime_tool(
        workspace_runtime::api::WorkspaceAccessHandle::open(
            workspace_runtime::api::WorkspaceHandle::new(
                workspace_runtime::api::WorkspaceKind::Projectless,
                root.path(),
            )
            .expect("workspace handle"),
            None,
            None,
        )
        .expect("workspace access"),
    )
    .expect("ls tool");
    let resolved = resolve_application_context(
        parsed,
        ApplicationRunOptions {
            session,
            tools: vec![tool],
            extension_host_options: Some(ExtensionHostOptions {
                mcp: Some(mcp_host()),
                ..Default::default()
            }),
            ..Default::default()
        },
        root.path().to_path_buf(),
        root.path().to_path_buf(),
    )
    .expect("application context resolves");
    let ids = tool_ids(&resolved.tools);
    assert_eq!(ids.len(), 3, "explicit tool + search + use: {ids:?}");
    assert!(ids.iter().any(|id| id == "ls"));
}
