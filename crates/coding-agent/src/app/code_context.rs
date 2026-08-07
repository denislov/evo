//! ARC-830：按需 code context 注入 seam。
//!
//! 产品原则：**不把完整符号图塞入 system prompt**。本 seam 提供
//! 「给定符号名称片段 → 预算内的符号摘要」的按需查询入口：
//!
//! - 查询经 `CodeIntelligenceHandle` 提交 `SymbolSearch`（服务查询面）；
//! - [`code_intelligence::context`] 负责字节预算截断 + 显式标记；
//! - 本模块只做编排与渲染，无结果返回 `Ok(None)`（不产生 context
//!   块，不污染 prompt）。
//!
//! 注入深度（债务登记）：agent-core 的 per-turn `assemble_context`
//! 公共 API 不在本 ARC 改动；本 seam 是 coding-agent 侧的接线点——
//! 后续 ARC 在 per-turn 路径调用 [`query_symbol_context`] 并把渲染块
//! 追加进 turn context 即可，无需再动查询面。

// Evo 独立设计（Grok 无按需 context 注入概念，见
// docs/refactor/phase8-tools-context.md 的差异清单）。
use code_intelligence::api::{
    BoundedSymbolSearch, CodeIntelligenceHandle, GraphQueryResult, QueryKind, QueryRequest,
    SymbolContextBudget, SymbolContextSnippet, context_field, render_symbol_context,
};
use serde_json::json;

use crate::app::error::ApplicationError;

/// 按需查询：给定符号名称片段，返回预算内的符号摘要。
///
/// - 无匹配符号 → `Ok(None)`（不产生 context 块）；
/// - 预算内截断（条数 / 字节）在结果中显式标记，不静默截断；
/// - 服务不可用（未启动 / 关闭中）→ 结构化 `SessionFailure`。
///
/// 当前为占位 seam（per-turn 调用点在后续 ARC 接线，见
/// `docs/refactor/phase8-tools-context.md` 债务登记「context 注入深度」；
/// 测试覆盖全部边界，lib 构建不产生调用点）。
#[allow(dead_code)]
pub async fn query_symbol_context(
    handle: &CodeIntelligenceHandle,
    symbol: &str,
    budget: &SymbolContextBudget,
) -> Result<Option<SymbolContextSnippet>, ApplicationError> {
    let search = submit_symbol_search(handle, symbol, budget.max_results).await?;
    if search.total == 0 {
        return Ok(None);
    }
    let snippet = render_symbol_context(&search, budget);
    Ok(Some(snippet))
}

/// 把摘要渲染为有界的 `<code_context>` 文本块（空摘要 → `None`）。
/// 占位说明同 [`query_symbol_context`]。
#[allow(dead_code)]
pub fn render_code_context_block(snippet: &SymbolContextSnippet) -> Option<String> {
    code_intelligence::api::render_context_text(snippet)
}

async fn submit_symbol_search(
    handle: &CodeIntelligenceHandle,
    symbol: &str,
    limit: usize,
) -> Result<BoundedSymbolSearch, ApplicationError> {
    let request = QueryRequest::new(
        QueryKind::SymbolSearch,
        json!({ context_field::SYMBOL: symbol, context_field::LIMIT: limit }),
    );
    let response = handle.submit(request).await.map_err(|error| {
        ApplicationError::SessionFailure(format!("on-demand code context query failed: {error}"))
    })?;
    match response.graph {
        Some(GraphQueryResult::Search(search)) => Ok(search),
        _ => Err(ApplicationError::SessionFailure(
            "on-demand code context query returned no search result".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_intelligence::api::{
        CodeIntelligenceService, CodeIntelligenceServiceOptions, GraphBackendOptions,
        GraphQueryBackend, IndexBudget,
    };
    use std::sync::Arc;

    fn write_workspace(root: &std::path::Path, files: &[(&str, &str)]) {
        for (rel, content) in files {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, content).unwrap();
        }
    }

    async fn service_with_workspace(
        files: &[(&str, &str)],
    ) -> (
        tempfile::TempDir,
        CodeIntelligenceHandle,
        code_intelligence::api::CodeIntelligenceTask,
    ) {
        let dir = tempfile::tempdir().unwrap();
        write_workspace(dir.path(), files);
        let backend = GraphQueryBackend::new(GraphBackendOptions {
            root: dir.path().to_path_buf(),
            cache_path: None,
            identity: code_intelligence::api::CacheIdentity {
                workspace: workspace_runtime::api::WorkspaceId::user_supplied(
                    workspace_runtime::api::WorkspaceKind::Source,
                    "code-context-test",
                )
                .unwrap(),
                revision: code_intelligence::api::RevisionId::parse("rev-1").unwrap(),
                parser_version: code_intelligence::api::ParserVersion::Version(1),
            },
            registry: code_intelligence::api::LanguageRegistry::builtin(),
            budget: IndexBudget::default(),
        })
        .expect("backend builds");
        let service = CodeIntelligenceService::new(CodeIntelligenceServiceOptions {
            identity: code_intelligence::api::CacheIdentity {
                workspace: workspace_runtime::api::WorkspaceId::user_supplied(
                    workspace_runtime::api::WorkspaceKind::Source,
                    "code-context-test",
                )
                .unwrap(),
                revision: code_intelligence::api::RevisionId::parse("rev-1").unwrap(),
                parser_version: code_intelligence::api::ParserVersion::Version(1),
            },
            cache_path: None,
            budget: IndexBudget::default(),
            languages: code_intelligence::api::LanguageRegistry::builtin(),
            backend: Some(Arc::new(backend)),
        });
        let (handle, task) = service.start().expect("service starts");
        (dir, handle, task)
    }

    #[tokio::test]
    async fn no_matches_yields_none() {
        let (_dir, handle, task) = service_with_workspace(&[("a.rs", "pub fn alpha() {}\n")]).await;
        let result = query_symbol_context(&handle, "nonexistent", &SymbolContextBudget::default())
            .await
            .expect("query succeeds");
        assert!(result.is_none());
        handle.shutdown("test");
        task.join().await;
    }

    #[tokio::test]
    async fn bounded_truncation_is_marked_and_rendered() {
        let (_dir, handle, task) = service_with_workspace(&[(
            "a.rs",
            "pub fn target_alpha() {}\npub fn target_beta() {}\npub fn target_gamma() {}\n",
        )])
        .await;
        let budget = SymbolContextBudget {
            max_results: 2,
            max_bytes: 4096,
        };
        let snippet = query_symbol_context(&handle, "target_", &budget)
            .await
            .expect("query succeeds")
            .expect("matches exist");
        assert_eq!(snippet.total, 3);
        assert_eq!(snippet.kept, 2);
        assert!(snippet.truncated, "truncation must be marked");
        let block = render_code_context_block(&snippet).expect("non-empty block");
        assert!(block.contains("<code_context>"));
        assert!(block.contains("1 more match(es) truncated"));
        handle.shutdown("test");
        task.join().await;
    }

    #[tokio::test]
    async fn stopped_service_returns_structured_error() {
        let (_dir, handle, task) = service_with_workspace(&[("a.rs", "pub fn alpha() {}\n")]).await;
        handle.shutdown("test");
        task.join().await;
        let error = query_symbol_context(&handle, "alpha", &SymbolContextBudget::default())
            .await
            .expect_err("stopped service must fail");
        assert!(matches!(error, ApplicationError::SessionFailure(_)));
    }

    #[tokio::test]
    async fn zero_result_budget_still_returns_all_within_byte_budget() {
        let (_dir, handle, task) = service_with_workspace(&[("a.rs", "pub fn alpha() {}\n")]).await;
        let budget = SymbolContextBudget {
            max_results: 0, // 不限条数
            max_bytes: 4096,
        };
        let snippet = query_symbol_context(&handle, "alpha", &budget)
            .await
            .expect("query succeeds")
            .expect("matches exist");
        assert_eq!(snippet.kept, 1);
        assert!(!snippet.truncated);
        handle.shutdown("test");
        task.join().await;
    }
}
