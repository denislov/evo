//! `ScopeGraph`：单文件的符号图（petgraph 有向图）。
//!
//! 移植自 Grok `graph.rs` 中 `ScopeGraph` 部分；Evo 裁剪：
//!
//! - 节点直接携带名字（不再依赖 src 切片，见 `nodes.rs`），因此
//!   `get_definitions(src)` 等带 src 的方法改为无参版本；
//! - 新增 containment 边（`(child_def, parent_def)`），满足 ARC-810 的
//!   containment 需求——Grok 只有 DefToScope（def → 作用域），没有
//!   def → def 的父子符号关系；
//! - `QueryVersion` 已在 `identity.rs`，`ScopeGraphIndex` 在 `index.rs`，
//!   序列化在 `persist.rs`，`Snippet` 不需要，全部不在此文件。
//! - ref → def 的解析边（`RefToDef` / `RefToImport`）仍构建，但不参与
//!   持久化（跨文件查询由 `index.rs` 的二级索引回答）。

// Adapted from xai-codebase-graph, SOURCE_REV d6937fe255dce4133c3d000a50f9cb94de12f06f
// (scope_graph/graph.rs: ScopeGraph, ScopeStack, from_symbols); trimmed and
// extended for Evo — name-carrying nodes, containment edges, no src
// accessors, no direct serde (persistence goes through `persist.rs`).
use petgraph::Graph;
use petgraph::{Direction, visit::EdgeRef};

use super::edges::EdgeKind;
use super::nodes::{LocalDef, LocalImport, LocalScope, NodeKind, Reference};
use super::persist::PersistedGraph;
use super::range::Range;

/// 图节点索引（u32 紧凑索引）。
pub type NodeIndex = petgraph::graph::NodeIndex<u32>;

/// 单文件符号图。
#[derive(Debug, Clone)]
pub struct ScopeGraph {
    /// 原始图（节点 = scope / def / import / ref）。
    pub(crate) graph: Graph<NodeKind, EdgeKind>,
    /// 根 scope 节点。
    root_idx: NodeIndex,
    /// 语言主 id（诊断用）。
    lang: String,
    /// containment 边：`(child_def, parent_def)` 节点索引对。定义按
    /// 提取顺序编号，child 的声明体被 parent 的声明体严格包含。
    containment: Vec<(NodeIndex, NodeIndex)>,
}

impl ScopeGraph {
    pub fn new(range: Range, lang: String) -> Self {
        let mut graph = Graph::new();
        let root_idx = graph.add_node(NodeKind::scope(range));
        Self {
            graph,
            root_idx,
            lang,
            containment: Vec::new(),
        }
    }

    pub fn lang(&self) -> &str {
        &self.lang
    }

    pub fn root_idx(&self) -> NodeIndex {
        self.root_idx
    }

    /// containment 边（`(child_def, parent_def)` 节点索引）。
    pub fn containment(&self) -> &[(NodeIndex, NodeIndex)] {
        &self.containment
    }

    /// 登记一条 containment 边。
    pub fn add_containment(&mut self, child: NodeIndex, parent: NodeIndex) {
        self.containment.push((child, parent));
    }

    pub fn is_definition(&self, node_idx: NodeIndex) -> bool {
        matches!(self.graph[node_idx], NodeKind::Def(_))
    }

    pub fn is_reference(&self, node_idx: NodeIndex) -> bool {
        matches!(self.graph[node_idx], NodeKind::Ref(_))
    }

    pub fn is_import(&self, node_idx: NodeIndex) -> bool {
        matches!(self.graph[node_idx], NodeKind::Import(_))
    }

    /// 按字节范围找一个 def / ref / import 节点（范围包含给定区间）。
    pub fn node_by_range(&self, start_byte: usize, end_byte: usize) -> Option<NodeIndex> {
        self.graph
            .node_indices()
            .filter(|&idx| self.is_definition(idx) || self.is_reference(idx) || self.is_import(idx))
            .find(|&idx| {
                let range = self.graph[idx].range();
                start_byte >= range.start_byte() && end_byte <= range.end_byte()
            })
    }

    /// 给定范围内最紧（最小字节跨度）的 def 节点。
    pub fn tightest_node_for_range(&self, start_byte: usize, end_byte: usize) -> Option<NodeIndex> {
        self.graph
            .node_indices()
            .filter(|&idx| self.is_definition(idx))
            .filter(|&idx| {
                let range = self.graph[idx].range();
                range.start_byte() >= start_byte && range.end_byte() <= end_byte
            })
            .min_by_key(|&idx| self.graph[idx].range().byte_size())
    }

    /// 最小的包含 `range` 的 scope（从 `start` 开始逐层下钻）。
    fn scope_by_range(&self, range: Range, start: NodeIndex) -> Option<NodeIndex> {
        let target_range = self.graph[start].range();
        if target_range.contains(&range) {
            let child_scopes = self
                .graph
                .edges_directed(start, Direction::Incoming)
                .filter(|edge| *edge.weight() == EdgeKind::ScopeToScope)
                .map(|edge| edge.source())
                .collect::<Vec<_>>();
            for child_scope in child_scopes {
                if let Some(t) = self.scope_by_range(range, child_scope) {
                    return Some(t);
                }
            }
            return Some(start);
        }
        None
    }

    /// 插入一个局部作用域。
    pub fn insert_local_scope(&mut self, new: LocalScope) {
        if let Some(parent_scope) = self.scope_by_range(new.range, self.root_idx) {
            let new_scope = NodeKind::Scope(new);
            let new_idx = self.graph.add_node(new_scope);
            self.graph
                .add_edge(new_idx, parent_scope, EdgeKind::ScopeToScope);
        }
    }

    /// 找到包含 `range` 的最紧局部作用域。
    pub fn find_tightest_local_scope(&self, range: &Range) -> LocalScope {
        let mut current_node = self.root_idx;
        loop {
            let mut found = false;
            for edge in self.graph.edges_directed(current_node, Direction::Incoming) {
                if let EdgeKind::ScopeToScope = edge.weight() {
                    let node = &self.graph[edge.source()];
                    if let NodeKind::Scope(scope) = node
                        && scope.range.contains(range)
                    {
                        current_node = edge.source();
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                break;
            }
        }
        if let NodeKind::Scope(scope) = &self.graph[current_node] {
            scope.clone()
        } else {
            unreachable!("root node is always a scope")
        }
    }

    /// 插入一个 import（挂到包含它的作用域）。
    pub fn insert_local_import(&mut self, new: LocalImport) {
        if let Some(defining_scope) = self.scope_by_range(new.range, self.root_idx) {
            let new_imp = NodeKind::Import(new);
            let new_idx = self.graph.add_node(new_imp);
            self.graph
                .add_edge(new_idx, defining_scope, EdgeKind::ImportToScope);
        }
    }

    /// 插入一个 hoisted 定义（挂到定义作用域的父作用域；无父则挂本作用域）。
    pub fn insert_hoisted_def(&mut self, new: LocalDef) {
        if let Some(defining_scope) = self.scope_by_range(new.range, self.root_idx) {
            let new_def = NodeKind::Def(new);
            let new_idx = self.graph.add_node(new_def);
            let target_scope = self.parent_scope(defining_scope).unwrap_or(defining_scope);
            self.graph
                .add_edge(new_idx, target_scope, EdgeKind::DefToScope);
        }
    }

    /// 插入一个全局定义（挂到根 scope）。
    pub fn insert_global_def(&mut self, new: LocalDef) {
        let new_def = NodeKind::Def(new);
        let new_idx = self.graph.add_node(new_def);
        self.graph
            .add_edge(new_idx, self.root_idx, EdgeKind::DefToScope);
    }

    fn parent_scope(&self, start: NodeIndex) -> Option<NodeIndex> {
        if matches!(self.graph[start], NodeKind::Scope(_)) {
            return self
                .graph
                .edges_directed(start, Direction::Outgoing)
                .filter(|edge| *edge.weight() == EdgeKind::ScopeToScope)
                .map(|edge| edge.target())
                .next();
        }
        None
    }

    /// 插入一个局部定义（挂到包含它的作用域）。
    pub fn insert_local_def(&mut self, new: LocalDef) {
        if let Some(defining_scope) = self.scope_by_range(new.range, self.root_idx) {
            let new_def = NodeKind::Def(new);
            let new_idx = self.graph.add_node(new_def);
            self.graph
                .add_edge(new_idx, defining_scope, EdgeKind::DefToScope);
        }
    }

    fn scope_stack(&self, start: NodeIndex) -> ScopeStack<'_> {
        ScopeStack {
            scope_graph: self,
            start: Some(start),
        }
    }

    /// 插入一个引用并尝试解析：沿 scope 栈向上，在每一层匹配同名 def /
    /// import（namespace 语义：def 与 ref 的 `SymbolId` namespace 不同则
    /// 不匹配；空 symbol id 匹配所有 namespace）。
    pub fn insert_ref(&mut self, new: Reference) {
        let mut possible_defs = vec![];
        let mut possible_imports = vec![];
        let local_scope_idx = self.scope_by_range(new.range, self.root_idx);
        let start = local_scope_idx.unwrap_or(self.root_idx);
        for scope_idx in self.scope_stack(start) {
            for local_def in self
                .graph
                .edges_directed(scope_idx, Direction::Incoming)
                .filter(|edge| *edge.weight() == EdgeKind::DefToScope)
                .map(|edge| edge.source())
            {
                if let NodeKind::Def(def) = &self.graph[local_def]
                    && new.name == def.name
                {
                    match (&def.symbol_id, &new.symbol_id) {
                        // 都有符号 id 但 namespace 不同：不匹配。
                        (Some(d), Some(r)) if d.namespace_idx != r.namespace_idx => {}
                        _ => possible_defs.push(local_def),
                    }
                }
            }
            for local_import in self
                .graph
                .edges_directed(scope_idx, Direction::Incoming)
                .filter(|edge| *edge.weight() == EdgeKind::ImportToScope)
                .map(|edge| edge.source())
            {
                if let NodeKind::Import(import) = &self.graph[local_import]
                    && new.name == import.name
                {
                    possible_imports.push(local_import);
                }
            }
        }

        if !possible_defs.is_empty() || !possible_imports.is_empty() {
            let new_ref = NodeKind::Ref(new);
            let ref_idx = self.graph.add_node(new_ref);
            for def_idx in possible_defs {
                self.graph.add_edge(ref_idx, def_idx, EdgeKind::RefToDef);
            }
            for imp_idx in possible_imports {
                self.graph.add_edge(ref_idx, imp_idx, EdgeKind::RefToImport);
            }
        }
    }

    /// 无条件插入一个引用（跨文件引用跟踪：即使文件内没有定义也保留）。
    pub fn insert_ref_unconditional(&mut self, new: Reference) {
        let new_ref = NodeKind::Ref(new);
        self.graph.add_node(new_ref);
    }

    /// 全部定义（按提取顺序）。
    pub fn definitions(&self) -> Vec<&LocalDef> {
        self.graph
            .node_indices()
            .filter_map(|idx| match &self.graph[idx] {
                NodeKind::Def(def) => Some(def),
                _ => None,
            })
            .collect()
    }

    /// 全部定义节点索引 + 定义（按提取顺序）。
    pub fn definition_nodes(&self) -> Vec<(NodeIndex, &LocalDef)> {
        self.graph
            .node_indices()
            .filter_map(|idx| match &self.graph[idx] {
                NodeKind::Def(def) => Some((idx, def)),
                _ => None,
            })
            .collect()
    }

    /// 全部引用（按提取顺序）。
    pub fn references(&self) -> Vec<&Reference> {
        self.graph
            .node_indices()
            .filter_map(|idx| match &self.graph[idx] {
                NodeKind::Ref(reference) => Some(reference),
                _ => None,
            })
            .collect()
    }

    /// 全部 import（按提取顺序）。
    pub fn imports(&self) -> Vec<&LocalImport> {
        self.graph
            .node_indices()
            .filter_map(|idx| match &self.graph[idx] {
                NodeKind::Import(import) => Some(import),
                _ => None,
            })
            .collect()
    }

    /// 按名字找定义（返回首个匹配的范围）。
    pub fn find_definition(&self, name: &str) -> Option<Range> {
        self.graph.node_indices().find_map(|idx| {
            if let NodeKind::Def(def) = &self.graph[idx]
                && def.name == name
            {
                return Some(def.range);
            }
            None
        })
    }

    /// 按名字找全部引用范围。
    pub fn find_references(&self, name: &str) -> Vec<Range> {
        self.graph
            .node_indices()
            .filter_map(|idx| {
                if let NodeKind::Ref(reference) = &self.graph[idx]
                    && reference.name == name
                {
                    return Some(reference.range);
                }
                None
            })
            .collect()
    }

    /// 从持久化数据重建查询友好的图（defs / imports 挂根 scope，
    /// refs 无解析边——与 Grok `from_symbols` 同一语义；文件内 ref 解析
    /// 由跨文件二级索引回答）。
    pub fn from_persisted(lang: String, data: &PersistedGraph) -> Self {
        let root_range = data
            .definitions
            .first()
            .map(|d| d.range)
            .unwrap_or_default();
        let mut graph = Graph::new();
        let root_idx = graph.add_node(NodeKind::scope(root_range));

        for def in &data.definitions {
            let local_def = LocalDef::new(
                def.name.clone(),
                def.symbol_type.clone(),
                def.range,
                def.symbol_id,
                LocalScope { range: root_range },
            );
            let def_idx = graph.add_node(NodeKind::Def(local_def));
            graph.add_edge(def_idx, root_idx, EdgeKind::DefToScope);
        }
        for import in &data.imports {
            let local_import = LocalImport::new(import.name.clone(), import.range);
            let import_idx = graph.add_node(NodeKind::Import(local_import));
            graph.add_edge(import_idx, root_idx, EdgeKind::ImportToScope);
        }
        for reference in &data.references {
            graph.add_node(NodeKind::Ref(Reference::new(
                reference.name.clone(),
                reference.range,
                reference.symbol_id,
            )));
        }

        let mut graph = Self {
            graph,
            root_idx,
            lang,
            containment: Vec::new(),
        };
        for (child, parent) in &data.containment {
            if let (Some(c), Some(p)) = (
                graph.def_index(*child as usize),
                graph.def_index(*parent as usize),
            ) {
                graph.containment.push((c, p));
            }
        }
        graph
    }

    /// 把定义序号（持久化 containment 索引）映射为节点索引。
    fn def_index(&self, ordinal: usize) -> Option<NodeIndex> {
        self.graph
            .node_indices()
            .filter(|&idx| self.is_definition(idx))
            .nth(ordinal)
    }

    /// 转持久化数据（defs / refs / imports / containment；解析边不持久化）。
    pub fn to_persisted(&self) -> PersistedGraph {
        let definitions = self.definitions();
        let definition_nodes = self.definition_nodes();
        let containment = self
            .containment
            .iter()
            .filter_map(|(child, parent)| {
                let child_ordinal = definition_nodes.iter().position(|(idx, _)| idx == child)?;
                let parent_ordinal = definition_nodes.iter().position(|(idx, _)| idx == parent)?;
                Some((child_ordinal as u32, parent_ordinal as u32))
            })
            .collect();
        PersistedGraph {
            definitions: definitions
                .iter()
                .map(|def| super::persist::PersistedDef {
                    name: def.name.clone(),
                    symbol_type: def.symbol_type.clone(),
                    range: def.range,
                    symbol_id: def.symbol_id,
                })
                .collect(),
            references: self
                .references()
                .iter()
                .map(|reference| super::persist::PersistedRef {
                    name: reference.name.clone(),
                    range: reference.range,
                    symbol_id: reference.symbol_id,
                })
                .collect(),
            imports: self
                .imports()
                .iter()
                .map(|import| super::persist::PersistedImport {
                    name: import.name.clone(),
                    range: import.range,
                })
                .collect(),
            containment,
        }
    }

    /// 符号计数（诊断）。
    pub fn stats(&self) -> (usize, usize) {
        (self.definitions().len(), self.references().len())
    }
}

/// 从当前 scope 到根 scope 的遍历迭代器。
pub struct ScopeStack<'a> {
    scope_graph: &'a ScopeGraph,
    start: Option<NodeIndex>,
}

impl<'a> Iterator for ScopeStack<'a> {
    type Item = NodeIndex;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(start) = self.start {
            let parent = self
                .scope_graph
                .graph
                .edges_directed(start, Direction::Outgoing)
                .find(|edge| *edge.weight() == EdgeKind::ScopeToScope)
                .map(|edge| edge.target());
            let original = start;
            self.start = parent;
            Some(original)
        } else {
            None
        }
    }
}
