//! ScopeGraph 的节点类型。
//!
//! 移植自 Grok `nodes.rs`；Evo 扩展：`LocalDef` / `LocalImport` / `Reference`
//! 直接携带 `name`（Grok 依赖调用方持有 src 再从字节切片取名，Evo 的查询
//! 面不允许依赖 src，因此在提取阶段落名）。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (scope_graph/nodes.rs); Evo extension: nodes carry their name/symbol_type
// strings directly instead of slicing from source bytes.
use serde::{Deserialize, Serialize};

use super::range::Range;

/// 符号的全局唯一类型标识：`namespace_idx` 指向语言配置的
/// `namespaces[n]`，`symbol_idx` 指向该 namespace 内的第 k 个类型名。
/// 用于 ref 解析时的 namespace 匹配（如同名函数与类型互不误配）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SymbolId {
    pub namespace_idx: usize,
    pub symbol_idx: usize,
}

impl SymbolId {
    pub fn new(namespace_idx: usize, symbol_idx: usize) -> Self {
        Self {
            namespace_idx,
            symbol_idx,
        }
    }
}

/// 源码中的一个局部作用域（大括号 / 函数体等）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct LocalScope {
    pub range: Range,
}

impl LocalScope {
    pub fn new(range: Range) -> Self {
        Self { range }
    }
}

/// 源码中的一个局部定义。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct LocalDef {
    /// 定义的名字（提取阶段落名）。
    pub name: String,
    /// 符号类型（如 `function` / `class` / `struct`），来自 capture 名后缀。
    pub symbol_type: String,
    /// 标识符所在范围（不含整个声明体）。
    pub range: Range,
    /// 类型感知解析用的符号 id。
    pub symbol_id: Option<SymbolId>,
    /// 定义可见的作用域。
    pub scope: LocalScope,
}

impl LocalDef {
    pub fn new(
        name: String,
        symbol_type: String,
        range: Range,
        symbol_id: Option<SymbolId>,
        scope: LocalScope,
    ) -> Self {
        Self {
            name,
            symbol_type,
            range,
            symbol_id,
            scope,
        }
    }
}

/// 源码中的一个局部 import（`use Foo` / `import { Foo }` 的标识符位置）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct LocalImport {
    pub name: String,
    pub range: Range,
}

impl LocalImport {
    pub fn new(name: String, range: Range) -> Self {
        Self { name, range }
    }
}

/// 源码中对一个符号的引用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    pub name: String,
    pub range: Range,
    /// 类型感知解析用的符号 id。
    pub symbol_id: Option<SymbolId>,
}

impl Reference {
    pub fn new(name: String, range: Range, symbol_id: Option<SymbolId>) -> Self {
        Self {
            name,
            range,
            symbol_id,
        }
    }
}

/// ScopeGraph 节点的种类。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeKind {
    Scope(LocalScope),
    Def(LocalDef),
    Import(LocalImport),
    Ref(Reference),
}

impl NodeKind {
    pub fn scope(range: Range) -> Self {
        Self::Scope(LocalScope::new(range))
    }

    /// 节点整体范围（def 返回其定义可见的作用域范围，捕获完整上下文）。
    pub fn range(&self) -> Range {
        match self {
            Self::Scope(scope) => scope.range,
            Self::Def(def) => def.scope.range,
            Self::Ref(reference) => reference.range,
            Self::Import(import) => import.range,
        }
    }

    /// 标识符的实际范围（跳转定位用）。
    pub fn identifier_range(&self) -> Range {
        match self {
            Self::Scope(scope) => scope.range,
            Self::Def(def) => def.range,
            Self::Ref(reference) => reference.range,
            Self::Import(import) => import.range,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn def_identifier_range_and_scope_range() {
        let scope = LocalScope::new(Range::default());
        let def = LocalDef::new(
            "foo".into(),
            "function".into(),
            Range::default(),
            None,
            scope,
        );
        assert_eq!(def.name, "foo");
        assert_eq!(def.symbol_type, "function");
        assert_eq!(def.range, Range::default());
        assert_eq!(def.scope.range, Range::default());
    }

    #[test]
    fn node_kind_identifier_range_selects_identifier() {
        let scope_range = Range::new(
            crate::graph::range::Position::new(0, 0, 0),
            crate::graph::range::Position::new(10, 0, 50),
        );
        let def = LocalDef::new(
            "bar".into(),
            "class".into(),
            Range::new(
                crate::graph::range::Position::new(2, 4, 20),
                crate::graph::range::Position::new(2, 7, 23),
            ),
            None,
            LocalScope::new(scope_range),
        );
        let node = NodeKind::Def(def);
        assert_eq!(node.range().start_line(), 0);
        assert_eq!(node.identifier_range().start_line_1indexed(), 3);
        assert_eq!(node.identifier_range().start_byte(), 20);
    }
}
