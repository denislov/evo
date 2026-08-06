# Phase 5 / ARC-510：Prompt queue/interjection

> 状态：完成（2026-08-06）
> 前序：ARC-500（Agent actor 化完成）
> 目标：给 prompt queue entry 增加 id/version metadata，支持 stale-edit conflict，引入 interjection queue kind，确保安全边界注入

## 当前架构

### Queue 类型

- `AgentInputQueue`（`agent-core/src/agent/queue.rs:11`）：只有 `Steering`、`FollowUp`
- queue entry 直接是 `AgentMessage`，无 id/version/owner metadata
- `enqueue_message`：检查 item limit（32）和 byte limit（1MB），push_back
- `drain_queue`：`QueueMode::All` 或 `QueueMode::OneAtATime`

### Queue 消费（安全边界）

1. **`drain_queued_input`**（`nodes.rs:108`，turn 开始时 `DrainQueuedInput` 状态）：
   drain `steering_queue`（按 `steering_mode`），extend 到 messages
2. **`maybe_prepare_next_turn`**（`nodes.rs:517`，turn 结束后）：
   - Stop/Length：drain `follow_up_queue`（按 `follow_up_mode`），Continue
   - ToolUse：直接 Continue（下个 turn 开始时 drain steering）

tool-request/tool-result pairing 不被破坏：steering 只在 turn 开始注入，follow_up 只在 assistant stop（非 ToolUse）后注入。

### Actor command（ARC-500 后）

- `AgentCommand::Steer/SteerContent/FollowUp/FollowUpContent`：fire-and-forget，通过 `try_send`
- `AgentCommand::ClearQueues`：清空 steering + follow_up queue
- `AgentCommand::Abort`：cancel token

### coding-agent 层

- `CodingAgentControlKind`：Abort、Steer、FollowUp
- `enqueue_control_payload`：control_id 去重、PayloadConflict、QueueCapacityExceeded
- `PromptControlHandle`：steer/steer_content/follow_up/follow_up_content/abort

## 目标改动

### 1. `PromptQueueEntry` 类型

在 `agent-core/src/agent/queue.rs` 引入：

```rust
pub(crate) struct PromptQueueEntry {
    pub id: String,          // stable entry id (from coding-agent control_id)
    pub version: u32,        // incremented on each edit
    pub message: AgentMessage,
}
```

`AgentState` 和 `AgentTurnContext` 中的 `steering_queue`/`follow_up_queue`/`interjection_queue` 类型从 `VecDeque<AgentMessage>` 改为 `VecDeque<PromptQueueEntry>`。

`drain_queue` 返回 `Vec<AgentMessage>`（drain 时 strip metadata，只返回 message）。

### 2. `AgentInputQueue` 增加 `Interjection`

```rust
pub enum AgentInputQueue {
    Steering,
    FollowUp,
    Interjection,
}
```

Interjection 是高优先级 steering：在 `drain_queued_input` 中优先于 steering drain。用于需要立即注入但不应打断 tool pair 的用户输入。

### 3. `AgentState` 增加 `interjection_queue`

```rust
pub(crate) struct AgentState {
    // ... existing ...
    pub interjection_queue: VecDeque<PromptQueueEntry>,
}
```

### 4. `AgentCommand` 增加 edit/remove

```rust
EditQueueEntry {
    entry_id: String,
    expected_version: u32,
    new_message: AgentMessage,
    reply: oneshot::Sender<Result<(), AgentQueueError>>,
},
RemoveQueueEntry {
    entry_id: String,
    expected_version: u32,
    reply: oneshot::Sender<Result<(), AgentQueueError>>,
},
```

actor 在 turn 进行中也可以处理 edit/remove（直接修改 turn working copy 中的 queue 或 state 中的 queue）。

### 5. `AgentQueueError` 增加新 variant

```rust
#[error("queue entry {entry_id} is stale: expected version {expected_version}, actual {actual}")]
StaleVersion { entry_id: String, expected_version: u32, actual: u32 },
#[error("queue entry {entry_id} not found")]
NotFound { entry_id: String },
```

### 6. Interjection drain 优先级

`drain_queued_input`（`nodes.rs:108`）改为：
1. 先 drain `interjection_queue`（All mode）
2. 再 drain `steering_queue`（按 `steering_mode`）

### 7. `AgentHandle` / `Agent` 增加 interjection/edit/remove API

- `Agent::interject(text)` / `Agent::interject_content(content)`
- `Agent::edit_queue_entry(entry_id, expected_version, new_message)`
- `Agent::remove_queue_entry(entry_id, expected_version)`

### 8. coding-agent 适配

- `CodingAgentControlKind` 增加 `Interject`
- `PromptControlCommand` 增加 `Interject`/`InterjectContent`
- `PromptControlHandle` 增加 `interject`/`interject_content`
- `dispatch_control` 增加 Interject 分支

## 关键决策

1. **owner/last_editor 在 coding-agent 层**：agent-core 不引入客户端概念。多客户端 ownership 由 coding-agent 的 `ClientHandle` 和 `NotOwner` rejection 处理。agent-core 层的 `PromptQueueEntry` 只需要 id + version 来支持 stale-edit conflict。

2. **edit/remove 在 actor 中串行化**：actor 独占 state，edit/remove 直接修改 queue，无锁。turn 进行中也可以 edit/remove（修改 turn working copy 中的 queue）。

3. **interjection 优先级**：interjection 在 drain 时优先于 steering，但仍在安全边界（turn 开始时）注入，不会打断 tool pair。

4. **drain 时 strip metadata**：`drain_queue` 返回 `Vec<AgentMessage>`，因为 messages 列表不需要 queue entry metadata。

5. **combined display texts**：queue entry 的 display text 由 `AgentMessage` 的文本/content 决定，不需要额外字段。多个 queue entry 的合并显示在 UI 层处理。

## 分步实现

### 步骤 1：引入 PromptQueueEntry + AgentInputQueue::Interjection
- 定义 `PromptQueueEntry`、`AgentInputQueue::Interjection`
- `AgentQueueError` 增加 `StaleVersion`、`NotFound`
- `enqueue_message` 改为接收 `PromptQueueEntry`
- `drain_queue` 改为返回 `Vec<AgentMessage>`

### 步骤 2：AgentState + AgentTurnContext 适配
- `steering_queue`/`follow_up_queue` 改为 `VecDeque<PromptQueueEntry>`
- 增加 `interjection_queue`
- `drain_queued_input` 增加 interjection 优先 drain
- actor handle_command 中 steer/follow_up 构造 `PromptQueueEntry`

### 步骤 3：AgentCommand edit/remove + AgentHandle API
- 增加 `EditQueueEntry`、`RemoveQueueEntry` command
- actor handle_command 处理 edit/remove（带 version 检查）
- `AgentHandle` 增加 edit/remove/interject 方法
- `Agent` 增加 interject/edit/remove 公共 API

### 步骤 4：TurnRunner 适配
- steer/follow_up/interject 方法构造 `PromptQueueEntry`
- clear_queues 清空 interjection_queue

### 步骤 5：coding-agent 适配
- `CodingAgentControlKind` 增加 `Interject`
- `PromptControlCommand` 增加 `Interject`/`InterjectContent`
- `PromptControlHandle` 增加 `interject`/`interject_content`
- `dispatch_control` 增加 Interject 分支

### 步骤 6：测试
- edit with correct version -> success
- edit with stale version -> StaleVersion conflict
- edit non-existent -> NotFound
- remove queue entry
- interjection drain 优先于 steering
- tool-request/tool-result pairing 不被破坏
- clear_queues 清空所有三个 queue

## 验证

```text
cargo test --locked -p agent-core --all-features
cargo test --locked -p coding-agent --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
bash scripts/architecture-gate.sh
```
