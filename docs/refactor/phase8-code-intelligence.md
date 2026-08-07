# Phase 8 / ARC-800：抽取 `code-intelligence`

> 状态：完成
> 前序：Phase 7 Gate（extension host / user hooks / MCP adapter）
> 目标：抽取独立 `code-intelligence` crate —— 服务 API 与 tool adapter
> 分离，核心可被 CLI/Desktop/agent tool 共用；索引缓存带
> workspace/revision/parser-version identity 与 corruption recovery；
> 大仓库有文件数/字节/解析时长/并发预算。本 ARC 只做骨架：不实现
> codebase graph 本体（ARC-810）与 LSP（ARC-820），但为它们预留扩展点。

## 决策

### 服务 API：Arc handle + actor（`service.rs`）

- 参照 `extension-host` 的 `ExtensionHost`/`Handle`/`Task` 模式（Phase 7）：
  `CodeIntelligenceService::new`（同步探测缓存）-> `start` -> 返回
  `CodeIntelligenceHandle`（`submit` / `shutdown`）+ `CodeIntelligenceTask`
  （`join` 回收 [`ServiceExit`]）。
- 查询经有界通道（容量 32）顺序处理；`QueryRequest` 带 `kind`（
  `QueryKind`）与 `context`（JSON，骨架不解释，ARC-810/820 定义各自结构）。
- **`Status` 由 actor 直接回答**（identity / 缓存状态 / 预算快照）；
  其余 kind 委托 `QueryBackend` trait —— **ARC-810/820 实现该 trait 并注入
  `CodeIntelligenceServiceOptions::backend` 即可接入，无需改动 actor**。
  骨架默认 `SkeletonQueryBackend` 对未实现 kind 返回结构化
  `Unimplemented { kind, phase }`（FileSymbols/Definition/Reference ->
  ARC-810；Diagnostics -> ARC-820）。
- **shutdown 顺序**（确定性，幂等）：状态 `Running -> Stopping`（新提交
  被拒）-> 发 watch 信号 -> 当前 in-flight 请求完成并返回响应 ->
  **确定性退出**（actor 每处理完一个请求检查 shutdown 信号，避免
  `tokio::select!` 公平性导致已排队请求被继续处理）-> 队列中未处理请求
  统一收到 `ShuttingDown`（cancel 语义）-> 状态 `Stopped`。
- **panic 策略**：backend 查询在独立 task 中执行，panic 被捕获为
  `JoinError`（fail closed：停止派发 + `ServiceExit::panicked`），join 不
  传播 panic；响应丢失时 `submit` 返回 `QueryPanicked`。
- 所有 handle drop（无 shutdown）-> channel 关闭 -> actor 自行退出
  （`SendersDropped`）。
- 与 Grok 差异：Grok 的 `IndexManager` 无服务生命周期概念（channel actor
  只处理文件事件）；Evo 的 handle/task/shutdown/panic 语义为自研（参考
  extension-host）。

### 索引缓存 identity（`identity.rs` + `cache.rs`）

- 三要素：`workspace`（复用 `workspace-runtime` 的 `WorkspaceId`，
  手动 serde 桥接——该类型本身无 serde）、`revision`（
  [`RevisionId`]，索引基线：git HEAD / 变更集快照 / 用户标签，非空 +
  ≤128 可打印 ASCII）、`parser_version`（[`ParserVersion`]，移植 Grok
  `QueryVersion` 思想：`Legacy` 强制重建 + `Version(u64)` 与当前
  grammar/query 哈希比对）。
- [`CacheIdentity::mismatch`] 逐要素比对（[`IdentityDiff`]），任一项
  不一致 -> `CacheIdentityMismatch` 错误（带 expected/found），重建即可
  恢复；三项独立测试覆盖。
- 缓存文件格式（`cache.rs`）：magic `"EVOIX"` + format 版本 + identity
  长度前置 + identity JSON + payload 长度前置 + payload JSON。加载顺序
  magic -> format -> identity（先于 payload，mismatch 直接返回不做无谓
  反序列化）-> payload（含 schema 版本）。任何失败 -> 结构化
  `CacheCorrupted` / `CacheFormat` / `CacheIdentityMismatch`，绝不 panic。
- 保存走「同目录临时文件 + rename」原子写（同 `extension-host`
  credentials 的持久化纪律）；失败清理临时文件，旧缓存不受影响。
- 载荷 [`IndexCacheData`]（schema 版本 + built_at + `CachedFileEntry`
  基线元数据：相对路径/size/mtime，借鉴 Grok `FileMeta` 的 staleness
  检测）。ARC-810 追加 graph 序列化字段（必须 `#[serde(default)]`）。
- [`probe_cache`] 只读探测投影为 `CacheStatus`（Missing/Ready/
  RebuildRequired{reason}），服务启动时调用，失败不 panic。

### 预算类型（`budget.rs`）

- [`IndexBudget`] 四维：文件数（默认 200k）/ 总字节（默认 2 GiB）/
  单文件解析时长（默认 30s）/ 并发解析数（默认 8）；`0` = 不限；
  serde 部分指定走默认值。参照 `extension-host` 的 `ExtensionBudget` 风格。
- [`IndexBudgetTracker`] 记账：`reserve_file`（文件数 + 字节，失败不留下
  半状态）、`parse_start`/`parse_end`（并发配对）、`parse_time_limit`
  （`0` -> `None`）、`snapshot`。强制逻辑由 ARC-810 构建路径启用。

### 语言注册表（`languages.rs`）

- 移植 Grok `LanguageRegistry` 结构（by_extension / by_id / configs）与
  `TSLanguageConfig` 形状：`LanguageConfig` 只有 language id + 扩展名。
  ARC-810 追加 grammar fn / namespaces / query 文本。
- 内建注册表覆盖 ARC-810 首批语言：Rust / TypeScript / JavaScript /
  Python / Go。
- [`LanguageRegistry::query_hash`]：确定性哈希（按主 id 排序后哈希
  id + 扩展名），供 `ParserVersion::Version` 使用；ARC-810 落地 query 后
  改为 Grok `compute_query_hash` 方式（哈希主 id + query 文本）。

### 错误类型（`error.rs`）

- thiserror 结构化错误：缓存损坏/格式/identity 不匹配（均带 rebuild
  required 语义）、无效 revision、预算超限、未运行/关闭中/已运行、
  `Unimplemented { kind, phase }`（骨架占位）、`QueryPanicked`、IO。
- `CacheIdentityMismatch` 的两个 identity 装箱（clippy `result_large_err`）。

### 给 ARC-810 / ARC-820 的扩展点

- **`QueryBackend` trait**：graph backend（ARC-810）与 LSP diagnostics
  backend（ARC-820）实现后注入即可，actor 零改动。
- **`QueryKind`**：FileSymbols / Definition / Reference（ARC-810）、
  Diagnostics（ARC-820）已声明，当前返回 `Unimplemented`（错误带 phase）。
- **`IndexCacheData`**：ARC-810 追加 graph 序列化字段。
- **`LanguageConfig`**：ARC-810 追加 grammar / query 字段。
- **`IndexBudgetTracker`**：ARC-810 构建路径强制校验。
- **`LanguageRegistry::query_hash`**：ARC-810 切换为 query 文本哈希。

## 落点

| 变更 | 位置 |
| --- | --- |
| 新 crate | `crates/code-intelligence/`（加入 workspace members + `[workspace.dependencies]`） |
| 错误类型 | `crates/code-intelligence/src/error.rs` |
| identity 三要素 + ParserVersion | `crates/code-intelligence/src/identity.rs` |
| 索引缓存（格式/原子写/recovery/probe） | `crates/code-intelligence/src/cache.rs` |
| 预算类型 + 记账 | `crates/code-intelligence/src/budget.rs` |
| 语言注册表 | `crates/code-intelligence/src/languages.rs` |
| 服务 API（handle/task/actor/backend trait） | `crates/code-intelligence/src/service.rs` |
| 公开 API 清单 | `crates/code-intelligence/src/api.rs` |
| 测试 | `crates/code-intelligence/src/{identity,cache,budget,languages,service}_tests.rs` |
| 依赖边登记 | `scripts/architecture/internal-dependencies.tsv`（`code-intelligence -> workspace-runtime`） |
| 设计文档 | `docs/refactor/phase8-code-intelligence.md`（本文件） |
| provenance 登记 | `docs/refactor/provenance/grok-build.md` |

## 验证

```text
cargo test -p code-intelligence --all-features
69 passed（lib 69）
- identity 11：Legacy 强制重建/版本匹配/版本不匹配/Version round-trip/
  RevisionId 合法与非法/Revision round-trip/golden JSON/workspace serde
  桥接/三要素独立 mismatch/Display
- cache 18：save-load round-trip/缺失 miss/纯内存/三要素 mismatch 各自
  触发重建（含重建闭环）/截断损坏/垃圾 magic/未知 format 版本/identity
  JSON 损坏/payload JSON 损坏/未知 schema（load 与 save 双路径）/crash-
  reopen 恢复循环/失败保存保留旧缓存/probe 投影/staleness/原子 rename/
  load 幂等
- budget 11：默认值序列化/部分指定/文件数上限/字节上限/失败不记账/并发
  上限配对/时长上限/零=不限/snapshot round-trip/kind 一致性
- languages 9：首批语言覆盖/id+扩展名+路径查询/id 别名/supported/同语言
  判定/哈希确定性/哈希随内容变化/哈希顺序无关/config 形状
- service 20：Status round-trip/完整生命周期/二次 start 拒绝/shutdown
  拒绝新提交/join 后拒绝/幂等 shutdown/全 handle drop 退出/in-flight
  完成 + 队列取消/panic fail-closed/未实现 kind 带 phase/缓存状态投影
  （Ready/RebuildRequired）/并发串行/burst/golden JSON/round-trip/状态
  序列化

cargo clippy -p code-intelligence --all-targets --all-features -- -D warnings  通过
cargo fmt --all -- --check  通过
scripts/architecture-gate.sh  通过（dependency_edges=22，含新边）
```

## 与 Grok 参考实现的差异清单

1. **identity 三维**：Grok 只有 `QueryVersion`（`Legacy` + `Version(u64)`，
   语义仅针对 query）；Evo 扩展为 workspace + revision + parser-version
   三要素，`CacheIdentity::mismatch` 逐项报告。
2. **缓存格式**：Grok 用 bincode + magic `"SGIX"`，legacy 格式靠
   `LoadResult` 变体提示重建；Evo 用 JSON + 长度前置头，损坏/截断/格式
   全部映射为结构化错误变体（`CacheCorrupted`/`CacheFormat`/
   `CacheIdentityMismatch`），并加原子写（Grok 直接覆盖写）。
3. **错误分类**：Grok `CacheError` 无 identity 概念；Evo 把 identity
   mismatch 提升为一级错误，与 corruption 区分（诊断上可区分「重建」与
   「损坏」）。
4. **服务生命周期**：Grok `IndexManager` 只消费文件事件，无 start/
   shutdown/join/panic 治理；Evo 参照 extension-host 自研 handle/task
   模式（本 ARC 无文件事件面，事件面留给 ARC-810 增量 reindex）。
5. **语言注册表**：Grok 注册 5 种语言且带 grammar fn + query 文本；Evo
   骨架只有 id/扩展名映射，`query_hash` 基于 id/扩展名（Grok 基于
   primary id + query 文本），ARC-810 补齐。
6. **预算**：Grok 无索引预算概念（只有单文件 5MB 跳过常量）；Evo 四维
   预算 + 记账器（参照 extension-host 风格）。
7. **测试**：无直接复制；按 Evo 语义重写（fault injection / crash-reopen
   / identity mismatch / 生命周期 transition）。
8. **编码约定**：移植参考均小步重写 + 文件头 `Adapted from
   xai-codebase-graph, SOURCE_REV d6937fe...` 注释；未整文件复制。

## 债务登记

- 无执行债务（execution-debt.tsv 为空条目新增）。
- ARC-810 落地时 `LanguageRegistry::query_hash` 需切换为 query 文本哈希
  （已在代码注释登记）。
- `ServiceState` 未包含 `Failed` 终态（extension-host 有）；骨架中 backend
  panic 直接落到 `Panic` 退出原因，`Failed` 态视 ARC-810 需要再引入。

## 后续

- ARC-810：实现 graph `QueryBackend`；`IndexCacheData` 追加 graph 字段；
  `LanguageConfig` 追加 grammar/query；`IndexBudgetTracker` 强制；增量
  reindex 事件面（channel actor 复用本骨架的 dispatch 结构）。
- ARC-820：**已实现**（见 `phase8-lsp.md`）——LSP 采用独立
  `LspService`/`LspHandle` 查询面（async 网络往返，无法塞入同步
  `QueryBackend::query`）；`QueryKind::Diagnostics` 保持
  `Unimplemented` 占位，tool adapter（ARC-830）直接消费
  `LspHandle::query`/`diagnostics`。
- ARC-830：tool adapter 消费 `api.rs` 公开面，独立 ToolCapabilities 与
  output budget。
