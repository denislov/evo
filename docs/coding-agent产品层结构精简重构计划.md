# coding-agent 产品层结构精简重构计划

> 状态：已完成（CAG-300、Phase 1、Phase 2、Phase 3、Phase 4 全部通过 Gate）
> 决策日期：2026-07-30
> 最近更新：2026-07-30
> 基线 commit：`54c9349`（工作区干净）
> 适用范围：`crates/coding-agent` 内部结构；不改变 `api::` 公开语义，允许改公开类型名（单列任务评估）
> 总原则：行为等价优先于结构美观；允许破坏内部实现；不以兼容错误结构为目标；每个 Phase 独立可验证、可回滚

## 一、执行摘要

本计划把 `coding-agent` 的 operation 生命周期从「四条同构管线 + 两份镜像枚举 + 九个碎片模块」
收敛为「一份 envelope + 一份枚举 + 一个生命周期目录」，并清理组合根与 `services/` 层里不携带
信息的空壳。

分层结构（`ai ← agent-core ← coding-agent ← cli/desktop`）、每 crate 一个 `api` 门面、
event sourcing 作为状态权威 —— 这三条**不动**。它们是这个架构里唯一让 CLI 与 Desktop 两个
异构适配器共存的原因。

要解决的是产品层内部的**结构性重复**，而不是功能规模：

| crate | 生产代码 | api 导出符号 |
| --- | --- | --- |
| ai | 8.5k | 61 |
| agent-core | 7.6k | 143 |
| **coding-agent** | **50.7k** | **242** |

核心变换：

```text
旧：CodingAgentOperation ──into_internal──▶ Operation
                                            ├─ run_sync_operation      （15 arm，13 个 unsupported）
                                            ├─ run_sync_mut_operation  （15 arm， 8 个 unsupported）
                                            ├─ run_operation           （15 arm， 6 个 unsupported）
                                            └─ submit_internal         （运行时 if 三重否定）
      每条管线各自重复 6 步 envelope

新：CodingAgentOperation ─descriptor.dispatch_mode─▶ 唯一 envelope
                                                     ├─ SyncRead  handler
                                                     ├─ SyncMut   handler
                                                     └─ Async     handler
      envelope 只存在一份；unsupported_dispatch 这个概念消失
```

## 二、阻断性前提：产品层当前没有可执行测试

这是开工前必须记录的事实，它决定了 Phase 0 不可跳过。

`54c9349 "reduce tests"` 删除了 59,942 行，其中包含 `ai`、`agent-core`、`coding-agent`
的几乎全部测试。当前实测基线：

| crate | `#[test]` 标注 | **实际执行** |
| --- | --- | --- |
| ai | 0 | 0 |
| agent-core | 0 | 0 |
| **coding-agent** | **101** | **0** |
| cli | 106 | 106 通过 |
| tui | 268 | 140 通过 |
| desktop | 220 | 286 通过（5 ignored） |

`coding-agent` 的 101 个 `#[test]` **全部**位于 `runtime/facade/tests.rs`（6852 行）。该文件
未在 `runtime/facade.rs` 中声明为模块，因此从不参与编译。实测把它声明回去会产生
**135 个编译错误**：它引用了已被删除的 `ai::api::testing`、已改名的 `ClientConnectionId` /
`ClientDraftKind`，以及一批已不存在的 `*_for_tests` 辅助方法。它是旧 API 的快照，不能直接复活。

因此：**本计划要重构的正是全仓库唯一零覆盖的模块**。在建立安全网之前进行 Phase 1 是不可接受的。

`cli`（106）与 `desktop`（286）的测试对 `coding-agent` 提供的是**编译期**覆盖（它们编译against
公开 API），`desktop/src/runtime/tests.rs` 另有 17 处引用 `submit` / `CodingAgentOperation`，
提供有限的行为覆盖。这是现有资产，Phase 0 要先确认它到底覆盖了什么。

## 三、成功标准

全部完成时必须同时满足：

**行为等价**

- 每个 operation 变体的 dispatch 目标、admission class、terminal policy、durability 与重构前逐项一致，并由表驱动测试锁定。
- 三条管线的 finalize 尾部（freeze → resolve_finalization → emit_recovery_pending → persist_terminal_outbox → guard.finish）语义不变，包括 `run_operation` 额外的 session naming。
- `cli` 106、`tui` 140、`desktop` 286 个测试保持全绿，无新增 ignored，不靠更新 golden 掩盖回归。

**结构**

- 不存在第二个与 `CodingAgentOperation` 变体一一对应的内部枚举。
- `unsupported_dispatch` 及其 27 个填充 match arm 从代码中消失。
- envelope 六步只存在一份实现。
- `RuntimeHost` 访问链最深两级；不存在单字段包装类型和零字段转发 service。
- 一次 operation 的完整生命周期可在单个目录内读完。
- `src/` 下无空目录。

**可验证性**

- `coding-agent` 从 0 个可执行测试变为拥有覆盖 operation 契约表与 dispatch 路由的定向测试。
- `coding-agent` 拥有与 `tui` 同级的 API 边界编译期守卫。
- `docs/architecture.md` 与代码一致。

## 四、已核实的现状与根因

### 4.1 四条管线共享逐字重复的 envelope

`runtime/dispatch.rs`（808 行）的三个函数与 `runtime/execution.rs:55` 的 `submit_internal`
共享同一信封：

```text
resolve_operation_admission_with_id      （runtime/admission.rs:54）
→ OperationScheduler::admit(mode)        （runtime/scheduler.rs）
→ guard.commit_execution
→ ┄┄ 唯一真正不同的部分：match operation ┄┄
→ finalizer.freeze                       （runtime/finalization.rs:44）
→ session_coordinator.resolve_finalization
→ event_hub.service.emit_recovery_pending
→ persist_operation_terminal_outbox       （runtime/dispatch.rs:693）
→ guard.finish
```

尾部五步在 `dispatch.rs:61-78`、`dispatch.rs:322-339`、`dispatch.rs:641-659` **逐字重复**；
`run_operation` 仅多一行 `schedule_session_naming_after_prompt`。

### 4.2 descriptor 表已声明 dispatch mode，代码却不查表

`OperationDescriptor`（`runtime/outcome.rs:241`）已包含 `dispatch_mode` 字段，
`descriptor_for_internal_operation`（`runtime/outcome.rs:623`）为 15 个变体各返回一份契约。

决定性证据在 `runtime/intent.rs:157`：

```rust
pub(crate) fn unsupported_dispatch(admission: &OperationExecution) -> CodingSessionError {
    CodingSessionError::UnsupportedCapability {
        capability: format!(
            "{} operation requires {} dispatcher",
            admission.kind.as_str(),
            admission.descriptor.dispatch_mode.dispatcher_label(),  // ← 表已知道正确答案
        ),
    }
}
```

错误消息本身就是从表里读出正确 dispatcher 拼出来的。也就是说：不变量存在于数据中，却用运行时
错误兜底，而非让类型或路由查表。代价是三个穷举 match 共 42 个 arm，其中 **15 个做实际工作、
27 个是 `unsupported_dispatch` 填充**。

`submit_internal`（`runtime/execution.rs:71-82`）是同一问题的另一种写法 —— 一个三重否定的
运行时 `if`，外加 `unreachable!("runtime-owned operation class checked before spawn")`
（`execution.rs:173`）作为兜底断言。

### 4.3 `CodingAgentOperation` 与 `Operation` 是手写 1:1 镜像

两个 15 变体枚举，payload 类型**完全相同**（`PromptTurnOptions`、`SelfHealingEditRequest`、
`AgentInvocationOptions`、`AgentTeamOptions` 均已是公开类型）。
`into_internal`（`runtime/outcome.rs:905-955`）是 50 行纯搬运。

全部差异只有三类：

| 类别 | 实例 |
| --- | --- |
| 纯改名 | `Compact`↔`ManualCompaction`、`InvokeAgent`↔`AgentInvocation`、`InvokeTeam`↔`AgentTeam`、`ApproveDelegation`↔`ApproveDelegationConfirmation`、`RejectDelegation`↔`RejectDelegationConfirmation` |
| 字段降级 | `reuse: BranchSummaryReusePolicy` → `reuse_existing: bool` |
| 归一化 | `ExportCurrent` / `ExportCurrentHtml(path)` → `Export(ExportOptions)` |

这道边界不产生隔离价值：payload 既已公开，内部枚举无法独立演化。唯一有实质内容的是 Export
归一化，而那是一个函数的工作量，不是一个枚举的。

### 4.4 组合根含三个不携带信息的空壳

`runtime/owners.rs`：

```rust
EventHub { service: EventService }   // 单字段包装
CapabilityService;                   // 零字段 unit struct，全文 19 行，只转发一个调用
OperationFinalizer;                  // ZST + Copy，freeze() 本应是 FinalizationDecision::freeze()
```

代价体现在调用点密度：`runtime_host.operation_supervisor.control` 41 次、
`.session_coordinator.persistence` 23 次、`.event_hub.service` 13 次 —— 三级链中间一级为纯容器。
另有 `client_projection.coordinator` 读作 "coordinator 的 coordinator"（8 次）。

### 4.5 `services/` 混装三类不同性质的东西

| 文件 | 行数 | 性质 |
| --- | --- | --- |
| `event.rs` | 1921 | 有状态真服务 |
| `authorization.rs` | 974 | 有状态真服务 |
| `runtime.rs` | 531 | 有状态真服务 |
| `session.rs` | 57 | 4 个自由函数，`default_cwd()` 即 `env::current_dir()` |
| `capability.rs` | 19 | 零字段 unit struct 转发 |
| `redaction.rs` | 50 | 2 个纯函数 |

且 `services/session.rs` 与 `session/service.rs`（3168 行）是本 crate 最易读错的一对名字。

### 4.6 `runtime/` 切分线不沿概念走

`admission`(197) / `scheduler`(172) / `intent`(166) / `finalization`(179) / `owners`(73) /
`operation`(295) / `outcome`(1096) / `submission`(365) / `control`(1320) —— 九个模块共同描述
一次 operation 的生命周期。其中：

- `scheduler.rs` 只有 `admit` 与 `admit_query`；
- `intent.rs` 同时放着 `QueryIntent` 和 `OperationPermit`，而 `OperationPermit` 是 `admit` 的返回值，本该与 scheduler 同处；
- `finalization.rs` 是一个 ZST 加三个 payload 类型。

### 4.7 命名与遗留

- `runtime/client/projection.rs`(1706) 的内容是 connection / submission / control；`runtime/client/product_projection.rs`(1521) 才是 projection。名字与内容相反。
- 201 个 `CodingAgent*` 前缀标识符。`api` 已分 10 个子模块，前缀冗余：`api::client::CodingAgentClientProjectionLifecycle` 四段重复。
- 空目录且未在 `lib.rs` 声明：`src/protocol/`、`src/adapters/json/`、`src/adapters/rpc/`、`src/plugins/contributions/`。
- crate root 的 API 边界源码守卫只有 `tui` 有（`crates/tui/tests/api_contract.rs`）。API 面最大的 `coding-agent` 没有。
- `docs/architecture.md` 已与代码不符：称 coding-agent 有 `api.rs`（实为 lib.rs 内联）、`tools/shell/` 目录（实为 `shell.rs`）、`limits/` 目录（实为 `limits.rs`）、集成测试在 `tests/agent/` 与 `tests/execution/`（已删除）。

## 五、目标结构

### 5.1 Operation 的单一表达

```rust
// api::operation 的公开枚举保持唯一权威
pub enum CodingAgentOperation { /* 15 变体，不变 */ }

impl CodingAgentOperation {
    /// 唯一需要的内部归一化：只处理 Export 的 view/html 分歧
    pub(crate) fn export_options(&self) -> Option<ExportOptions>;
    pub(crate) fn descriptor(&self) -> OperationDescriptor;
}
```

不再有第二个变体一一对应的枚举。

### 5.2 单一 envelope + 按 mode 分派

```rust
impl CodingAgentSession {
    async fn execute_operation_envelope(
        &mut self,
        operation: CodingAgentOperation,
        submission: Option<SubmissionCommitGuard>,
    ) -> Result<CodingAgentOperationOutcome, CodingSessionError>;
}
```

三个 handler 各自只 match**属于自己 mode**的变体。不属于本 mode 的变体在路由层就被
`descriptor.dispatch_mode` 拦下，`unsupported_dispatch` 不再需要。

### 5.3 生命周期单目录

```text
runtime/operation/
├── mod.rs        # OperationExecution、outcome 与 lifecycle 公共类型
├── contract.rs   # OperationDescriptor、OperationContract 表（原 outcome.rs 的契约部分）
├── admission.rs  # resolve_admission（原 admission.rs + scheduler.rs）
├── permit.rs     # OperationPermit（从 intent.rs 迁入）
└── finalize.rs   # FinalizationDecision::freeze 及 payload 类型
```

`QueryIntent` 留在 `runtime/`（它不是 operation）。

## 六、执行任务与顺序

任务 ID 延续 `CAG-*`（产品层）。顺序是依赖顺序，不建议为并行打破。

每个任务的 Gate 统一含：`cargo fmt --check`、`cargo clippy -p coding-agent`（不新增 warning）、
`cargo test -p cli -p tui -p desktop` 全绿、`git diff` 自审。

---

### Phase 0：安全网

#### CAG-300：基线、孤立测试处置与 operation 契约安全网

> 状态：已完成。`coding-agent` 从 0 个可执行测试变为 8 个，覆盖 15 个 operation 变体的完整
> descriptor 契约与三个 dispatch family 的真实路由。两次变异测试确认安全网可捕获回归。
>
> **孤立测试的最终处置为「保留作历史参考，不恢复编译」。** 原计划判定其为旧 API 快照应删除；实测归因后
> 改变结论：`runtime/facade/tests.rs` 的 129 个用例中包含本轮重构恰好需要的不变量
> （`canonical_run_uses_each_metadata_dispatch_family`、
> `resolve_operation_admission_returns_structured_static_contract`、
> 7 个 `run_operation_*_uses_guard_and_preserves_*_error`、
> 6 个 `submitted_*_finishes_*_not_*`、3 个 `export_current_html_*`）。它是资产而非垃圾。
> 文件仍未参与编译（`runtime/facade.rs` 不声明该模块），因此不冒充覆盖。Phase 1 已把本轮需要的
> descriptor、dispatch 与 Export 归一化不变量提升为 8 个可执行测试；剩余旧用例依赖已删除的
> test-only 后门，不在本计划恢复。档案中的 operation 名称已同步到本轮最终结构。
>
> Gate：`cargo fmt --check` 干净；新增文件 0 个 clippy 警告（剩余警告为 `app/bootstrap.rs`
> 与 `app/embedding.rs` 的既有项）；`coding-agent` 8 个测试通过；`cli` 106、`tui` 140、
> `desktop` 286 保持全绿。

**135 个编译错误的归因**（决定不整体复活的依据）：

| 类别 | 数量 | 内容 | 判定 |
| --- | --- | --- | --- |
| 已删除的测试专用后门 | 18 | `current_capability_generation_for_tests`(8)、`queue_pending_delegation_for_tests`(3)、`arm_update_manifest_failure_for_tests`(3)、`arm_append_events_failure_for_tests`、`non_persistent_with_event_capacity_for_tests`、`install_submission_transition_probe_for_tests`、`for_tests` | **不复活** —— 复活等于把 test-only API 重新塞回生产类型，与 `54c9349` 的清理方向相反 |
| 已改名/改签名的生产 API | ~100 | `persistent_session_service`(27)、`active`(20)、`prompt_control_handle`(8)、`pending_delegation_confirmations`(8)、`begin`(7)、`terminal_status`(6)、`set_prompt_draft`(5)、`resolve_operation_admission`(3, 现为 `_with_id`) 等 | 机械可修，按需逐个迁移 |
| fixture 路径变更 | ~17 | `ai::api::testing` → `ai::api::provider::faux`（`FauxProvider` 只是**移动**了，未删除）；`ClientConnectionId` / `ClientDraftKind` 已改名 | 一行 import 即可修 |

关键实测结论：**核心 harness 完好**。`CodingAgentSession::{create_internal, non_persistent_internal,
run_internal}`、`FauxProvider::{with_call_queue, text_call}`、`crate::test_support::ProviderGuard`
全部存在，因此真实行为测试是可建的 —— 这也是本任务能在无 fixture 重建成本下产出行为覆盖的原因。

**新增测试**：

- `runtime/tests.rs` —— 纯契约表，无 fixture 依赖：
  - `every_operation_variant_resolves_its_declared_contract`：15 变体 × kind/class/dispatch/outcome_family/terminal_policy/root_evidence/lineage
  - `admission_class_derives_access_capacity_and_durability`：断言四元组由 admission class 派生
  - `priority_cancellation_and_child_policy_follow_kind_and_dispatch`：断言三项由 kind 与 dispatch mode 派生
  - `every_operation_variant_is_covered`：新增变体未登记表则失败
  - `internal_mirror_resolves_the_same_contract_as_the_public_enum`：捕获 `CodingAgentOperation::contract()` 与 `descriptor_for_internal_operation()` 两份独立映射之间的漂移（CAG-310 删除镜像后此测试随之删除）
  - CAG-310 删除上一项镜像漂移测试后，以 `export_variants_normalize_to_their_runner_modes`
    替换：锁定 `ExportCurrent` / `ExportCurrentHtml` 到 runner options 的两个归一化分支；测试总数仍为 8
- `runtime/dispatch_tests.rs` —— 真实 session + FauxProvider：
  - `run_internal_routes_every_dispatch_family_to_its_runner`：Prompt(Async) / ExportCurrent(SyncReadOnly) / SetSessionName(SyncMutable) 三族各自到达 runner 并产出对应 outcome
  - `sync_mutable_runner_errors_are_not_masked_as_unsupported_dispatch`：断言确切错误 `UnsupportedCapability("session names require a persistent Rust-native session")`
  - `async_runner_errors_are_not_masked_as_unsupported_dispatch`：断言确切错误 `Input("compact operation requires a compaction invocation")`

后两个测试初版用 `if let` 做宽松断言，探针确认 `Compact` 的实际错误是 `Input` 而非
`UnsupportedCapability`，该断言恒真。已改为断言确切 variant 与 message。

**变异验证**（证明安全网非装饰）：

| 变异 | 捕获者 |
| --- | --- |
| `RejectDelegation` 的 `dispatch_mode` 由 `SyncMutable` 改为 `Async` | `every_operation_variant_resolves_its_declared_contract` 失败于 `RejectDelegation: dispatch mode` |
| `run_internal` 把 `SyncMutable` 路由到只读 handler | `run_internal_routes_every_dispatch_family_to_its_runner` 与 `sync_mutable_runner_errors_...` 双双失败 |

**另一项现状更正**：`run_internal`（`runtime/submission.rs:259-268`）**已经**按
`descriptor.dispatch_mode` 选择 handler。因此第 4.2 节描述的问题准确形态是：路由层查表正确，
但三个 handler 各自又做了一遍防御性穷举，那 27 个 `unsupported_dispatch` arm 在
`run_internal` 路径上**不可达**。CAG-311 因此比原计划简单 —— 是删除死防御代码并收窄 handler
入参类型，而非重建路由。`submit_internal`（`runtime/execution.rs:71-82`）是唯一绕过
`run_internal` 的公开入口，它的三重否定 `if` 与 `unreachable!` 仍需单独处理。

工作（已完成）：

1. 记录基线：commit `54c9349`、工作区干净、各 crate 实测测试数见第二节表格。
2. 确认 `cli`(106) 与 `desktop`(286) 的覆盖性质：全部位于 presentation / wire / bridge 层。
   `desktop/src/runtime/tests.rs` 的 17 处 `submit` 引用是 `try_submit_prompt` —— 只向 bounded
   channel 投递 typed command，从不构造 `CodingAgentSession`。产品层 operation 生命周期
   （admission / dispatch / finalization / submission）此前**零行为覆盖**。
3. 归因 135 个编译错误并给出上表判定。
4. 新增两组测试并完成变异验证。

---

### Phase 1：Operation 的表达与派发（核心）

#### CAG-310：删除 `Operation` 内部镜像枚举

> 状态：已完成。`Operation` 与 `into_internal` 已删除；admission、dispatch、submission 全部直接
> 使用唯一的 `CodingAgentOperation`。实现阶段进一步发现原有 `OperationContract` 也是 15 变体的
> 1:1 enum 镜像，已改为无变体的静态契约记录 `struct`，公开 operation 的一次穷举 match 是唯一
> 变体权威。原计划拟引入的 `NormalizedOperation` 没有创建：实测只有 Export 需要形态归一化，使用
> `export_options()` 足够；BranchSummary 的 `reuse` 可在 runner 分支直接判定。创建一个 15 变体的
> `NormalizedOperation` 反而会重建本任务要删除的镜像。

主要文件：`runtime/operation.rs`、`runtime/outcome.rs`、`runtime/dispatch.rs`、`runtime/admission.rs`、`runtime/execution.rs`

工作：

- 直接复用公开枚举；Export 通过 `export_options()` 归一化，BranchSummary 在 runner 分支判定 reuse。
- 删除 `Operation` 与 `into_internal`。
- `descriptor_for_internal_operation` 改为 `CodingAgentOperation::descriptor()`。

完成标准：`grep -c 'Operation::' ` 的穷举 match 处从 4 处降为 1 处（契约表）；CAG-300 的 contract 测试逐项不变。

#### CAG-311：让 descriptor 驱动 dispatch

> 状态：已完成。`run_internal` 只进入 `execute_operation_envelope`，由
> `descriptor.dispatch_mode` 选择三个 handler；27 个填充 arm 与
> `IntentRouter::unsupported_dispatch` 已删除。handler 各自只列本 mode 的有效变体，末尾的
> `unreachable!` 是契约表与枚举形态漂移时的内部 invariant，不再把路由错误伪装成产品层
> `UnsupportedCapability`。`submit_internal` 的三重否定也已改为只接收 `InvokeAgent` /
> `InvokeTeam` 的窄类型，原有 runtime-owned dispatch `unreachable!` 已删除。

主要文件：`runtime/dispatch.rs`、`runtime/execution.rs`、`runtime/intent.rs`

工作：

- 路由层按 `descriptor.dispatch_mode` 选择 handler。
- 三个 handler 各自只 match 本 mode 的变体。
- 删除 `IntentRouter::unsupported_dispatch` 及 27 个填充 arm。
- 删除 `execution.rs:71-82` 的三重否定 `if` 与 `execution.rs:173` 的 `unreachable!`。

完成标准：`unsupported_dispatch` 零引用；CAG-300 的 routing 测试改为断言路由层分派正确，全绿。

#### CAG-312：抽出统一 envelope

> 状态：已完成。三条 session dispatch 管线现共享唯一的 `execute_operation_envelope`：admission、
> scheduler permit、submission commit、handler、freeze、session finalization、recovery pending、
> terminal outbox、submission finish 与 session naming hook 均只编排一次。`OperationPermit` 的所有权
> 保持到 finalization 完成；只有 `ForkSession` 延续旧语义显式提前 release。
>
> 原计划的 `dispatch.rs ~350 行` 估算经实现验证不成立：三个真实 handler 本身约 500 行，另有
> terminal outbox 约 110 行；删除重复 envelope 与 27 个死分支后文件由 808 行降到 742 行。
> 行数不是重复度的有效代理，完成 Gate 改以「session finalization 尾部一份、路由 fallback 零份」
> 为准。handler 与 outbox 会在 CAG-330 按生命周期目录拆文件，不在本任务制造临时模块。
> `submit_internal` 是 detached runtime-owned root，走 `resolve_non_session` 与 deferred terminal
> draft，不是 session envelope 的第四份副本，因此保留独立的 non-session finalization 路径。

主要文件：`runtime/dispatch.rs`

工作：

- 抽出 `execute_operation_envelope`，尾部五步只存在一份。
- `run_operation` 的 session naming 作为 envelope 的可选 hook，不复制整段尾部。

完成标准：`dispatch.rs` 从 808 行降至 ~350 行；finalize 尾部单份实现。

---

### Phase 2：组合根与 services 层

#### CAG-320：展平空壳、收敛 services

> 状态：已完成。`RuntimeHost` 直接持有 `events: EventService`；`CapabilityService` 已删除并由
> `CodingAgentCapabilities::from_runtime_state` 直接投影；`OperationFinalizer` 已删除，freeze 与
> non-session resolve 归入 `FinalizationDecision`；client owner 字段由含混的 `coordinator` 改为
> `snapshots`。`services/` 现只含 `event` / `authorization` / `runtime`：redaction 移到根级
> `redaction.rs`（与 `bounded_io.rs` 相邻），session cwd 辅助归入 `session/service.rs`，replay owner
> 派生归入 `runtime/session_coordinator.rs`，空转的 finalized-write 转发函数已在 operation 中直接
> 调用 outcome 方法。
>
> Gate：`cargo fmt --check` 干净；clippy 仍仅有 `app/bootstrap.rs` 与 `app/embedding.rs` 两个
> 既有 warning；coding-agent 8、CLI 106、TUI 268、Desktop 286（5 ignored）全绿。

工作：

- 删除 `EventHub`（`event_hub.service` → `events`）、`services/capability.rs`。
- `OperationFinalizer::freeze` → `FinalizationDecision::freeze(execution, result)`。
- `client_projection.coordinator` → `client_projection.snapshots`。
- `services/` 只留 `event` / `authorization` / `runtime`；`redaction.rs` 移至 `bounded_io` 旁；`services/session.rs` 的函数内联回调用点或移入 `session/`。

完成标准：`RuntimeHost` 访问链最深两级；`services/` 无零字段 service。

---

### Phase 3：模块重组

#### CAG-330：合并 operation 生命周期模块

> 状态：已完成。`runtime/` 顶层模块文件由 20 个收敛到 12 个；operation 的 contract、
> admission、permit、control、dispatch、execution、submission、finalize 与相关测试全部集中到
> `runtime/operation/`。原 `scheduler.rs` 的 admission policy 已合并进 `admission.rs`，
> `OperationPermit` 从 `intent.rs` 移至 `operation/permit.rs`，`intent.rs` 只保留 query intent。
> 迁移为模块归属与 import 调整，没有改变 operation 行为。
>
> Gate：`cargo fmt --check` 干净；clippy 仍仅有 `app/bootstrap.rs` 与 `app/embedding.rs` 两个
> 既有 warning；coding-agent 8、CLI 106、TUI 268、Desktop 286（5 ignored）全绿；
> `git diff --check` 干净。

按 5.3 的目标结构搬迁。纯移动 + 改 `use`，不改逻辑。

完成标准：`runtime/` 顶层模块数从 20 降至约 12；一次 operation 的生命周期在单目录内可读完。

---

### Phase 4：命名与遗留

#### CAG-340：命名、空目录、API 守卫、文档

> 状态：已完成。client 内部文件已按职责改名为 `connection.rs` 与 `projection.rs`；4 个遗留叶目录
> 及其 2 个空父目录已删除。`coding-agent/tests/api_contract.rs` 现守卫 crate root 只有 `api`
> 可以公开。实现时核对发现 TUI 并未使用 trybuild，而是同类独立源码边界测试，因此本任务按
> 实际权威模式对齐，没有引入无依据的 trybuild 依赖。`docs/architecture.md` 已同步内联 API、
> operation 生命周期目录、services、`shell.rs`、`limits.rs` 与真实测试路径。
> `CodingAgent*` 前缀按计划仅评估，不在本轮改名。
>
> Gate：`cargo fmt --check` 与 `git diff --check` 干净；clippy 仍仅有两个既有 warning；
> coding-agent 8 个 unit、1 个 API contract、7 个 doc-test，CLI 106、TUI 268、Desktop 286
>（5 ignored）全绿。

工作：

- `runtime/client/projection.rs` → `connection.rs`；`product_projection.rs` → `projection.rs`。
- 删除 4 个空目录。
- 为 `coding-agent` 增加 crate-root API 边界守卫，对齐 `tui` 的独立源码契约测试。
- 同步 `docs/architecture.md`（含第 4.7 节列出的全部不符项）。
- `CodingAgent*` 前缀精简：**单列评估，不在本计划内执行**。它会改动 `cli`/`desktop` 的全部调用点，且与结构问题无关，应作为独立一轮机械改名。

完成标准：文档与代码一致；API 守卫生效。

## 七、风险与回滚

| 风险 | 处置 |
| --- | --- |
| 产品层零测试，重构无行为判据 | CAG-300 先建表驱动安全网；未完成不进入 Phase 1 |
| 孤立测试文件被误当作覆盖 | CAG-300 明确保留为不参与编译的历史参考；所有 Gate 只统计真实执行测试 |
| Export 归一化语义在删除镜像枚举时漂移 | contract 测试覆盖 `ExportCurrent` / `ExportCurrentHtml` 两个变体的 descriptor 与 `writes_html()` 分支 |
| envelope 抽取吞掉 `run_operation` 的 session naming | 作为显式 hook 参数，contract 测试断言 Prompt 变体仍触发 |
| Phase 3 纯搬迁引入 `use` 循环 | 搬迁与逻辑改动分离在不同 commit，便于二分 |

每个 Phase 独立 commit，可单独 revert。Phase 1 的三个任务共享 CAG-300 的测试，建议连续完成后
再整体评估，不中途切换到 Phase 2。

## 八、明确不做的事

- 不改分层与依赖方向。
- 不改 event sourcing 作为状态权威。
- 不改 `api::` 的语义或子模块划分。
- 不在本计划内做 `CodingAgent*` 前缀改名（CAG-340 已说明理由）。
- 不为提高覆盖率而恢复被 `54c9349` 删除的全部测试 —— 只建立本次重构所需的判据。是否重建产品层完整测试资产是独立决策，不在本计划范围。

---

<sub>文档版本：1.0 | 基线 commit：54c9349</sub>
