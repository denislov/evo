# Phase 8 / ARC-810：Codebase graph

> 状态：完成（2026-08-07）
> 前序：ARC-800（`code-intelligence` 骨架，见 `phase8-code-intelligence.md`）
> 目标：在 ARC-800 骨架上实现 codebase graph 本体 —— 首批五语言
> （Rust / TypeScript / JavaScript / Python / Go）的符号图、跨文件索引、
> read-only 查询、增量 reindex 与 reconcile、索引持久化。

## 决策

### 图结构：单文件 `ScopeGraph`（petgraph）+ 跨文件 `CodebaseIndex`

- **`ScopeGraph`**（`graph/scope.rs`）：移植 Grok 的 per-file petgraph
  `Graph<NodeKind, EdgeKind>`——节点为 scope / def / import / ref，边为
  `ScopeToScope` / `DefToScope` / `ImportToScope` / `RefToDef` /
  `RefToImport`（Grok `edges.rs` 五边不变）。Evo 扩展：
  - 节点直接携带 `name` / `symbol_type`（Grok 依赖调用方持 src 再从字节
    切片取名；Evo 查询面不携带 src，提取阶段落名）；
  - **containment 边**（`(child_def, parent_def)`）：Grok 只有 def→作用域，
    没有 def→def 的父子符号关系；Evo 在提取阶段用 `@definition.{sym}`
    capture 的**声明体**范围（而非名字标识符的单 token 范围）做嵌套推导
    （O(n²) 双循环，单文件 def 数量有限，简单性优先）。
- **`CodebaseIndex`**（`graph/index.rs`）：跨文件二级索引——符号名 →
  `(rel_path, 1-indexed line)` 定义 / 引用位置，加上 reverse index
  （`file_to_defs` / `file_to_refs`，文件删除 / 移动为 O(符号数)）与全局
  alias 表。用 `BTreeMap` 字符串键（确定性排序），去掉 Grok 的
  `StringInterner` 内存优化（首批规模不需要，见债务登记）。
- **import / export 语义**：import 边 = `RefToImport`（Grok 同款，ref →
  import 节点）；export 由提取产物的文件级 `exports` 列表承载
  （`name.reference.export` capture），不设独立 export 边类型——与 Grok
  一致（Grok 也把 export capture 当普通 reference）。Rust / Go / Python
  的查询文本没有 export capture，仅 TS / JS 覆盖（见债务登记）。

### 语言查询契约（`.scm` 直接移植 Grok）

- 五语言配置在 `languages/{rust,typescript,javascript,python,golang}.rs`，
  capture 名约定 `name.definition.{sym}` / `name.reference.{sym}` /
  `alias.original` / `alias.name`（Grok 契约，逐字移植；language ids 归一
  化为 Evo 小写，扩展名按 Evo 注册表基线）。
- `LanguageConfig::with_grammar` 承载 namespaces / query 文本 / grammar fn；
  `query_hash()` 切换为 Grok 的 `compute_query_hash` 方式（primary id +
  query 文本哈希），grammar 变化必然伴随 query 变化，query 文本足以代表
  解析器版本（ARC-800 债务偿还）。
- `symbol_id_of`（namespaces → `SymbolId`）保留 Grok 的 ref→def 匹配的
  namespace 语义（同名函数与类型不误配）。

### 查询契约（read-only）

- 实现 ARC-800 预留的 `QueryBackend`（`GraphQueryBackend`），`QueryKind`
  三种：
  - `FileSymbols`：context `{"path": "rel/path"}` → 文件符号树
    （containment 内嵌 children，按位置排序）；未索引文件 →
    `GraphQueryError::FileNotIndexed`。
  - `Definition` / `Reference`：context 两种形态——`{"symbol": "Foo"}`
    （按名，含 alias 解析）或 `{"path", "line", "column"}`（位置查询，
    参照 Grok navigation.rs：现读文件 + tree-sitter 解析 +
    `find_smallest_named_node_at_point`，1-indexed 行 / 列）。Reference
    支持 `{"include_definition": true}` 并入定义位置。
- 全部位置为 workspace-relative（正斜杠），行 / 列 1-indexed（Grok
  `Location` 契约）。
- 响应经 `QueryResponse.graph` 携带（`#[serde(default)]` 保持骨架兼容）；
  `status` 由 actor 用真实状态回填（backend 返回占位值）。
- `QueryBackend` trait 增加同步 `fn shutdown(&self) {}` 默认方法：服务
  actor 确定性退出前调用（停止增量消费 → 等待在途 → 持久化），panic 被
  catch_unwind 吞掉（fail closed，不改变退出语义）。

### 构建与预算强制（`graph/build.rs`）

- 全量构建：`ignore::WalkBuilder`（hidden + gitignore + git_global +
  git_exclude）收集 → 预算记账 → rayon 线程池并发解析 → 顺序合并。
- `IndexBudget` 四维强制：
  - 文件数 / 总字节：收集阶段 `IndexBudgetTracker::reserve_file` 记账，
    超限 → 该文件与后续全部跳过（结构化 `IndexSkipReason::BudgetExceeded`
    + 终止收集；预算在构建期快照，只增不减）；
  - 并发解析：rayon 线程池大小 = `max_concurrent_parses`（0 = 可用核心数）；
  - 单文件解析时长：`parse_time_limit` 计时，超限跳过（`ParseTimeout`）。
- 跳过原因全部结构化（`IndexSkip`：路径 + `IndexSkipReason`），构建报告
  携带；语言不支持 / 空文件 / 超 5 MiB（`MAX_INDEXABLE_FILE_SIZE`，
  Grok 同款）/ 二进制前缀（8 KiB 含 NUL）/ 读取失败 / 解析失败均有记录。
- 增量路径（`reindex_file`）不做预算强制（构建期快照；增量事件量级小），
  保留语言 / 空 / 大小 / 二进制检查（见债务登记）。

### 增量 reindex 与 reconcile（`graph/incremental.rs`）

- 新增依赖边 `code-intelligence → change-tracker`：`IncrementalIndexer`
  消费 `FsEventService.events()` 的 `FsEvent` 流：
  - `Created` / `Modified`（文件）→ `reindex_file` 重解析替换；
  - `Removed` → `remove_file`（符号消失，reverse index O(符号数)）；
  - `Renamed` → 旧路径 `rename_file`（移动符号）+ 目标重解析（内容可能
    已变）；
  - `WatchGap` / broadcast `Lagged` → 全量 `reconcile`：重新扫描
    workspace，删除消失文件、重解析 stale 文件（meta 对比）、加入新文件，
    修正漂移；
  - `Git` 事件与目录事件忽略（revision 语义由调用方决定，见债务登记）。
- 消费循环 `tokio::select!`（事件 + watch shutdown 信号），顺序处理；
  事件之间天然有序（单 receiver 顺序消费）。
- shutdown 顺序：`stop()` 发 watch 信号 → std mpsc 完成信号同步等待在途
  事件处理完（允许 `QueryBackend::shutdown` 保持同步签名）→ 调用方
  （`GraphQueryBackend::shutdown`）持久化 → 关闭；`Drop` 兜底同序。
- 与 Grok 差异：Grok 的 `IndexManager` 自建 channel + background refresh +
  磁盘锁；Evo 复用 change-tracker（debounce / rename 配对 / gitignore 过滤
  已归一化），WatchGap 全量 reconcile 收敛（无磁盘锁，见债务登记）。

### 持久化（`graph/persist.rs` + `cache.rs` 扩展）

- `IndexCacheData` 追加 `graph: Option<GraphCacheData>`（`#[serde(default)]`
  向后兼容）：schema 版本 + query 哈希（诊断冗余）+ per-file 持久化条目
  （rel_path + `FileMeta` + `PersistedGraph` + exports + 全局 alias 表）。
- `PersistedGraph` 只序列化查询所需结构：definitions（name / symbol_type /
  range / symbol_id）/ references / imports / containment；`RefToDef` /
  `RefToImport` 解析边不持久化（跨文件查询由二级索引回答，与 Grok 相同
  ——Grok 也不序列化 per-file graphs）。
- 格式选择 **JSON**（与 ARC-800 缓存载荷一致）：格式统一、可诊断、
  复用同一套 corruption / identity 错误路径；单文件符号量级下体积可接受
  （Grok 的二进制 "SGIX" 是内存 / 体积优化，见债务登记）。
- 恢复路径：probe 缓存 → identity 匹配 + graph 字段存在 → `from_persisted`
  重建（schema 不匹配 / 损坏 / identity mismatch / graph 缺失 → 全量重建，
  corruption recovery 沿用 ARC-800 机制）。

## 落点

| 变更 | 位置 |
| --- | --- |
| 图结构（range/nodes/edges/scope/extract） | `crates/code-intelligence/src/graph/{range,nodes,edges,scope,extract}.rs` |
| 跨文件索引 + 持久化数据模型 | `crates/code-intelligence/src/graph/{index,persist}.rs` |
| 查询 / 导航 | `crates/code-intelligence/src/graph/query.rs` |
| 构建（预算强制）+ 增量解析 | `crates/code-intelligence/src/graph/build.rs` |
| 增量 reindex actor + reconcile | `crates/code-intelligence/src/graph/incremental.rs` |
| QueryBackend 实现 + 服务接线 | `crates/code-intelligence/src/graph/backend.rs` |
| 语言 grammar / query | `crates/code-intelligence/src/languages/{rust,typescript,javascript,python,golang}.rs` |
| 服务骨架扩展（shutdown 钩子 / QueryResponse.graph） | `crates/code-intelligence/src/service.rs` |
| 缓存载荷扩展 | `crates/code-intelligence/src/cache.rs` |
| 依赖 | 根 `Cargo.toml`（tree-sitter 0.25.10、grammar 五件、petgraph 0.6.5、ignore 0.4、rayon 1.10） |
| 依赖边登记 | `scripts/architecture/internal-dependencies.tsv`（`code-intelligence → change-tracker`） |
| 测试 | `crates/code-intelligence/src/graph/{graph,build,incremental,query,persistence,backend}_tests.rs` + `test_support.rs` |
| 设计文档 | `docs/refactor/phase8-codebase-graph.md`（本文件） |
| provenance 登记 | `docs/refactor/provenance/grok-build.md`（ARC-810 段） |

## 验证

```text
cargo test -p code-intelligence --all-features
150 passed（lib 150；ARC-800 既有 69 项不回归 + ARC-810 新增 81 项）
- graph_tests 22：五语言 fixture 提取 golden（defs/refs/alias/export/
  containment 各语言断言）、行号 1-indexed、ScopeGraph 插入/查找/定位、
  持久化 round-trip 语义保持、query_hash 随 query 文本变化、QueryResponse
  graph 字段 serde 兼容
- build_tests 11：跨文件构建正确性（符号/引用/containment 树）、budget
  四维（文件数/字节/解析超时/并发）、超 5MiB 跳过、不支持语言跳过、
  空文件/二进制跳过、构建确定性、gitignore+隐藏文件
- incremental_tests 11：modified/created/removed/renamed 单文件更新、
  连续事件顺序处理、WatchGap reconcile、Lagged reconcile、真实
  FsEventService 集成（写文件→watcher→索引）、stop 等待在途
- query_tests 14：文件符号树（containment）、按名/按位置 def-ref、
  alias 解析（use Foo as Bar）、include_definition、越界位置、EOF 无符号、
  不支持语言、文件缺失、未索引文件、同语言优先排序、空 workspace、
  定义/引用按名查询、alias 引用解析
- persistence_tests 9：GraphCacheData round-trip、JSON golden、schema
  拒绝、persist→reopen（crash-reopen）、identity mismatch 重建、corruption
  recovery、legacy 无 graph 缓存重建、shutdown 持久化、meta stale 检测
- backend_tests 9：服务端到端查询、结构化错误、shutdown 顺序（停止增量
  →等待在途→持久化→reopen 命中）、SendersDropped 仍持久化、shutdown 拒
  绝新提交、增量事件经服务可见、懒构建 rebuild、队列 cancel

cargo clippy -p code-intelligence --all-targets --all-features -- -D warnings  通过
cargo fmt --all -- --check  通过
scripts/architecture-gate.sh  通过（dependency_edges=23，含 code-intelligence→change-tracker）
```

## 与 Grok 参考实现的差异清单

1. **containment 边**：Grok 只有 `DefToScope`；Evo 新增 def→def 父子符号
   边，由 `@definition.{sym}` capture 的声明体范围嵌套推导（Grok 查询
   数据 + Evo 推导逻辑）。
2. **节点携带名字**：Grok 从 src 字节切片取名（调用方必须持 src）；Evo
   提取阶段落名（name / symbol_type），查询面零 src 依赖。
3. **def 去重**：Grok 的 `extract_symbols_fast` 对多 pattern 命中同一声明
   （如 TS `const x = fn()` 同时匹配 function 与 variable pattern）会重复
   提取；Evo 按名字节点范围去重收敛为单定义。
4. **预算四维强制**：Grok 只有单文件 5 MiB 跳过常量；Evo 按
   `IndexBudget` 强制文件数 / 总字节 / 解析时长 / 并发（rayon 池大小），
   跳过记录结构化。
5. **增量事件面**：Grok 自建 channel actor + background refresh + 磁盘
   锁；Evo 消费 change-tracker 的 `FsEvent` 语义事件（debounce / rename
   配对已归一化），`WatchGap` / `Lagged` 触发全量 reconcile。
6. **Renamed 语义**：Grok rename 只 reindex 目标；Evo `rename_file`（移动
   符号位置）+ 目标重解析。
7. **持久化**：Grok 二进制 "SGIX" + interner；Evo JSON（与缓存层统一），
   无 interner，BTreeMap 确定性键。
8. **导入面**：`StringInterner` / ahash / num_cpus / crossbeam / git2 /
   dunce 未引入；用 std 集合与 rayon 线程池。
9. **服务接线**：Grok 无服务生命周期；Evo 通过 ARC-800 的 `QueryBackend`
   注入（`GraphQueryBackend`），`QueryBackend::shutdown` 为 Evo 扩展
   （ARC-800 债务偿还：`IndexCacheData.graph` / `LanguageConfig` grammar /
   `query_hash` 切换均已落地）。

## 债务登记

- **export 边缺口**：仅 TS / JS 查询文本有 `name.reference.export` capture，
  Rust / Go / Python 无 export capture（`pub fn` / `export` 关键字没有
  专门的 export 提取）；export 语义为文件级名字列表，无独立 export 边
  类型。ARC-830 若需要按 export 过滤的查询再做（对应执行债务登记）。
- **跨语言引用解析**：ref → def 的解析是名字匹配（+ namespace 匹配），
  不做类型系统 / 模块路径解析——同名符号跨语言（如 rust 与 ts 都有
  `target`）会返回多位置，靠「同语言优先」排序收敛。Grok 同款语义。
- **增量时未索引文件的语义**：`reindex_file` 不做预算强制（预算在构建期
  快照）；增量期间超 5 MiB / 二进制 / 语言不支持的变更文件被跳过并记录，
  不进入索引。全量 rebuild 时才重新强制预算。
- **alias 残留**：alias 是全局表（Grok 同款），`remove_file` / `rename_file`
  不清理 alias 条目；已删除文件的位置已从 definitions / references 清除，
  alias 只影响「名字是否仍是别名」的查询扩展。
- **Git 事件不触发 revision 重建**：`FsEvent::Git(HeadMoved)` 被忽略；
  revision 的切换由调用方（identity 构造者）负责——revision 变化时缓存
  identity mismatch 自动触发全量重建。
- **启动窗口事件丢失**：`GraphQueryBackend::new` 同步全量构建期间的事件
  流未被消费（广播窗口丢弃）；由下一次 `WatchGap` / `Lagged` 或启动后
  的 reconcile 收敛。服务与增量 actor 同时启动的应用不受影响。
- **无 StringInterner / ahash**：Evo 首批规模用 std 集合；超大仓库（
  Grok 的 ~1GB 索引场景）的内存优化若需要再引入（对应执行债务登记）。
- **containment O(n²)**：嵌套推导双循环；单文件 def 数量有限（百级），
  超大规模单文件（数千 def）可优化为排序栈（登记说明）。
- **grok-build 磁盘锁 / background refresh**：未移植（Evo 单进程模型，
  WatchGap reconcile 承担收敛职责）；多进程共享索引的场景再引入。

## 后续

- ARC-820：**已实现**（见 `phase8-lsp.md`）——LSP 采用独立
  `LspService`/`LspHandle`（独立 actor，查询面 async 网络往返）；
  `QueryKind::Diagnostics` 保持 `Unimplemented` 占位（不并入
  `QueryBackend`，理由见 `phase8-lsp.md`）。
- ARC-830：tool adapter 消费 `api.rs` 公开面（`GraphNavigator` /
  `GraphQueryBackend` / `Location` / `FileSymbol` / `LspHandle`），
  独立 ToolCapabilities 与 output budget；需要时再做 export 过滤查询
  与跨语言引用解析增强。
