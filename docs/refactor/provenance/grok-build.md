# grok-build provenance

- Status: evaluated; no production code copied by Phase 0.
- Local source: `third-party/grok-build`.
- Checkout revision at architecture study: `ed6d543643628663873c5de28298e022ed634238`.
- Recorded upstream `SOURCE_REV`: `d6937fe255dce4133c3d000a50f9cb94de12f06f`.
- Primary license: Apache-2.0; verify each selected module before copying.
- Primary notice sources: `third-party/grok-build/THIRD-PARTY-NOTICES` and crate-local notice files.
- Planned destinations and adaptation mode: see `docs/Evo完整架构重构计划.md` section 5.
- Sync policy: selective one-time adaptation; Evo does not track Grok aggregation crates wholesale.

每个实际移植任务必须在本文件追加源 crate、具体路径、测试、目标路径和本地修改，不能只引用本总记录。

---

## ARC-700 `extension-host` 抽取（2026-08-06）

Status: adapted（逐文件小步改写，未整文件复制；参考均标注 `Adapted from xai-grok-hooks` 来源注释）
Upstream repository: https://github.com/bytecodealliance/xai-grok（vendored at `third-party/grok-build`）
Upstream revision: `d6937fe255dce4133c3d000a50f9cb94de12f06f`
Source paths: `third-party/grok-build/crates/codegen/xai-grok-hooks/src/`
  - `lib.rs`（模块组织与 crate 文档风格）
  - `config.rs`（config layer 容错与「坏层跳过其余照常」模式、TOML/JSON 双路径）
  - `discovery.rs`（目录缺失为空的容错、坏文件继续扫描、稳定排序、dedup 思路）
  - `trust.rs`（folder trust 单一权威设计原则；Evo 不复制 legacy 迁移代码）
  - `error.rs`（结构化 thiserror 错误携带 path/name 模式）
  - `event.rs`（envelope 元数据字段集与 payload 判别思路；别名解析思想）
  - `matcher.rs` / `runner/mod.rs`（仅阅读作为 ARC-710 参考，本 ARC 未移植）
License/notices: Apache-2.0（`third-party/grok-build/THIRD-PARTY-NOTICES`）
Destination paths: `crates/extension-host/src/{lib,api,error,event,config,discovery,trust,budget,diagnostic}.rs`、`crates/extension-host/src/host/{mod,tests_host}.rs`；coding-agent 端口 `crates/coding-agent/src/services/ports.rs`（仅参考设计，无代码复制）
Tests carried over: 无直接复制；按 Evo 语义重写 —— DTO golden/round-trip、向后兼容 default、config merge 优先级与冲突、discovery 容错、trust 边界、lifecycle/shutdown/panic/budget
Local modifications:
  - 事件 DTO 版本化（Grok 无 version）；payload 改 internally-tagged（Grok untagged 且仅 Serialize）
  - 事件业务字段按 Evo 重设计，不照抄事件全集
  - discovery 从「目录内散落 JSON」改为「每扩展一个目录 + extension.json manifest」
  - trust 从 legacy 迁移辅助改为 TrustStore 抽象 + 三态判定 + EnableRequest 首次启用 DTO
  - budget 从 per-hook timeout 改为 per-extension 多维 per-session 预算
  - 新增 host 生命周期（discovery/config/trust/lifecycle/budget/diagnostics/shutdown）——
    Grok 无 host 概念，属 Evo 独立设计（参考了 `xai-grok-config` 的 layer 思想）
  - runner/matcher 未移植（ARC-710）
Sync policy: 不跟随上游（一次性适配）；后续 ARC 若再参考 `matcher.rs`/`runner/*` 需单独登记。

---

## ARC-710 User hooks（2026-08-07）

Status: adapted（小步改写 + 重新设计，未整文件复制；参考文件均标注 `Adapted from xai-grok-hooks` 来源注释）
Upstream repository: https://github.com/bytecodealliance/xai-grok（vendored at `third-party/grok-build`）
Upstream revision: `d6937fe255dce4133c3d000a50f9cb94de12f06f`
Source paths: `third-party/grok-build/crates/codegen/xai-grok-hooks/src/`
  - `matcher.rs`（simple-vs-regex 精确/正则语义、pipe 列表防锚定错误、缺省=通配）
  - `runner/command.rs`（shell 元字符路由、timeout 终止进程树、输出截断上限 64KB、
    exit-code 阶梯、JSON 决策优先于退出码、deny/block 默认 reason 文案）
  - `dispatcher.rs`（首个 deny 短路、Stop 信号聚合与 first force-stop wins、
    fail-open 语义与理由）
  - `event.rs`（信封元数据、SubagentStopPhase、payload 判别思想、`MAX_PAYLOAD_SIZE` 截断）
  - `runner/http.rs`：仅阅读评估；HTTP runner 未移植（SSRF 安全 fetch 管线在 Evo `ai`
    crate 内部，下沉共享 HTTP 层为 Phase 8+ 工作，登记为验证债务）
License/notices: Apache-2.0（`third-party/grok-build/THIRD-PARTY-NOTICES`）
Destination paths:
  - `crates/extension-host/src/{matcher,hook,runner,dispatcher}.rs`（新模块）
  - `crates/extension-host/src/{event,discovery,host/mod,budget,api}.rs`（扩展）
  - `crates/extension-host/src/runner/tests_interpret.rs`、
    `crates/extension-host/src/dispatcher/tests_dispatcher.rs`、
    `crates/extension-host/tests/hook_runner.rs`（测试）
  - coding-agent 接线：`crates/coding-agent/src/services/ports.rs`（LiveExtensionHostPort /
    ExtensionEventSink / ExtensionHostService 扩展）、`session/view.rs`、
    `runtime/facade/{lifecycle,connection}.rs`、`runtime/owners.rs`、
    `application/operation/dispatch.rs`、`operations/{prompt/mod.rs,prompt/context.rs,
    prompt/context/setup.rs,merge/runner.rs}`、`services/{runtime.rs,authorization.rs}`、
    `runtime/facade/hooks_tests.rs`（测试）
Tests carried over: 无直接复制；按 Evo 语义重写 —— matcher 四维条件、
priority 排序、gate transition-table（Tool/Stop 各 9+ 行）、runner 超时/取消/洪泛/
崩溃/退出码/sandbox 注入、host shutdown 在途 hook 取消、coding-agent 适配器集成
（session 生命周期事件、gate 决策暴露、首次启用展示、permission 事件）
Local modifications:
  - 事件通道：Grok stdin 注入 → Evo 环境变量（`ProcessSpec` 固定 stdin=null）
  - matcher 新增 path（前缀语义）与 profile 维度；新增 `priority` 确定排序
    （Grok 无优先级，按配置顺序）
  - runner 强制 `SandboxProfile` + `SandboxCapability` 探测：sandbox 失败是唯一
    fail-closed 类别（Grok 全部 fail-open，且无沙箱概念）
  - gate 拆 host 通道（Observe 派发）与 `HookGate`（产品同步调用），避免事件双跑
  - 结构化结果枚举（Success/ToolDecision/StopSignals/TimedOut/Cancelled/
    OutputLimited/Failed/SpawnFailed/SandboxUnsupported）替代 Grok 的字符串失败
  - dispatch 事件逐条独立 task + panic fail-closed + shutdown 取消在途 hook
    （Grok 无 host 生命周期）
  - 无 HTTP runner；`StopSignals` 聚合三信号；JSON 决策协议保留但简化字段集
Sync policy: 不跟随上游（一次性适配）；`runner/http.rs` 若后续移植需单独登记。

---

## ARC-720 MCP provider adapter（2026-08-07）

Status: adapted（小步改写 + 重新设计，未整文件复制；参考文件均标注 `Adapted from xai-grok-mcp` 来源注释）
Upstream repository: https://github.com/bytecodealliance/xai-grok（vendored at `third-party/grok-build`）
Upstream revision: `d6937fe255dce4133c3d000a50f9cb94de12f06f`
Source paths: `third-party/grok-build/crates/codegen/xai-grok-mcp/src/`
  - `servers.rs`（`ResilientRwTransport` 单行解码失败跳过的读纪律、`McpError`
    分类、ClientStateKind 生命周期、401/auth-rejection 识别、工具调用重试结构）
  - `liveness.rs`（transport 死亡检测的 one-shot watcher 思想；Evo 改为主动 ping）
  - `credentials.rs`（0600 权限、原子写、加载时收紧权限的持久化纪律）
  - `oauth.rs`（OAuth 编排预算与结构化失败；Evo 改用 RFC 8628 device flow）
  - `mcp_http_client.rs`（重连退避的量级参考；Evo 在 lifecycle 层实现）
  - `wire.rs` / `acp_transport.rs`（仅阅读确认 ACP 不在本任务范围）
  - `third-party/grok-build/crates/codegen/xai-grok-hooks/src/env_expand.rs`
    （仅阅读确认环境注入模式；MCP server 环境为配置声明白名单，不继承宿主）
License/notices: Apache-2.0（`third-party/grok-build/THIRD-PARTY-NOTICES`）
Destination paths:
  - `crates/extension-host/src/mcp/{wire,transport,lifecycle,state,credentials,oauth,meta}.rs`
  - `crates/extension-host/src/mcp/{mod.rs}`、`src/{lib,api}.rs`（公开面）
  - `crates/extension-host/src/host/mod.rs`（`ExtensionHostOptions.mcp` 与生命周期接线）
  - `crates/extension-host/tests/mcp_lifecycle.rs`、`src/bin/fake_mcp_server.rs`（测试 + 测试辅助二进制）
  - `crates/workspace-runtime/src/process/peer.rs`（`PeerProcess`：sandbox 强制的交互式子进程）
  - `crates/coding-agent/src/app/{bootstrap,startup}.rs`、`src/tools/mcp_meta_tools_tests.rs`（meta tools 装配）
Tests carried over: 无直接复制；按 Evo 语义重写 —— wire golden/round-trip 与非法输入拒绝、
lifecycle transition-table（13 行）、OAuth device flow（mock 端点轮询/拒绝/取消/refresh）、
stdio 集成（握手/发现/转发/timeout/取消/liveness 重连/崩溃重连/list_changed/洪泛/坏 JSON/
shutdown 在途取消/不可用语义）
Local modifications:
  - wire 从 rmcp 依赖改为手写 JSON-RPC 2.0 严格解析（未知字段拒绝，fail closed）
  - transport 从 rmcp `TokioChildProcess`/streamable HTTP 改为手写 JSON lines stdio +
    reqwest 同步 POST；stdio 子进程强制 `SandboxProfile`（Grok 无沙箱）
  - liveness 从 transport-closed 轮询改为主动 `ping` 心跳（可配置间隔/超时）
  - OAuth 从浏览器授权码流程改为 RFC 8628 device flow；401 后单次 refresh/retry
  - credential 从 rmcp `StoredCredentials` 简化为 token/refresh 分离的 `McpCredentials`；
    跨进程 flock 登记债务
  - 状态机从四态枚举改为 `apply_event` 纯决策层 + transition 表 + 状态发布
  - 新增 `mcp_search`/`mcp_use` meta tools（Grok 无此概念，全部工具直注册）
  - tools/list_changed 热更新 + `tools_version` 订阅（Grok 仅 ACP 推送）
Sync policy: 不跟随上游（一次性适配）；ACP 与 HTTP SSE 若后续实现需单独登记。

---

## ARC-800 `code-intelligence` 抽取（2026-08-07）

Status: adapted（小步改写 + 重新设计，未整文件复制；参考文件均标注 `Adapted from xai-codebase-graph` 来源注释）
Upstream repository: https://github.com/bytecodealliance/xai-grok（vendored at `third-party/grok-build`）
Upstream revision: `d6937fe255dce4133c3d000a50f9cb94de12f06f`
Source paths: `third-party/grok-build/crates/codegen/xai-codebase-graph/src/`
  - `scope_graph/graph.rs`（前 120 行：`QueryVersion` —— Legacy 强制重建 +
    `Version(u64)` 比对；移植为 `ParserVersion` 并扩展为三维 identity）
  - `manager/cache.rs`（106 行：缓存文件 + 格式检测/错误分类思路；Evo 改为
    JSON + 长度前置头 + 原子写，legacy bincode 标记改为结构化错误变体）
  - `types/mod.rs`（`FileMeta`：size + mtime 秒/纳秒的 staleness 检测；
    移植为 `CachedFileEntry`）
  - `languages/mod.rs` + `languages/types.rs`（registry 双索引查询结构 +
    `TSLanguageConfig` 形状 + `compute_query_hash` 排序哈希思路；Evo 骨架
    去掉 tree-sitter 依赖，grammar/query 字段留给 ARC-810）
  - `index_manager.rs`（前 100 行：channel-based actor 思想；Evo 的服务
    生命周期为自研，参照 extension-host 的 handle/task/shutdown/panic 模式）
License/notices: Apache-2.0（`third-party/grok-build/THIRD-PARTY-NOTICES`）
Destination paths: `crates/code-intelligence/src/{lib,api,error,identity,cache,budget,languages,service}.rs`、`crates/code-intelligence/src/{identity,cache,budget,languages,service}_tests.rs`
Tests carried over: 无直接复制；按 Evo 语义重写 —— identity 三要素 mismatch、
fault injection（截断/垃圾 magic/坏 JSON/未知 schema）、crash-reopen 恢复、
预算边界（文件数/字节/并发/时长）、生命周期 transition（start/stop/
shutdown 幂等/join 拒绝）、panic fail-closed、round-trip golden
Local modifications:
  - identity 三维：Grok 只有 QueryVersion；Evo 扩展 workspace（复用
    workspace-runtime `WorkspaceId`）+ revision + parser-version，逐要素
    mismatch 报告
  - 缓存格式：Grok bincode + `SGIX` magic + legacy 变体；Evo JSON +
    长度前置头 + 原子 rename，损坏/格式/identity 均为结构化错误
  - 服务生命周期：Grok IndexManager 无 start/shutdown/join；Evo 参照
    extension-host 自研（handle/task/watch/panic fail-closed/队列 cancel）
  - 预算四维（文件数/总字节/解析时长/并发）：Grok 无预算概念
  - 语言注册表：Evo 骨架无 tree-sitter；`query_hash` 基于 id/扩展名，
    ARC-810 切换为 query 文本哈希
Sync policy: 不跟随上游（一次性适配）；tree-sitter grammar 与增量 reindex
若后续移植需单独登记。

---

## ARC-810 codebase graph（2026-08-07）

Status: adapted（小步改写 + 裁剪/重写，未整文件复制；参考文件均标注
`Adapted from xai-codebase-graph` 来源注释）
Upstream repository: https://github.com/bytecodealliance/xai-grok（vendored at `third-party/grok-build`）
Upstream revision: `d6937fe255dce4133c3d000a50f9cb94de12f06f`
Source paths: `third-party/grok-build/crates/codegen/xai-codebase-graph/src/`
  - `scope_graph/graph.rs`（1726 行，必须裁剪拆分）：
    - `ScopeGraph` / `ScopeStack` / `from_symbols` 段 → Evo
      `graph/scope.rs`（去掉 src 依赖的名字切片——Evo 节点直接携带
      name/symbol_type；新增 containment 边；去掉 QueryVersion/Snippet）
    - `scope_graph_from_definitions_query` + `extract_symbols_fast` 段 →
      Evo `graph/extract.rs`（合并单一入口；新增 containment 推导——
      用 `@definition.{sym}` capture 的声明体范围而非名字标识符范围；
      新增 exports 收集；def captures 去重——Grok 多 pattern 会重复
      提取同一声明，Evo 收敛为单定义）
    - `ScopeGraphIndex` 段 → Evo `graph/index.rs`（去掉 StringInterner
      内存优化，改 BTreeMap 字符串键保证确定性；保留 reverse index
      file_to_defs/file_to_refs 的 O(符号数) 删除/移动；新增文件级
      exports 表与 to_persisted/from_persisted）
  - `scope_graph/nodes.rs` → `graph/nodes.rs`（NodeKind/LocalDef/LocalImport/
    LocalScope/Reference/SymbolId；Evo 扩展 name/symbol_type 字段）
  - `scope_graph/edges.rs` → `graph/edges.rs`（EdgeKind 五边不变）
  - `types/range.rs` → `graph/range.rs`（裁剪：去掉 line-end-index 辅助
    与显示转换；保留 Position/Range/contains/1-indexed accessor）
  - `languages/{rust,ts,javascript,python,golang}.rs` → `languages/*.rs`：
    `.scm` 查询文本与 namespaces 逐字移植（数据/契约层，直接携带）；
    language ids 归一化为 Evo 小写；TS/JS 扩展名按 Evo 注册表基线
    （tsx/mts/cts、jsx/mjs/cjs、pyi）
  - `manager/builder.rs` + `index_manager.rs`（process_file_fast/
    reindex_file）→ `graph/build.rs`：Evo 新增 `IndexBudget` 四维强制
    （文件数/字节在收集阶段 reserve_file 记账、并发用 rayon 线程池
    大小、单文件解析时长计时）、结构化跳过记录（IndexSkipReason）、
    5 MiB 上限与二进制前缀检测与 Grok 一致
  - `index_manager.rs` 事件面（channel actor）→ `graph/incremental.rs`：
    Evo 复用 change-tracker 的 FsEventService（debounce/rename 配对/
    gitignore 过滤），本模块只消费语义事件；WatchGap/Lagged → 全量
    reconcile；shutdown 用 std mpsc 完成信号同步等待在途
  - `navigation.rs`（Navigator/NavigationResult/Location/
    find_smallest_named_node_at_point）→ `graph/query.rs`：1-indexed
    行/列契约保留；Evo 新增文件符号树查询（containment）与按名查询
License/notices: Apache-2.0（`third-party/grok-build/THIRD-PARTY-NOTICES`）
Destination paths: `crates/code-intelligence/src/graph/{mod,range,nodes,edges,scope,extract,index,query,build,incremental,backend,persist}.rs`、`crates/code-intelligence/src/languages/{rust,typescript,javascript,python,golang}.rs`、`crates/code-intelligence/src/graph/{test_support,graph_tests,build_tests,incremental_tests,query_tests,persistence_tests,backend_tests}.rs`
Tests carried over: 无直接复制；按 Evo 语义重写 —— 每语言一个 fixture 的
definition/reference/alias/export/containment 提取 golden（query 文本与
树结构断言）、budget 四维超限与跳过记录、增量 modified/created/removed/
renamed/reconcile、查询边界（无符号/越界/不支持语言/未索引）、持久化
round-trip golden + identity mismatch + corruption recovery + crash-reopen、
异步 shutdown/cancel/panic、真实 change-tracker 集成
Local modifications:
  - containment 边：Grok 只有 DefToScope（def→作用域）；Evo 新增
    def→def 的父子符号边（`@definition.*` capture 声明体嵌套推导）
  - import/export：RefToImport 边保留（Grok 同款）；export 由提取产物
    exports 列表承载，无独立 export 边类型（与 Grok 相同，见债务登记）
  - 增量策略：Grok rename 只 reindex；Evo Renamed 事件 = rename_file
    （移动符号）+ 目标重解析；Grok 有 background refresh 与磁盘锁，
    Evo 用 WatchGap 全量 reconcile 收敛（见债务登记）
  - 持久化：Grok 自定义二进制 "SGIX"；Evo JSON（与 ARC-800 缓存层
    统一格式，可诊断、复用 corruption 路径），只序列化查询所需结构
  - 无 StringInterner / ahash / num_cpus / crossbeam / git2 / dunce：
    Evo 用 std 集合与 rayon 线程池（见债务登记）
Sync policy: 不跟随上游（一次性适配）；LSP diagnostics（ARC-820）与
tool adapter（ARC-830）若复用图结构需单独登记。

## ARC-820 LSP lifecycle（2026-08-07）

**LSP 参考 Grok 的 async-lsp 用法与 Evo 的 MCP 模式重建，无直接移植代码。**
Grok 的 LSP 客户端（`implementations/lsp/`：client/manager/diagnostics/
pull/refresh，基于 `async-lsp = 0.2.3`）在本仓库无 vendored 源码可对照；
本实现按 Evo 语义自研（见 `docs/refactor/phase8-lsp.md`）：
- **async-lsp 未引入**（评估：0.2.x 面向 server 角色，client 侧
  ClientState 需自搭，SandboxProfile/TaskRegistry ownership/document
  replay/edit 校验均在其外；手写 Content-Length 帧 wire 与 MCP wire
  先例一致，见 `docs/refactor/phase8-lsp.md` 依赖决策）
- 协议 wire 形状（JSON-RPC 2.0 消息判别/错误码）是协议标准，解析纪律
  参照 Evo 自有 `crates/extension-host/src/mcp/wire.rs`（Phase 7）重写，
  帧协议为 LSP 的 Content-Length 头（非 MCP 的 JSON lines）
- 会话（id 分发 / 通知 fan-out / 取消 / 超时 / 进程治理）参照 Evo 自有
  `crates/extension-host/src/mcp/transport.rs` 的 RpcSession 形状重写，
  新增服务器→客户端请求回执与坏帧 fail closed（帧流无法恢复边界）
- 生命周期状态机、document replay、diagnostics stale policy、edit
  转换层（WorkspaceEdit → 校验 → EditPlan → 注入 applicator →
  ChangeReceipt）均为 Evo 自研，无 Grok 对应物
Destination paths: `crates/code-intelligence/src/lsp/{mod,wire,state,
transport,documents,diagnostics,edit,query}.rs`、
`crates/code-intelligence/src/lsp/server/{mod,actor}.rs`、
`crates/code-intelligence/src/bin/fake_lsp_server.rs`（测试辅助）
Tests carried over: 无直接复制；按 Evo 语义重写 —— wire 帧/严格解析/
截断/超大帧、状态机 transition 表、document 版本单调/UTF-16 偏移、
diagnostics stale 状态机、restart+backoff 指数与上限、replay、
shutdown 顺序与在途取消、edit 校验（路径越界/版本不匹配/未打开）与
受限 applicator、进程级 fake server 集成
Local modifications: 全部（无上游源码）；Grok 的 diagnostics 无版本
状态机（直接转发 UI），Evo 增加 stale policy；Grok 无 document 状态
（async-lsp server 端才有）
Sync policy: 不跟随上游；若后续引入真实语言服务器接入（ARC-830），
LSP 查询语义映射与 async-lsp 无关。

## ARC-830 Tool/context 集成（2026-08-07）

**纯 Evo 集成层，无 Grok 移植。** 本 ARC 是 read-only 查询工具、
按需 context 注入与共用排序接口的产品集成，Grok 无对应物（其 graph/
LSP 查询直接暴露给模型/UI，无 ToolCapabilities / output budget /
截断标记概念；context 侧把符号图缓存整体给模型）：
- `tool-contract::ranking`（RelevanceScorer / ResultRanker /
  TokenOverlapScorer）为 Evo 自研共享排序契约，供 graph 符号搜索与
  MCP `mcp_search` 两侧共用（不共享存储实现）
- `code_graph` / `code_lsp` DynamicTool（`crates/code-intelligence/
  src/tools/`）为 Evo 自研：独立 ToolCapabilities
  （WorkspaceLocalReadOnly）+ 条数/字节双层 output budget（超限显式
  标记）+ cancel 贯通；`QueryKind::SymbolSearch` 为 Evo 扩展查询面
  （Grok 只有精确名导航）
- 按需 context 注入（`crates/code-intelligence/src/context.rs` +
  `crates/coding-agent/src/app/code_context.rs` seam）为 Evo 自研，
  不把完整符号图塞入 system prompt；Grok 无此概念
- LSP 结果语义映射（hover markdown 提取 / location 归一化）承接
  ARC-820 的 Evo 查询面，与 async-lsp 无关
Destination paths: `crates/tool-contract/src/ranking.rs`、
`crates/code-intelligence/src/{context,tools/{mod,budget,graph,lsp}}.rs`
及 `graph/query.rs`（search）、`service.rs`（QueryKind::SymbolSearch）、
`graph/backend.rs`（Search 变体）、`tests/tools_lsp.rs`、
`crates/extension-host/src/mcp/meta.rs`（rank_search_matches）、
`crates/coding-agent/src/app/{bootstrap,startup,code_context}.rs`、
`crates/coding-agent/src/tools/code_tools_tests.rs`
Tests carried over: 无直接复制；全部按 Evo 语义重写 —— 工具参数校验/
结果转换/预算截断标记/cancel 贯通/结构化错误、context 边界、排序契约
（空结果/稳定顺序/limit）、三态装配
Local modifications: 全部（无上游源码）
Sync policy: 不跟随上游。
