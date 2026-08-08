> 历史输入（2026-08-05）。本文是学习材料的续篇，不是待执行建议；当前所有权、依赖图和 Gate 以 `docs/architecture.md`、`docs/development-gates.md` 为准。

总体判断：Evo 不需要复制 `grok-build` 约 70 个 crate 的规模，也不适合整体搬运它的 `shell/workspace/pager` 聚合层。Evo 当前真正需要吸收的是 Grok 在“工具协议、隔离工作区、文件变更归因、扩展机制和进程安全”上的成熟基础设施。

最优先的问题不是 UI，也不是模型接入，而是：**多个 Agent 目前可能在同一个 cwd 中并发写文件，却没有 worktree 隔离和合并协议。** 这会限制 Team 并发能力，也是后续 hunk review、子 Agent 自治和并行开发的基础风险。

**架构对照**

Evo 当前大致是：

```text
cli ─────┐
         ├──> coding-agent facade/application/runtime/domain
desktop ─┘                    │
                              v
                         agent-core ──> ai
```

Grok 更接近：

```text
pager-bin
  ├── pager / TUI
  └── shell / headless / ACP
          │
          v
      workspace composition
          │
          ├── tool protocol/runtime/implementations
          ├── worktree / fsnotify / hunk tracker
          ├── hooks / MCP / sandbox
          └── journal / auth / circuit breaker
```

Evo 的核心架构并不差：`coding_agent::api` 已经是明确且有测试保护的产品 facade，[README](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/README.md:17)；模块层次有 AST 守卫，[module_layering.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/tests/module_layering.rs:10)；operation admission、durability、cancellation、outbox、recovery、filesystem authorization 和跨平台进程树终止都已比较成熟。因此不建议重写核心 runtime。

当前主要结构问题是：

- 根 package 仍是无业务意义的 Hello World，[main.rs](/home/whai/dev_wkspace/agent-repo/evo/src/main.rs:1)。应删除，或者让它成为真正的 composition root。
- `coding-agent` 已接近 6 万行，产品 API、application、session、events、tools 和 platform 都在一个 crate 中。五层边界主要依赖目录约定和源码扫描，而不是 Rust crate 编译边界。
- 不过不应立刻把五层机械拆成五个 crate。优先抽取已经有多个潜在消费者的稳定边界：tool contract、tool runtime、workspace isolation 和 file-change domain。
- `AgentTool` 直接依赖 `ai::ContentBlock`，[tool.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/agent-core/src/agent/types/tool.rs:1)。这使工具执行、AI conversation 表示和 provider 层耦合，未来接 MCP、远程工具或 plugin 会比较困难。
- CLI 的 `interactive/root.rs`、`render.rs`、`loop.rs`，以及 Desktop 的 reducer/pane 已形成数千行热点。应按 state machine、commands、effects、views 拆分 adapter 内部，而不是继续向核心 facade 泄漏 UI 状态。
- Team 当前最多并发两个成员，[runner.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/operations/team_invocation/runner.rs:29)，而子 Agent 会继承父 operation 的 filesystem/shell capability，[delegation/mod.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/operations/delegation/mod.rs:117)。这意味着授权边界存在，但写入空间没有隔离。
- Changed-file review 的授权和 revision 重验证较好，但 diff、增删行和来源归因仍为空，[context_fold.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/runtime/client/context_fold.rs:170)。它目前更像 tool event 投影，还不是完整 review domain。
- Shell authorization 是用户决策和审计边界，不是 kernel enforcement。获准执行的 shell 仍可能读取 workspace 外文件或访问网络。

**推荐移植分级**

| 模块 | 收益 | 成本/耦合 | 建议方式 | 优先级 |
|---|---:|---:|---|---:|
| `xai-fast-worktree` 子集 | 极高 | 中高 | 适配移植 builder、copy/reflink fallback、registry、GC、dirty sync | P0 |
| `xai-hunk-tracker` + `xai-fsnotify` | 极高 | 中 | 适配其 actor、因果事件流、来源归因和 hunk accept/reject | P0 |
| `xai-tool-types/protocol/runtime` 设计 | 高 | 中 | 不直接替换，抽取 Evo 自己的 `tool-contract`/`tool-runtime` | P0 |
| `xai-grok-hooks` | 高 | 中 | 适配 event table、matcher、discovery、bounded runner、trust | P1 |
| `web_fetch` 安全组件 | 高 | 低中 | 可直接或轻适配移植 SSRF、redirect、budget、cache、HTML 转换 | P1 |
| `xai-grok-sandbox` | 高 | 高 | 只借鉴并重构为 child-process-first sandbox | P1 |
| background task runtime | 高 | 中 | 只移植 task registry、ownership、wait/output protocol | P1 |
| `xai-circuit-breaker` | 中高 | 低 | 基本可直接移植，用于 provider、HTTP 和未来 MCP | P1 |
| `xai-codebase-graph` | 中高 | 中 | 先作为 read-only local tool，再接增量索引 | P2 |
| MCP | 高 | 高 | tool contract 稳定后作为外部 tool provider adapter | P2 |
| LSP runtime | 中高 | 高 | 第二阶段以后，需完整 lifecycle、document、diagnostics UI | P3 |
| updater/auth/extra-CA/journal | 中 | 低中 | 按 Evo 发布、企业网络和 SQLite 使用情况选择性移植 | P2/P3 |

`xai-fast-worktree` 是最值得投入的模块。它覆盖 copy/reflink、git worktree、overlay、Btrfs、dirty-state 同步、取消、registry、orphan cleanup 和 GC，[api.rs](/home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/crates/codegen/xai-fast-worktree/src/api.rs:225)。Evo 首期不需要搬 overlay/Btrfs 的全部复杂度，只需实现：

```text
parent workspace
    │
    ├── child A managed worktree ──> changeset A
    ├── child B managed worktree ──> changeset B
    └── explicit review/merge/conflict resolution
```

每个 child operation 的 cwd、filesystem capability 和 shell capability 都应绑定到独立 worktree。worktree 创建、关闭、异常恢复和 GC 必须进入 durable event/operation lifecycle，不能只做临时目录工具。

`xai-hunk-tracker` 与 `xai-fsnotify` 应紧随其后。前者能区分 Agent edit、外部修改 Agent 文件和纯外部修改，并支持 hunk accept/reject、snapshot 和 session stats，[lib.rs](/home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/crates/codegen/xai-hunk-tracker/src/lib.rs:1)；后者提供 debounce、gitignore、动态目录 watch、VCS operation state 和增量 consumer，[lib.rs](/home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/crates/codegen/xai-fsnotify/src/lib.rs:1)。两者结合后，Evo 的 changed-file snapshot 才能成为可信的 review source of truth。

工具架构建议采用 Grok 的设计思想，但不要照搬类型体系。Grok 的 `Tool` 支持 typed input/output、动态描述、capabilities 和 streaming，[tool.rs](/home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/crates/common/xai-tool-runtime/src/tool.rs:36)；`ToolCallContext` 通过 typed extensions 注入 cwd、session、cancel 和 trace，[context.rs](/home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/crates/common/xai-tool-runtime/src/context.rs:12)。Evo 可以形成：

```text
tool-contract
  ToolId / ToolInput / ToolOutput / ToolError / ToolCapabilities

tool-runtime
  ToolContext / cancellation / authorization / streaming / registry

coding-agent adapter
  ToolOutput <-> ai::ContentBlock
```

这项抽取是 MCP、plugin、远程工具和更完整 LSP 集成的前置工程。现阶段没有必要加入 wire protocol，也不应让 `coding-agent` 直接依赖 Grok 的 MCP/workspace/config 依赖图。

Hooks 值得产品化。Grok 已覆盖 session、prompt、tool、permission、stop、subagent 和 compaction 等事件，[event.rs](/home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/crates/codegen/xai-grok-hooks/src/event.rs:87)。Evo 应复用自己的 product events、authorization 和现有 process primitive；project hooks 必须复用统一 folder trust，不能创建第二套信任数据库。

Sandbox 不能直接复制。Grok 的 Landlock/Seatbelt 是 process-wide file confinement，并对子进程网络使用 seccomp，[lib.rs](/home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/crates/codegen/xai-grok-sandbox/src/lib.rs:8)。Evo 同时承载 Desktop、多 project session 和 projectless workspace，直接 process-wide 限制会产生冲突。正确顺序是先在 shell spawn 边界实现 child-process filesystem/network profile；未来若需要强隔离，再把每个 workspace runtime 放到独立 worker process。

**实施路线**

第一阶段聚焦架构和多 Agent 安全：处理根 Hello World；抽取 tool contract/runtime；实现 managed worktree；让 child capability 绑定独立 cwd；补齐 merge、conflict、recovery 和 GC。验收标准是两个 Agent 能并行修改同名文件，父 workspace 不被直接污染，并能显式审阅和合并。

第二阶段建立可信 review 与扩展层：接入 fsnotify semantic stream、hunk tracker、diff/line stats/source attribution、hunk accept/reject；随后加入 hooks、安全 `web_fetch`、background task registry 和 child-process sandbox。现有 [process.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/platform/process.rs:274) 已有 timeout、bounded output、cancel 和进程树终止，不应被 Grok shell runtime 替换。

第三阶段工具平台化：加入 codebase graph、增量索引、LSP、MCP、circuit breaker、extra CA 和 refresh-aware auth。最后再依据安装渠道决定 updater、crash reporting 和 telemetry，而不是提前绑定 Grok 的发布体系。

明确不建议整体移植 `xai-grok-shell`、`xai-grok-workspace`、`xai-grok-pager`、Markdown/Mermaid renderer，也不建议复制 Grok 的 crate 数量。Evo 已有自己的 session、durability、authorization、process 和 UI 技术栈；移植聚合层会造成大规模重复和依赖倒置。

许可方面，Grok 第一方代码是 Apache-2.0，[README](/home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/README.md:127)。但 `xai-grok-tools` 中部分实现来自 OpenAI Codex 和 OpenCode，实际复制代码时必须保留对应 notice 和修改声明，[THIRD_PARTY_NOTICES.md](/home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md:1)。

本轮是只读架构调研，没有修改文件，也没有运行两个 workspace 的全量构建或测试。Grok 固定的 Rust 1.94 toolchain 当前未安装；已通过 stable toolchain 的 `cargo metadata --no-deps` 验证其 workspace 和依赖结构。
