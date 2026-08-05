# Phase 2 Typed Shell 完成记录

日期：2026-08-05

范围：ARC-230 / `bash` typed runtime、bounded progress/terminal contract 与 process teardown。

## 本阶段已完成

- `bash` 使用 `TypedTool<BashArgs>`，schema 拒绝未知字段；command 有 1 MiB 输入预算、NUL 检查，timeout 必须为 `(0, 600]` 秒，默认 120 秒。
- definition 固定为 `ToolId("bash")`、`SideEffect`、`Sequential`、`cancel=true`、`timeout=true`、`streaming=true`、`provider_executed=false`。
- adapter 只负责 args、`ToolProgress`、`ToolOutput/ToolError` 映射；进程启动、shell path、environment allowlist、process group / Windows job object、stdout/stderr drain 和 descendant teardown 继续由 Evo `platform::process` 拥有。
- progress 与 terminal 都来自同一个 `OutputCollector` tail snapshot，统一受 2000 行/50 KiB policy；progress 明确标记 `stream=merged,cumulative=true`。
- terminal details 固定输出 status、exitCode、stdoutBytes、stderrBytes；非零退出映射 `Execution`，内部命令超时映射 `Timeout`，取消映射 `Cancelled`，spawn/pipe failure 映射 `Execution/Unavailable`。
- `tool-runtime` 为每次执行派生 cancellation token；deadline 到达后先取消 child token，再有界等待 typed tool 清理。agent turn 外层同样只对声明 `cancel=true` 的 runtime tool 等待 teardown，避免提前 drop process future 使 descendant 逃逸。
- tool progress sink 在所有成功、错误、参数失败、取消和 timeout terminal 路径关闭；terminal 返回后继续 emit 会得到 `Protocol` error。
- `RuntimeSnapshot` 通过 `ToolId("bash")` inventory 注册 typed tool，`RuntimeService` 根据 operation 的 `ShellCapability` 构造 runtime tool；legacy closure、marker 和 capability rebinding facade 已物理删除。
- `agent-core` 将 tool execution control 从 oversized `nodes.rs` 拆到 `turn/tool_execution.rs`；`nodes.rs` 从 1217 行降到 1043 行。

## 验证

已通过：

```text
cargo test -p tool-runtime --all-features
9 passed, 0 failed

cargo test -p agent-core --all-features
59 passed, 1 ignored release baseline

cargo test -p coding-agent --lib --all-features
162 passed, 0 failed
```

覆盖行为包括 typed schema/capabilities、成功/空输出/非零退出、内部 timeout、runtime deadline、operation cancellation、协作 teardown、process descendant teardown、16 MiB noisy output bounded/throttled、progress sink terminal closure 和 environment secret deny-by-default。

## 后续范围

- ARC-240 已将 profile/allowlist/authorization/delegation capability inventory 改为 `ToolId`/`ToolDefinition`。
- ARC-250 已将 custom/delegation tools 迁入 typed runtime，并删除 agent-core `AgentTool` 与 Legacy dispatch。
- OS-level filesystem/network confinement、background task ownership 与长任务查询属于 Phase 6，不在 typed adapter 内复制实现。
