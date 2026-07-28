# `coding-agent` 接入指南

`coding-agent` 是 Evo 的产品运行时边界。CLI、Desktop 或其他宿主只需要依赖
`coding_agent::api`，不需要了解 provider、tool、session repository、event outbox
或 operation scheduler 的内部实现。

本文面向应用适配器作者，说明如何：

- 解析项目配置、认证、模型、Profile 和本地资源；
- 创建、打开和切换会话；
- 提交 prompt 及其他产品操作；
- 将事件流投影成可渲染的客户端状态；
- 处理工具授权、取消、steer 和 follow-up；
- 从事件丢失、界面重建或连接接管中恢复；
- 正确关闭运行时。

## 支持边界

只从以下分类模块导入类型：

| 模块 | 用途 |
| --- | --- |
| `api::embedding` | 应用启动、项目上下文、模型/Profile/资源目录 |
| `api::runtime` | 会话创建、打开、关闭和运行时控制 |
| `api::operation` | prompt、compact、agent、team 等产品操作 |
| `api::client` | 客户端连接、快照、投影、提交、控制和重连 |
| `api::event` | 有序产品事件及协议版本 |
| `api::authorization` | 工具授权请求和决策 |
| `api::settings` | 运行时设置、展示设置和主题 |
| `api::view` | 会话、Profile、能力和 transcript 的只读 DTO |
| `api::review` | changed-file review 和外部编辑器目标 |
| `api::error` | 可安全展示的错误和诊断 |

不要依赖 crate 根目录的符号，也不要引用 `app`、`runtime`、`services`、
`operations` 等私有模块。`api` 分类是调用方唯一受支持的源码边界。

上层适配器负责界面、输入、命令排队、线程切换、协议序列化和错误展示。
`coding-agent` 负责配置解析、认证材料保护、操作准入、会话持久化、工具授权、
事件顺序、恢复语义和运行时关闭。

## 选择启动入口

### 应用适配器：`CodingAgentApplicationStartup`

CLI、RPC server 和普通 Desktop 适配器优先使用：

```rust,no_run
use std::path::PathBuf;

use coding_agent::api::embedding::CodingAgentApplicationStartup;

# fn load() -> Result<(), coding_agent::api::error::CodingAgentPublicError> {
let application = CodingAgentApplicationStartup::resolve(PathBuf::from("."))?;
# Ok(())
# }
```

它一次性解析产品配置，并提供：

- `session_bootstrap`：打开调用参数所选择的会话；
- `prepare_prompt` / `prepare_prompt_with_images`：校验和规范化输入；
- `prompt_operation`、`compact_operation`、`agent_invocation_operation` 等操作工厂；
- `auth_controller`、`settings_controller` 和 `profile_catalog`；
- 模型摘要、默认 Profile、诊断和会话查询。

### 交互式应用：`CodingAgentInteractiveStartup`

全屏 TUI、内联 TUI 或需要产品展示默认值的宿主使用
`CodingAgentInteractiveStartup::resolve`。它在
`CodingAgentApplicationStartup` 之外提供主题、终端展示模式、模型轮换、资源命令和
会话选择项。

配置或本地资源发生变化时调用 `reload()`，然后以新的 startup 替换适配器持有的
旧 startup。不要由界面层自行重新实现配置合并。

### 自定义宿主：`CodingAgentEmbeddingContext`

需要自行提供项目选择器、模型选择器或会话浏览器的 Desktop 宿主使用：

```rust,no_run
use std::path::PathBuf;

use coding_agent::api::embedding::{
    CodingAgentEmbeddingContext, CodingAgentEmbeddingOptions,
};

# fn load() -> Result<(), coding_agent::api::error::CodingAgentPublicError> {
let context = CodingAgentEmbeddingContext::load(
    CodingAgentEmbeddingOptions::new(PathBuf::from(".")),
)?;
let project = context.snapshot();
println!("model={}, cwd={}", project.selected_model_id, project.cwd.display());
# Ok(())
# }
```

通过 `select_model`、`select_default_agent_profile` 和
`reload_local_resources` 更新上下文，通过 `create_session`、`open_session`、
`open_or_create_session` 和 `list_sessions` 管理会话。认证值和 provider client
始终保留在 crate 内，不会出现在 `CodingAgentEmbeddingSnapshot` 中。

## 推荐的适配器结构

`CodingAgentSession` 是唯一的可变运行时所有者。不要把它放进 UI state，也不要让
多个界面回调并发持有它。推荐让一个 Tokio task 或专用 runtime thread 独占 session：

```text
UI / protocol thread
    |
    | bounded AdapterCommand
    v
runtime owner task
    | owns CodingAgentApplicationStartup or CodingAgentEmbeddingContext
    | owns Option<CodingAgentSession>
    | owns the active operation future
    |
    | bounded AdapterUpdate
    v
UI projection / wire serializer
```

适配器命令应携带自己的 `command_id` 或幂等键。运行时 task 在完成或拒绝命令时回传
同一个标识。不要在渲染线程中等待模型、磁盘、工具或关闭操作。

`CodingAgentClientConnection` 是可克隆的控制句柄。它代表一个逻辑客户端和一个连接
generation，可放入 UI 控制路径。再次使用相同 `CodingAgentClientId` 连接会接管旧
generation；旧句柄之后应视为 stale。

## 最小运行示例

以下代码展示一次完整的 prompt 生命周期。产品界面应在此基础上增加下一节的事件
投影。

```rust,no_run
use std::path::PathBuf;

use coding_agent::api::client::{
    CodingAgentClientId, CodingAgentDraftId, CodingAgentSubmissionDraft,
};
use coding_agent::api::embedding::CodingAgentApplicationStartup;
use coding_agent::api::error::CodingAgentPublicError;
use coding_agent::api::operation::CodingAgentOperationOutcome;

async fn run_one_prompt(
    cwd: PathBuf,
    input: &str,
) -> Result<CodingAgentOperationOutcome, CodingAgentPublicError> {
    let application = CodingAgentApplicationStartup::resolve(cwd)?;
    let mut session = application
        .session_bootstrap
        .clone()
        .with_new_session()
        .open()
        .await?;
    let connection = session.connect(CodingAgentClientId::new("example-primary"))?;

    let prompt = application.prepare_prompt(input)?;
    let draft = CodingAgentSubmissionDraft::new(
        CodingAgentDraftId("prompt-1".into()),
        prompt.display_text(),
    );
    let operation = application.prompt_operation(prompt);

    // UI/RPC 提交必须先建立 client-owned submission。它原子地关联
    // draft、operation_id、session owner 和后续终态。
    let submission =
        connection.prepare_client_submission(&mut session, Some(draft), operation)?;
    let outcome = submission.run(&mut session).await?;

    connection.detach()?;
    session.shutdown().await?;
    Ok(outcome)
}
```

关键约束：

- prompt 必须携带 `CodingAgentSubmissionDraft`；其他操作必须传 `None`；
- draft 的展示文本必须来自同一个 `CodingAgentPreparedPrompt::display_text()`；
- `CodingAgentPreparedSubmission` 只能交给创建它的 session；
- handoff 失败时调用 `submission.discard(&mut session)`；
- prompt、compact 和会话写操作使用 `run()`；
- `submit()` 只用于支持异步执行的非 session root（目前是 agent/team 类操作），并
  返回必须 `join()` 的 `CodingAgentOperationTask`；
- 丢弃 `CodingAgentOperationTask` 只会分离等待句柄，不等价于取消操作。

简单的无界面、无 client 关联脚本可以直接调用 `session.run(operation)`，但 CLI、
Desktop 和 RPC 应使用 `prepare_client_submission`，以获得一致的草稿、控制、
acknowledgement 和恢复语义。

## 快照、事件和客户端投影

### 不要在 UI 中手写事件 reducer

`CodingAgentProductEvent` 是有序事件信封。每个事件包含：

- `stream_id()` 和单调递增的 `sequence()`；
- `session_id()`、`operation_id()`、parent/root operation 关联；
- `family()`、`kind_name()` 和类型化 `event()`；
- `delivery_class()`、`durability()` 和可选 terminal 信息。

UI 应使用 `CodingAgentClientProjection` 将快照和事件归并成 bounded、可直接展示的
状态。它提供 messages、tools、authorizations、changes、usage、diagnostics、
recoveries、profiles、capabilities 和 lifecycle，并返回精确的 invalidation areas。

建立完整初始投影：

```rust,no_run
use coding_agent::api::client::{
    CodingAgentClientBootstrap, CodingAgentClientProjection,
};
use coding_agent::api::runtime::CodingAgentSession;
use coding_agent::api::error::CodingAgentPublicError;

fn bootstrap_projection(
    session: &CodingAgentSession,
) -> Result<CodingAgentClientProjection, CodingAgentPublicError> {
    let bootstrap = CodingAgentClientBootstrap {
        snapshot: session.snapshot(),
        transcript: session.transcript_snapshot()?,
        pending_recoveries: session.recovery_pending()?,
    };
    // ProjectionIssue 是 adapter contract 错误；应用应记录并重新获取完整快照。
    Ok(CodingAgentClientProjection::from_bootstrap(bootstrap)
        .expect("coding-agent returned an internally inconsistent bootstrap"))
}
```

对每个事件调用 `projection.apply(&event)`：

- `Applied(changes)`：只刷新 `changes.areas()` 指定的界面区域；
- `IgnoredDuplicate`：忽略重复事件；
- `NeedsResync(issue)`：停止应用增量事件，获取新快照并替换投影。

事件成功应用后调用 `connection.acknowledge(event.sequence())`。ack 表示该逻辑客户端
已经处理到某个 sequence，不是持久化确认，也不能在实际应用事件之前提前发送。

### 推荐使用 reconnect receiver

`session.subscribe_product_events_public()` 适合日志、转发器或短生命周期的无状态
consumer；接收者跟不上时会返回带 `EventStreamLag` context 的
`CodingAgentPublicError`。

有状态 CLI/Desktop 应从 connection snapshot 的 cursor 建立可恢复流：

1. 用 `connection.snapshot` 创建投影；
2. `connection.acknowledge(snapshot.cursor.last_event_sequence)`；
3. 调用 `connection.reconnect_from_cursor(&snapshot.cursor)`；
4. `Replayed` 时先顺序应用 `events`，再消费返回的 receiver；
5. `FreshSnapshotRequired` 时用 `recovery.snapshot` 替换投影，然后从新 cursor
   重新执行步骤 2；
6. receiver 返回 `FreshSnapshotRequired` delivery 时执行相同替换；
7. 每个成功应用的事件都按 sequence ack。

不要把本地“最后渲染的 sequence”猜成 cursor。始终保存
`CodingAgentSnapshotCursor`，并校验 `UI_SNAPSHOT_PROTOCOL_VERSION`。跨进程协议还应
协商 `PRODUCT_EVENT_PROTOCOL_VERSION`。

### 终态 acknowledgement

大多数操作通过 terminal ProductEvent 锚定终态，此时应用和 ack 该 terminal event
即可。少数操作的终态只存在于同步 outcome：

```rust,no_run
use coding_agent::api::client::{
    CodingAgentClientConnection, CodingAgentSubmittedOperationStatus,
    CodingAgentSubmittedTerminalAnchor,
};
use coding_agent::api::error::CodingAgentPublicError;

fn acknowledge_terminal(
    connection: &CodingAgentClientConnection,
) -> Result<(), CodingAgentPublicError> {
if let Some(submitted) = connection.state()?.submitted_operation {
    if let CodingAgentSubmittedOperationStatus::Terminal {
        anchor: CodingAgentSubmittedTerminalAnchor::OutcomeOnly { acknowledgement },
        ..
    } = submitted.status
    {
        connection.acknowledge_outcome(acknowledgement)?;
    }
}
Ok(())
}
```

操作 future 完成后检查一次 `submitted_operation`。未确认的 outcome-only 终态会继续
占用客户端提交槽。

## 工具授权

默认交互会话使用 `ToolAuthorizationMode::Interactive`。授权请求会同时出现在：

- `CodingAgentSnapshot::pending_authorizations`；
- `CodingAgentClientProjection` 的 Authorizations area；
- `CodingAgentToolProductEvent::AuthorizationRequired` 事件。

界面只展示 `ToolAuthorizationRequest::preview` 中的 bounded 字段，不要重新解析工具
原始参数。用户决定后：

```rust,no_run
use coding_agent::api::authorization::ToolAuthorizationDecision;
use coding_agent::api::client::CodingAgentClientConnection;
use coding_agent::api::error::CodingAgentPublicError;

fn decide_first_authorization(
    connection: &CodingAgentClientConnection,
) -> Result<(), CodingAgentPublicError> {
let Some(request) = connection
    .pending_tool_authorizations()?
    .into_iter()
    .next()
else {
    return Ok(());
};

connection.decide_tool_authorization(
    &request.identity(),
    ToolAuthorizationDecision::AllowOnce,
)?;
Ok(())
}
```

必须传完整 identity；只传 `authorization_id` 会丢失 operation、turn、tool call 和
capability generation 的关联保护。对 stale 或已经解决的请求按普通产品错误处理，
不要在适配器中强制覆盖。

运行时关闭或 capability generation 撤销会取消等待中的授权。UI 收到相应事件后应
关闭确认浮层。

## 运行中控制

从 connection 获取 operation-scoped 控制句柄：

```rust,no_run
use coding_agent::api::client::{
    CodingAgentClientConnection, CodingAgentControlId, CodingAgentControlRejection,
};

fn queue_controls(
    connection: &CodingAgentClientConnection,
    operation_id: &str,
) -> Result<(), CodingAgentControlRejection> {
let control = connection.prompt_control(operation_id);
control.steer(
    CodingAgentControlId("ui:42:steer:1".into()),
    "先修复编译错误",
)?;
control.follow_up(
    CodingAgentControlId("ui:42:follow-up:1".into()),
    "完成后运行测试",
)?;
control.abort(
    CodingAgentControlId("ui:42:abort:1".into()),
    "user cancelled",
)?;
Ok(())
}
```

每个用户意图使用稳定且唯一的 `CodingAgentControlId`。相同 ID 可用于幂等重试；不要
为同一次点击在重试时生成新 ID。

文本和图片先通过 `prepare_prompt` / `prepare_prompt_with_images`，再使用
`steer_prepared` 或 `follow_up_prepared`。需要先展示队列的界面可通过 connection
保存 draft，再使用 `steer_draft` / `follow_up_draft` 提交。

`CodingAgentControlRejection` 是预期的竞态结果，例如操作已经结束、generation 已
过期或控制类型不适用。将它作为命令拒绝反馈，不要当成 runtime crash。

## 会话管理

优先通过 startup 的 `CodingAgentSessionBootstrap` 打开会话：

| 需求 | 调用 |
| --- | --- |
| 使用启动参数选择的会话 | `session_bootstrap.open()` |
| 新建持久会话 | `clone().with_new_session().open()` |
| 创建新的随机会话身份 | `clone().with_fresh_session().open()` |
| 按产品 session id 打开 | `clone().with_session_id(id).open()` |
| fork 已有会话 | `clone().with_forked_session(id).open()` |
| 临时内存会话 | `clone().without_persistence().open()` |

不要根据 session id 自行拼接目录。列表、快照、树和 HTML 导出使用
`CodingAgentSessionQuery`，或使用 `CodingAgentEmbeddingContext` 的对应方法。

切换会话前：

1. 停止当前输入准入；
2. 完成或取消 active operation；
3. 保持事件 consumer 运行直至终态；
4. detach 当前 client；
5. `session.shutdown().await`；
6. 打开新 session，建立新的 connection、snapshot、projection 和 receiver。

不同 session 的 snapshot 或 event stream 不能替换现有 projection。

## 设置、认证、主题和本地资源

- 运行时偏好通过 `CodingAgentApplicationStartup::configure_runtime_preferences` 或
  `CodingAgentSettingsController` 修改；
- 认证通过 `CodingAgentAuthController` 修改；宿主只接收 provider 状态，不接收
  secret；
- Desktop/TUI 只使用 `CodingAgentThemeSnapshot` 的语义化颜色 token，不读取主题
  JSON；
- 主题热更新使用 `CodingAgentThemeWatcher` / `CodingAgentThemeReloadReceiver`；
- 项目资源更新使用 startup `reload()` 或 embedding context
  `reload_local_resources()`；
- Profile 和 model 必须从公开 catalog 选择，不要在适配器中构造 provider runtime。

替换设置、认证、模型或 Profile 后，应以控制器返回的新 snapshot 更新 UI。对于会
影响后续操作的变更，使用更新后的 operation factory 创建新操作；不要复用变更前
已经构造的 operation。

## 错误处理

所有产品错误都投影为 `CodingAgentPublicError`：

- `category`：适合界面分组；
- `code()` / `code`：适合稳定分支、日志和 wire error code；
- `retryable`：表示产品层认为调用可以重试；
- `summary`：已裁剪且可安全展示；
- `context`：operation、recovery、event gap、protocol version 或 capacity 等结构化
  上下文。

不要根据 `summary` 文本做控制流判断。适配器自己的 channel closed、thread panic、
render failure 等错误应使用自己的错误类型，不要伪装成
`CodingAgentPublicError`。

启动时的非致命问题位于 startup/embedding snapshot 的 `diagnostics` 中。展示这些
诊断不应阻止用户进入界面，除非对应启动 API 本身返回 `Err`。

## 正确关闭

关闭分为两个阶段：

1. 从 session 取得 `runtime_shutdown_handle()`，让外层信号处理器或窗口关闭回调调用
   `request_shutdown()`。这是非阻塞且幂等的：关闭新准入并取消等待中的授权。
2. 由持有 session 的 runtime task 完成 active operation 的 abort/join，继续消费终态
   事件，然后调用 `session.shutdown().await` 完成持久化和 runtime drain。

推荐顺序：

```text
stop adapter command admission
  -> runtime_shutdown_handle.request_shutdown()
  -> abort/join active operation when present
  -> session.shutdown().await
  -> consume Runtime::ShutDown / close event receiver
  -> detach/drop client connection
  -> join runtime task or runtime thread
```

不要只 drop `CodingAgentSession`，也不要在 GUI 主线程同步 join runtime thread。
`shutdown()` 是幂等的，返回 `ShutDown` 或 `AlreadyShutDown`。

如果界面可能在 session owner 正被移动时关闭，提前克隆
`CodingAgentRuntimeShutdownHandle`。这正是 phase A handle 的用途。

## CLI 与 Desktop 的责任清单

CLI/Desktop 应实现：

- 一个独占 `CodingAgentSession` 的 runtime owner；
- 有界 command/update channel 和本地 command id；
- 稳定的 `CodingAgentClientId`、control id 和 draft id；
- 基于 `CodingAgentClientProjection` 的展示状态；
- cursor replay、fresh snapshot replacement 和逐事件 ack；
- authorization UI 及 identity 完整回传；
- active operation 的 abort、join 和 outcome acknowledgement；
- session 切换及两阶段关闭；
- `CodingAgentPublicError` 到本地 UI/wire error 的无损映射；
- ProductEvent/UI snapshot 协议版本协商（存在进程边界时）。

CLI/Desktop 不应实现：

- provider 选择、认证 secret 解析或模型能力判断；
- session 路径拼接、event log/outbox 读取或持久化；
- operation 并发准入、终态判定或恢复算法；
- 工具风险重算、授权 identity 构造或 capability generation 管理；
- ProductEvent 的第二套 reducer；
- 对私有模块或内部错误字符串的依赖。

满足这条边界后，适配器可以完全不知道 `coding-agent` 的源码组织，只依赖分类公共
API、快照、事件、命令结果和本文定义的生命周期。
