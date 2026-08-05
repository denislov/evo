# Phase 2 AI/Tool Adapter 完成记录

日期：2026-08-05

范围：ARC-200。

## 已完成

- 在 `agent-core/src/agent/tool_adapter.rs` 建立唯一 AI/tool 转换边界，集中负责：
  - `ToolContent -> ai_protocol::ContentBlock`
  - `ToolOutput -> AgentToolResult`
  - `ToolProgress -> AgentToolOutput`
  - `ToolError -> AgentToolResult`
  - local executable/provider declaration -> `ai_protocol::Tool`
- `ToolContent::Json` 固定映射为 deterministic compact JSON text；text、image、details 和 `terminate` 原样保留。
- `ToolErrorKind` 固定进入 `details.tool_error.kind`，contract details 保留在 `details.tool_error.details`；用户可见错误文本保持原消息。
- agent turn engine 自身产生的 unknown tool、invalid arguments、cancel、timeout、progress/result budget 错误已改用结构化 `ToolErrorKind`。
- provider-executed `web_search` 已从 `AgentTool` constructor 和本地执行分支中物理删除；本地 `AgentTool { kind: WebSearch, .. }` 注册会 fail closed。
- `AgentState` 单独持有 `Vec<ToolDefinition>` provider declarations；本地 executable tools 与 provider declarations 执行上完全分离，仅在 provider request adapter 汇合。
- `web_search` declaration 使用 `ToolDefinition`、`ToolId`、capabilities、behavior version 和 authorization risk；不再携带永远报错的占位 closure。
- provider declaration 按最终 resolved model、显式 no-tools、profile allowlist 和 operation capability snapshot 动态过滤；profile model override 不再沿用旧模型计算出的 server tool 集合。
- capability admission、delegation runtime seed 和 delegated capability release 继续包含 provider tool name，不因执行表分离而静默丢失。

## 固定映射

```text
ToolContent::Text  -> ContentBlock::Text
ToolContent::Image -> ContentBlock::Image
ToolContent::Json  -> compact deterministic JSON text

ToolOutput.details   -> AgentToolResult.details
ToolOutput.terminate -> AgentToolResult.terminate
ToolError.kind       -> details.tool_error.kind
ToolError.details    -> details.tool_error.details
ToolProgress         -> ToolCallUpdate payload
```

上述规则由四组 golden tests 固定：output、error、progress、provider declaration。

## 边界不变量

- `tool-contract` 继续不依赖 `ai-protocol`。
- AI/tool 转换只存在于 `agent-core` adapter，不进入内建工具和 `tool-runtime`。
- provider-executed declaration 不能注册成本地 executable tool。
- local tool 与 provider declaration 的 ID 不能重复。
- ARC-250 已删除 executable `AgentTool`、`add_tool` 和 Legacy dispatch。当前 output/result 类型只承担 typed `ToolOutput/ToolError/ToolProgress` 到 conversation/event 的唯一映射，不再是双执行路径或 compatibility facade；后续可在不改变职责的前提下重命名。
- `turn/nodes.rs` 为 1185 行，未扩大 1199 行的 oversized debt 基线。

## 验证

```text
cargo check --workspace --all-targets --all-features
cargo test -p agent-core --all-features
cargo test -p coding-agent --lib
scripts/release-api-snapshots.sh
scripts/architecture-gate.sh
scripts/core-perf-gate.sh
```

最近结果：

```text
agent-core: 57 passed, 1 ignored release baseline
coding-agent: 130 passed
release API snapshots: passed
architecture: 577 Rust files, 12 production edges, 33 oversized debts, 0 execution debts
agent first text delta: 66 us
100k session hydration release test body: 0.21 s
noisy process output bounded/throttled test: passed
```

## 下一步

进入 ARC-210，按 `read -> ls -> find -> grep` 顺序迁移到 `tool-runtime` typed registry。第一步先迁移 `read`，同时补充 revision/fingerprint 输出，作为 ARC-220 mutation fence 的前置契约。
