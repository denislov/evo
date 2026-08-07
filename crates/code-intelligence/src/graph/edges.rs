//! ScopeGraph 的边类型。
//!
//! import/export 语义说明：`RefToImport` 覆盖 import 边；export 由提取
//! 阶段的 `exports` 列表承载（见 [`super::extract`]），图内不设独立的
//! export 边类型——与 Grok 一致（Grok 把 `name.reference.export` 当普通
//! reference capture 处理，无独立 export 边）。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (scope_graph/edges.rs); unchanged — Evo keeps the same five edge kinds.
use serde::{Deserialize, Serialize};

/// 图中两个节点之间的关系。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeKind {
    /// 嵌套 scope 到其父 scope。
    ScopeToScope,
    /// 定义到其定义 scope。
    DefToScope,
    /// import 到其定义 scope。
    ImportToScope,
    /// 引用到其定义。
    RefToDef,
    /// 引用到其 import。
    RefToImport,
}
