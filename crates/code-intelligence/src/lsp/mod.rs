//! LSP 模块（ARC-820）：语言服务器生命周期治理。
//!
//! 模块组织（详见 `docs/refactor/phase8-lsp.md`）：
//!
//! - [`wire`]：LSP JSON-RPC wire（Content-Length framing 读写、严格解析、
//!   错误分类）——手写，不引入 async-lsp / lsp-types（决策见文档）。
//! - [`state`]：生命周期状态机（纯决策层 + transition 表测试）与指数退避。
//! - [`documents`]：document open/change/close 状态、版本跟踪、change
//!   合并、重启 replay 列表、UTF-16 偏移换算。
//! - [`diagnostics`]：push/pull 诊断存储 + stale policy 状态机。
//! - [`transport`]：stdio 帧会话（SandboxProfile 强制 spawn、读循环、
//!   id 分发、服务器请求回执、取消/超时）。
//! - [`server`]：`LspService` actor（生命周期驱动、restart + backoff +
//!   document replay、确定性 shutdown）。
//! - [`edit`]：`workspace/applyEdit` → 校验 → mutation 计划 → 注入的
//!   受限 applicator（ChangeReceipt）；本模块绝不直接写磁盘。
//! - [`query`]：hover / definition / references 查询面（统一入口）。
//!
//! 与 [`crate::service::CodeIntelligenceService`] 平级：LSP 是独立服务
//! （独立 handle + actor），不并入 `QueryBackend` trait（查询是 async
//! 网络往返，`QueryBackend::query` 是同步签名；且生命周期差异大——
//! 子进程 start/restart vs 内存索引）。

pub mod diagnostics;
pub mod documents;
pub mod edit;
pub mod query;
pub mod server;
pub mod state;
pub mod transport;
pub mod wire;

pub use crate::lsp::diagnostics::{
    DiagnosticItem, DiagnosticSeverity, DiagnosticStaleness, DiagnosticStore, StalePolicy,
    StoredDiagnostics,
};
pub use crate::lsp::documents::{
    ContentChange, DocumentError, DocumentStore, DocumentUri, OpenDocument, Position, Range,
};
pub use crate::lsp::edit::{
    EditApplicator, EditError, EditPlan, PlannedChange, TextDocumentEdit, TextEdit, WorkspaceEdit,
};
pub use crate::lsp::query::{LspQuery, LspQueryKind, LspQueryResult};
pub use crate::lsp::server::{
    BackoffConfig, LivenessConfig, LspError, LspExit, LspHandle, LspServerConfig, LspService,
    LspShutdownReason, LspSnapshot, LspTask,
};
pub use crate::lsp::state::{LspEvent, LspLifecycleState};

#[cfg(test)]
mod diagnostics_tests;
#[cfg(test)]
mod edit_tests;
#[cfg(test)]
mod wire_tests;
