# Phase 2 ToolId Inventory 收敛记录

日期：2026-08-05

范围：ARC-240 / ARC-250，完整 ToolId inventory 与 legacy executable 收敛。

## 已完成

- `AgentProfile.tools` 与 profile TOML DTO 已改为 `Vec<ToolId>`；无效 tool id 在反序列化边界直接拒绝。
- `ToolCapabilitySet` 使用 `BTreeSet<ToolId>`，能力快照、operation admission、delegation capability release 和 filesystem/shell capability 推导不再用裸字符串授权。
- `RuntimeSnapshot` 使用 `typed_tool_ids` 与 `Vec<ToolId>` profile allowlist；provider declaration 直接按 `ToolDefinition.id` 过滤。
- authorization inventory 使用 `BTreeMap<ToolId, Risk>`；custom tool name 无法转换成 `ToolId` 时 fail closed，不再静默丢失风险元数据。
- product、builtin runtime、server-side 与 delegation inventory 均在内部生成 `ToolId`；字符串只留在 CLI 输入、公开 catalog、持久化 seed 和 provider/tool-call 协议边界。
- builtin 工具唯一由 `register_builtins -> builtin_runtime_tool_ids -> ToolRegistry` 注册。启动和 delegation restore 不再构造 `Vec<AgentTool>` marker。
- 已物理删除 filesystem/bash 的 legacy `AgentTool` wrapper、inventory marker、`builtin_tools`、`bind_builtin_tool_to_capabilities`、`typed_tool_names` 和字符串 product inventory。
- provider-side `web_search` 继续只作为 `ToolDefinition` 发送给 provider，不进入本地 executable registry。
- custom injected tools 已从 `Vec<AgentTool>` 改为 `Vec<Arc<dyn DynamicTool>>`，与 builtin tool 进入同一个 `ToolRegistry`，统一执行 ID 冲突、requirements、capability 与 authorization 校验。
- `delegate_agent` / `delegate_team` 已通过 `FunctionTool` 注册到 typed runtime；执行上下文直接使用 `ToolCallContext`，取消令牌和 operation/call identity 不再经过 legacy closure adapter。
- `agent-core` 已物理删除 `AgentTool`、`Agent::add_tool`、`ExecutableTool::Legacy`、local provider declaration conversion 和重复的 schema/argument validator。
- `agent-core/src/agent/types/tool.rs` 从 982 行收缩到 150 行，过期 oversized debt 已删除；保留的 output/result 类型只承担 typed runtime 到 conversation/event 的结果映射，不持有 executable。
- delegation declaration adapter 已拆到独立 `operations/delegation/tool.rs`，主模块保持在 900 行 production 上限内。

## 验证

```text
cargo test -p coding-agent --lib --all-features
162 passed, 0 failed

cargo test -p agent-core --all-features
48 passed, 1 ignored release baseline

cargo check --workspace --all-targets --all-features
passed

scripts/release-api-snapshots.sh
passed

scripts/architecture-gate.sh
rust_files=585, dependency_edges=14, oversized_debts=32, execution_debts=0

scripts/core-perf-gate.sh
first text delta=99 us; 100k hydration and noisy output gates passed
```
