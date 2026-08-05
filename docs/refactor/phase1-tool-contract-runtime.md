# Phase 1 Tool Contract / Runtime 完成记录

日期：2026-08-05

范围：ARC-120、ARC-130，以及 `AgentTool` 对新 contract ownership 的最小接入。

## 已完成

- 新增 `tool-contract`：`ToolId`、`ToolKind`、`ToolExecutionMode`、`ToolCapabilities`、`ToolBehaviorVersion`、`AuthorizationRisk`、requirements、typed schema、output/progress/error vocabulary。
- schema 由 `schemars` 固定配置生成，inline nested schemas，并执行 32 KiB、depth、node、property、enum、branch、description 和 range 预算校验；未知 keyword fail closed。
- `ToolId` 和 `ToolBehaviorVersion` 的 serde 入口复用不变量校验，不能反序列化非法 ID 或零版本。
- 新增 `tool-runtime`：object-safe dynamic tool、typed adapter、typed context extensions、registry、requirements validation、stable listing、per-tool sequential gate、cancel、absolute deadline 和 progress terminal guard。
- typed adapter 拒绝 Rust 参数类型与声明 schema 漂移。
- cancel/deadline 同时覆盖等待 sequential gate 和实际执行；unknown tool、invalid arguments、cancel、timeout、success/error 均关闭 progress sink。
- `ToolExecutionMode` 的 ownership 从 `agent-core` 迁至 `tool-contract`，旧公开路径已删除。
- `AgentTool` authorization risk 从 `x-evo-authorization-risk` schema magic key 迁至显式 `AuthorizationRisk` metadata；仓库内旧 key 已清零。

## 依赖边界

新增 production edges：

```text
agent-core -> tool-contract
coding-agent -> tool-contract
tool-runtime -> tool-contract
```

`tool-contract` 不依赖 `ai-protocol`，`tool-runtime` 不依赖 `agent-core`、`coding-agent` 或产品授权 UI。

## 验证

```text
cargo check --workspace --all-targets --all-features
cargo test -p tool-contract
cargo test -p tool-runtime
cargo test -p agent-core --all-features
cargo test -p coding-agent --lib
scripts/release-api-snapshots.sh
scripts/architecture-gate.sh
```

最近结果：`agent-core` 54 passed / 1 ignored，`coding-agent` 129 passed，Architecture Gate 为 569 Rust files、11 production dependency edges、33 grandfathered oversized debts、0 execution debts。

## 后续边界

旧 `AgentTool = closure + ai_protocol::ContentBlock` 仍只作为 Phase 2 的待替换模型保留。本阶段没有引入 adapter、dual-write 或旧路径 re-export；Phase 2 将通过唯一 AI/tool adapter 迁移内建工具并删除旧模型。
