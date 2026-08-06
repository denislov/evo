# Evo 完整架构重构计划

> 状态：执行中（Phase 0～4 完成，Phase 4 Gate 通过，准备进入 Phase 5）
> 决策日期：2026-08-05
> 基线 commit：`2cd3ddf`
> 输入材料：`docs/grok-build架构学习-1.md`、`docs/grok-build架构学习-2.md`
> 适用范围：整个 Rust workspace、CLI、Desktop、TUI、持久化格式与第三方模块移植
> 重构策略：允许破坏内部 Rust API 和尚未承诺稳定的产品行为；持久化数据必须迁移或显式拒绝；不长期保留兼容层

---

## 执行进度

| 阶段 | 状态 | 已完成证据 |
| --- | --- | --- |
| Phase 0 | 完成 | `docs/refactor/phase0-baseline.md`、`phase0-contract-inventory.md`、architecture/performance/API Gate、provenance registry |
| Phase 1 / ARC-100 | 完成 | 根 manifest 已虚拟化；删除 Hello World package；统一 workspace metadata/dependencies |
| Phase 1 / ARC-110 | 完成 | `ai-protocol` 已拥有 provider-neutral model/conversation/stream/hooks；`agent-core` production dependency 已移除 `ai` |
| Phase 1 / ARC-120～130 | 完成 | `tool-contract`、`tool-runtime` 已建立真实编译边界；typed schema、显式 authorization risk、registry、requirements、cancel/timeout/progress/concurrency contract 已通过测试 |
| Phase 1 / ARC-140 | 完成 | `event-journal` 已拥有 durable frame、lease、append/checkpoint、torn-tail repair、bounded tail；session codec/policy 保留在 `coding-agent` |
| Phase 1 Gate | 完成 | workspace/release API/architecture/core perf/desktop headless/native perf 全部通过 |
| Phase 2 / ARC-200 | 完成 | `docs/refactor/phase2-ai-tool-adapter.md`；唯一 AI/tool adapter、结构化错误映射、provider declaration 与 local executable 已分离 |
| Phase 2 / ARC-210 / read | 完成 | `docs/refactor/phase2-read-tool.md`；typed runtime、结构化输出/错误、图片与读取预算、revision/fingerprint 均已固定 |
| Phase 2 / ARC-210 / ls | 完成 | typed args、schema、capabilities、structured errors 与目录 identity details 已固定 |
| Phase 2 / ARC-210 / find | 完成 | `docs/refactor/phase2-find-tool.md`；typed glob args、capability walk、结构化错误与 root fingerprint 已固定 |
| Phase 2 / ARC-210 / grep | 完成 | `docs/refactor/phase2-grep-tool.md`；typed search args、regex/glob、bounded search、结构化错误与 details 已固定 |
| Phase 2 / ARC-210 | 完成 | `read/ls/find/grep` 全部通过 typed runtime 与完整 Gate；旧 inventory marker 进入 ARC-240/250 清理 |
| Phase 2 / ARC-220 | 完成 | `docs/refactor/phase2-edit-engine.md`；write/edit/hashline/apply_patch 已共用 typed runtime、四级 matcher、mutation fence、revision identity 与 ChangeReceipt |
| Phase 2 / ARC-230 | 完成 | `docs/refactor/phase2-shell-tool.md`；bash 已迁移 typed runtime，复用既有 process primitive，并统一 bounded progress/terminal 与协作 teardown |
| Phase 2 / ARC-240 | 完成 | `docs/refactor/phase2-tool-inventory.md`；profile/capability/runtime/authorization/delegation inventory 已统一使用 `ToolId` |
| Phase 2 / ARC-250 | 完成 | custom/delegation/builtin 全部进入 typed runtime；`AgentTool`、`add_tool`、Legacy dispatch、marker/facade 与重复 schema validator 已物理删除 |
| Phase 2 Gate | 完成 | coding-agent/agent-core/tool-runtime tests、workspace check、release API、architecture 与 core perf Gate 全部通过 |
| Phase 3 / ARC-300 | 完成 | `docs/refactor/phase3-workspace-runtime.md`；identity/lease/opaque access handle、filesystem capability、path binding、mutation fence、process primitive 全部进入 `workspace-runtime`，coding-agent 仅持 workspace handle |
| Phase 3 / ARC-310 | 完成 | `docs/refactor/phase3-worktree-builder.md`；managed identity、Git worktree + copy/reflink fallback、fail-closed dirty/untracked sync、process-tree cancellation、ignore/path policy 已落地并测试 |
| Phase 3 / ARC-320 | 完成 | `docs/refactor/phase3-worktree-registry.md`；原子文件 registry、owner process identity、启动 maintenance（interrupted/stale/dead-owner 清理 + orphan 保留）、GC（age/owner liveness/disk budget/dry-run）已落地并测试 |
| Phase 3 / ARC-330 | 完成 | `docs/refactor/phase3-child-isolation.md`；写权限有效交集 child 申请独立 managed worktree、不再 clone 父 capability、projectless/read-only/显式 shared-cwd 分别定义策略、team 并发上限改由 worktree capacity 决定（固定 `2` 已删除） |
| Phase 3 / ARC-340 | 完成（复验 2026-08-05） | `docs/refactor/phase3-merge-protocol.md`；creation-time baseline、Git/copy-mode ChangeSet/MergeProposal、fail-closed 4096 limit、parent untracked/symlink conflict、事务 journal 与 startup recovery、可取消 Merge/Discard admitted operation、CLI/Desktop/Team supervisor 产品入口 |
| Phase 3 / ARC-350 | 完成（复验 2026-08-05） | `docs/refactor/phase3-worktree-test-matrix.md`；双 child、dirty baseline、symlink、rename/delete、binary、large file、create cancel、真实 registry reopen、确定性 ENOSPC partial-apply/journal rollback、GC crash 边界均已固定 |
| Phase 3 / ARC-351 | 非阻塞验证债务（最迟 Phase 10） | Windows/macOS 真实平台上的路径、symlink、文件锁与 Git worktree 差异；先运行 `workspace-runtime` 轻量 CI pilot，根据实测磁盘峰值选择 hosted 或 self-hosted runner，不阻塞 Phase 4 |
| Phase 3 Gate | 通过（2026-08-05） | 并行 child 默认完全隔离；父 workspace 在 merge 前逐字节不变；异常退出后 registry/worktree 可恢复或安全清理；workspace 全量测试与 architecture gate 通过 |
| Phase 4 / ARC-400 | 完成（复验 2026-08-06） | `docs/refactor/phase4-change-tracker.md`；`change-tracker` crate（仅依赖 workspace-runtime）：单 actor fs event service、bounded debounce、directory type fact、dynamic directory 补扫、rename chain/ambiguous/malformed/root/ignore 边界、nested `.git` 隔离、相对 gitdir canonical ownership、watch budget fail-closed、可靠 shutdown、带 root/sequence 的 Git HEAD/index/lock 元数据事件，36 个 watcher 测试覆盖 |
| Phase 4 / ARC-410 | 完成（复验 2026-08-06） | `docs/refactor/phase4-hunk-tracker.md`；baseline→current HunkTracker actor、receipt/event 因果关联、稳定 HunkId、来源归因、bounded diff/content/history、accept/reject plan 与 creation/deletion/empty-file fail-closed 语义；45 个 `change-tracker` 测试通过 |
| Phase 4 / ARC-420 | 完成（复验 2026-08-06） | `docs/refactor/phase4-review-domain.md`；统一 list/open/accept/reject/discard API、typed receipt 直连 filesystem tools、共享 Review DTO、live Review product event、Desktop/CLI projection 与 capability/revision/target 复验；coding-agent 178、Desktop 303 测试通过，workspace/release API/architecture/clippy Gate 通过 |
| Phase 4 / ARC-430 | 完成（复验 2026-08-06） | `docs/refactor/phase4-rewind.md`；三域一致恢复（session event cursor/branch + workspace snapshot restore + hunk tracker checkpoint）、新 branch 不截断历史、事务双向 rollback、startup recovery、capability/cursor/drafts 同步、fail-closed（source/stale/sidecar）；change-tracker 60、workspace-runtime 91、coding-agent 182 测试通过，architecture Gate 通过（oversized_debts=35，execution_debts=0） |
| Phase 4 / ARC-440 | 完成（复验 2026-08-06） | `docs/refactor/phase4-rewind-tests.md`；WatchGap full reconcile（重新扫描 tracked 文件、更新 current、生成 external fact、重置 Ready）、forwarder 自动触发、显式命令接口、reconcile 后 checkpoint 可用；change-tracker 66 测试通过，跨域矩阵覆盖 receipt/event 双顺序、hunk 漂移、rename、冲突 reject、stale accept、rewind 后 prompt、crash reopen |
| Phase 4 Gate | 通过（2026-08-06） | UI 展示的每个 diff 可追溯到文件事实；accept/reject/rewind 有 revision 防护；外部修改不会被错误归因给 Agent；WatchGap 后可 reconcile 恢复 |
| Phase 5 / ARC-500 | 完成（2026-08-06） | `docs/refactor/phase5-agent-actor.md`；`Arc<RwLock<AgentState>>`、`queues_cleared`、`RunGuard`、`TurnRunDropGuard` 已删除；Agent 持有 `AgentHandle`（bounded mpsc），actor 独占 AgentState；`TurnRunner` 替代 `TurnLoopStream`，`turn_continues` flag 在 tool-turn 间让出控制权；coding-agent async 适配；agent-core 73、coding-agent 185 测试通过 |
| Phase 5 / ARC-510 | 完成（2026-08-06） | `docs/refactor/phase5-prompt-queue.md`；`PromptQueueEntry`（id+version+message）替换裸 `AgentMessage`；`AgentInputQueue::Interjection` 高优先级 drain；`AgentQueueError::StaleVersion`/`NotFound` 支持 edit/remove 带版本检查；coding-agent `CodingAgentControlKind::Interject` 适配 |
| Phase 5 / ARC-520 | 完成（2026-08-06） | `docs/refactor/phase5-context-compaction.md`；`TokenEstimationConfig`（bytes_per_token override）；`prepare_compaction` 孤立 ToolResult 保护（切点不拆 tool pair）；fitted 降级（summarization 失败不 abort）；`CompactionSampler`（model/max_tokens seam）；summary UTF-8 安全截断 |
| Phase 5 / ARC-530 | 完成（2026-08-06） | `docs/refactor/phase5-session-actor.md`；shutdown 顺序固定（stop admission -> cancel/join operation -> commit terminal -> drain writer -> close actor）；3 个 shutdown 可靠性测试 |
| Phase 5 / ARC-540 | 完成（2026-08-06） | TurnRunner context panic 修复（cancel_token clone + pending buffer + pending_commit）；7 个可靠性测试（mailbox saturation、actor panic、provider hang、concurrent steer/follow-up/abort、current-thread runtime、shutdown 无泄漏、slow consumer） |
| Phase 5 Gate | 通过（2026-08-06） | `Arc<RwLock<AgentState>>` 和 `queues_cleared` 删除；每 session 只有一个状态写入者（Agent actor）；所有 command 有有界失败语义（MailboxFull/ActorClosed/StaleVersion/NotFound）；agent-core 73、coding-agent 185 测试通过，clippy/fmt/architecture gate 通过（execution_debts=0） |

Phase 0 基线固定在重构前结构；后续 crate/LOC 变化不回写覆盖该基线，只新增阶段完成报告。

---

## 一、执行摘要

本计划不是把 `grok-build` 缩小后复制到 Evo，而是借其成熟模块完成 Evo 的下一次架构跃迁：

1. 将工具协议、工具执行、workspace、文件变更跟踪和事件日志从 `coding-agent` 中抽成真正的编译边界。
2. 先解决多 Agent 共用 cwd 的安全问题，再提高并发数和自治程度。
3. 将 changed-file snapshot 升级为有来源归因、可接受/拒绝 hunk、可恢复的 review domain。
4. 将 shell authorization 补强为 child-process filesystem/network enforcement。
5. 在稳定的 tool runtime 上建设 hooks、MCP、codebase graph 和 LSP，而不是把它们直接塞进产品层。
6. 将 `agent-core` 从共享锁对象演进为 bounded actor，将 CLI 交互循环和 Desktop reducer 收敛成可测试状态机。
7. 完成后清除全部迁移适配器、feature flag、dual-write 和执行债务，不保留“以后再删”的旧路径。

计划允许激进拆分，但不追求 Grok 的 crate 数量。目标 workspace 约 12～14 个职责明确的 crate；禁止创建
`common`、`utils`、`shared-types` 这类无所有权边界的垃圾桶 crate。

最高优先级依次是：

```text
tool contract/runtime
        ↓
managed worktree + child capability isolation
        ↓
fs event stream + hunk tracker + merge/review
        ↓
agent/session actor + background task + sandbox
        ↓
hooks/MCP/code intelligence
        ↓
CLI/Desktop/TUI 适配器收敛
```

---

## 二、两份调研结论的取舍

### 2.1 直接采纳的共识

- 不整体移植 `xai-grok-shell`、`xai-grok-workspace`、`xai-grok-pager`。
- `coding-agent` 继续保留唯一产品 facade，但内部能力必须逐步下沉到独立 crate。
- Tool contract/runtime 是 MCP、plugin、远程工具和 LSP tool adapter 的前置工程。
- 多 Agent 共用 workspace 是当前最重要的结构缺口，必须以 managed worktree 解决。
- `xai-hunk-tracker` 与 `xai-fsnotify` 应成组引入，单独接任意一个都不能形成可信 review。
- Evo 已有成熟 process runner、durable session、authorization、skills loader，不重复搬运 Grok 的同类聚合层。
- Sandbox 采用 child-process-first，不采用 Grok 的 process-wide 策略。
- Grok 第一方代码可按 Apache-2.0 使用；来自 Codex/OpenCode 的实现必须保留 notice 和修改声明。

### 2.2 对第一份报告的修正

- `token-estimation`、`circuit-breaker`、`interjection-core` 虽然易移植，但不能排在 worktree/review 主线之前。
- 不直接用 Grok tool runtime 替换 `AgentTool`；先建立 Evo 自己的 contract，再逐个迁移内建工具。
- 不直接复制完整 compaction；只吸收 token policy、tool-pair 切分、失败降级和 provider-neutral sampler seam。
- 不把完整 Markdown/pager 作为架构优先项。TUI 仅吸收 Elm dispatch、checkpoint streaming 和虚拟化思想。
- 不照搬“每 session 一个 OS 线程”。Evo 先使用 bounded Tokio actor；只有出现真实 `!Send` 需求才升级到线程隔离。
- `apply_patch`、hashline 和 seek-sequence 进入统一 edit engine，不能形成三套互不一致的写文件工具。

### 2.3 对第二份报告的扩展

- 除 worktree、review、hooks、web fetch 外，完整计划还必须处理 `Agent` 的 `Arc<RwLock<AgentState>>`、
  CLI 超大交互循环、provider resilience、prompt queue 乐观锁和 session rewind。
- `coding-agent` 的五层 AST 守卫保留到 crate 抽取完成；最终由 Cargo 编译边界承担主约束，AST 守卫只检查
  `coding-agent` crate 内剩余层次。
- 当前根 package 必须删除，根 `Cargo.toml` 变为 virtual workspace manifest；不再保留 Hello World binary。

---

## 三、当前基线与主要问题

### 3.1 当前依赖

```text
ai <- agent-core <- coding-agent <- cli
                              \--- desktop
tui <- cli
```

当前优点必须保留：

- `coding_agent::api` 是 CLI/Desktop 的唯一产品边界。
- operation descriptor 统一 admission、durability、cancellation 和 child policy。
- session event log、outbox、recovery、bounded hydration、client projection/reconnect 已有可靠性测试。
- filesystem capability 绑定实际目标并重验证，避免典型 TOCTOU。
- process runner 已支持 timeout、bounded output、cancel、Unix process group 和 Windows Job Object。
- `coding-agent` 已有 AST 分层守卫和 900 行文件上限。

当前问题按严重度排序：

| 问题 | 影响 | 目标处理阶段 |
| --- | --- | --- |
| 子 Agent 继承父 cwd/capability | 并发写冲突、无法可靠归因和合并 | Phase 3 |
| Tool 与 `ai::ContentBlock` 耦合 | MCP/plugin/remote tool 难接入 | Phase 1～2 |
| changed-file 主要由 tool event 推导 | 看不到外部编辑，diff 与增删行不可信 | Phase 4 |
| shell 无 OS-level confinement | 授权后可越出 workspace 或联网 | Phase 6 |
| `AgentState` 使用共享 `RwLock` | queue/turn commit 竞态复杂，状态所有权不清 | Phase 5 |
| `coding-agent` 承担过多基础设施 | 约 6 万行，编译边界不足 | 全程 |
| CLI/Desktop 存在数千行热点 | reducer、effect、render 责任集中 | Phase 9 |
| 根 package 无实际用途 | composition root 不明确 | Phase 1 |

---

## 四、目标架构

### 4.1 目标 crate 图

以下箭头表示“上层依赖下层”：

```text
                                ai
                                │
                          ai-protocol

tool-runtime ───────────> tool-contract
agent-core ─────────────> tool-runtime + ai-protocol

change-tracker ─────────> workspace-runtime
extension-host ─────────> tool-runtime + workspace-runtime
code-intelligence ──────> workspace-runtime

event-journal             （独立 append-only/journal 基础设施）

coding-agent
  ├──> ai + ai-protocol
  ├──> agent-core
  ├──> tool-runtime
  ├──> workspace-runtime
  ├──> change-tracker
  ├──> event-journal
  ├──> extension-host
  └──> code-intelligence

cli ───────> coding-agent + tui
desktop ───> coding-agent
```

### 4.2 目标 crate 职责

| Crate | 职责 | 明确禁止 |
| --- | --- | --- |
| `ai-protocol` | provider-neutral message、content、usage、model request/stream event、provider port | HTTP、API key、具体 provider wire 类型 |
| `ai` | provider registry、auth resolution、HTTP/SSE、重试、具体 provider adapter | agent loop、tool execution、产品事件 |
| `tool-contract` | ToolId、typed schema、capabilities、input/output/error/progress、behavior version | AI ContentBlock、workspace handle、HTTP client |
| `tool-runtime` | registry、requirements、execution context、cancel、timeout、stream、concurrency policy | 具体文件/网络工具实现、产品授权 UI |
| `agent-core` | turn engine、actor、queue/interjection、context assembly、compaction policy、skills/resources | 产品 session 仓储、workspace 实现、UI DTO |
| `workspace-runtime` | capability、bounded FS、process、managed worktree、background task、sandbox policy | AgentMessage、ProductEvent、UI 状态 |
| `change-tracker` | fs semantic events、hunk actor、diff、来源归因、snapshot、accept/reject engine | CLI/Desktop 展现、session repository |
| `event-journal` | append-only record、lease、bounded tail、outbox、checkpoint、fault recovery | ProductEvent 业务枚举、projection、用户设置 |
| `extension-host` | user hooks、MCP lifecycle、credential seam、external tool provider | 内建文件工具、产品 UI、provider 实现 |
| `code-intelligence` | tree-sitter graph、incremental index、LSP lifecycle/diagnostics | Agent loop、MCP、产品事件持久化 |
| `coding-agent` | operation、authorization、profiles、session domain、product events、projection、facade、composition | 重复实现 FS/process/tool runtime/journal |
| `tui` | 通用终端组件、Markdown、editor、viewport/virtualization | 产品 session 和 tool 语义 |
| `cli` | 参数、RPC、interactive reducer/effects、终端 adapter | repository、provider、capability 内部类型 |
| `desktop` | GPUI reducer/effects/views、native integration | repository、provider、tool implementation |

### 4.3 必须长期成立的依赖原则

1. `tool-contract` 不依赖 `ai-protocol`；tool result 通过 adapter 转成 conversation content。
2. `agent-core` 不依赖具体 `ai` provider crate，只依赖 `ai-protocol` 中的 provider port。
3. `workspace-runtime` 不依赖 `coding-agent`、`agent-core` 或任何 UI。
4. `event-journal` 不认识 ProductEvent；编码/解码和业务 transaction 在 `coding-agent`。
5. CLI/Desktop 只能依赖 `coding_agent::api`，不得直接依赖新基础 crate。
6. 所有用户可配置执行能力共用同一 folder trust 和 authorization source of truth。
7. 所有文件写入最终经过 `workspace-runtime` 的 mutation fence，并通知 `change-tracker`。
8. 所有 child Agent 默认使用独立 managed worktree；共享 cwd 只能是显式、可审计的单 Agent 模式。

---

## 五、Grok 模块落点

| Grok 模块/设计 | Evo 落点 | 方式 | Phase | 说明 |
| --- | --- | --- | --- | --- |
| `xai-tool-types/protocol/runtime` | `tool-contract`、`tool-runtime` | 设计重建 | 1～2 | 不复制 wire/computer hub 耦合，保留 typed schema、capabilities、context、stream invariant |
| `xai-fast-worktree` | `workspace-runtime` | 裁剪移植 | 3 | 首版只取 Git worktree、copy/reflink、dirty sync、registry、recovery、GC |
| `xai-fsnotify` | `change-tracker` | 适配移植 | 4 | 保留 semantic stream、debounce、watch budget、Git operation state |
| `xai-hunk-tracker` | `change-tracker` | 适配移植 | 4 | 保留 actor、origin、stable hunk、snapshot、accept/reject，裁剪非必要 analytics |
| Codex apply-patch / Grok hashline | `workspace-runtime` edit engine | 适配移植 | 2 | 合并成单一编辑内核，保留来源 notice 和修改声明 |
| `xai-token-estimation` | `agent-core` context policy | 直接/轻适配 | 5 | 保留边界、rounding、饱和运算测试，允许模型级 override |
| `xai-interjection-core`、`xai-prompt-queue` | `agent-core` actor/queue | 适配移植 | 5 | 对齐 Evo steering/follow-up/abort 和多客户端 ownership |
| `xai-grok-compaction` | `agent-core` compaction | 只移植策略层 | 5 | 不替换现有 session 事实，吸收 tool-pair、sampler seam、fallback ladder |
| Grok background task/bash task | `workspace-runtime` | 只移植 registry/protocol | 6 | 复用 Evo process runner，不复制 shell implementation |
| `xai-grok-sandbox` | `workspace-runtime` | 重新设计 | 6 | child-process-first；不采用 Desktop process-wide confinement |
| Grok `web_fetch` SSRF/cache | built-in tool + shared HTTP | 模块级移植 | 6 | 必须连同 redirect、DNS/IP 和 budget tests 一起迁移 |
| `xai-circuit-breaker` | `ai` shared transport | 直接/轻适配 | 6 | provider/endpoint 级 key，复用注入 clock 测试 |
| `xai-grok-auth`、`extra-ca`、`secrets` | `ai` transport/auth | seam/模块移植 | 6 | 不复制 xAI credential 字段；保留 401 refresh、CA、scrub 思路 |
| `xai-grok-hooks` | `extension-host` | 适配移植 | 7 | 复用 Evo trust、authorization、process、event DTO |
| `xai-grok-mcp` | `extension-host` | 参考状态机后重建 | 7 | tool provider adapter；隔离 transport/OAuth 大依赖 |
| `xai-codebase-graph` | `code-intelligence` | 适配移植 | 8 | 先 read-only，再接 fs incremental update |
| Grok LSP lifecycle | `code-intelligence` | 参考设计后重建 | 8 | 复用 task ownership、sandbox、workspace edit |
| Markdown checkpoint/virtual pager | `tui` | 只借鉴设计 | 9 | 不复制 pager 聚合层和 44 万行 UI |
| PTY e2e/scenario runner | workspace test support | 适配方法论 | 9 | mock provider + terminal emulator + 声明式场景 |
| `xai-sqlite-journal` | worktree/index registry | 条件移植 | 3/8 | 只有采用 SQLite 时引入，连同 NFS/WAL 保护 |
| `xai-grok-update` | 发布工具 | 条件适配 | 10 | 发布渠道、签名和 rollback 确定后才实施 |

---

## 六、迁移与工程纪律

### 6.1 兼容策略

- Rust 内部 API：允许直接破坏，不保留 deprecated alias。
- `coding_agent::api`：允许在独立 Phase 中破坏，但 CLI/Desktop 必须同 commit 迁移。
- CLI flags/RPC wire：已有用户可见行为应提供一次性迁移说明；不维护两个长期版本。
- session/config 数据：引入显式 schema version；提供原地 migration、备份和幂等测试。
- feature flag：只允许作为单个 Phase 内的迁移开关，Phase Gate 前必须删除。
- dual-write：仅允许在 migration 验证期间存在，最长一个 Phase，最终 Gate 必须为零。

### 6.2 第三方代码移植协议

每个移植模块必须新增 provenance 记录，至少包含：

```text
upstream repository
upstream SOURCE_REV / commit
source path
license
third-party notices
copied tests
local modifications
local owner crate
```

移植顺序固定为：测试与契约 -> 最小实现 -> Evo adapter -> 删除旧实现 -> license audit。

### 6.3 每个任务的完成定义

- 实现、测试、文档和迁移同时完成。
- 不存在未登记的 TODO、fallback、旧 module re-export 或 dead feature。
- 相关生产文件不超过 900 行；测试文件不超过 1,200 行；generated/vendor 文件除外。
- 新增公共类型有边界测试或 round-trip golden。
- 新增状态机有 transition-table 测试。
- 新增异步服务有 shutdown、cancel、queue saturation 和 task panic 测试。
- 文件/日志相关功能有 fault injection 或 crash-reopen 测试。

### 6.4 全局 Gate

在现有 `scripts/gate.sh` 基础上逐步加入：

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
workspace dependency allowlist / cycle check
production/test file size check across all first-party crates
public facade leak check
session/config migration fixtures
Linux integration tests
Windows target check for process/worktree/sandbox cfg branches
PTY scripted scenarios
Desktop deterministic replay/visual scenarios
license/provenance audit
执行债务清零检查
```

---

## 七、分阶段执行计划

## Phase 0：冻结基线与建立重构闸门

目标：在改变 crate 边界前固定行为、性能和持久化基线。

### ARC-000 记录基线

- 记录每个 crate 的生产/测试 LOC、编译时间、测试数量、依赖数量和最大文件。
- 记录 CLI 冷启动、Desktop 启动、100k event session hydration、首 token、16 MiB shell 输出处理时间。
- 记录当前 session/config fixture 和所有公开 operation/event DTO。
- 将两份 Grok 调研报告标记为决策输入，不把其中的规模数据当长期真相。

### ARC-001 扩展 Gate

- 将 900 行 production 限额从 `coding-agent` 扩展到全部 first-party crate。
- tests 使用 1,200 行上限，避免为了行数拆散同一场景；fixture/generated 排除。
- 新增基于 `cargo metadata` 的 crate dependency allowlist 和 cycle check。
- 新增 CLI/Desktop 禁止绕过 `coding_agent::api` 的 AST 检查。
- 新增 `TODO(ARC-*)` 债务格式校验和最终零债务检查。

### ARC-002 固定跨层契约

- 保存 ProductEvent 全 family round-trip golden。
- 保存 CLI RPC wire fixtures、Desktop replay fixtures、tool call/result fixtures。
- 保存 session reopen、torn tail、outbox recovery、authorization target revalidation 测试。
- 为 root/child/team operation 记录 capability snapshot 和 terminal event 序列。

### ARC-003 建立第三方 provenance 目录

- 新增统一 provenance/notice 模板。
- 登记 Grok、Codex、OpenCode 来源边界。
- 禁止从 `third-party/grok-build` 复制代码但不复制对应测试和 notice。

Phase 0 Gate：现有行为全部通过；基线文档可重复生成；工作区无迁移代码。

---

## Phase 1：建立目标 crate 骨架与纯契约边界

目标：只改变依赖和所有权，不改变产品行为。

### ARC-100 根 workspace 收敛

- 删除根 `[package]` 和 `src/main.rs`，根 `Cargo.toml` 变为 virtual workspace manifest。
- 显式设置 workspace resolver、edition、rust-version、license 和共享依赖版本。
- CLI/Desktop 继续作为实际 composition binaries。
- 清理重复依赖版本和未使用 workspace dependency。

### ARC-110 抽取 `ai-protocol`

- 从 `ai` 移出 provider-neutral conversation、stream event、usage、model request 和 provider port。
- provider-specific wire DTO、HTTP/SSE、auth、retry 保留在 `ai`。
- `agent-core` 改为只依赖 `ai-protocol`；`coding-agent` composition root 注入具体 provider streamer。
- 为所有 provider conversion 建立 protocol compatibility tests。

### ARC-120 抽取 `tool-contract`

- 定义 `ToolId`、`ToolKind`、`ToolCapabilities`、`ToolBehaviorVersion`。
- 定义 provider-neutral `ToolContent`、`ToolOutput`、`ToolProgress`、`ToolErrorKind` 和 `ToolError`。
- capabilities 至少包含：read-only、concurrency、cancel、timeout、streaming、provider-executed。
- schema 使用 Rust typed args + `schemars` 生成；保留严格预算验证，禁止任意 schema 扩张。
- 将 authorization risk 从 magic schema key 移到显式 metadata。

### ARC-130 抽取 `tool-runtime`

- 定义 object-safe dynamic tool 和 typed tool adapter。
- `ToolCallContext` 提供 typed extensions：operation/turn/call、cwd、cancel、deadline、trace、progress sink。
- registry 支持 duplicate detection、requirements validation、per-turn listing 和 behavior version。
- runtime 负责 timeout、cancel、stream terminal invariant 和 concurrency policy，不负责产品授权。

### ARC-140 抽取 `event-journal` 最小内核

- 只迁移 append-only record、lease、bounded tail reader、atomic checkpoint/outbox primitive 和 torn-tail repair。
- ProductEvent、session manifest、projection 和 operation transaction 留在 `coding-agent`。
- 使用 codec trait/closure 注入业务序列化，不创建泛型抽象层级。
- 保留现有 fault injection 和 100k event bounded hydration 测试。

Phase 1 Gate：所有 UI、RPC、session 和工具行为与基线一致；旧路径没有 re-export 兼容层；依赖图符合目标 allowlist。

---

## Phase 2：工具平台类型化与内建工具迁移

目标：彻底删除旧 `AgentTool = closure + ContentBlock` 模型。

### ARC-200 建立 AI/tool adapter

执行状态：完成。完成证据见 `docs/refactor/phase2-ai-tool-adapter.md`。

- 在 `agent-core` 建立 `ToolOutput -> ai_protocol::ContentBlock` 的唯一转换点。
- provider-executed web search 通过声明 adapter 接入，不伪装成本地 executable tool。
- Tool result details、error、termination 和 progress 的 conversation 映射有 golden tests。

### ARC-210 迁移只读文件工具

执行状态：完成（2026-08-05）。

- 按 `read -> ls -> find -> grep` 顺序迁移 typed args、schema、capabilities 和 structured errors。
- 保留 bounded reads、walk budget、ignore/gitignore 和图片读取约束。
- 增加 read-before-edit 所需的文件 revision/fingerprint 输出。
- `read`、`ls`、`find`、`grep` 已完成。迁移期 inventory marker 必须在 ARC-240/250 改用 `ToolId` 后物理删除。

### ARC-220 建立统一 edit engine

执行状态：完成（2026-08-05）。完成证据见 `docs/refactor/phase2-edit-engine.md`。

- `write`、`edit`、未来 `apply_patch` 共用 mutation fence、path binding 和 change receipt。
- 匹配策略固定为 exact -> rstrip -> trim -> Unicode normalization；仅唯一命中时允许降级。
- 引入 hashline anchor：内容 hash、位置窗口、freshness、重叠检测、批量快照验证。
- 引入 patch parser/apply engine，但不复制 Codex shell/file runtime。
- 所有写入返回 `ChangeReceipt`：before/after revision、path、byte/line delta、origin、optional unified diff。
- 同路径写操作由 file operation lock 串行化，不同 worktree 可并行。

### ARC-230 迁移 shell 工具

执行状态：完成（2026-08-05）。完成证据见 `docs/refactor/phase2-shell-tool.md`。

- shell tool 只负责 typed args 和 ToolStream adapter。
- 继续使用 Evo 现有 process primitive，不迁移 Grok 的 process implementation。
- progress chunk 和 terminal output 使用同一 bounded policy，禁止 terminal 后继续发送 progress。

### ARC-240 Tool requirements 与行为版本

执行状态：完成（2026-08-05）。完成证据见 `docs/refactor/phase2-tool-inventory.md`。

- registry 启动时校验 `edit/apply_patch requires read revision` 等要求。
- 仅对确有迁移需求的工具提供 behavior version；没有真实旧 session 需求时不创建 legacy preset。
- tool allowlist、profile tool list 和 provider declaration 全部改用 `ToolId`。

### ARC-250 删除旧工具路径

执行状态：完成（2026-08-05）。完成证据见 `docs/refactor/phase2-tool-inventory.md`。

- 删除 `AgentTool` 中直接持有 `ai::ContentBlock`、手写 execute closure 和 magic metadata 的实现。
- 删除重复 schema builder、重复输出截断和旧 edit matcher。
- 更新 agent-core examples、CLI/Desktop tool event projection 和测试 fixture。

Phase 2 Gate：完成。builtin、custom injected 与 delegation tools 全部由 typed runtime 注册和执行；`AgentTool`、`Agent::add_tool`、Legacy dispatch、builtin marker/facade 和重复 schema validator 已物理删除。coding-agent/agent-core/tool-runtime tests、workspace check、release API、architecture 与 core performance Gate 全部通过。

---

## Phase 3：Managed Worktree 与多 Agent 隔离

目标：任何并行 child Agent 都不直接写父 workspace。

### ARC-300 抽取 `workspace-runtime`

执行状态：完成（2026-08-05）。完成证据见 `docs/refactor/phase3-workspace-runtime.md`。

- 迁移 filesystem capability、path binding、mutation fence、process primitive 和 workspace identity。
- `coding-agent` operation snapshot 仅持有 opaque workspace access handle，不再持有 filesystem/shell capability，也不知道内部 fd/process handle。
- workspace handle 明确区分 source workspace、managed child workspace 和 projectless workspace。

### ARC-310 实现 WorktreeBuilder 最小集

执行状态：完成（2026-08-05）。完成证据见 `docs/refactor/phase3-worktree-builder.md`。

- 首期支持 Git worktree 和 copy/reflink fallback。
- 不实现 overlay/Btrfs，除非基准证明 copy/reflink 无法满足性能目标。
- 支持 cancellation、dirty source snapshot、untracked 文件同步和 ignore policy。
- worktree identity 包含 owner operation、parent session、base revision、creation mode 和 lifecycle state。
- `WorktreeBuilder` 只接受 typed source handle，成功返回绑定 `WorkspaceLease` 与 creation report 的 `ManagedWorktree`；不完整 snapshot、未验证清理或不安全 destination 均 fail-closed。

### ARC-320 Registry、恢复与 GC

执行状态：完成（2026-08-05）。完成证据见 `docs/refactor/phase3-worktree-registry.md`。

- registry 可先使用原子文件/JSONL；需要并发查询后再引入 SQLite + `sqlite-journal`。
- lifecycle：Creating -> Ready -> Active -> MergePending -> Merged/Discarded -> Cleaning -> Removed。
- 启动时扫描 orphan、半创建目录、已结束 operation 和 registry/disk 不一致。
- GC 支持 age、owner liveness、disk budget 和 dry-run；绝不递归删除未验证身份的目录。

### ARC-330 Child capability 隔离

执行状态：完成（2026-08-05）。完成证据见 `docs/refactor/phase3-child-isolation.md`。

- delegation/team invocation 创建 child 前必须申请 managed worktree。
- child filesystem/shell cwd 绑定 child worktree；不再 clone 父 capability。
- projectless、read-only child 和显式 shared-cwd 模式分别定义策略。
- 并发上限由资源预算和 worktree capacity 决定，删除固定 `2` 的产品常量。

### ARC-340 Merge protocol

执行状态：完成（2026-08-05）。完成证据见 `docs/refactor/phase3-merge-protocol.md`。

- child terminal 不直接写回父目录，只产生 `ChangeSet` 和 `MergeProposal`。
- merge 以 parent base revision + child base revision 做乐观校验。
- 支持 clean apply、conflict、stale parent、discard 和 retry。
- merge 本身是 admitted durable operation，有 authorization、events、cancel fence 和 crash recovery。
- Team supervisor 只能选择提议，不能绕过 review/mutation capability。
- proposal event 携带完整、可审阅的 `ChangeSet` DTO；CLI 与 Desktop 使用同一产品 DTO，
  Team supervisor 复用当前 session authority。

### ARC-350 Worktree 测试矩阵

执行状态：完成（复验 2026-08-05）。完成证据见 `docs/refactor/phase3-worktree-test-matrix.md`。

- 两个 child 修改不同文件、同一文件不同 hunk、同一 hunk。
- dirty tracked/untracked source、symlink、rename/delete、binary、large file。
- create cancel、process crash、merge crash、GC crash、disk full。
- 当前 Linux 平台与平台中立协议语义由 ARC-350 阻塞验收；跨平台差异转入 ARC-351。

### ARC-351 跨平台 Worktree 验证债务（非阻塞）

执行状态：待清偿，不阻塞 Phase 4；最迟在 Phase 10 Final Gate 前完成并删除本债务。

- 在真实 Windows 与 macOS runner 上验证路径、symlink 权限、文件锁、rename/delete、
  Git worktree create/discard/GC 和 crash-reopen；交叉编译不能替代运行测试。
- CI 先只运行 `workspace-runtime`：关闭 incremental，不上传或缓存 `target/`，执行
  `cargo check --locked -p workspace-runtime --all-targets --all-features` 与
  `cargo test --locked -p workspace-runtime --all-features`。
- 当前本地全 workspace `target/` 约 35 GiB（`deps` 约 20 GiB、`incremental` 约
  14 GiB），因此不直接复制全量 `scripts/gate.sh` 到 hosted 三平台。pilot 必须记录
  clean build 磁盘峰值；峰值超过 runner 启动可用空间的 70% 时改用 self-hosted runner，
  否则才固化 hosted CI。
- 债务清偿证据包括真实平台命令结果、磁盘峰值、Git/symlink/文件锁差异修复，以及
  可重复的 CI workflow 或 self-hosted runner 操作说明。

Phase 3 Gate：完成（2026-08-05）。并行 child 默认完全隔离；父 workspace 在 merge 前逐字节不变；异常退出后 registry/worktree 可恢复或安全清理。

---

## Phase 4：文件因果事件、Hunk Review 与 Rewind

目标：把 review 从 tool event 投影升级为文件事实系统。

### ARC-400 抽取 `change-tracker`

执行状态：完成（复验 2026-08-06）。完成证据见 `docs/refactor/phase4-change-tracker.md`。

- 建立单 actor 所有权的 fs event service。
- 支持 debounce、rename pairing、dynamic directory watch、gitignore 和 watcher reuse/budget。
- 将原始 notify event 归一化成 semantic event；消费者不得直接依赖 `notify` 类型。
- 识别 Git operation start/completion 和 HEAD/index 变化。

### ARC-410 HunkTracker actor

执行状态：完成（复验 2026-08-06）。完成证据见
`docs/refactor/phase4-hunk-tracker.md`。

- 来源类型至少包括 AgentEdit、ExternalEditOnAgentFile、ExternalEdit、MergeApply、HookEdit。
- 使用 edit `ChangeReceipt` 与 fs event 因果窗口关联，不只依赖路径和时间戳猜测。
- hunk identity 支持内容+位置漂移匹配，保留稳定 HunkId。
- 保存 turn/session snapshot、before/after revision 和 bounded unified diff。

### ARC-420 Review domain/API

执行状态：完成（复验 2026-08-06）。完成证据见
`docs/refactor/phase4-review-domain.md`。

- changed-file snapshot 填充真实 diff、first changed line、added/removed lines 和 source attribution。
- 新增 list changes、open change、accept/reject hunk、accept/reject file、discard child proposal operation。
- accept/reject 前重验证 workspace、revision、hunk identity 和 authorization target。
- Desktop/CLI 使用同一 product DTO，不自行解析 tool output 推断修改。

### ARC-430 Rewind

执行状态：完成（复验 2026-08-06）。完成证据见 `docs/refactor/phase4-rewind.md`。

- rewind 目标是 session event cursor + workspace snapshot + active branch 三域一致恢复。
- 首版只支持 managed worktree/session 内 rewind，不直接回滚用户父 workspace。
- rewind 是新 branch，不截断历史日志；旧 branch 继续可导出。
- 恢复后 hunk tracker、prompt queue、capability generation 和 client cursor 同步更新。

### ARC-440 Review/rewind 测试

执行状态：完成（复验 2026-08-06）。完成证据见 `docs/refactor/phase4-rewind-tests.md`。

- Agent 写后外部编辑、外部编辑后 Agent 写、rename 后继续编辑。
- hunk 漂移、相邻 hunk 合并、冲突 reject、stale accept。
- watcher event 丢失后全量 reconcile。
- rewind 后继续 prompt、再 merge、crash reopen。

Phase 4 Gate：UI 展示的每个 diff 都可追溯到文件事实；accept/reject/rewind 有 revision 防护；外部修改不会被错误归因给 Agent。

---

## Phase 5：Agent/Session Actor、Prompt Queue 与 Context Engine

目标：消除 `Arc<RwLock<AgentState>>`，让状态拥有者和并发语义唯一。

### ARC-500 Agent actor

- `AgentHandle` 只持有 bounded `mpsc::Sender<AgentCommand>` 和只读 watch/broadcast channel。
- actor 独占 messages、tools、queues、cancel token、provider override 和 compaction state。
- command reply 使用 oneshot；mailbox 满、actor closed、reply dropped 都有结构化错误。
- `is_busy` 等 fail-safe query 在 actor 失效时返回保守结果。
- 不采用每 session OS 线程；使用 Tokio task，所有 actor state 保持 `Send`。

### ARC-510 Prompt queue/interjection

- queue entry 使用 id、version、owner、last editor、kind 和 combined display texts。
- stale edit 是显式 conflict/no-op，不允许覆盖新输入。
- steering、follow-up、interjection 和 abort 统一成有界 command 语义。
- 轮中消息在安全边界注入，不破坏 assistant tool-request/tool-result pairing。

### ARC-520 Context/compaction 策略

- 引入统一 token estimation，使用饱和运算、明确 rounding 和模型 override。
- compaction 切点禁止落在 tool pair 中间。
- 策略支持 verbatim -> fitted -> lossy 失败降级和专用 sampler seam。
- compaction reminder、branch summary 和资源提示有统一有界格式。
- 保留 Evo 现有 event sourcing，compaction 结果作为事件/branch 事实而不是原地改历史。

### ARC-530 Session actor 边界

- 每个打开 session 由一个 runtime actor 串行化 admission、Agent command、transaction commit 和 client publication。
- 磁盘 writer 保持独立 bounded worker，不能在 async actor 中执行 blocking I/O。
- 高频 stream update 使用有界、可合并通道；durable terminal/event 走不可丢失路径。
- shutdown 顺序固定：stop admission -> cancel/join operation -> commit terminal -> drain writer -> close actor。

### ARC-540 Actor 可靠性测试

- mailbox saturation、actor panic、provider hang、receiver lag、client disconnect/reconnect。
- prompt/steer/follow-up/abort 同时到达的全排列状态测试。
- current-thread runtime 不冻结；shutdown 无 task/process/worktree 泄漏。

Phase 5 Gate：`Arc<RwLock<AgentState>>` 和对应 queue merge workaround 删除；每 session 只有一个状态写入者；所有 command 均有有界失败语义。

---

## Phase 6：Background Task、Sandbox、安全 Web Fetch 与 Provider Resilience

目标：将长任务、安全边界和网络可靠性纳入统一 runtime。

### ARC-600 Background task registry

- 在 `workspace-runtime` 中增加 task id、owner operation/session/worktree、process handle、output spool 和 terminal state。
- shell 支持 foreground/background；background 不受单次 tool 600 秒硬超时限制，但受 session/task budget。
- 提供 list、output(cursor)、wait(any/all)、cancel 和 snapshot。
- output gap/truncation 显式报告，不能把丢失输出伪装成完整输出。
- session/worktree 关闭时按 ownership policy 终止或转交任务。

### ARC-610 Child-process sandbox

- 定义跨平台 `SandboxProfile`：read roots、write roots、exec policy、network policy、env policy。
- Linux 首选 Landlock + seccomp；macOS Seatbelt；Windows 使用受限 token/Job/AppContainer 能力按可行性分级。
- sandbox 在 child spawn 边界应用，不限制 Desktop 主进程。
- 平台不支持时 fail closed 或明确请求降级授权，不能静默变成 unrestricted。
- 先覆盖 shell/background task/hook/MCP stdio server，再覆盖 LSP。

### ARC-620 安全 `web_fetch`

- URL scheme、redirect 次数、DNS resolution 和 resolved IP 每跳重验证。
- 阻止 loopback、RFC1918、link-local、cloud metadata、IPv4-mapped IPv6 和 DNS rebinding。
- 限制 content-length、实际读取字节、解压后大小、解析时间和输出 token。
- 支持 cache、HTML -> Markdown、plain text；PDF/image/video 只在有明确 consumer 后启用。
- provider-side web search 与本地 fetch 是两个 ToolKind，不混为一谈。

### ARC-630 Provider resilience

- 适配 `circuit-breaker`：滑动窗口、half-open probe、可注入 clock、provider/endpoint key。
- auth seam 支持 cheap snapshot、401 后单次 refresh/retry。
- 增加 extra CA bundle，所有 HTTP client 复用统一 transport builder。
- secrets scrubber 覆盖日志、diagnostic、hook payload、telemetry 和 crash report。

Phase 6 Gate：长任务可查询和取消；获准 shell 仍受 OS policy 限制；web fetch SSRF 测试完整；provider failure 不引发无限重试风暴。

---

## Phase 7：Hooks 与 MCP 扩展平台

目标：在稳定 tool runtime 上开放外部扩展，不污染产品内核。

### ARC-700 抽取 `extension-host`

- 公共 extension event 使用版本化 DTO，不直接暴露内部 ProductEvent。
- host 负责 discovery、config merge、trust、lifecycle、budget、diagnostics 和 shutdown。
- 扩展产生的 tool/文件修改仍经过产品 authorization 和 workspace capability。

### ARC-710 User hooks

- 事件覆盖 session、prompt、tool、permission、stop、subagent、compaction、merge。
- matcher 支持 event/tool/path/profile 条件和确定优先级。
- runner 支持 command；HTTP hook 只有在安全 web client 和 network policy 完成后开放。
- gate 分 Observe、Tool、Stop；每类明确 blocking/fail-open/fail-closed 策略。
- project hooks 共用 folder trust；首次启用必须展示来源和能力。

### ARC-720 MCP provider adapter

- MCP 作为 external tool provider，实现 tool registry adapter，不进入 agent-core。
- 支持 stdio 和 HTTP；ACP transport 只有出现真实需求才加入。
- lifecycle 覆盖 initialize、liveness、per-tool timeout、reconnect、tool/resource change。
- credential store、OAuth 和 refresh 走统一 auth seam。
- 默认采用 `search_tool` + `use_tool` meta tools，避免把大量 MCP schema 全塞入 context。

### ARC-730 Extension tests

- untrusted hook、timeout、输出洪泛、非法 JSON、进程崩溃、重连风暴。
- MCP server 工具列表热更新、调用取消、OAuth 401 refresh、session shutdown。
- extension 修改文件的来源归因和 hunk review。

Phase 7 Gate：用户扩展不能绕过 trust/authorization/sandbox；MCP 工具与内建工具共用同一 Tool contract、事件和取消语义。

---

## Phase 8：Codebase Graph 与 LSP

目标：提供可增量更新的本地代码理解能力。

### ARC-800 抽取 `code-intelligence`

- 服务 API 与 tool adapter 分离；核心可被 CLI/Desktop/agent tool 共用。
- 索引缓存有 workspace/revision/parser-version identity 和 corruption recovery。
- 大仓库有文件数、字节、解析时间和并发预算。

### ARC-810 Codebase graph

- 首批支持 Rust、TypeScript/JavaScript、Python、Go。
- 建立 symbol、definition、reference、import/export 和 containment 边。
- 先提供 read-only query tool；不自动生成编辑。
- 接 `change-tracker` 增量 reindex，watcher gap 时可 reconcile。

### ARC-820 LSP lifecycle

- server start/restart/backoff、workspace config、document open/change/close replay。
- 支持 push/pull diagnostics 和 stale diagnostic policy。
- server process 使用 background task ownership 与 sandbox profile。
- LSP edit 必须转成 workspace edit/ChangeReceipt，不能直接写磁盘。

### ARC-830 Tool/context 集成

- graph/LSP query 有独立 ToolCapabilities 和 output budget。
- context 注入采用按需查询，不把完整符号图塞入 system prompt。
- graph 与 MCP tool search 共用结果排序接口，但不共享存储实现。

Phase 8 Gate：索引可从 cache 恢复并增量更新；LSP crash 可重启并恢复 document state；所有 edit 可进入 review。

---

## Phase 9：CLI、Desktop 与 TUI 适配器收敛

目标：产品内核稳定后清理两个 adapter 的结构热点。

### ARC-900 CLI Elm 化

- 将 interactive loop 改成 `Action -> reduce(State, Action) -> Transition{changes,effects}`。
- async effect 完成后只通过 `TaskResult Action` 回灌。
- command/key/action registry 单一事实来源，同时驱动快捷键、命令面板和帮助。
- 拆分 `interactive/root.rs`、`render.rs`、`loop.rs`，production 文件全部低于 900 行。
- RPC、headless 和 interactive 共用 product client，不共用 presentation state。

### ARC-910 Desktop reducer 收敛

- 保留现有 reducer/effect 架构，拆分超大 reducer/pane 为 domain reducer、effect executor 和 view model。
- Desktop 不解析 raw diff，不自行推导 operation terminal；全部消费 product projection。
- 统一 session/review/task/MCP/LSP inspector 状态机。
- 保持 deterministic replay 和视觉回归 fixture。

### ARC-920 TUI 渲染升级

- Markdown 引入 stable checkpoint + tail rerender，避免每个 stream chunk 全量解析。
- scrollback 使用稳定 row identity、prefix height index 和 viewport window paint。
- 只有 benchmark 证明必要时才引入完整虚拟化抽象。
- Diff review、background task、MCP/LSP 状态提供 feature-complete views。

### ARC-930 Scripted scenarios

- 引入 mock inference SSE server、PTY terminal emulator 和 YAML/JSON scenario runner。
- 场景覆盖 prompt/tool/auth/review/rewind/team/background/MCP/reconnect。
- Desktop 使用同一 product scenario 输入生成 deterministic replay，不强行共享 UI renderer。

Phase 9 Gate：CLI/Desktop 无超限 production 文件；核心 workflow 有 scripted e2e；两个 adapter 对同一 ProductEvent fixture 得到语义一致的终态。

---

## Phase 10：发布、可观测性与最终债务清算

目标：完成架构闭环，删除所有迁移遗留。

### ARC-1000 `coding-agent` 最终瘦身

- 审计每个模块所有权，将残留 FS/process/tool/journal/index 实现迁入对应 crate。
- `coding-agent` 只保留 product domain、application、composition 和 `api` facade。
- 根据实际依赖决定是否把稳定 ProductEvent/operation DTO 抽成 `coding-agent-protocol`；没有第二个独立客户端需求则不拆。
- 更新 crate graph 和 API 文档，删除过渡 AST 分类。

### ARC-1010 可观测性

- 结构化 tracing 覆盖 operation/session/tool/worktree/task/extension/index。
- 所有外发数据先经过 secrets scrubber 和大小预算。
- telemetry 默认关闭；开启时记录 schema version 和 consent。
- crash report 不包含 prompt、文件内容、API key 或未脱敏路径。

### ARC-1020 Updater/发布

- 先确定 CLI/Desktop 安装渠道、签名和 rollback 策略，再决定是否适配 Grok updater。
- updater 必须校验签名/哈希、支持 staged download、原子切换和失败回滚。
- 未确定发布渠道前不引入 updater crate。

### ARC-1030 最终清算

- 删除所有 `TODO(ARC-*)`、legacy alias、dual-write、migration feature 和旧 fixture。
- `cargo tree -d` 审计重复大依赖，清理不再使用的 notify/process/schema 库。
- 审计第三方 notice、provenance 和本地修改声明。
- 全量更新 `docs/architecture.md`、crate README、用户迁移说明和开发者 Gate 文档。
- 删除两份学习文档中已失效的行动建议，或明确标记为历史输入。

Phase 10 Final Gate：全 workspace Gate、跨平台 check、PTY scenarios、Desktop replay、license audit、数据 migration 和执行债务清零全部通过。

---

## 八、阶段依赖与建议提交粒度

| Phase | 必须依赖 | 可并行事项 | 禁止并行事项 |
| --- | --- | --- | --- |
| 0 | 无 | 基线、fixture、provenance | 任何行为改动 |
| 1 | 0 | ai/tool/journal 纯抽取可分支进行 | 同时迁移工具行为 |
| 2 | 1 | 只读工具可逐个迁移 | worktree integration、旧新 runtime 长期双轨 |
| 3 | 2 | registry/GC 与 builder 可并行 | 提高 team 并发上限后补隔离 |
| 4 | 3 | fs stream 与 review DTO 可并行 | 在来源归因完成前开放 accept/reject |
| 5 | 4 | token/compaction policy 可先做 | actor 与旧共享锁长期 dual-write |
| 6 | 3、5 | web fetch/provider resilience 可并行 | sandbox 未就绪就开放不受控 hooks |
| 7 | 2、6 | hooks 与 MCP transport 可并行 | 两套 trust/auth/tool registry |
| 8 | 4、6 | graph 与 LSP lifecycle 可并行 | LSP 直接写文件 |
| 9 | 4～8 API 稳定 | CLI 与 Desktop 可并行 | 内核 API 持续剧烈变化时重写 UI |
| 10 | 全部 | docs/license/dep audit | 保留迁移债务进入发布版本 |

建议每个 `ARC-*` 任务独立 commit；一个 Phase 可有多个 commit，但 Phase Gate 使用单独 gate commit 固定证据。

---

## 九、个人项目执行节奏

规模标记以专注开发时间估算：S 为 1～3 天，M 为 3～7 天，L 为 1～3 周，XL 为 3 周以上。
它只用于排序和控制在制品，不作为发布日期承诺。

| 里程碑 | Phase | 规模 | 可交付结果 | 推荐停靠点 |
| --- | --- | --- | --- | --- |
| M0 可重构基线 | 0 | M | 可重复 Gate、fixture、依赖/许可约束 | 不建议长期停留 |
| M1 工具内核 | 1～2 | XL | typed tool runtime、统一 edit engine、清除旧 AgentTool | 可以发布内部版本 |
| M2 安全多 Agent | 3～4 | XL | worktree 隔离、merge proposal、可信 hunk review、rewind | 第一个重大产品版本 |
| M3 Actor 与安全执行 | 5～6 | XL | 单写入者 actor、prompt queue、background、sandbox、safe fetch | 第二个重大产品版本 |
| M4 扩展平台 | 7～8 | XL | hooks、MCP、graph、LSP | 可按实际需求拆成两个版本 |
| M5 Adapter/发布收敛 | 9～10 | XL | CLI/Desktop 状态机、scenario e2e、债务清零 | 完整重构版本 |

个人项目建议严格限制同时进行的主任务数为 1：一个 `ARC-*` 未通过自身 Gate 前，不启动另一个会修改同一
crate 的任务。允许并行的只有测试 fixture、文档、独立平台实现和不共享文件的 leaf crate 移植。

关键路径是 `0 -> 1 -> 2 -> 3 -> 4 -> 5 -> 6`。Phase 7～8 是扩展能力，若产品短期不需要 MCP/LSP，
可以在 Phase 6 后先执行 Phase 9 的 adapter 收敛；但最终完成定义仍要求回补 Phase 7～8，或在计划中明确删掉
这些产品目标，不能无限期标为“以后做”。

---

## 十、关键风险与控制

| 风险 | 可能后果 | 控制措施 |
| --- | --- | --- |
| crate 过度拆分 | 编译变慢、类型搬运、抽象空壳 | 只拆稳定所有权和多消费者边界；禁止 generic common crate |
| tool runtime 双轨过久 | 行为漂移、授权绕过 | Phase 2 内逐工具迁移并删除旧路径 |
| worktree 合并错误 | 用户代码丢失 | 父 workspace merge 前不变；base revision 校验；备份与 conflict-first |
| watcher 丢事件/事件风暴 | diff 错误或资源耗尽 | debounce、budget、gap 标记、periodic reconcile |
| actor mailbox 饱和 | UI 卡死或命令丢失 | bounded queue、结构化 busy、terminal 独立可靠通道 |
| sandbox 平台差异 | 某平台静默失去保护 | capability report、fail-closed、跨平台 cfg check |
| MCP/hook 绕过授权 | 任意执行或数据泄漏 | 统一 trust、tool runtime、workspace capability、sandbox |
| 第三方代码许可遗漏 | 发布风险 | provenance + notice Gate，保留 upstream tests/source rev |
| session migration 失败 | 历史会话不可用 | versioned migration、备份、幂等、fixture、拒绝未知新版本 |
| UI 重写与内核同时变化 | 长期不可运行 | Phase 9 后置，内核先固定 product fixtures |

---

## 十一、明确不做或延后

- 不复制 Grok 的 crate 数量、完整 workspace composition、pager 或 shell runtime。
- 不在首版 worktree 中实现 overlay/Btrfs snapshot。
- 不采用 process-wide sandbox 限制 Desktop 主进程。
- 不为假想兼容性维护多个 tool behavior preset。
- 不在没有真实第二个协议消费者前拆 `coding-agent-protocol`。
- 不优先引入 memory/vector database；先完成 graph、LSP、review 和可靠 session。
- 不在发布渠道未确定前实现 updater。
- 不为了复用而共享 CLI/Desktop presentation state；只共享 product contract/projection。
- 不把 telemetry 当作核心重构完成条件，但最终架构必须预留脱敏后的观测 seam。

---

## 十二、最终成功标准

完成本计划后，Evo 应满足以下可验证条件：

1. 多个 Agent 可并行工作，默认各自拥有独立 worktree，父 workspace 只经显式 merge 改变。
2. 每次文件修改都有稳定 revision、origin、diff 和 hunk identity，支持安全 accept/reject/rewind。
3. 工具全部采用 typed contract/runtime，工具层不依赖 AI conversation 类型。
4. `AgentState` 不再由共享 `RwLock` 管理，每 session 只有一个状态写入者。
5. shell/background/hook/MCP/LSP 子进程均有 ownership、cancel、output budget 和 sandbox profile。
6. MCP、hooks、codebase graph、LSP 都是基础平台 adapter，不进入 agent loop 或 UI 内核。
7. `coding-agent` 明显收缩并只承担产品 domain/application/facade；Cargo 边界替代大部分目录级约束。
8. CLI/Desktop 只消费 `coding_agent::api`，并各自拥有纯 reducer + effect executor。
9. session/config migration、crash recovery、worktree GC 和 watcher reconcile 有自动化故障测试。
10. 全仓不存在旧实现、dual-write、迁移 feature、未完成执行债务或缺失第三方 notice。

这十条全部成立后，才视为“完整重构完成”；仅新增 Grok 模块、仅拆 crate 或仅改善 UI 都不构成完成。
