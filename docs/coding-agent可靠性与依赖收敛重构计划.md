# coding-agent 可靠性与依赖收敛重构计划

> 状态：**已完成**（Phase 0～Phase 6、债务清算与最终 Gate 全部通过）
> 决策日期：2026-08-01
> 基线 commit：`867ac13`（工作区干净）
> 前置计划：`docs/coding-agent产品层结构精简重构计划.md`（CAG-300~340，已完成）
> 适用范围：`crates/coding-agent` 全部内部结构 + `api::` 语义；允许破坏内部实现与公开类型形状
> 总原则：**correctness 先于结构，结构先于美观**；每个 Phase 独立可验证、独立可 revert；不为兼容旧结构写冗余代码

---

## 一、执行摘要

前置计划把 operation 生命周期从「四条同构管线」收敛成「一份 envelope + 一份枚举」，解决的是**结构性重复**。
本计划解决剩下的三件事，它们是前置计划显式排除在外的：

1. **correctness** —— 两份独立 review（Claude / GPT Codex）交叉核实出 15 项 bug，其中 5 项会造成
   数据损坏、进程泄漏或运行时冻结。
2. **依赖成环** —— 目录看起来分层，实际是双向环。`runtime` 同时是「顶层编排器」和「共享领域词汇表」，
   下层模块直接 reach 进它的内部实现模块。
3. **测试资产** —— 51.7k 行只有 32 个可执行测试（全 workspace 最低，低一个数量级），
   且零覆盖的恰好是 crash consistency、取消竞态、reconnect gap、partial commit 这些最难写对的地方。

核心变换：

```text
旧：runtime/ 既是编排器又是词汇表
      ├─ capability::{CapabilityGeneration, ModelCapability, FilesystemCapability, ...}   ← 下层 31 处引用
      ├─ operation::contract::{OperationRootTerminalEvidence, OperationDescriptor, ...}   ← 下层 25 处引用
      ├─ operation::control::{OperationKind, OperationControl, ...}                       ← 下层 24 处引用
      └─ facade::CodingSessionError
    ⇒ services/operations/session/events/tools 全部反向依赖 runtime，形成 6 组环

新：kernel/（零依赖词汇表）← platform/（无领域基础设施）← domain/（事实与持久化）
                                                        ← application/（编排）← api/（门面）
    单向依赖，由自动化守卫强制
```

工程闸门当前状态（全部实测）：

| 检查 | 结果 |
| --- | --- |
| `cargo test -p coding-agent --all-features` | 通过（31 unit + 1 integration + 7 doctest） |
| `cargo test --workspace` | 通过 |
| `cargo check --workspace --all-features` | **失败**（desktop-devtools transcript fixture） |
| `cargo clippy -p coding-agent --all-targets --all-features -- -D warnings` | **失败**（`redaction.rs` `repeat(1)`） |
| `cargo fmt --all -- --check` | **失败**（workspace 49 处差异） |

### 1.1 执行进度（2026-08-02）

| 任务 | 状态 | 当前证据 |
| --- | --- | --- |
| CAG-400 | **完成** | `scripts/gate.sh` 已落地；workspace fmt、Clippy `-D warnings`、all-features test 全绿。desktop transcript fixtures、redaction lint 及后续暴露的 workspace lint 已收敛。`cross-adapter-events.json` 因仍被 desktop visual replay 消费而保留，待 CAG-451 接入 coding-agent golden test。 |
| CAG-401 | **完成** | `coding_agent::test_support` 已作为 feature-gated 公共测试 API 提供；包含真实 `SessionLogStore`/`SessionTransactionWriter` 的 `TempSessionEnv`、ENOSPC/fsync fault injection、`FakeClock`、`SeqIdGenerator`、`CancellationHarness`、`ProcessFixture`、`ProductEventRecorder`。4 个外部 integration smoke tests 全绿，其中 partial commit 测试证明 torn tail 重开后被修复且 sequence/事件数不漂移。 |
| CAG-410 | **完成** | `tools/process_runner.rs` 已统一 bash 与 self-healing check：取消/超时共用进程树 teardown（Unix `libc::kill` process group；Windows Job Object）、显式 env 策略、50 KiB/2000 行有界 tail、64 KiB/100 ms update 节流。补齐 Windows 启动变量，并让 mutation 原子段结束后恢复 check 可取消性；shutdown 期间的取消会跨关闭窗口锁存。新增 12 个回归测试覆盖 bash golden、sleep/孙进程取消、timeout 同 teardown、16 MiB 输出预算、env allowlist、check timeout/cancel，以及真实 runtime shutdown drain。coding-agent 47 unit + 5 integration + 7 doctest、严格 Clippy、fmt 和 workspace gate 全绿；Windows Job Object 分支经独立 MSVC target 编译验证。 |
| CAG-411 | **完成** | `FileMutation::begin`/`MutationGuard` 已替代 async closure 外围 fence：edit 在 read/derive 前取得 owned guard，write/edit 进入 `spawn_blocking` 时把 guard 与实际 write+sync 一并移交，外层 future 取消不会提前释放；panic/cancel 均由 RAII 清理 registry。key 从 capability 生成的绝对目标出发，canonicalize 最深已存在祖先再拼缺失后缀，create/overwrite 与 symlink parent 视图一致。4 个回归测试覆盖 detached blocking owner、取消后同路径串行、panic 清理及 symlink create/overwrite key。coding-agent 51 unit + 5 integration + 7 doctest、严格 Clippy/fmt 与 workspace gate 全绿。 |
| CAG-412 | **完成** | edit 已改用 `String::from_utf8`，Latin-1/GBK 输入在写入前返回含非法 byte offset 与编码感知工具建议的明确错误，原文件逐字节不变。fuzzy uniqueness 仅在 fuzzy 路径将 `oldText` 归一化后计数，与 search 共用文本空间；重叠端点使用 `saturating_add`。2 个回归测试覆盖两种非 UTF-8 编码及 curly quote、NBSP、NFKC、trailing whitespace 四类多候选。coding-agent 53 unit + 5 integration + 7 doctest、严格 Clippy/fmt 与 workspace gate 全绿。 |
| CAG-413 | **完成** | 新增统一 `bounded_arg(args, key, default, max)`：`read` 的 offset/limit 与 `ls`/`find`/`grep` 的 limit/context 均改为严格非负整数解析、runtime cap，并在 schema 同步 `integer`、minimum/maximum；负数、浮点和字符串返回显式错误，`u64::MAX` 安全钳制。grep context window、read line window、diff context 及 limit 翻倍提示均使用饱和算术，达到最大值时提示收窄查询而非建议无效的同值 limit。新增 6 个测试覆盖极值、错误类型、schema/runtime 一致性和双端饱和；coding-agent 59 unit + 5 integration + 7 doctest、严格 Clippy/fmt 与 workspace gate 全绿。 |
| CAG-414 | **完成** | `FilesystemCapability::discard_operation_bindings` 已接入统一 `OperationPermit::drop` 终结边界，覆盖同步/异步、root/child、committed/aborted/failed 及 future drop；fork 的提前 `release()` 只释放 admission guard，binding 仍保留至真正终态。binding 记录增加 `Instant` 创建时间和 64 条硬上限，插入前后双重容量检查封住并发竞态，超限错误报告最老条目年龄且 authorization 发布 operation-scoped diagnostic；新增测试专用 `bound_len()`。2 个回归测试覆盖三终态批量清理/跨 operation 隔离、Linux workspace fd 回归基线，以及容量上限 fail-closed。coding-agent 61 unit + 5 integration + 7 doctest、严格 Clippy/fmt 与 workspace gate 全绿，Phase 1 Gate 完成。 |
| CAG-420 | **完成** | `SessionTransactionWriter` reply 已从同步 mpsc 改为 Tokio oneshot，默认写入入口、turn transaction、`SessionEventWriter`、`SessionService`、session coordinator、prompt/delegation/authorization/recovery 及公开 authorization/recovery API 全部沿调用链 async 化；仅 `Drop`、shutdown、同步 capability revocation、测试夹具和已由 `spawn_blocking` 包裹的 session 初始化/复制/启动恢复保留名字明确的 blocking 入口。session create/open/open-or-create 与 fork 的同步磁盘阶段显式进入 `spawn_blocking`。新增 `current_thread` 回归测试，以真实 persistent prompt、Faux tool call、interactive authorization decision、工具执行、第二轮模型响应和 durable terminal commit 证明单线程 runtime 不冻结。公开 README 示例同步更新为 async。coding-agent 62 unit + 5 integration + 7 doctest、严格 Clippy/fmt 与 workspace gate 全绿。 |
| CAG-421 | **完成** | writer command channel 已切换为 Tokio bounded mpsc；async 路径使用 5 秒有界等待，同步收尾入口使用相同 deadline 的有界重试。容量由 32 调为 128，依据是一次容纳 100-checkpoint burst 并保留 28% headroom，D-03 已关闭。队列超时映射为内部与公开产品事件均可识别的 `QueueSaturated`，同时产出 operation-scoped diagnostic。4 个回归测试覆盖 200 ms slow writer 下 100 个并发 checkpoint 无丢失/无硬失败、结构化超时、产品事件与 diagnostic。coding-agent 66 unit + 5 integration + 7 doctest、严格 Clippy/fmt 与 workspace gate 全绿。 |
| CAG-422 | **完成** | 新增 crate-wide `MutexExt`：所有可失败业务路径统一将 poison 映射为 `CodingSessionError::Resource`，`Debug`/`Drop`/后台诊断等不可返回边界才允许显式恢复 guard，并以进程级一次性 diagnostic 留痕。EventService、snapshot/client registry、operation control、authorization、session writer/repository、filesystem capability、mutation queue、theme watcher 等锁调用与公开 API/CLI/Desktop 消费端均已沿调用链传播错误；业务源码和测试中的原始 `.lock().unwrap()`/`.lock().expect()` 为 0。gate 新增跨行 grep 守卫；3 个 poison 测试覆盖 helper 映射、不可失败边界恢复及 `SnapshotCoordinator` 高层降级。coding-agent 69 unit + 5 integration + 7 doctest、workspace 严格 Clippy/fmt 与完整 gate 全绿，Phase 2 Gate 完成。 |
| CAG-430 | **完成** | `kernel/` 已接管 error、self-healing 纯 payload、ids、operation descriptor/value、capability value、control command/value 与 limits；含 `SnapshotCoordinator` 的 control state machine 归 `application/operation`，filesystem authority bundle 暂归 `platform`。旧 `runtime::{capability,error,operation,snapshot,session_coordinator,public_error}` shim 已删除；`services/operations/session/events/tools` 对 `crate::runtime::` 引用为 0，kernel 除 `crate::kernel::` 外无内部依赖。coding-agent 69 unit + 5 integration + 7 doctest 与 workspace all-features check 全绿。当前项：CAG-431。 |
| CAG-431 | **完成** | `platform/process` 接管统一 ProcessRunner、process-tree teardown、shell discovery 与 product-neutral text update callback；`platform/fs` 接管 filesystem capability/target、cap walk、mutation fencing 与 opened edit handle；`platform/io` 接管 bounded read、output truncation、redaction；`platform/time` 接管 Clock/IdGenerator。含产品 generation/session authority 的 snapshot/service 已明确归 `application/capability`，纯 revocation/access values 归 kernel。`tools/` 对 `std::process`/`tokio::process`/`cap_std`/`tokio::fs` 直接引用为 0，platform 对 domain/application 引用为 0；coding-agent 69 unit + 5 integration + 7 doctest、workspace all-features check 与严格 Clippy 全绿。当前项：CAG-432。 |
| CAG-432 | **完成** | 新增 `tests/module_layering.rs`，用 syn 解析 production `use crate::` 与 fully-qualified crate path，按 L0-L4 表检查反向依赖并对 layer graph 做 cycle detection；失败包含相对文件、精确行号、source/target layer 与引用路径。synthetic L2→L3 自检证明守卫会失败。`api_contract.rs` 新增 evolving session response DTO 守卫，8 个稳定 response DTO 改为 `#[non_exhaustive]`，adapter 统一改走 constructor。coding-agent 69 unit + 8 integration + 7 doctest、workspace all-targets check 与严格 Clippy 全绿；完整 `scripts/gate.sh` 通过，Phase 3 Gate 完成。当前项：CAG-440。 |
| CAG-440 | **完成** | `session/service.rs` 已收敛为 273 行聚合根，命令、查询、事务终结、recovery、持久化与 workspace persistence 分别落入 `session/service/{commands,queries,finalize,recovery,persistence}.rs` 和 `persistence/workspace.rs`；最大文件 845 行。新增 4 个 transition-table 测试，固定 failure definite/uncertain/queue-saturated 分类、skip 终态、recovery 退避上限与 due 判断。coding-agent 73 unit + 8 integration + 7 doctest、workspace all-targets check、严格 Clippy 与完整 `scripts/gate.sh` 全绿。当前项：CAG-441。 |
| CAG-441 | **完成** | Phase 3 已迁移的 `application/snapshot.rs` 由 2,049 行拆为 702 行聚合根及 `snapshot/{client_registry,lifecycle,capability_state,projection}.rs`（427/566/498/115 行），保留 `SnapshotCoordinator` 与公开内部路径不变。新增 3 个 transition-table 测试，覆盖 5 条 runtime shutdown 迁移、6 条 receiver generation/lifecycle 验证和 5 条 submission slot 允许/拒绝状态。coding-agent 76 unit + 8 integration + 7 doctest、workspace all-targets check、严格 Clippy 与完整 `scripts/gate.sh` 全绿。当前项：CAG-442。 |
| CAG-442 | **完成** | `services/event.rs` 由 1,750 行拆为 756 行的 event mapping/receiver 根，以及 `event/{publish,durable,emit}.rs`（288/203/689 行）；publish lock、retention/replay cut、durable outbox/deferred terminal 与事件族 emit 的责任已分离。新增 3 个 transition-table 测试，覆盖 retention 容量、reconnect recovery cursor 与 deferred terminal draft 替换/消费。coding-agent 79 unit + 8 integration + 7 doctest、workspace all-targets check、严格 Clippy 与完整 `scripts/gate.sh` 全绿。当前项：CAG-443。 |
| CAG-443 | **完成** | `operations/prompt/context.rs` 由 1,709 行拆为 879 行的类型/状态根，以及 `context/{setup,stream,finalize}.rs`（284/704/60 行），request/runtime/session 准备、stream→transaction 映射和终态构造已分责。新增 3 个 transition-table 测试，覆盖 7 类 prompt input、completion 幂等迁移和 5 类 success/abort/failure 终态。coding-agent 82 unit + 8 integration + 7 doctest、workspace all-targets check 与严格 Clippy 全绿；完整 Gate 首次遇到 desktop timer 单次超时，单测复跑与完整 Gate 复跑均通过，确认非本次调用路径回归。当前项：CAG-444。 |
| CAG-444 | **完成** | 新增窄端口 `SessionWriter`、`EventSink`、`CapabilityQuery` 与生产 adapter；authorization 内部仅持有 trait object，保留 concrete constructor 作为 composition root，prompt context 传递 `SessionWriterPort`。authorization 纯判断拆入 329 行 `evaluation.rs`，service 根 843 行；新增 3 个仅依赖 fake ports 的 transition-table 测试，覆盖持久化事实序列、allow/deny/grant 决策和 capability generation 失效。coding-agent 85 unit + 8 integration + 7 doctest，完整 `scripts/gate.sh` 全绿。CAG-440~444 的目标模块均已拆分且各有至少 3 个状态迁移表。 |
| Phase 4 Gate | **完成** | 继续按职责机械拆分 12 个历史超限文件：client connection/projection、session transaction/repository/replay、operation control/contract、app session/embedding、filesystem capability、self-healing runner、events model；所有 production/test Rust 文件均 ≤900 行，当前最大为 `runtime/client/projection.rs` 896 行。`scripts/gate.sh` 新增自动化 900 行上限守卫。完整 Gate 首次仍只遇到既有 desktop executor-neutral timer 单次超时，该单测复跑通过，随后完整 Gate 全绿。当前项：CAG-450。 |
| CAG-450 | **完成** | repository 新增 64 KiB 分块的 reverse visitor 与 item/byte 双预算，仅物化最近 10,000 events / 32 MiB；静态 hydration 使用独立 bounded-open 路径，只修复 torn tail，不构造 writer、不读 outbox、不做全量 startup replay。hydration/transcript/client/desktop DTO 贯通 `omitted_items` 与 opaque continuation，client projection 仍保留 10,000 items / 32 MiB 二次防线；完整 replay 仅由显式 `SessionExport` 边界触发。100k events 测试同时断言逆序扫描/容量上界与完整 bootstrap 的 cwd、10,000 retained、90,000 omitted、continuation sequence 和时间上界；coding-agent 86 unit + 8 integration + 7 doctest 全绿。正常 writer lease 仍保留全日志 sequence 连续性校验。当前项：CAG-451。 |
| CAG-451 | **完成** | 新建 `domain/projection/`，集中 AgentEvent→prompt stream、replay→public transcript、internal→public client snapshot、session summary 与 product DTO 的 `From` 转换；原 service/query/adapter 只保留消费或 re-export。coding-agent 自身首次接线 shared `cross-adapter-events.json`，以独立 `cross-adapter-projection.json` 锁定 cursor、message/tool、operation/delegation/usage reducer 结果；新增 `all-product-event-families.json`，对 Session/Agent/Team/Message/Tool/Runtime/Delegation/Workflow/Diagnostic/Capability 10 个 `ProductEventKind` family 做反序列化与逐字段 round-trip golden。coding-agent 88 unit + 8 integration + 7 doctest、严格 Clippy 与行数 Gate 全绿，Phase 5 完成。当前项：CAG-460。 |
| CAG-460 | **完成** | filesystem `read` 在 I/O 前识别 JPEG/PNG/GIF/WebP，复用 encoded/decode dimension/allocation 限额验证图片，并返回说明文本 + base64 image content；非法图片 fail closed。新增有效 1×1 PNG 与非法 WebP 回归测试。 |
| CAG-461 | **完成** | 从 `CodingAgentCapabilities` 与 CLI RPC mirror 删除永久为假的 `switch_session` / `switchSession`；会话切换继续由 adapter 关闭当前 owner、按 `CodingAgentSessionOpenTarget` 打开新 session。workspace all-target/all-feature check 通过。 |
| CAG-462 | **完成** | `http_proxy` 与 `websocket_connect_timeout_ms` 已下沉为 scoped `ai::TransportConfig`，7 个内建 provider 共用配置后的 `reqwest::Client`；非法 proxy 与 0 ms timeout 显式失败。Rust schema 删除 `transport`、`npm_command`、`collapse_changelog`、`warnings.anthropic_extra_usage`，README/CHANGELOG 已写迁移说明，配置 merge/resolve/reject 测试通过。 |
| CAG-463 | **完成** | 公开 summary/session API 不再泄漏仓储 `PathBuf`，统一返回 opaque `SessionStorageHandle`，只暴露 `session_id()`、`open_event_log()`、`export_path()`；CLI RPC owner state 与 command/prompt/stats 已同步迁移。100k hydration 测试同时验证 handle 身份、导出根和日志打开。 |
| CAG-464 | **完成** | D-01 复审确定 rename-into-place 会替换已授权对象、破坏 capability identity binding，当前契约固定为 opened-object mutation fence + `sync_all`，whole-file crash atomicity 明确列为当前 binding 模型非目标；D-02 核实无 CLI/Desktop consumer 后删除 `coding-agent/test-support` feature 与 public root module，可靠性 fixture 迁回私有 unit tests。架构文档与 crate README 已补齐五层图、唯一 `api::*` 边界、cooperative cancellation/atomic phase、bounded hydration 和 opaque storage 契约；债务台账已清空。当前项：最终 Gate。 |
| Phase 6 Gate | **完成** | 全仓旧 unsupported-setting 诊断、`switch_session`/`switchSession` 生产引用与 `coding-agent` 下游 test-support 暴露均为 0；债务台账无未结项，`git diff --check` 通过。所有 coding-agent production/test Rust 文件 ≤900 行，最大文件 890 行。`cargo fmt --all -- --check`、workspace all-target/all-feature 严格 Clippy 与 `cargo test --workspace --all-features` 已由完整 `scripts/gate.sh` 一次通过；计划全部完成。 |

> 上表是执行态证据；本节前面的“工程闸门当前状态”保留为基线记录，不再代表当前工作树。

---

## 二、前提与基线

### 2.1 测试资产分布

| crate | 可执行测试 | 生产 LOC | 密度 |
| --- | ---: | ---: | ---: |
| tui | 239 | 16,068 | 1 / 67 |
| agent-core | 51 | 8,350 | 1 / 164 |
| desktop | 235 | 44,443 | 1 / 189 |
| cli | 97 | 29,706 | 1 / 306 |
| ai | 6 | 8,743 | 1 / 1,457 |
| **coding-agent** | **32** | **51,741** | **1 / 1,617** |

零测试的文件恰好是最大也最难的那批：`client/connection.rs`(1667)、`client/projection.rs`(1524)、
`session/transaction.rs`(1175)、`session/repository.rs`(1579)、`services/event.rs`(1558)、
`services/authorization.rs`(956)、整个 `events/`、整个 `app/`、以及除 `read`/`diff` 外的全部 `tools/`。

这是本计划 Phase 0 不可跳过的原因：**没有安全网就不能改这些文件。**

### 2.2 模块依赖现状（排除测试文件的引用计数）

```
runtime  ↔ operations   58 / 58      services   → runtime  50（经 facade 仅 3）
runtime  ↔ events       25 / 16      operations ↔ session   27 /  4
runtime  ↔ session      17 / 12      operations ↔ services  23 /  6
runtime  ↔ tools         1 / 11      runtime    ↔ app       10 / 23
```

下层绕过 facade 直达 runtime 内部的路径：
`runtime::capability::`(31)、`runtime::operation::contract::`(25)、`runtime::operation::control::`(24)、
`runtime::snapshot::`(5)、`runtime::public_error::`(5)、`runtime::operation::admission::`(4)。

**这不是"共享错误类型"那种表面环。**但好消息是：被引用的绝大多数是**类型定义**而非行为，
搬迁是机械操作。逐符号统计（Phase 3 的输入）：

| 符号 | 引用数 | 归属 |
| --- | ---: | --- |
| `OperationRootTerminalEvidence` | 20 | → `kernel::operation` |
| `OperationKind` | 11 | → `kernel::operation` |
| `OperationCapabilitySnapshot` | 12 | → `kernel::capability` |
| `CapabilityGeneration` / `InstalledCapabilityGeneration` | 7 | → `kernel::capability` |
| `OperationControl` / `OperationCancellationHandle` / `PromptControl*` | 9 | → `kernel::control` |
| `ModelCapability` / `ActorId` / `Session{Read,Write}Capability` | 8 | → `kernel::capability` |
| `OPERATION_DESCRIPTOR_REVISION` / `product_terminal_operation` | 9 | → `kernel::operation` |
| `FilesystemCapability` / `FilesystemTarget` | 3 | → `platform::fs` |

### 2.3 文件体量现状（≥900 行）

```
3227  session/service.rs          1429  runtime/capability.rs
1938  runtime/snapshot.rs         1315  app/session.rs
1667  runtime/client/connection.rs 1296  runtime/operation/control.rs
1653  operations/prompt/context.rs 1215  app/embedding.rs
1579  session/repository.rs        1197  session/replay.rs
1558  services/event.rs            1175  session/transaction.rs
1524  runtime/client/projection.rs 1063  runtime/operation/contract.rs
                                    956  services/authorization.rs
                                    936  operations/self_healing_edit/runner.rs
                                    915  events/mod.rs
```

---

## 三、成功标准

全部完成时必须**同时**满足。

### 3.1 工程闸门

- `cargo fmt --all -- --check` 通过。
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` 通过。
- `cargo test --workspace --all-features` 通过。
- 上述三条固化成 `scripts/gate.sh`，任何 Phase 的 Gate 都跑它。

### 3.2 Correctness

- 第四章 B-01 ~ B-15 全部关闭，每项**至少一个确定性回归测试**（不接受"手动验证过"）。
- 取消语义可判定：任一 operation 被取消后，`bash`/`check command`/`write`/`edit` 均在有界时间内
  完成副作用回收；不存在"operation 已 aborted 但子进程仍在写文件"的窗口。
- 不存在从 async 上下文发起的同步 fsync。

### 3.3 依赖与边界

- **`crates/coding-agent` 内部模块依赖图无环**，由 `tests/module_layering.rs` 自动守卫（构建失败而非人工 review）。
- 分层单向：`kernel ← platform ← domain ← application ← app/api`，跨层反向引用为 0。
- 单文件 ≤ 900 行；`session/service.rs`、`runtime/snapshot.rs`、`services/event.rs` 三个隐性 aggregate 被拆解。
- `api::` 不再泄漏仓储物理布局（`session_storage_path` / `session_dir` 有替代方案或明确降级为 opaque handle）。

### 3.4 功能闭环

- 不存在"解析后只发 warning"的配置项：要么实现，要么从 schema 删除并在 CHANGELOG 说明。
- `switch_session` 要么实现，要么从 `CodingAgentCapabilities` 移除该字段。
- `read` 图片路径有明确结论（实现 / 显式拒绝 + 文档），不留 "yet"。
- 债务台账（第八章）清空。

### 3.5 可验证性

- `coding-agent` 可执行测试 ≥ 220（密度对齐 `desktop` 的 1/189 量级）。
- 覆盖必须包含：crash consistency（写入中断）、取消竞态、reconnect gap/lag、partial commit/InDoubt、
  projection 兼容性（golden events）。
- `tests/fixtures/` 下无孤儿 fixture。

---

## 四、已核实问题清单

两份 review 交叉合并，**全部经本次会话独立复核**（读源码或实际执行确认）。锚点行号会随改动漂移。

### 4.1 Bug

| ID | 问题 | 锚点 | 严重度 |
| --- | --- | --- | ---: |
| **B-01** | `write`/`edit` 取消后 `spawn_blocking` 逃逸出 mutation fence。`JoinHandle` 被 drop 不会终止 blocking closure，但 `tokio::sync::Mutex` guard 随外层 future 一起释放 → 下一次同路径 mutation 立刻拿到锁 → **两个 truncate/write/sync 并发操作同一文件**。且两次调用持有的是各自 `FilesystemTarget` 里不同的 `Arc<Mutex<File>>`，std Mutex 也不构成保护。 | `mutation_queue.rs:56,66-69`<br>`write.rs:52,116`<br>`edit.rs:518` | **高** |
| **B-02** | self-healing check command 三重缺失：无 timeout、无输出上限（`Command::output()` 全缓冲）、await 期间不响应取消（`run_typed` 只在**步骤之间**轮询）。叠加 `shutdown()` 会无限等待 active operation drain → `with_check_command("tail -f x")` 可永久冻结 runtime shutdown。另外 `shell_check_command` 用 `sh -c` **继承完整环境变量**，与 `bash` 工具的 `env_clear()`+allowlist 姿态相反。 | `runner.rs:690,704,729,753` | **高** |
| **B-03** | `bash` 取消路径不做 process-tree teardown。timeout 分支调 `terminate_child_process_tree`（`kill -KILL -<pid>`），取消分支只 drop future 靠 `kill_on_drop(true)` 杀直接子进程。配合 `process_group(0)`，`long_running_child & wait` 被取消后后台进程存活并被 reparent。 | `shell.rs:351,354,400,456,513` | **高** |
| **B-04** | `edit` 对非 UTF-8 文件 `from_utf8_lossy` 解码后**全文回写**。非法字节序列永久变成 U+FFFD，且损坏范围是整个文件而非目标 replacement 区域。全 crate 无任何二进制/编码有效性检测。 | `edit.rs:521,533,373` | **高** |
| **B-05** | `edit` fuzzy uniqueness 在错误的归一化空间计数。`fuzzy_find_text` 在 fuzzy 空间搜索，`count_occurrences(&base, &e.old_text)` 却拿**未 fuzzy 归一化**的 `old_text` 去 fuzzy 化的 `base` 里数 → 得 0 → 唯一性保护失效 → 静默改掉第一个匹配。触发条件：curly quote / NBSP / NFKC 差异 + 文件内多处匹配。 | `edit.rs:136,145,149` | **高** |
| **B-06** | 会话持久化在 tokio worker 线程上同步 fsync。完整链路：`services/authorization.rs:180 async fn` → `:254 persist_authorization_events`(sync) → `service.rs:279 SessionEventWriter::append`(sync) → `transaction.rs:176 execute` → `:207 response.recv()` 阻塞 → 写线程 `repository.rs:504 sync_data()`。全 crate 11 处 `spawn_blocking` **无一处**包住会话写。 | 见左 | 中高 |
| **B-07** | 写队列满即硬失败而非背压。`sync_channel` 容量 32，`try_send` 满了直接返回 `session transaction writer queue is full`。 | `transaction.rs:7,194` | 中 |
| **B-08** | filesystem binding 无 operation 级清扫。bindings map 只在 `take`/`discard` 单条移除，没有按 `operation_id` 的批量清理。已批准但未执行的 tool call（operation 中途取消）会永久留下持有打开 `File`/`Dir` 的条目 —— **fd 泄漏**，不只是内存。 | `capability.rs:104,409,443` | 中 |
| **B-09** | mutation queue key 不一致。已存在文件走 `canonicalize`，不存在的走原始路径（write 创建场景）。父目录含 symlink 时，同一文件的 create 与 overwrite 落到**两把不同的锁**上，串行化失效。相对路径还用 `std::env::current_dir()` 而非 capability root。 | `mutation_queue.rs:10-27` | 中 |
| **B-10** | `grep` 的 `context` 参数无上限。schema 只声明 `number`，运行时可得 `usize::MAX`，随后 `line_index + context` —— debug build panic，release build wrap 出错误范围。`read` 已用 `saturating_add` 修过同类问题，grep 没同步。 | `grep.rs:61,155` | 中低 |
| **B-11** | hydration 先物化完整 transcript 再截断。`read_events` 把整个 event log 读进 `Vec` 无上限，`hydrated_view` 再收集完整 transcript，之后才交给 projection 按 10,000 items / 32 MiB 截断。**最终态 bounded，初始化过程不 bounded。** | `repository.rs:559`<br>`service.rs:2107`<br>`projection.rs:888,909` | 中 |
| **B-12** | Windows 下 `env_clear()` + allowlist 缺 `SYSTEMROOT`/`COMSPEC`/`PATHEXT`/`WINDIR`，大量 Windows 命令会失败。crate 有完整 Windows 分支（Git Bash 探测、`CREATE_NO_WINDOW`、`junction` dev-dep），属支持目标。 | `shell.rs:129,346` | 中 |
| **B-13** | Mutex 中毒策略三套并存：100 处 `.lock().unwrap()`、8 处 `poisoned.into_inner()` 恢复、13 处 `map_err`。`snapshot.rs` 独占 44 处 unwrap —— 一次 panic 会把整个 runtime 级联成不可用。 | 全 crate | 中低 |
| **B-14** | `push_output` 每收到 8 KB 就重建整个 128 KB tail 字符串 + 跑一次截断 + 触发一次 UI 回调，大输出下 O(n²)。 | `shell.rs:291` | 低 |
| **B-15** | `write`/`edit` 截断后原地写，非崩溃原子（代码注释已承认，因为 rename 会脱离 authorization-bound handle）。 | `write.rs:90`<br>`edit.rs:383` | 低（记债） |

**已排除的疑似问题**（复核后确认不是 bug，记录以免重复排查）：

- `snapshot.rs:950` 的 `unreachable!` 确实不可达 —— `ConnectionLifecycle::ShuttingDown` 只在
  `request_shutdown` 内与 `runtime_lifecycle` **同锁同时**设置，而 `detach` 先过 `validate_runtime`。
- `wait_for_active_operation_to_drain` 的唤醒完备 —— `set_active_operation(None)` 会 bump
  `lifecycle_epoch` 并 `send_replace`。
- `read` 的 `offset+limit` 溢出已由 `867ac13` 的 `select_lines` 修复并覆盖测试。

### 4.2 功能缺口

| ID | 问题 | 锚点 |
| --- | --- | --- |
| **F-01** | `read` 遇图片只返回 `"Image content is not supported in headless mode yet; omitted."`。但 `image`+`base64` 依赖、`limits.rs` 四个图片限额、`CodingAgentPromptImage`/`CodingAgentImageContent` 均已就位 —— 「用户在 prompt 带图」通了，「模型自己读图片文件」没通。且是**先读完最多 5 MB 再判断扩展名丢弃**。 | `read.rs:140-143` |
| **F-02** | `CodingAgentCapabilities::switch_session` 永远 `Unsupported`，理由 "not exposed yet"。 | `facade/context.rs:421,510` |
| **F-03** | 六个 settings 解析、merge 后只发 warning：`collapse_changelog`、`transport`、`npm_command`、`http_proxy`、`websocket_connect_timeout_ms`、`warnings.anthropic_extra_usage`。其中 **`http_proxy` 用户以为配了代理，实际直连**。 | `config/settings.rs:513` |
| **F-04** | `test-support` feature 只转发依赖 feature，crate 自己的 `test_support` 是 `pub(crate)`，外部 integration consumer 用不了。命名与实现不一致。 | `Cargo.toml`、`lib.rs:217` |
| **F-05** | `api::` 泄漏仓储物理布局：`CodingAgentSession::session_storage_path()`、`CodingAgentSessionSummary::session_dir`。注释称 legacy，但 **cli 的 rpc 层在实际消费**（`rpc/commands.rs:855`、`rpc/prompt.rs:1265`），不是死代码。 | `facade/connection.rs:6`<br>`facade/context.rs:266` |

### 4.3 工程闸门缺口

| ID | 问题 |
| --- | --- |
| **E-01** | `cargo check --workspace --all-features` 失败：`desktop/src/app/devtools/native_replay.rs:749,758,781` 缺 `started_at`/`model_id`/`completed_at`。公开 DTO 演进没同步到可选 feature consumer。 |
| **E-02** | `clippy -- -D warnings` 失败（`redaction.rs:135` `repeat(1)`）。 |
| **E-03** | `cargo fmt --all -- --check` 失败，workspace 49 处差异。 |
| **E-04** | `tests/fixtures/client_projection/cross-adapter-events.json` 无任何测试引用。 |
| **E-05** | 公开 DTO 全部 public fields + struct literal 构造，新增字段是高频破坏性改动（E-01 的根因）。 |

---

## 五、目标架构

### 5.1 五层单向依赖

```text
┌─ app/ + lib.rs(api::) ────────────────────── L4 组合根与门面
│    startup / bootstrap / embedding / settings / theme / invocation
│    职责：解析配置、装配对象图、把内部类型投影成 api:: DTO
├─ application/ ───────────────────────────── L3 编排
│    runtime/     admission · dispatch · snapshot · client · lifecycle
│    operations/  prompt · agent · team · compaction · export · self-healing · delegation
│    services/    authorization · event dispatch
│    职责：决定"什么时候做什么"；持有状态机；只依赖 L0-L2
├─ domain/ ────────────────────────────────── L2 事实与持久化
│    session/     repository · transaction · replay · manifest · event
│    events/      ProductEvent 定义 · outbox
│    职责：事实的表示与落盘；不知道 operation 怎么被调度
├─ platform/ ──────────────────────────────── L1 无领域基础设施
│    process/     ProcessRunner（timeout · cancel · bounded output · process-tree teardown）
│    fs/          FilesystemCapability · FilesystemTarget · cap-std 封装 · mutation fencing
│    io/          bounded_io · redaction
│    time/        Clock · IdGenerator
│    职责：把 OS 能力包装成可测、可取消、有界的原语；不含任何产品概念
└─ kernel/ ────────────────────────────────── L0 零依赖领域词汇
     error.rs      CodingSessionError
     ids.rs        SessionId · OperationId · TurnId · ProfileId
     operation.rs  OperationKind · OperationDescriptor · OperationRootTerminalEvidence
                   · dispatch mode · admission class · durability · terminal policy
     capability.rs CapabilityGeneration · OperationCapabilitySnapshot · ModelCapability
                   · ActorId · Session{Read,Write}Capability · ToolCapabilitySet
     control.rs    OperationControl · OperationCancellationHandle · PromptControl*
     limits.rs
     职责：只有类型、常量和纯函数；不 import 任何 crate 内部模块
```

**规则**：依赖只能向下。`tools/` 归入 L1（`platform/fs` + `platform/process` 之上的薄 AgentTool 适配层），
`config/` `theme/` `profiles/` `workspace/` `resources/` 归入 L2（都是"读磁盘得到事实"）。

### 5.2 为什么这样切

- **L0 存在的唯一理由**：让 `services`/`operations`/`session`/`events`/`tools` 不再需要 `use crate::runtime::*`。
  2.2 节统计的 79 处跨模块引用里，绝大多数落在 L0，搬完即解环。
- **L1 存在的唯一理由**：B-01/B-02/B-03 是同一个缺失导致的 —— 没有统一的"有界、可取消、能回收进程树"的执行原语。
  `bash` 自己实现了一半（timeout + process group），self-healing check 一半都没有。抽成 `platform::process::ProcessRunner`
  后两处共用，取消语义由类型保证而不是靠每个调用点记得写。
  同理 `platform::fs` 持有 mutation fencing，B-01/B-09 从"调用方要记得包 queue"变成"拿不到 handle 就写不了"。
- **L2/L3 分界**：`session/service.rs` 现在同时知道 product event、prompt outcome、self-healing edit、
  finalization、event service、workspace migration、export —— 它是 L2 却在做 L3 的事。拆开后
  L2 只回答"这个事实怎么存/怎么读"。

### 5.3 aggregate 拆分目标

| 现状 | 拆分为 |
| --- | --- |
| `session/service.rs` 3227 | `session/commands.rs`（写命令）<br>`session/queries.rs`（hydration/view，带 bounded policy）<br>`session/finalize.rs`（finalization + recovery）<br>`session/persistence.rs`（repository/transaction 适配） |
| `runtime/snapshot.rs` 1938 | `runtime/client_registry.rs`（客户端注册与代际）<br>`runtime/lifecycle.rs`（runtime/connection 状态机）<br>`runtime/capability_state.rs`（capability generation 安装与转换）<br>`runtime/projection.rs`（snapshot 投影） |
| `services/event.rs` 1558 | `services/event/publish.rs`（发布与序号）<br>`services/event/durable.rs`（outbox/terminal draft）<br>`services/event/emit_*.rs`（按 family 分组的 emit 门面） |
| `runtime/capability.rs` 1429 | `kernel/capability.rs`（词汇）+ `platform/fs/capability.rs`（cap-std 与 binding） |
| `operations/prompt/context.rs` 1653 | 按 turn 阶段拆：`setup` / `stream` / `finalize` |

### 5.4 services 面向 ports

`services/` 目前直接引用具体的 `SessionService`、`PromptTurnContext`、`EventService`。
目标：定义窄 trait（`SessionWriter`、`EventSink`、`CapabilityQuery`），`services` 只依赖 trait。
好处不是抽象本身，而是 **authorization / event 这两个最需要 fault-injection 测试的模块变得可单测**。

---

## 六、执行任务与顺序

任务 ID 延续 `CAG-4xx`。**每个 Phase 一个或多个独立 commit，可单独 revert。**
每个任务的 Gate 都包含 `scripts/gate.sh`（fmt + clippy -D warnings + test --workspace --all-features）。

---

### Phase 0：闸门与安全网（已完成）

#### CAG-400　修复工程闸门 ✅

- 修 `desktop/src/app/devtools/native_replay.rs` 三处 transcript fixture（E-01）。
- 修 `redaction.rs:135` `repeat(1)`（E-02）。
- `cargo fmt --all`（E-03）—— 单独 commit，不与逻辑改动混合。
- 处置 `tests/fixtures/client_projection/cross-adapter-events.json`（E-04）：CAG-451 会用它做 golden，
  若届时不用则删除。
- 落地 `scripts/gate.sh`，内容为三条闸门命令。

**Gate**：`scripts/gate.sh` 全绿。

#### CAG-401　测试基础设施 ✅

没有这个，Phase 1 每个回归测试都要手搓一套环境，成本会压垮计划。

- `test_support/` 提供：
  - `TempSessionEnv` —— tempdir + 真实 `SessionLogStore`/`SessionTransactionWriter`，可注入 I/O 故障
    （写到第 N 字节返回 `ENOSPC`、fsync 失败、进程在 commit 中途"崩溃"）。
  - `FakeClock` / `SeqIdGenerator` —— 消除时间与 id 的不确定性。
  - `CancellationHarness` —— 在指定 await point 触发取消，用于 B-01/B-02/B-03 的确定性复现。
  - `ProcessFixture` —— 可控子进程（睡眠 / 疯狂输出 / 派生孙进程 / 忽略 SIGTERM）。
  - `ProductEventRecorder` —— 收集事件流，断言顺序、序号连续、durability class。
- 把 `test-support` feature 的语义定死（F-04）：要么 `pub`（对外提供 harness），要么改名为
  crate-internal 并从 `Cargo.toml` 移除 feature。**决策：改为 `pub`**，因为 cli/desktop 的
  integration test 需要能构造 session 环境。

**Gate**：harness 自身有 smoke test；`TempSessionEnv` 能复现一次 partial commit 并被断言到。

---

### Phase 1：Correctness（已完成；不动模块结构）

> 这一阶段刻意**不搬文件**。目的是让每个修复的 diff 只包含行为变化，便于二分。

#### CAG-410　统一 `ProcessRunner`（B-02, B-03, B-12, B-14，已完成）

在 `tools/` 内先建 `process_runner.rs`（Phase 3 再搬到 `platform/process/`）。契约：

```rust
pub(crate) struct ProcessSpec {
    program: ProgramKind,      // Shell { path } | Direct { .. }
    command: String,
    cwd: PathBuf,
    env: EnvPolicy,            // Inherit | AllowList(..) —— 显式，不再靠调用点记得
    timeout: Duration,         // 必填，无默认继承
    output_budget: OutputBudget,
}

pub(crate) async fn run(
    spec: ProcessSpec,
    cancel: &CancellationToken,
    on_update: Option<&ToolUpdateCallback>,
) -> ProcessOutcome;   // Completed{code} | TimedOut | Cancelled | Failed
```

要求：

1. 取消在 `run` **内部**用 `tokio::select!` 处理，退出前走与 timeout **同一条** teardown（Unix process group
   `killpg`，Windows Job Object）。不再用"drop future + `kill_on_drop`"充当进程树协议。
2. `killpg` 改用 `libc`（已是依赖）替代 shell 出去调 `kill`。
3. `EnvPolicy::AllowList` 补全 Windows 必需变量（`SYSTEMROOT`/`COMSPEC`/`PATHEXT`/`WINDIR`/`PROGRAMFILES`/
   `USERPROFILE`/`APPDATA`/`LOCALAPPDATA`）。
4. 输出增量式有界（ring buffer），`on_update` 节流（时间窗 + 字节阈值），消除 B-14 的 O(n²)。
5. `bash` 工具与 self-healing check **共用**此原语。check command 从此有 timeout、有输出上限、
   可取消、env 策略显式。

**测试**：睡眠进程被取消后进程组消失；派生孙进程的命令被取消后孙进程消失；疯狂输出命令内存有界；
timeout 与 cancel 走同一 teardown；check command 取消后 `shutdown()` 能返回。

#### CAG-411　mutation fencing 重做（B-01, B-09, B-15，已完成）

根因是「fence 的所有权在 async future 上，真正的写在 blocking closure 里」。修法：

1. **把 fence 与写入放进同一个 owner**：`platform::fs`（暂居 `tools/filesystem/`）提供
   `FileMutation::begin(target) -> MutationGuard`，`MutationGuard` 在 `spawn_blocking` **内部**
   获取与释放，外层 future 被 drop 不影响它。
2. 取消语义明确定义为：**mutation 一旦进入 blocking 阶段就是 atomic phase**，取消只影响"是否上报结果"，
   不影响"是否完成写入"。这是唯一能同时保证数据一致与可取消的语义，写进 doc comment。
3. key 归一化统一（B-09）：一律用 capability root 解析 + 逐段 canonicalize 已存在的父目录，
   缺失叶子不参与 canonicalize，保证 create/overwrite 同 key。
4. registry 清理改成 RAII（`MutationGuard::Drop`），消除取消/panic 泄漏。
5. B-15 记入债务台账：原地写非崩溃原子是 authorization-bound handle 的必然结果，
   除非改 binding 模型（允许 rename-into-place 并重新绑定）。**本计划不改**，Phase 6 复审。

**测试**：写入进行中取消 → 断言下一次同路径 mutation 必须等前一次完成；两个并发 write 同文件 → 断言串行；
父目录为 symlink 时 create+overwrite 命中同一把锁。

#### CAG-412　edit 正确性（B-04, B-05，已完成）

1. 非 UTF-8 **直接拒绝**：`String::from_utf8(raw)` 失败返回明确错误（提示文件编码 + 建议用
   `bash` + 编码感知工具）。不做 lossy，不做 byte-offset 替换（后者会把 fuzzy match 复杂度推高一个量级）。
2. uniqueness 与 search 使用**同一文本空间**：`count_occurrences(&base, &normalize_for_fuzzy(&e.old_text))`，
   且仅当 `used_fuzzy` 时归一化，非 fuzzy 路径保持原样。
3. 补 `matched` 重叠检查的 `saturating_add`。

**测试**：Latin-1/GBK 文件被拒绝且**内容零字节改动**；curly quote + 两处匹配 → 报 duplicate 而非静默改第一处；
NBSP、NFKC、trailing whitespace、多候选各一例。

#### CAG-413　工具数值参数收口（B-10，已完成）

- `grep` 的 `context`：schema 加 `maximum`，运行时再 cap，算术改 `saturating_add`。
- 全面扫描 `tools/` 下所有从 JSON 取数并参与算术的位置（`limit`、`offset`、`context`、`limit*2` 提示运算），
  统一走一个 `bounded_arg(args, key, default, max)` helper。

**测试**：`context: u64::MAX`、`limit: u64::MAX`、负数、浮点、字符串 —— 每种都断言不 panic 且行为合理。

#### CAG-414　capability binding 生命周期（B-08，已完成）

- `FilesystemCapability` 增加 `discard_operation_bindings(operation_id)`，在 operation 终结
  （committed / aborted / failed 三条路径）统一调用。
- binding 表加**条目上限**与创建时间，超限时拒绝新绑定并发 diagnostic（防御性，避免泄漏变成静默 fd 耗尽）。
- 增加 `bound_len()` 供测试断言。

**测试**：授权通过但 operation 立即取消 → 断言 binding 表回到 0；断言 fd 数不增长。

**Phase 1 Gate**：`scripts/gate.sh` 全绿 + B-01~B-05、B-08、B-10、B-12、B-14 各自的回归测试全绿。

---

### Phase 2：异步边界与失败策略（已完成）

#### CAG-420　消除 async 上下文的同步 fsync（B-06，已完成）

- `SessionTransactionWriter` 的 reply 通道从 `std::sync::mpsc::sync_channel` 改为 `tokio::sync::oneshot`。
- `execute` 提供两个入口：`execute_async(&self) -> impl Future`（默认）与 `execute_blocking`（仅供
  真正的同步上下文，如 `Drop`/shutdown 收尾）。
- 沿调用链把 `SessionEventWriter::append`、`SessionService` 的写命令改成 `async fn`，
  或在无法改的调用点显式 `spawn_blocking`（**不允许**默默阻塞）。
- `authorize_with_event_writer` 是最热的路径，优先改。

**测试**：在 `current_thread` runtime 上跑一次带 tool authorization 的完整 prompt —— 当前会冻结，改后必须通过。
这个测试同时把"public API 只要求 active Tokio runtime"这句承诺变成可验证的。

#### CAG-421　写队列背压（B-07，已完成）

- `try_send` → 有界等待（`send_timeout` 或 async 版本 + 超时）。
- 超时后不再返回裸错误，而是产出结构化降级：`SessionWriteFailure { reason: QueueSaturated, .. }`，
  并发 diagnostic product event，让适配器能显示"持久化落后"而不是整个 operation 失败。
- 容量 32 → 依据实测调整，并加注释说明依据。

**测试**：注入慢 fsync（每次 200 ms），并发提交 100 个 checkpoint → 断言无事件丢失、无硬失败。

#### CAG-422　Mutex 中毒策略统一（B-13，已完成）

- 定一条规则并全 crate 执行：**中毒即视为不变量已破坏**，统一走
  `lock().unwrap_or_else(|p| p.into_inner())` + 一次性 diagnostic，**还是** 统一 map 成
  `CodingSessionError::Resource`。
- 决策：**统一为 `map_err` → `CodingSessionError::Resource`**，理由是 `snapshot.rs` 的 44 处 unwrap
  会把单点 panic 放大成整个 runtime 不可用，而 runtime 的其余部分（client registry、lifecycle）
  完全有能力对单个 session 的失败做降级。
- 无法返回 Result 的位置（`Debug` impl、`Drop`）用 `into_inner()` 并注明。

**Gate**：`grep -c "\.lock()\.unwrap()"` 为 0；新增一条 clippy lint 或 CI grep 守卫。

---

### Phase 3：依赖解环（已完成；本计划的结构核心）

> **纯搬迁与逻辑改动分离在不同 commit**，便于二分。

#### CAG-430　抽取 `kernel/`

> 执行校正（2026-08-02）：结构索引证明原清单把三个含运行时所有权的 aggregate 误列为
> “零依赖词汇表”：`CodingSessionError` 直接携带 self-healing runner DTO，
> `OperationCapabilitySnapshot` 直接持有 filesystem/shell authority，`OperationControl` 直接持有
> `SnapshotCoordinator`。CAG-430 因此只搬纯值类型；self-healing payload 先抽为 kernel value，JSON/application conversion 留在外层，
> capability authority bundle 在 CAG-431 搬完 platform handle 后拆分，control state machine 归
> application，`kernel/control.rs` 只接管 command/identity/cancellation value。不得为了匹配目录名
> 把这些反向依赖原样搬进 kernel。

按 2.2 节的符号清单机械搬迁，不改任何逻辑：

```
kernel/error.rs       ← runtime/facade.rs 的 CodingSessionError
kernel/ids.rs         ← session/id.rs 的 id 类型（Clock/IdGenerator 归 platform）
kernel/operation.rs   ← runtime/operation/contract.rs 的 descriptor 表与 OperationRootTerminalEvidence
                        + runtime/operation/control.rs 的 OperationKind
kernel/capability.rs  ← runtime/capability.rs 的 CapabilityGeneration / OperationCapabilitySnapshot
                        / ModelCapability / ActorId / Session*Capability / ToolCapabilitySet
kernel/control.rs     ← runtime/operation/control.rs 的 OperationControl / CancellationHandle / PromptControl*
kernel/limits.rs      ← limits.rs（原样）
```

约束：`kernel/` 内**不允许出现 `use crate::` 除 `crate::kernel::`**，用 CAG-432 的守卫强制。

**Gate**：`services`/`operations`/`session`/`events`/`tools` 对 `crate::runtime::` 的引用降到 0；
测试数量与结果不变（纯搬迁）。

#### CAG-431　抽取 `platform/`

```
platform/process/     ← CAG-410 的 ProcessRunner
platform/fs/          ← runtime/capability.rs 剩余的 cap-std 部分（FilesystemCapability/Target/binding）
                        + CAG-411 的 mutation fencing
platform/io/          ← bounded_io.rs + redaction.rs
platform/time/        ← session/id.rs 的 Clock / IdGenerator
tools/                保留，但降级为「AgentTool 适配层」：只做 JSON 参数解析 + 调 platform + 组织输出
```

**Gate**：`tools/` 下不再有直接的 `std::process` / `cap_std` / `tokio::fs` 调用；
`platform/` 不引用 `domain`/`application`。

#### CAG-432　依赖方向守卫

`tests/module_layering.rs`：解析 `src/**/*.rs` 的 `use crate::` 语句，对照分层表断言无反向引用、无环。
失败信息要直接指出"文件 X 第 N 行从 L2 引用了 L3 的 Y"。

同时把 `api_contract.rs` 扩展为 E-05 的守卫：断言公开 DTO 不可被下游 struct literal 构造
（加 `#[non_exhaustive]` 或私有字段 + constructor），避免再出现 E-01。

**Gate**：守卫测试存在且全绿；故意加一条反向 `use` 能让它失败（自检）。

---

### Phase 4：aggregate 拆分（已完成）

按 5.3 节执行。每个文件一个独立 commit。

- **CAG-440**　`session/service.rs` → commands / queries / finalize / persistence
- **CAG-441**　`runtime/snapshot.rs` → client_registry / lifecycle / capability_state / projection
- **CAG-442**　`services/event.rs` → publish / durable / emit_*
- **CAG-443**　`operations/prompt/context.rs` → setup / stream / finalize
- **CAG-444**　`services` 面向 ports（`SessionWriter` / `EventSink` / `CapabilityQuery` trait），
  authorization 与 event 变成可单测

**Gate**：单文件 ≤ 900 行；每个被拆的模块新增至少 3 个针对其状态机的 transition-table 测试。

---

### Phase 5：有界 hydration 与表示收敛（已完成）

#### CAG-450　bounded hydration（B-11）

- `read_events` 增加 `visit_events_rev` / budget 参数，从 active leaf 反向读取最近 N 项或 byte budget。
- `hydrated_view` 返回 `omitted_items` + continuation cursor。
- export 等确需完整 replay 的场景走独立 API（`SessionExport`），与 UI bootstrap 分开。
- projection 的 10,000 / 32 MiB 上限保留为二次防线。

**测试**：构造 100k 事件的 session，断言 bootstrap 时间与峰值内存有界，且 `omitted_items` 正确。

#### CAG-451　事实表示转换集中

`AgentEvent → ProductEventDraft → SessionEventEnvelope → SessionReplay/TranscriptItem →
CodingAgentSessionTranscriptItem → CodingAgentClientProjection` 这条链的转换现在散在 5 个文件。

- 建立 `domain/projection/` 集中所有 `From`/`TryFrom`。
- 建立 **golden schema 测试**：`tests/fixtures/` 存一批固定的事件序列 + 期望的各级表示，
  任何一级 DTO 变更都会让 golden 失败。`cross-adapter-events.json`（E-04）在这里接线。

**Gate**：转换逻辑集中；golden 测试覆盖每种 `ProductEventKind`。

---

### Phase 6：功能闭环与债务清算（已完成）

- **CAG-460**（F-01）`read` 图片：实现 base64 image content 返回（依赖与限额已就位），
  并把扩展名判断提到读文件**之前**。若决定不实现，则删掉 `image` 依赖与四个图片限额常量，
  错误信息改为明确拒绝而非 "yet"。**默认决策：实现。**
- **CAG-461**（F-02）`switch_session`：**决策为删除该字段**。已核实 desktop 实际通过
  `open_target(&session_id)` + `CodingAgentSessionOpenTarget` 打开新 session 完成切换
  （`desktop/src/runtime/worker/mod.rs:468`），并不需要 `CodingAgentSession` 上的 switch 操作。
  该字段还镜像到了 RPC 协议层并同样永远 unsupported（`cli/src/protocol/types.rs:1019,1105`），
  删除需同步这两处 —— 这是一个泄漏到对外协议的永假能力位，留着只会误导客户端。
- **CAG-462**（F-03）settings 债务：
  - `http_proxy` / `websocket_connect_timeout_ms` → **实现**（下沉到 `ai` crate 的 HTTP 传输配置）。
  - `transport` / `npm_command` / `collapse_changelog` / `warnings.anthropic_extra_usage` → **从 schema 删除**，
    在 CHANGELOG 与 README 说明是 TS 版遗留。
- **CAG-463**（F-05）仓储权威收口：`session_storage_path()` / `session_dir` 改为返回
  opaque `SessionStorageHandle`，暴露 cli 真正需要的操作（打开日志、导出路径）而非裸 `PathBuf`。
  需同步改 `cli/src/rpc/commands.rs:855`、`cli/src/rpc/prompt.rs:1265`。
- **CAG-464**　清空第八章债务台账；更新 `docs/architecture.md` 的 coding-agent 章节与
  `crates/coding-agent/README.md`（分层图、api 边界、取消语义、bounded hydration 契约）。

**Gate**：旧的 Rust runtime unsupported-setting 诊断文本全仓为 0；债务台账为空；架构文档与代码一致。

---

## 七、风险与回滚

| 风险 | 处置 |
| --- | --- |
| Phase 1 改 correctness 时缺少判据 | CAG-401 先建 harness；未完成不进入 CAG-410 |
| `ProcessRunner` 统一后 bash 行为漂移（输出格式、退出码语义） | CAG-410 先为**现有** bash 行为补 golden 测试，再重构；golden 不允许随重构更新 |
| mutation "atomic phase" 语义变更影响上层取消体验 | 语义写进 doc comment 与 README；`agent-core` 侧的 tool deadline 行为一并复核 |
| Phase 3 纯搬迁引入 `use` 循环或编译爆炸 | 搬迁与逻辑改动分离 commit；CAG-432 守卫在同一 Phase 内落地 |
| Phase 4 拆分改变状态机行为 | 每个拆分先补 transition-table 测试再动刀，测试不随重构更新 |
| B-06 改 async 波及面大（调用链长） | 允许分两步：先 `spawn_blocking` 包住（行为等价、立即消除阻塞），再逐步 async 化 |
| CAG-463 破坏 cli | cli 改动与 coding-agent 改动同一 commit，`--workspace --all-features` 是 Gate |
| Phase 5/6 与前置计划的 `api::` 语义冲突 | 本计划显式允许改 `api::` 语义；变更集中在 CAG-461/463 两个任务，单独评审 |

**每个 Phase 独立 commit，可单独 revert。**Phase 1 的五个任务共享 CAG-401 的 harness，
建议连续完成后整体评估，不中途切到 Phase 2。

---

## 八、债务台账

当前未结债务：**无**。

CAG-464 已处理完本计划产生的两项记录：D-01 经复审关闭，rename-into-place 在当前
opened-object capability binding 下会改变授权对象身份，因此不引入；现有保证明确为
mutation fence 内完成原地写与 `sync_all`，不承诺 whole-file crash atomicity。D-02 经
consumer 审计关闭：CLI/Desktop 均未消费 `coding-agent/test-support`，该 feature 和公开
module 已删除，仍有价值的 fixture 与故障注入测试保留为 crate 私有 unit-test 资产。

原 D-04（命名前缀/API 分类）和 D-05（其他 crate 测试密度）从一开始就是第九章明确的
独立决策/范围外事项，不是本计划延期产生的执行债务。

---

## 九、明确不做的事

- 不改 crate 间分层与依赖方向（`ai ← agent-core ← coding-agent ← cli/desktop`）。
- 不改 event sourcing 作为状态权威。
- 不做 `CodingAgent*` 前缀改名（独立 API 命名决策）。
- 不改 `api::` 的九个分类子模块划分；只在 CAG-461/463 两处改语义。
- 不为覆盖率而恢复 `54c9349` 删除的历史测试 —— 新测试按 3.5 节的**覆盖维度**写，不按行数凑。
- 不处理 `ai` / `agent-core` 的测试密度（独立质量计划）。

---

## 十、任务索引

| Phase | 任务 | 关闭的问题 |
| --- | --- | --- |
| 0 | CAG-400 修复工程闸门 | E-01 E-02 E-03 E-04 |
| 0 | CAG-401 测试基础设施 | F-04（部分） |
| 1 | CAG-410 统一 ProcessRunner | B-02 B-03 B-12 B-14 |
| 1 | CAG-411 mutation fencing 重做 | B-01 B-09 |
| 1 | CAG-412 edit 正确性 | B-04 B-05 |
| 1 | CAG-413 工具数值参数收口 | B-10 |
| 1 | CAG-414 capability binding 生命周期 | B-08 |
| 2 | CAG-420 消除同步 fsync | B-06 |
| 2 | CAG-421 写队列背压 | B-07 |
| 2 | CAG-422 Mutex 中毒策略统一 | B-13 |
| 3 | CAG-430 抽取 kernel/ | 依赖成环 |
| 3 | CAG-431 抽取 platform/ | 依赖成环 |
| 3 | CAG-432 依赖方向守卫 | E-05 |
| 4 | CAG-440~444 aggregate 拆分 | 隐性 aggregate |
| 5 | CAG-450 bounded hydration | B-11 |
| 5 | CAG-451 表示转换集中 | E-04 E-05 |
| 6 | CAG-460 read 图片 | F-01 |
| 6 | CAG-461 switch_session | F-02 |
| 6 | CAG-462 settings 债务 | F-03 |
| 6 | CAG-463 仓储权威收口 | F-05 |
| 6 | CAG-464 债务清算与文档 | D-01 D-02 |

---

<sub>文档版本：1.0 | 基线 commit：867ac13 | 来源：Claude Opus 5 review + GPT Codex review，全部条目经独立复核</sub>
