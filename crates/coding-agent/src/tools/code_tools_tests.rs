//! code-intelligence 工具的产品装配测试（ARC-830）：三态——
//! 无 code-intelligence 配置 → 工具列表不变；只有 graph → 注册
//! `code_graph`；graph + LSP → 注册 `code_graph` + `code_lsp`。

use std::path::PathBuf;
use std::sync::Arc;

use code_intelligence::api::{
    CacheIdentity, CodeIntelligenceService, CodeIntelligenceServiceOptions, GraphBackendOptions,
    GraphQueryBackend, IndexBudget, LanguageRegistry, ParserVersion, RevisionId,
};
use tool_runtime::api::DynamicTool;
use workspace_runtime::api::{TaskOwner, WorkspaceId, WorkspaceKind};

use crate::app::bootstrap::{ApplicationRunOptions, CodeIntelligenceRunOptions, SessionRunOptions};
use crate::app::invocation::CodingAgentInvocationOptions;
use crate::app::startup::resolve_application_context;

fn test_identity() -> CacheIdentity {
    CacheIdentity {
        workspace: WorkspaceId::user_supplied(WorkspaceKind::Source, "code-tools-test").unwrap(),
        revision: RevisionId::parse("rev-1").unwrap(),
        parser_version: ParserVersion::Version(1),
    }
}

fn write_workspace(root: &std::path::Path, files: &[(&str, &str)]) {
    for (rel, content) in files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }
}

struct CodeIntelligenceHarness {
    handle: code_intelligence::api::CodeIntelligenceHandle,
    task: code_intelligence::api::CodeIntelligenceTask,
}

async fn start_code_intelligence(root: &std::path::Path) -> CodeIntelligenceHarness {
    let backend = GraphQueryBackend::new(GraphBackendOptions {
        root: root.to_path_buf(),
        cache_path: None,
        identity: test_identity(),
        registry: LanguageRegistry::builtin(),
        budget: IndexBudget::default(),
    })
    .expect("backend builds");
    let service = CodeIntelligenceService::new(CodeIntelligenceServiceOptions {
        identity: test_identity(),
        cache_path: None,
        budget: IndexBudget::default(),
        languages: LanguageRegistry::builtin(),
        backend: Some(Arc::new(backend)),
    });
    let (handle, task) = service.start().expect("service starts");
    CodeIntelligenceHarness { handle, task }
}

fn run_options(harness: &CodeIntelligenceHarness) -> ApplicationRunOptions {
    let session = SessionRunOptions::enabled(PathBuf::from("."));
    ApplicationRunOptions {
        session,
        code_intelligence: Some(CodeIntelligenceRunOptions {
            graph: harness.handle.clone(),
            lsp: None,
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

#[tokio::test]
async fn without_code_intelligence_tool_list_is_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let parsed = CodingAgentInvocationOptions::default();
    let session = SessionRunOptions::enabled(PathBuf::from("."));
    let resolved = resolve_application_context(
        parsed,
        ApplicationRunOptions {
            session,
            code_intelligence: None,
            ..Default::default()
        },
        root.path().to_path_buf(),
        root.path().to_path_buf(),
    )
    .expect("application context resolves");
    assert!(
        resolved.tools.is_empty(),
        "no code-intelligence config must not add tools, got {:?}",
        tool_ids(&resolved.tools)
    );
}

#[tokio::test]
async fn graph_configured_registers_code_graph() {
    let root = tempfile::tempdir().unwrap();
    write_workspace(root.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let harness = start_code_intelligence(root.path()).await;
    let parsed = CodingAgentInvocationOptions::default();
    let resolved = resolve_application_context(
        parsed,
        run_options(&harness),
        root.path().to_path_buf(),
        root.path().to_path_buf(),
    )
    .expect("application context resolves");
    let ids = tool_ids(&resolved.tools);
    assert!(
        ids.iter().any(|id| id == "code_graph"),
        "code_graph must be registered, got {ids:?}"
    );
    assert!(
        !ids.iter().any(|id| id == "code_lsp"),
        "code_lsp must not be registered without an LSP handle, got {ids:?}"
    );
    harness.handle.shutdown("test");
    harness.task.join().await;
}

#[tokio::test]
async fn graph_and_lsp_configured_registers_both_tools() {
    let root = tempfile::tempdir().unwrap();
    write_workspace(root.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let harness = start_code_intelligence(root.path()).await;
    // LSP handle：spawn 必然失败的配置即可（装配不要求服务可用）。
    let lsp_config = code_intelligence::api::LspServerConfig::new(
        "/nonexistent/lsp-binary",
        root.path().to_path_buf(),
        TaskOwner::Operation("code-tools-test".into()),
    );
    let (lsp_handle, lsp_task) = code_intelligence::api::LspService::new(lsp_config)
        .start()
        .expect("lsp service starts");
    let mut options = run_options(&harness);
    options.code_intelligence = Some(CodeIntelligenceRunOptions {
        graph: harness.handle.clone(),
        lsp: Some((lsp_handle.clone(), root.path().to_path_buf())),
    });
    let parsed = CodingAgentInvocationOptions::default();
    let resolved = resolve_application_context(
        parsed,
        options,
        root.path().to_path_buf(),
        root.path().to_path_buf(),
    )
    .expect("application context resolves");
    let ids = tool_ids(&resolved.tools);
    assert!(
        ids.iter().any(|id| id == "code_graph"),
        "code_graph must be registered, got {ids:?}"
    );
    assert!(
        ids.iter().any(|id| id == "code_lsp"),
        "code_lsp must be registered, got {ids:?}"
    );
    lsp_handle.shutdown();
    lsp_task.join().await;
    harness.handle.shutdown("test");
    harness.task.join().await;
}

#[tokio::test]
async fn explicit_tools_survive_code_tool_append() {
    let root = tempfile::tempdir().unwrap();
    write_workspace(root.path(), &[("a.rs", "pub fn alpha() {}\n")]);
    let harness = start_code_intelligence(root.path()).await;
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
    let mut options = run_options(&harness);
    options.tools = vec![tool];
    let parsed = CodingAgentInvocationOptions::default();
    let resolved = resolve_application_context(
        parsed,
        options,
        root.path().to_path_buf(),
        root.path().to_path_buf(),
    )
    .expect("application context resolves");
    let ids = tool_ids(&resolved.tools);
    assert_eq!(ids.len(), 2, "explicit tool + code_graph: {ids:?}");
    assert!(ids.iter().any(|id| id == "ls"));
    assert!(ids.iter().any(|id| id == "code_graph"));
    harness.handle.shutdown("test");
    harness.task.join().await;
}
