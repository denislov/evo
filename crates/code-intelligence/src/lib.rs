//! # code-intelligence
//!
//! Evo 的本地代码理解服务（Phase 8）：在稳定 `workspace-runtime` 之上
//! 提供可增量更新的索引与查询 API 边界，服务 API 与 tool adapter 分离，
//! 核心可被 CLI / Desktop / agent tool 共用。
//!
//! 本 crate 落地：
//!
//! - 服务 API：[`service::CodeIntelligenceService`]（Arc handle + actor），
//!   查询请求经有界通道顺序处理，确定性 shutdown 顺序、panic fail-closed。
//! - 索引缓存：[`cache::IndexCache`] —— workspace / revision /
//!   parser-version 三要素 identity；损坏 / 截断 / 旧格式 -> 结构化错误 +
//!   重建路径，不 panic；原子写避免 crash 半成品。
//! - 预算类型：[`budget::IndexBudget`]（文件数 / 总字节 / 单文件解析时长 /
//!   并发解析数）+ 记账器 [`budget::IndexBudgetTracker`]。
//! - 语言注册表：[`languages::LanguageRegistry`]（language id ↔ 扩展名 +
//!   tree-sitter grammar / query），首批五语言：Rust / TypeScript /
//!   JavaScript / Python / Go。
//! - **codebase graph（ARC-810）**：[`graph`] 模块 —— 单文件符号图
//!   （`ScopeGraph`，含 containment 边）+ 跨文件索引（`CodebaseIndex`）+
//!   查询（`GraphNavigator`）+ 全量构建（`IndexBuilder`，预算强制）+
//!   增量 reindex（`IncrementalIndexer`，消费 change-tracker 事件流）+
//!   持久化（`GraphCacheData`，JSON）。
//!
//! 扩展点（留给后续 ARC）：
//!
//! - [`service::QueryBackend`]：ARC-820 提供 LSP diagnostics backend；
//!   实现该 trait 即接入服务，无需改动 actor。
//! - [`service::QueryKind`]：`Diagnostics`（ARC-820）已声明，当前返回
//!   `Unimplemented`。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f;
// module organization (index_manager / manager/cache / languages / types)
// consulted; crate-level design is Evo's own.
pub mod api;
pub mod budget;
pub mod cache;
pub mod error;
pub mod graph;
pub mod identity;
pub mod languages;
pub mod service;

pub use crate::api::*;

#[cfg(test)]
mod budget_tests;
#[cfg(test)]
mod cache_tests;
#[cfg(test)]
mod identity_tests;
#[cfg(test)]
mod languages_tests;
#[cfg(test)]
mod service_tests;
