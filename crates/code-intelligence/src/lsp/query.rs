//! LSP 查询面：hover / definition / references 等 read-only 查询的统一
//! 入口。
//!
//! 设计决策（详见 `docs/refactor/phase8-lsp.md`）：LSP 查询**不并入**
//! `QueryBackend` trait —— `QueryBackend::query` 是同步签名（actor 在
//! 独立 task 中执行），而 LSP 查询是 async 网络往返（等待语言服务器
//! 响应），无法塞进同步 trait；且 LSP 服务与索引服务生命周期不同
//! （子进程 start/restart vs 内存索引）。ARC-830 的 tool adapter 直接
//! 消费 [`LspHandle::query`]。
//!
//! 本模块只定义查询形状与参数组装；实际请求经
//! [`crate::lsp::server::LspHandle`] 转发，原始 JSON 响应由 ARC-830 做
//! 语义映射（hover 的 markdown 提取、definition 的位置解析等见债务）。

// Evo 独立设计：查询面形状为 Evo 自研（Grok 的 LSP 工具直接调
// async-lsp 的 client 请求）。
use serde::{Deserialize, Serialize};

use crate::lsp::documents::Position;

/// 查询类型（read-only）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspQueryKind {
    Hover,
    Definition,
    References,
}

impl LspQueryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LspQueryKind::Hover => "hover",
            LspQueryKind::Definition => "definition",
            LspQueryKind::References => "references",
        }
    }

    /// 对应的方法名。
    pub fn method(self) -> &'static str {
        match self {
            LspQueryKind::Hover => "textDocument/hover",
            LspQueryKind::Definition => "textDocument/definition",
            LspQueryKind::References => "textDocument/references",
        }
    }
}

/// 一次查询（位置 + 类型）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspQuery {
    pub kind: LspQueryKind,
    pub uri: String,
    pub position: Position,
}

/// 查询结果：服务器原始响应 JSON。
///
/// `null` 表示服务器没有结果（hover 无内容 / 无定义）。语义映射
/// （markdown / 位置列表）由 ARC-830 消费层负责。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspQueryResult {
    pub kind: LspQueryKind,
    pub uri: String,
    /// 服务器返回的 `result`（`null` = 无结果）。
    pub result: serde_json::Value,
}

/// 组装请求 params（`textDocument` + `position`，references 带
/// `context.includeDeclaration`）。
pub fn query_params(query: &LspQuery) -> serde_json::Value {
    let mut params = serde_json::json!({
        "textDocument": {"uri": query.uri},
        "position": {
            "line": query.position.line,
            "character": query.position.character,
        },
    });
    if query.kind == LspQueryKind::References {
        params["context"] = serde_json::json!({"includeDeclaration": true});
    }
    params
}

/// 构造查询请求。
pub fn query_request(query: &LspQuery) -> (String, serde_json::Value) {
    (query.kind.method().to_string(), query_params(query))
}
