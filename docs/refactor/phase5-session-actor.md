# Phase 5 / ARC-530：Session actor 边界

> 状态：已完成
> 前序：ARC-500（Agent actor 化）、ARC-510（prompt queue）、ARC-520（compaction）
> 目标：确保 session 层面有固定的 shutdown 顺序、Agent actor 与 session shutdown 协调、高频 stream update 与 durable event 分离

## 当前架构

### Shutdown 序列（`runtime/facade/connection.rs:112`）

`shutdown_internal` 已有固定顺序：
1. `request_shutdown()`：设置 `RuntimeLifecycle::ShuttingDown`，拒绝新操作（stop admission）
2. `cancel_all()`：取消 tool authorization
3. `cancel_open_operations_for_shutdown()`：取消正在运行的操作
4. `wait_for_active_operation_to_drain()`：等待操作完成（join operation）
5. `shutdown_writer()`：关闭磁盘 writer（drain writer）
6. `emit_runtime_shutdown()`：发出 shutdown 事件（commit terminal）
7. `finish_shutdown()`：设置 `RuntimeLifecycle::ShutDown`，detach 客户端（close actor）

### ARC-530 要求的顺序

> stop admission -> cancel/join operation -> commit terminal -> drain writer -> close actor

当前顺序与要求的差异：
- 当前：stop admission -> cancel operation -> join operation -> drain writer -> commit terminal -> close
- 要求：stop admission -> cancel/join operation -> commit terminal -> drain writer -> close

**需要调整**：`emit_runtime_shutdown`（commit terminal）应在 `shutdown_writer`（drain writer）之前，确保终止事件先入 writer 队列再 drain。

### Agent actor 生命周期

- Agent 在 operation task 内创建（`agent_invocation::run` / `prompt/runner.rs`）
- session shutdown 时 operation 被 cancel/join，Agent 随 task drop
- Agent drop 时 `AgentHandle`（mpsc::Sender）被 drop，actor 在 `commands.recv()` 返回 None 时退出
- ARC-500 引入了显式 `Agent::shutdown()`，但 session shutdown 时未被显式调用

### 高频 stream update vs durable event

- **高频 stream update**：Agent event stream（有界 mpsc，`EVENT_STREAM_CAPACITY`），可丢弃
- **durable terminal/event**：SessionTransactionWriter（独立 worker 线程），不可丢失
- 两者已分离 ✅

### SnapshotCoordinator

使用 `std::sync::Mutex<SnapshotState>` 串行化 admission、publication 等。功能上等价于 actor 的串行化，但不是 actor 模型。

## 目标改动

### 1. 调整 shutdown 顺序

将 `emit_runtime_shutdown`（commit terminal）移到 `shutdown_writer`（drain writer）之前：

```rust
pub(crate) async fn shutdown_internal(&mut self) -> Result<CodingAgentShutdownOutcome, CodingSessionError> {
    // 1. stop admission
    if self.runtime_host.client_projection.snapshots.request_shutdown()? == RuntimeLifecycle::ShutDown {
        return Ok(CodingAgentShutdownOutcome::AlreadyShutDown);
    }
    // 2. cancel tool authorization
    self.runtime_host.authorization_service.cancel_all("tool authorization cancelled by runtime shutdown")?;
    // 3. cancel operations
    self.runtime_host.operation_supervisor.control.cancel_open_operations_for_shutdown()?;
    // 4. join operations (wait for drain)
    self.runtime_host.client_projection.snapshots.wait_for_active_operation_to_drain().await?;
    // 5. commit terminal event (into writer queue)
    self.runtime_host.events.emit_runtime_shutdown()?;
    // 6. drain writer (flush all pending writes including terminal)
    self.runtime_host.session_coordinator.shutdown_writer()?;
    // 7. close actor
    self.runtime_host.client_projection.snapshots.finish_shutdown()?;
    Ok(CodingAgentShutdownOutcome::ShutDown)
}
```

### 2. Agent actor 显式 shutdown

在 operation task 的 cleanup 中，显式调用 `Agent::shutdown()`。当前 Agent 在 task 内创建，task 被 cancel 后 Agent drop。

需要在 `agent_invocation::run` 和 `prompt/runner.rs` 中，在 task 结束前（正常或 cancel）调用 `agent.shutdown()`。

或者更简单的方式：在 `CodingAgentOperationTask` 的 `join_internal` 中，在 join 之前发送 shutdown 信号。

但实际上，Agent 是在 task 内部的局部变量，外部无法访问。最简单的方式是在 Agent 的 Drop impl 中调用 shutdown（发送 Shutdown command）。但 Agent 只持有 AgentHandle（Clone 的 mpsc::Sender），Drop 时只是 drop sender。

当前 Agent drop 时 sender drop -> actor 的 `commands.recv()` 返回 None -> actor 退出（commit any in-flight turn）。这已经是合理的 shutdown 行为。

**决策**：不改变 Agent 的 drop 行为。Actor 通过 sender drop 自然退出，且会 commit in-flight turn。显式 `shutdown()` 命令只是加速这个过程（发送 Shutdown command 而非等 sender drop）。

> 更正（2026-08-06 复验）：显式 `shutdown()` 与 sender drop **并不完全相同**——复验发现 turn 进行中收到 Shutdown 命令时，`run_actor` 的让出窗口 drain loop 曾直接 `return` 跳过 commit（已修复，见 `phase5-agent-actor.md` 复验记录）。修复后两者一致：均走 graceful shutdown（abort turn + commit）。

### 3. 文档化 session actor 边界

在设计文档中明确：
- **Agent actor**：独占 AgentState，通过 bounded mailbox 串行化所有 Agent command
- **SnapshotCoordinator**：通过 Mutex 串行化 admission、publication（功能等价于 actor）
- **SessionTransactionWriter**：独立 worker 线程，串行化磁盘写入
- **高频 stream update**：Agent event stream（有界 mpsc，可丢弃）
- **durable event**：SessionTransactionWriter（不可丢失）

### 4. shutdown 可靠性验证

增加测试验证：
- shutdown 后新 command 返回结构化错误
- shutdown 无 task 泄漏（operation task 被 join）
- shutdown 无 writer 泄漏（writer 被 drain）
- shutdown 顺序正确（terminal event 在 writer drain 之前入队）

## 关键决策

1. **不重写 SnapshotCoordinator 为 actor**：当前 Mutex 模型在功能上正确，重写风险大、收益低。Mutex 串行化等价于 actor 串行化。

2. **Agent actor shutdown 通过 sender drop 隐式完成**：显式 `shutdown()` 与 sender drop 行为等价（2026-08-06 修复 `phase5-agent-actor.md` 中记录的不一致后成立），不需要额外集成。

3. **shutdown 顺序调整**：commit terminal 在 drain writer 之前，确保终止事件被持久化。

4. **高频 update 和 durable event 已分离**：不需要额外改动。

## 分步实现

### 步骤 1：调整 shutdown 顺序
- `shutdown_internal` 中交换 `emit_runtime_shutdown` 和 `shutdown_writer` 的顺序
- 确保测试通过

### 步骤 2：增加 shutdown 可靠性测试
- shutdown 后 `ensure_runtime_running` 返回错误
- shutdown 后 `enqueue_control` 返回 rejection
- shutdown 顺序测试（terminal event 在 writer drain 之前）

### 步骤 3：文档化
- 更新设计文档

## 验证

```text
cargo test --locked -p coding-agent --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
bash scripts/architecture-gate.sh
```

## 实现记录

### shutdown 顺序调整

`shutdown_internal`（`runtime/facade/connection.rs`）中 `emit_runtime_shutdown` 已移至 `shutdown_writer` 之前：

1. `request_shutdown`（stop admission）
2. `cancel_all`（cancel tool authorization）
3. `cancel_open_operations_for_shutdown`（cancel operation）
4. `wait_for_active_operation_to_drain`（join operation）
5. `emit_runtime_shutdown`（commit terminal）
6. `shutdown_writer`（drain writer）
7. `finish_shutdown`（close actor）

`emit_runtime_shutdown` 经 `publish_without_root_terminal` → `publish` 仅更新内存 snapshot state 并向 product event broadcast channel 发送事件（`RuntimeEvent::ShutDown` 的 durability 为 `LiveOnly`，不进入 writer 队列），**不依赖 writer**。交换顺序后终止事件先于 writer drain 提交，且不影响 writer 行为。

### shutdown 可靠性测试

在 `application/operation/dispatch_tests.rs` 新增三个测试：

- `shutdown_rejects_new_operations_after_completion`：shutdown 后 `run_internal` 经 `ensure_runtime_running` 返回 `Lifecycle { reason: RuntimeShutDown }`。
- `shutdown_rejects_control_commands_after_completion`：shutdown 后 `enqueue_control`（经 `prompt_control().steer()`）返回 `CodingAgentControlRejection { reason: RuntimeShutDown }`。
- `shutdown_emits_terminal_event_before_draining_writer`：订阅 product event 后 shutdown，订阅者收到 `Runtime::ShutDown` 事件，证明 `emit_runtime_shutdown` 在 `shutdown_writer` / `finish_shutdown` 之前执行。

### Agent actor 与 stream/event 边界

按设计文档决策，不改变 Agent drop 行为（sender drop → actor 退出并 commit in-flight turn），不重写 `SnapshotCoordinator` 为 actor。高频 stream update（有界 mpsc，可丢弃）与 durable event（`SessionTransactionWriter` 独立 worker，不可丢失）已分离，无需额外改动。
