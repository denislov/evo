# Evo 架构

> 版本：0.7.2
> 最后核对：2026-08-08
> 适用范围：Rust workspace、CLI、Desktop、TUI、持久化、扩展、发布与第三方来源

本文是当前架构的权威说明。历史决策和分阶段证据位于
[`docs/Evo完整架构重构计划.md`](Evo完整架构重构计划.md) 与
[`docs/refactor/`](refactor/)；两份 Grok 学习文档仅作为历史输入，不再代表待执行路线。

## 1. 总体原则

- `coding-agent` 是唯一产品 facade；CLI/Desktop 只访问 `coding_agent::api`。
- FS、process、sandbox、worktree、journal、tool、index 等 authority 各有唯一 crate，不在上层复制。
- 所有队列、输出、扫描、索引、事件重放和外发数据都有显式预算与结构化失败。
- 不保留旧实现、dual-write、migration feature、legacy alias 或长期 fallback。
- 持久化格式显式版本化；支持的旧版本通过一次性、幂等 migration 收敛，未知新版本 fail closed。
- Telemetry 默认关闭；日志、crash report 和 telemetry 不包含 prompt、文件内容、secret 或未脱敏路径。
- 发布更新先 staged download 和 SHA-256 校验，再执行切换；失败保留当前版本。

## 2. Workspace 分层

箭头表示“依赖”。

```text
foundation
  ai-protocol
  event-journal
  observability
  release-updater
  tool-contract
  tui
  workspace-runtime

runtime
  ai                -> ai-protocol + observability
  tool-runtime      -> tool-contract
  agent-core        -> ai-protocol + tool-contract + tool-runtime
  change-tracker    -> workspace-runtime
  extension-host    -> observability + tool-contract + tool-runtime + workspace-runtime
  code-intelligence -> change-tracker + tool-contract + tool-runtime + workspace-runtime

product
  coding-agent      -> agent-core + ai + ai-protocol + change-tracker
                       + code-intelligence + event-journal + extension-host
                       + observability + tool-contract + tool-runtime
                       + workspace-runtime

adapters
  cli               -> coding-agent + observability + release-updater + tui
  desktop           -> coding-agent + observability + release-updater
  scenario-testing  -> coding-agent + tui
```

第一方依赖边由
[`scripts/architecture/internal-dependencies.tsv`](../scripts/architecture/internal-dependencies.tsv)
固定；Architecture Gate 对照 Cargo metadata 并拒绝循环或未审阅的新边。

## 3. Crate 所有权

| Crate | 唯一职责 | 公共边界 |
| --- | --- | --- |
| `ai-protocol` | provider-neutral 模型、conversation、stream、auth/compat DTO | `ai_protocol::api` |
| `ai` | provider registry、HTTP/SSE、认证、resilience、SSRF-safe fetch | `ai::api` |
| `tool-contract` | ToolId、schema、capability、risk、output/error、ranking | `tool_contract::api` |
| `tool-runtime` | typed registry、validation、context、cancel/deadline/progress、dispatch | `tool_runtime::api` |
| `agent-core` | bounded Agent actor、turn state machine、queue、compaction、hooks | `agent_core::api` |
| `workspace-runtime` | FS capability、mutation fence、process、sandbox、task、worktree/merge/GC | `workspace_runtime::api` |
| `event-journal` | frame codec、write lease、tail repair、checkpoint、bounded tail read | `event_journal::api` |
| `change-tracker` | semantic FS event、hunk identity、attribution、review/reconcile | crate root re-export |
| `extension-host` | hook/MCP discovery、trust、budget、gate、transport、OAuth、lifecycle | `extension_host::api` |
| `code-intelligence` | tree-sitter graph、incremental index、LSP、bounded query tools/context | `code_intelligence::api` |
| `observability` | scrub、outbound budget、telemetry consent/schema、crash report | crate root |
| `release-updater` | GitHub Release query、asset contract、download、hash、stage/install | crate root |
| `coding-agent` | product domain、application orchestration、composition、public facade | `coding_agent::api` |
| `tui` | terminal/input/editor/layout/render/Markdown/VirtualTerminal primitive | `tui::api` |
| `cli` | CLI/TUI/print/JSON/RPC adapter、update 命令 | binary |
| `desktop` | GPUI application/reducer/runtime/UI/replay/update confirmation | `DesktopApplicationOptions` |
| `scenario-testing` | shared scripted scenario、semantic oracle、mock SSE、terminal replay | test-support API |

每个 crate 的 README 记录更具体的边界和单 crate 验证命令。

## 4. 核心运行流

### 4.1 Product operation

```text
adapter command
  -> coding-agent admission
  -> operation owner
  -> Agent / tool / workspace / extension / index authority
  -> ordered ProductEvent
  -> durable event service（需要持久化的事件）
  -> CodingAgentClientProjection
  -> CLI/Desktop render
```

产品事件是 adapter 的 source of truth。Desktop/CLI 不解析 raw diff、不读取 repository
内部状态，也不自行推导 operation terminal。

### 4.2 Agent turn

```text
Start
  -> DrainQueuedInput
  -> CompactRuntimeContext
  -> PrepareProviderRequest
  -> ApplyProviderHook
  -> ProviderStream
  -> DecideAfterAssistant
  -> ExecuteTools
  -> PrepareNextTurn
  -> Continue | Done | Error | Aborted
```

Agent actor 是状态唯一写入者。Steer、follow-up、interject、edit/remove queue entry
均经 bounded command；provider/tool hook 和 cancellation 在状态边界观察。

### 4.3 Tool execution

```text
ToolDefinition
  -> product authorization / hook gate
  -> tool-runtime validation
  -> ToolCallContext
  -> builtin or MCP DynamicTool
  -> bounded progress/result
  -> ProductEvent + transcript
```

文件工具只持有 `WorkspaceAccessHandle` 绑定出的目标。Mutation fence 在 write/truncate
和 `sync_all` 完成前不释放；调用 future 被取消不等价于中断已进入 fence 的写入。

### 4.4 Workspace 与 child isolation

每个可写 child 默认创建独立 managed worktree，cwd、filesystem capability 与 shell
capability 均绑定到 child。父 workspace 在显式 merge 前保持不变。Merge 使用
creation-time baseline、ChangeSet、conflict validation、transaction journal 与 startup
recovery；discard、GC 和异常退出恢复都经 registry。

Shell、hook 与 MCP stdio process 在 spawn 边界应用 `SandboxProfile`。能力不足平台按
声明 fail closed，不静默 unrestricted。

### 4.5 Review、rewind 与外部修改

`change-tracker` 将 watcher fact、tool receipt 和 hook attribution 汇入单一 hunk actor。
Review list/open/accept/reject/discard 使用稳定 HunkId、revision 与 target revalidation。
WatchGap 触发 full reconcile。

Rewind 是 session cursor/branch、workspace snapshot 与 hunk checkpoint 的跨域事务。
失败执行 rollback；未完成 recovery 会阻止新的 session write。

## 5. 持久化与 migration

- Event journal：长度受限 JSON frame、单 writer lease、torn-tail repair、committed sequence。
- Session：versioned manifest、event log、outbox/recovery identity、bounded hydration。
- Workspace：versioned registry、owner process identity、merge journal、checkpoint。
- Desktop preferences：显式 `PREFERENCES_SCHEMA_VERSION`，未知字段/版本按契约拒绝。
- Code intelligence：workspace/revision/parser identity 与 cache schema 一起校验。
- Telemetry consent：consent schema 与 event schema 分离版本化。

当前不存在 dual-write 或 migration feature。受支持旧数据的 migration 必须具备备份、
幂等测试和未知新版本拒绝；已删除字段不通过 alias 恢复。用户可见变更见
[`docs/user-migration.md`](user-migration.md)。

## 6. 扩展与代码智能

Hook 与 MCP 共用 extension trust、diagnostic、budget 和 lifecycle，但 gate 与 observe
通道分离。Tool/Stop gate 的失败矩阵是显式契约；MCP 工具通过 meta tools 发现并进入统一
Tool contract，不将全部 schema 注入模型上下文。

Code intelligence 是只读派生数据。Tree-sitter graph 与 LSP diagnostics 共享 service
lifecycle、identity、budget 和 query API；watcher gap 或 cache mismatch 可重建，不能影响
workspace 的写入 authority。

## 7. 可观测性与发布

业务层只记录 opaque ID、kind/state、计数和 duration。所有外发字段先经过
`SecretsScrubber` 和 `OutboundPolicy`；telemetry 需要显式 consent，默认使用
`NoopTelemetrySink`。Crash report 不收集 prompt、tool arguments、文件内容、command、
provider error 原文或原始路径。

发布源固定为 GitHub Releases。首版资产矩阵为 Linux x86_64 与 Windows x86_64，CLI 和
Desktop 独立打包。安装脚本与 updater 都校验 `checksums.txt`；CLI 自动检查只提示，
`coding-agent update` 和 Desktop 确认弹窗才允许安装。

## 8. 第三方与本地修改

- `third-party/gpui-component`：构建时由脚本按固定 revision 重建，并应用
  `patches/gpui-component/`；来源和本地修改见 provenance。
- `third-party/grok-build`：仅是架构学习和一次性适配来源，不作为 workspace 依赖。
- crates.io/git transitive dependencies：由 Cargo metadata license audit 检查。
- 发布 notice 与例外说明位于根目录 `THIRD_PARTY_NOTICES.md`。

新增复制、翻译或语义改写必须先更新 `docs/refactor/provenance/`，记录上游 revision、
源/目标路径、license/notice、测试、本地修改和同步策略。

## 9. 开发 Gate

权威命令与平台矩阵见
[`docs/development-gates.md`](development-gates.md)。最小提交前验证：

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
scripts/release-api-snapshots.sh
scripts/architecture-gate.sh --final
scripts/license-audit.sh
git diff --check
```

`--final` 要求 oversized debt 与 execution debt 都为空。跨平台、PTY、Desktop replay 和
release workflow 在对应 CI/job 中执行；任何无法在本机运行的 Gate 必须明确报告，不得以
“已有问题”长期跳过。
