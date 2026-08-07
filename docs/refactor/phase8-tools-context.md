# Phase 8 / ARC-830：Tool/context 集成

> 状态：完成（2026-08-07）
> 前序：ARC-800（`code-intelligence` 骨架）、ARC-810（codebase graph）、
> ARC-820（LSP lifecycle）
> 目标：graph/LSP 查询面接入产品工具装配 —— read-only 查询工具（独立
> ToolCapabilities + output budget）、按需 context 注入（不把完整符号图
> 塞入 system prompt）、graph 与 MCP tool search 共用结果排序接口
> （不共享存储实现）。

## 决策

### 工具划分：`code_graph` 一个工具 + `code_lsp` 一个工具

拒绝「一个 `code` 工具带子命令」的方案，理由：

1. **生命周期不同**：graph 是内存索引（同步查询 + 有界队列，查询延迟
   毫秒级）；LSP 是子进程服务（async 网络往返，有 start/restart/backoff，
   可能长时间不可用）。合并会让模型对一个工具混合两种失败模式。
2. **失败模式不同**：graph 的错误是「未索引 / 无符号 / 参数问题」；
   LSP 的错误是「未就绪 / 重启中 / 网络失败」。独立工具让模型能按
   需求选择，结构化错误分类各自清晰。
3. **ToolCapabilities 独立**：两个工具虽然都声明 read-only + cancel +
   timeout + streaming=false，但保持独立定义允许未来单独演进
   （如 LSP 查询加 per-tool timeout 更短、graph 查询声明并行）。
4. **与 MCP meta tools 先例一致**：search/use 也是两个独立工具，模型
   用搜索发现、按需求调用。

工具形态参照 `extension-host` 的 MCP meta tools（`DynamicTool`，无类型
JSON → JSON），放在 `crates/code-intelligence/src/tools/`（tool adapter
与服务 API 分离，只依赖 `api.rs` 公开面）。

| 工具 | id | 查询 | 位置契约 |
| --- | --- | --- | --- |
| graph | `code_graph` | `symbols` / `definitions` / `references` / `search` | 路径 workspace-relative；行/列 **1-indexed**（graph 契约） |
| LSP | `code_lsp` | `hover` / `definition` / `references` | 路径 workspace-relative；行/列 **0-indexed UTF-16**（LSP 契约） |

`code_graph` 的 `search` 模式新增 `QueryKind::SymbolSearch`（ARC-830 扩展
查询面）：按名称片段搜索符号，相关度排序 + 预算内截断，供工具与按需
context 注入共用。`GraphQueryResult` 增加 `Search` 变体（serde 向后兼容）。

### ToolCapabilities 声明

两个工具一致：

```text
read_only: true          // 只读查询，无副作用
execution: Parallel
cancel: true             // 取消经 ToolCallContext.cancel 贯通
timeout: true            // ToolRuntime 的 deadline 控制
streaming: false
provider_executed: false
authorization_risk: WorkspaceLocalReadOnly   // 同类只读风险
```

`authorization_risk` 用 `WorkspaceLocalReadOnly`（工具只读本 workspace
的索引 / 语言服务器状态，不产生外部副作用；`AuthorizationRisk` 枚举中
无 ExternalRead 变体，WorkspaceLocalReadOnly 即最接近的只读风险）。

### Output budget：条数 + 字节双层截断，超限显式标记

`tools/budget.rs` 定义 [`QueryOutputBudget`]（默认 100 条 / 64 KiB）与
两个截断原语：

- `truncate_items`：条数截断（`0` = 不限）；
- `truncate_by_bytes`：按序列化字节逐条累加截断（`0` = 不限）。

执行流程：结果先按条数截断 → 再按字节截断。**任何一层截断都在输出
JSON 中显式标记**（`"truncated": true`，search 还带 `count` / 后端截断
标记），模型必须知道结果被裁剪过；不静默截断。

- `code_graph` 载荷：`{query, count, results[], truncated?}`；`search`
  还带 `symbol`，后端 `BoundedSymbolSearch.truncated` 也会并入标记；
- `code_lsp` 载荷：hover → `{query, path, result(文本), truncated?}`
  （文本字节截断带 `…[truncated]` 标记）；definition/references →
  `{query, path, count, results[], truncated?, truncated_to?}`。

### 取消贯通

两个工具都在 `tokio::select! { biased; cancel.cancelled() → Cancelled;
handle.submit/query → 结果 }` 中执行查询。工具返回 `Cancelled` 后查询
future 被 drop（服务侧的 oneshot 响应丢弃，服务继续处理，无泄漏）；
ToolRuntime 的 deadline 在工具外层独立生效。

### 按需 context 注入（不把完整符号图塞入 system prompt）

- **查询入口**（`code-intelligence/src/context.rs`）：
  [`SymbolContextBudget`]（`max_results` 默认 20 + `max_bytes` 默认
  8 KiB）→ `query_symbol_context`（搜索 + 截断）→ `SymbolContextSnippet`
  （`total` / `kept` / `truncated` 显式标记）→ `render_context_text`
  （有界 `<code_context>` 文本块）。
- **注入点**（`coding-agent/src/app/code_context.rs`）：按需查询经
  `CodeIntelligenceHandle` 提交 `SymbolSearch`，无匹配 → `Ok(None)`
  （不产生 context 块）；服务不可用 → 结构化 `SessionFailure`。
  **占位说明**：agent-core 的 per-turn `assemble_context` 公共 API
  不在本 ARC 改动（约束：不动 agent-core 公共 API）；本 seam 是
  coding-agent 侧的接线点，per-turn 调用路径留给后续 ARC（见债务登记
  「context 注入深度」）。无 code-intelligence 配置时该 seam 不参与
  任何 context 组装，产品行为不变。
- 配置了 graph 时系统提示**不**包含符号图——模型通过 `code_graph`
  工具按需查询（先例：MCP 的 search/use meta tools 避免把大量 schema
  塞入 context）。

### 共用排序接口（`tool-contract::ranking`）

graph 与 MCP tool search 共用**同一排序接口**，但**不共享存储实现**
（graph 索引与 MCP 工具目录各自持有数据）：

```text
RelevanceScorer::score(query, text) -> f64     // 0.0 ~ 1.0
ResultRanker::rank(query, items, text_of, limit) -> Vec<RankedResult<T>>
DefaultResultRanker（TokenOverlapScorer 打分）
```

契约：相关度降序稳定排序（同分保持输入顺序）；`limit` 截断（`0` = 不限）；
空查询词全 0 分（列表语义）。打分：精确文本 `1.0` > 整词命中 `0.8` >
词前缀 `0.4` > 词内子串 `0.2`（除以查询词 token 数）——同名符号应排在
同名包含项之前。

放置理由：`tool-contract` 是两边的公共依赖（extension-host 已依赖；
code-intelligence 新增依赖边），且排序语义是「tool 结果的相关度契约」，
符合该 crate 的职责。`workspace-runtime` 主题不符（capability/sandbox）。

两侧接入：

- **graph 侧**（`graph/query.rs::search_symbols`）：候选收集（上限
  `MAX_SYMBOL_SEARCH_CANDIDATES` = 4096，有界查询）→ 相关度排序
  （文本 = 符号名）→ `limit` 截断 → 幸存者补引用数；`total` / `truncated`
  显式标记；
- **MCP 侧**（`extension-host/src/mcp/meta.rs::rank_search_matches`）：
  子串过滤保持原语义，命中结果经同一接口按相关度排序（文本 =
  server + name + description）；无查询词保持发现顺序（列表语义）。
  extension-host 侧为最小改动，既有测试全部保持。

### coding-agent 装配（三态）

- `ApplicationRunOptions` 新增 `code_intelligence: Option<CodeIntelligenceRunOptions>`
  （graph handle + 可选 `(LspHandle, workspace_root)`）；
- `resolve_application_context` 配置时经 `code_intelligence::tools::code_tools`
  追加工具（`code_graph`；LSP 一并配置时 + `code_lsp`）；
- **无配置 → 不追加任何工具，行为不变**（与 MCP meta tools 三态一致）；
- 新依赖边：`coding-agent → code-intelligence`（master plan 4.1 目标图
  确认）、`code-intelligence → tool-contract`、`code-intelligence →
  tool-runtime`，已登记 `scripts/architecture/internal-dependencies.tsv`。

### 与 Grok 差异

1. **无对应物**：Grok 的 graph/LSP 查询直接暴露给模型/UI，无
   ToolCapabilities / output budget / 截断标记概念；Evo 的 read-only
   query tool + 双层预算 + 显式标记为自研。
2. **context 注入**：Grok 把符号图缓存直接给模型（完整符号图）；
   Evo 按需查询 + 预算内摘要（`<code_context>` 块）。
3. **排序接口**：Grok 无共用排序概念（MCP 侧 Grok 无 meta tool）；
   Evo 的 `tool-contract::ranking` 为自研共享契约。
4. **SymbolSearch**：Grok 无按名称片段搜索（只有精确名导航）；
   Evo 新增 `QueryKind::SymbolSearch` 扩展查询面。
5. **LSP 结果语义映射**：Grok 直传 async-lsp 结果；Evo 在 tool 层做
   hover markdown 提取 / location 归一化（ARC-820 债务偿还）。
6. **测试**：无直接复制；按 Evo 语义重写（服务级端到端、cancel
   贯通、结构化错误、契约边界）。

## 落点

| 变更 | 位置 |
| --- | --- |
| 共用排序接口 | `crates/tool-contract/src/ranking.rs`（+ `api::ranking` 重导出） |
| 符号搜索（查询面扩展） | `crates/code-intelligence/src/graph/query.rs`（`SymbolHit` / `BoundedSymbolSearch` / `search_symbols`） |
| `QueryKind::SymbolSearch` | `crates/code-intelligence/src/service.rs` |
| backend 接线 | `crates/code-intelligence/src/graph/backend.rs`（`Search` 变体 + `limit` 上下文） |
| 按需 context 入口 | `crates/code-intelligence/src/context.rs` |
| 输出预算 | `crates/code-intelligence/src/tools/budget.rs` |
| `code_graph` 工具 | `crates/code-intelligence/src/tools/graph.rs` |
| `code_lsp` 工具 | `crates/code-intelligence/src/tools/lsp.rs` |
| 工具装配入口 | `crates/code-intelligence/src/tools/mod.rs`（`code_tools`） |
| MCP 排序接入 | `crates/extension-host/src/mcp/meta.rs`（`rank_search_matches`） |
| coding-agent 装配参数 | `crates/coding-agent/src/app/bootstrap.rs`（`CodeIntelligenceRunOptions`） |
| 装配接线 | `crates/coding-agent/src/app/startup.rs` |
| context seam | `crates/coding-agent/src/app/code_context.rs` |
| 依赖边登记 | `scripts/architecture/internal-dependencies.tsv`（+3 条边） |
| 公开 API | `crates/code-intelligence/src/api.rs`（context / search / tools 导出） |
| 测试 | `tools/{graph,lsp}_tests.rs` + `graph/search_tests.rs` + `context.rs` 内嵌 + `tools/budget.rs` 内嵌 + `tests/tools_lsp.rs` + coding-agent `tools/code_tools_tests.rs` + `app/code_context.rs` 内嵌 + tool-contract `ranking.rs` 内嵌 + extension-host `meta.rs` 内嵌 |
| 设计文档 | `docs/refactor/phase8-tools-context.md`（本文件） |
| provenance 登记 | `docs/refactor/provenance/grok-build.md`（ARC-830 段） |

## 验证

```text
cargo test -p tool-contract --all-features
  12 passed（ranking 8 项为 ARC-830 新增，其余为既有）
cargo test -p code-intelligence --all-features
  304 passed（lib 261 = 既有 207 + 新增 54；集成 lsp_lifecycle 24 +
  lsp_transport 15 + tools_lsp 4 新增）
cargo test -p extension-host --all-features
  186 passed（meta 新增 4 项 ranking 契约，其余为既有）
cargo test -p coding-agent --all-features
  250 passed（新增 code_tools 4 + code_context 4，其余为既有；
  另有 2 个既有时序敏感测试在并行负载下偶发失败，见遗留问题）
cargo clippy -p tool-contract -p code-intelligence -p coding-agent
  -p extension-host --all-targets --all-features -- -D warnings  通过
cargo fmt --all -- --check  通过
scripts/architecture-gate.sh  通过（dependency_edges=26，含 3 条新边；
  oversized_debts=36 无新增，execution_debts=0）
```

测试覆盖要点：

- **工具**：每个查询模式的参数校验（缺字段 / 未知模式 / 位置缺列）、
  结果转换（symbols/definitions/references/search 载荷形状）、output
  budget 超限截断 + 显式标记（条数与字节两层）、cancel 贯通（graph：
  阻塞 backend 端到端；LSP：预取消 + fake server `--query-delay-ms`
  in-flight 取消）、未启动服务 / 关闭中 / 未索引文件 / 未就绪 LSP 的
  结构化错误（`Unavailable` / `Cancelled` / `InvalidArguments` /
  `Execution` 分类）。
- **context**：无结果 → `None`；条数预算截断 + 标记；字节预算截断 +
  标记；渲染块形状；服务停止 → 结构化错误。
- **排序接口**：空结果、全等分数稳定顺序、降序排序、`limit` 截断、
  空查询词稳定、大小写不敏感 + 前缀/子串降级、自定义 scorer；graph
  侧（search_symbols 契约）+ MCP 侧（rank_search_matches 契约）共用
  同一接口的输入输出测试。
- **装配**：三态（无配置工具列表不变 / 有 graph 注册 code_graph / 有
  graph + LSP 注册两者）+ 显式工具不被覆盖。

## 与 Grok 参考实现的差异清单

（见上文「与 Grok 差异」；本 ARC 无 Grok 代码移植，`tool-contract` /
`code-intelligence` / `extension-host` / `coding-agent` 全部为 Evo 自研
增量。）

## 债务登记

- **context 注入深度**：per-turn 注入点未接线——`assemble_context`
  是 agent-core 公共 API（本 ARC 约束不改）；coding-agent 侧 seam
  （`app/code_context.rs::query_symbol_context` + 渲染）已就绪并全测试，
  lib 构建中为 `#[allow(dead_code)]` 占位（per-turn 调用路径由后续 ARC
  在 coding-agent 侧接线，接线后删除该 allow）。
- **diagnostics 工具化**：`code_lsp` 只覆盖 hover/definition/references；
  诊断查询（`LspHandle::diagnostics` / `pull_diagnostics`）未做成工具，
  需要时按同一模式加 `code_lsp` 的 `diagnostics` 查询模式。
- **`QueryKind::Diagnostics`**：保持 ARC-800 的 `Unimplemented` 占位
  （LSP 独立查询面，不并入 `QueryBackend`，决策见 `phase8-lsp.md`）。
- **MCP 排序无 limit**：`mcp_search` 命中数不做上限（meta 工具本就
  极轻量）；若未来工具目录膨胀再加 limit。
- **export 过滤查询**：ARC-810 债务继续登记（`code_graph` 未加按
  export 过滤，需要时在 `search` 模式扩展）。

## 遗留问题

- **coding-agent 既有时序敏感测试**：`durable_rewind_restores_workspace_tracker_branch_and_client_state`
  与 `temp_session_env_repairs_a_partial_commit_on_reopen` 在并行负载
  下偶发失败（fault-injection / staleness 时序，单独运行 15/15 通过；
  移除 ARC-830 全部测试后同样失败，确认为既有 flake，与本次改动
  无关）。建议后续 ARC 单独修复这两个测试的时序假设。
