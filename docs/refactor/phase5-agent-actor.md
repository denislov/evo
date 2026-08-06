# Phase 5 / ARC-500：Agent actor

> 状态：完成（2026-08-06）
> 前序：Phase 4（review/rewind/reconcile 闭合）
> 目标：消除 `Arc<RwLock<AgentState>>` 和 `queues_cleared` workaround，让状态拥有者和并发语义唯一

## 当前架构分析

### AgentState（`agent-core/src/agent/runtime.rs:32`）

```rust
pub struct AgentState {
    pub messages: Vec<AgentMessage>,
    pub tool_runtime: Option<ToolRuntime>,
    pub provider_tools: Vec<ToolDefinition>,
    pub config: AgentConfig,
    pub cancel_token: CancellationToken,
    pub steering_queue: VecDeque<AgentMessage>,
    pub follow_up_queue: VecDeque<AgentMessage>,
    pub(crate) provider_request_override: Option<ProviderRequestOverride>,
}
```

### Agent（`agent-core/src/agent/runtime.rs:58`）

```rust
pub struct Agent {
    state: Arc<RwLock<AgentState>>,
    running: Arc<AtomicBool>,        // admission gate（compare_exchange）
    queues_cleared: Arc<AtomicBool>, // queue merge workaround
}
```

- `Arc<RwLock<AgentState>>`：所有方法通过 `state.read()/write().unwrap()` 访问
- `running`：同步 admission（prompt/run 前 compare_exchange false->true）
- `queues_cleared`：`clear_queues` 在 turn 进行中设置 flag，turn commit 时 swap 并丢弃 queued input

### TurnLoopStream（`agent-core/src/agent/turn/runtime.rs`，760 行）

手写 `Stream`，poll 时推进 turn state machine：
1. `start_turn`：获取写锁，从 state 克隆 `AgentTurnContext`，清空 live queues
2. `run_typed_turn`：9 状态 turn state machine（Start -> DrainQueuedInput -> Compact -> PrepareProvider -> ApplyHook -> ProviderStream -> Decide -> ExecuteTools -> PrepareNextTurn）
3. `commit`：获取写锁，`context.apply_to_state(&mut state, discard_queues)`
4. `TurnRunDropGuard`：stream drop 时 commit context（防丢失）

### AgentTurnContext（`agent-core/src/agent/turn/context.rs`，162 行）

turn 期间持有 state 的克隆副本 + `live_state: Option<Arc<RwLock<AgentState>>>`：
- `sync_live_queues()`：获取写锁 drain steering/follow_up queues（turn 期间接收新输入）
- `take_provider_request_override()`：获取写锁 take override
- `apply_to_state()`：把 turn 结果写回 state

### 调用链

```
Client (CLI/Desktop)
  -> CodingAgentClientConnection.steer/follow_up/abort
    -> coordinator.enqueue_control
      -> PromptControlHandle -> PromptControlCommand
        -> prompt runner -> agent.steer/follow_up/abort
          -> Arc<RwLock<AgentState>> 直接访问
```

prompt runner 使用 Agent 的方法：`prompt`, `try_prompt`, `run`, `steer`, `follow_up`,
`abort`, `messages`, `add_message`, `replace_messages`, `set_tool_runtime`,
`add_provider_tool`, `skill`, `prompt_from_template`。

## 目标架构

### AgentHandle

```rust
pub struct AgentHandle {
    commands: mpsc::Sender<AgentCommand>,
}
```

只持有 bounded mpsc sender。`Clone`（mpsc::Sender 是 Clone 的）。

### AgentCommand

```rust
enum AgentCommand {
    Prompt { text: String, reply: oneshot::Sender<Result<AgentEventStream, AgentAdmissionError>> },
    Run { reply: oneshot::Sender<Result<AgentEventStream, AgentAdmissionError>> },
    Steer { text: String, reply: oneshot::Sender<Result<(), AgentQueueError>> },
    SteerContent { content: Vec<ContentBlock>, reply: oneshot::Sender<Result<(), AgentQueueError>> },
    FollowUp { text: String, reply: oneshot::Sender<Result<(), AgentQueueError>> },
    FollowUpContent { content: Vec<ContentBlock>, reply: oneshot::Sender<Result<(), AgentQueueError>> },
    Abort { reply: oneshot::Sender<()> },
    ClearQueues { reply: oneshot::Sender<()> },
    Messages { reply: oneshot::Sender<Vec<AgentMessage>> },
    AddMessage { message: AgentMessage, reply: oneshot::Sender<()> },
    ReplaceMessages { messages: Vec<AgentMessage>, reply: oneshot::Sender<()> },
    SetToolRuntime { runtime: ToolRuntime, reply: oneshot::Sender<Result<(), ToolDefinitionError>> },
    AddProviderTool { definition: ToolDefinition, reply: oneshot::Sender<Result<(), ToolDefinitionError>> },
    SetResources { resources: AgentResources, reply: oneshot::Sender<()> },
    ProviderRequestSnapshot { reply: oneshot::Sender<(Context, Option<StreamOptions>)> },
    SetProviderRequestOverride { context: Context, stream_options: Option<StreamOptions>, reply: oneshot::Sender<()> },
    BeforeProviderRequestHook { reply: oneshot::Sender<Option<BeforeProviderRequestHook>> },
    SetBeforeProviderRequestHook { hook: Option<BeforeProviderRequestHook>, reply: oneshot::Sender<()> },
    DrainSteeringQueue { reply: oneshot::Sender<Vec<AgentMessage>> },
    DrainFollowUpQueue { reply: oneshot::Sender<Vec<AgentMessage>> },
    Shutdown,
}
```

`AgentEventStream = mpsc::Receiver<AgentEvent>`（消费者从 prompt/run 收到的 stream）。

### Actor task

```rust
async fn run_actor(mut state: AgentState, mut commands: mpsc::Receiver<AgentCommand>) {
    let mut turn: Option<TurnRunner> = None;  // turn engine 进行中
    loop {
        tokio::select! {
            command = commands.recv() => match command { ... },
            event = async { turn.as_mut()?.next_event().await }, if turn.is_some() => { ... }
        }
    }
}
```

actor 独占 `AgentState`（`&mut`），无锁。steer/follow_up 直接修改
`state.steering_queue`。turn engine 在 actor 内运行，事件通过 mpsc 发送给消费者。

### TurnRunner（替代 TurnLoopStream）

turn engine 从手写 Stream 改为 actor 内的 async loop：

```rust
struct TurnRunner {
    state: Vec<AgentMessage>,      // turn 期间的 working copy
    turn: u32,
    event_tx: mpsc::Sender<AgentEvent>,
    cancel_token: CancellationToken,
}

impl TurnRunner {
    async fn next_event(&mut self) -> Option<AgentEvent> { ... }
}
```

关键改动：
- `AgentTurnContext` 拆分为 `AgentState`（持久状态）+ `TurnState`（turn 临时状态）
- `sync_live_queues` 不再需要锁：actor 独占 state，steer/follow_up command 直接修改 state
- `queues_cleared` workaround 删除：actor 串行化，clear_queues 直接清空 state queues
- `TurnRunDropGuard` 删除：actor 在 turn 结束或 abort 时 commit，不依赖 Drop

## 关键决策

1. **保持 `Agent` 名称和大部分 API 签名不变**：`prompt()` 仍返回 `AgentStream`，
   `steer()` 仍返回 `Result<(), AgentQueueError>`。内部通过 command 转发到 actor。
   `prompt()` 返回 lazy stream（`async_stream::stream!`），在首次 poll 时发送 command。

2. **`try_prompt` / `try_run` / `skill` / `prompt_from_template` 改为 async**：
   这些方法需要同步获取 admission 结果。改为 async 后，调用者（prompt runner）
   在 async context 中 await。影响范围限于 agent-core 内部和 coding-agent prompt runner。

3. **不采用 OS 线程**：使用 Tokio task，所有 actor state 保持 `Send`。

4. **mailbox 有界**：`mpsc::channel(256)`。mailbox 满、actor closed、reply dropped
   都返回结构化错误。

5. **turn engine 在 actor 内运行**：通过 `select!` 同时推进 turn 和处理 commands。
   steer/follow_up 在 turn 进行中直接修改 state queues（无锁）。

6. **is_busy query 在 actor 失效时返回保守结果**（true）。

## 分步实现计划

### 步骤 1：AgentCommand + AgentHandle + actor task 骨架
- 定义 `AgentCommand` enum
- 定义 `AgentHandle`（mpsc::Sender）
- 实现 `run_actor` task（独占 AgentState，处理 commands）
- `Agent::new` 启动 actor task

### 步骤 2：迁移简单 command
- steer/follow_up/steer_content/follow_up_content
- abort/clear_queues
- messages/add_message/replace_messages
- set_tool_runtime/add_provider_tool/set_resources
- provider_request_snapshot/set_provider_request_override
- before_provider_request_hook/set_before_provider_request_hook
- drain_steering_queue/drain_follow_up_queue

### 步骤 3：迁移 turn engine
- TurnLoopStream -> TurnRunner（actor 内 async loop）
- AgentTurnContext 拆分为 AgentState + TurnState
- 删除 live_state / sync_live_queues / take_provider_request_override 的锁访问
- prompt/run 通过 command 触发，返回 AgentEventStream

### 步骤 4：删除 Arc<RwLock> + queues_cleared
- Agent 不再持有 Arc<RwLock<AgentState>>
- 删除 queues_cleared workaround
- 删除 TurnRunDropGuard

### 步骤 5：适配 coding-agent
- prompt runner 适配 async try_prompt/try_run/skill/prompt_from_template
- services/runtime 适配 set_tool_runtime/add_provider_tool
- control handle 适配

### 步骤 6：适配测试
- agent-core 49 个测试
- coding-agent prompt 相关测试

### 步骤 7：ARC-540 可靠性测试
- mailbox saturation、actor panic、provider hang
- shutdown 无泄漏

## 验证

```text
cargo test --locked -p agent-core --all-features
cargo test --locked -p coding-agent --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
bash scripts/architecture-gate.sh
```

Phase 5 Gate：`Arc<RwLock<AgentState>>` 和 `queues_cleared` workaround 删除；
每 session 只有一个状态写入者；所有 command 均有有界失败语义。

## 完成验证（2026-08-06）

- `Arc<RwLock<AgentState>>`、`queues_cleared`、`RunGuard`、`TurnRunDropGuard` 已物理删除
- `Agent` 现在只持有 `AgentHandle`（bounded `mpsc::Sender`），`Agent::new` 启动 actor task
- actor 独占 `AgentState`（`&mut`），通过 `tokio::select!` 交错处理 command 与 turn 推进
- `TurnRunner` 替代 `TurnLoopStream`：actor 内 async loop，`turn_continues` flag 在 tool-turn 结束后让出控制权
- `AgentTurnContext` 拆分为持久 `AgentState` + turn 临时 working copy，无锁 queue 同步
- coding-agent `build_agent_runtime_with_capabilities`、`prepare_summary_prompt`、`prepare_summary_context`、`build_agent_runtime` 已 async 适配
- `cargo check --workspace --all-features`、`cargo clippy -D warnings`、`cargo fmt --check` 通过
- `cargo test -p agent-core`：52 passed, 0 failed, 1 ignored
- `cargo test -p coding-agent`：182 passed, 0 failed
- architecture gate：`execution_debts=0`，`oversized_debts=35`（既有基线）

## 复验与修复记录（2026-08-06）

Review 发现两个问题并已修复，同时做了文件拆分（因修复后超 900 行上限）：

### 1. Shutdown 在 turn 进行中丢失 working copy（P2-1）

`run_actor` 的 `turn_continues` 让出窗口内 drain pending commands 时，`handle_command`
返回 `true`（Shutdown）后直接 `return`，绕过 loop 末尾的 graceful shutdown 块
（abort turn + drain + `commit_turn`），in-flight turn 的 working copy 被 drop。
与 `Agent::shutdown` 的 doc 契约（"any in-flight turn is committed"）及 ARC-530 文档
"显式 shutdown 与 sender drop 效果相同"的断言相矛盾。

修复（`actor.rs`）：drain loop 用 `shutting_down` flag + `break` 外层 loop，走
graceful shutdown 路径。

### 2. Event send 阻塞时 mailbox 被饿死（P2-2 引入风险的根治）

steer 系列改为 await reply 后，若 consumer（coding-agent runner）在等待 reply 时
停止消费 event stream，actor 可能卡在 `tx.send(event).await`（event channel 满）
而不再处理 mailbox，形成循环等待。修复：event send 挂起期间用嵌套 `select!`
继续处理 `commands.recv()`，保持 reply 可达；consumer drop（`tx.is_closed`）仍走
abort + pending_commit 路径。

### 3. 文件拆分

- `runtime.rs`（878 → 333 行）：保留 `Agent` 公共 API、`AgentState`、`next_message_id`。
- 新增 `actor.rs`（605 行）：`run_actor`、`handle_command`、admit/commit、tool runtime helpers。
- `turn/runtime.rs`（873 → 736 行）：保留 `TurnRunner`、`run_typed_turn`。
- 新增 `turn/transitions.rs`（185 行）：状态机 transition 表 + transition-table 测试。

### 复验结果

- `cargo test -p agent-core --all-features`：77 passed（新增 shutdown-during-turn、
  shutdown-during-provider-hang、queue limit 入队失败语义等测试），3 次重复无 flaky
- `cargo test -p coding-agent --all-features`：185 passed
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo fmt --check` 通过
- architecture gate：`oversized_debts=35`（既有基线，无新增）、`execution_debts=0`
- release 首 token 性能基线（`agent_core_release_faux_first_text_delta_baseline`）通过

### 已知观察项（不阻塞，Phase 10 前处理）

- `run_actor` 的 `turn_continues` 让出使用 `sleep(1µs)` 轮询 consumer drop / 新命令，
  依赖调度器行为，测试已覆盖但可考虑显式握手。
- `TurnRunner` 内部事件缓冲为 unbounded（`mpsc::unbounded`），实际风险低（对外 bounded
  64 + backpressure），但与 ARC-530 "有界可合并通道"表述有出入。
