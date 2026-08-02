结论：`coding-agent` 并不是“主体功能没做完”的半成品，核心链路——operation admission、prompt/agent/team 执行、capability snapshot、session event log、durable outbox、recovery、client snapshot/reconnect——都已经形成了完整闭环，而且设计意图相当明确。

但当前实现确实存在几项值得优先处理的潜在 bug，其中有 3 项会影响取消、进程回收或文件一致性；另有 1 项 `--all-features` 集成构建已实际失败。架构方面可以概括为：

> 概念模型清楚，代码依赖边界不够清楚；可靠性设计很强，但实现规模和层间耦合已经开始拖累可维护性。

我的主观评分：

| 维度 | 评价 |
|---|---:|
| 架构概念清晰度 | 7/10 |
| 运行时可靠性设计 | 7.5/10 |
| 当前实现可维护性 | 5.5/10 |
| 测试充分度 | 4/10 |
| 综合 | 6/10 |

以下按严重程度说明。

## 高优先级问题

### 1. 文件写入取消时，mutation queue 可能提前释放，导致同一文件出现并发写和数据损坏

`write`/`edit` 用全局 per-file queue 串行化 mutation。问题在于 queue guard 只活在异步 future 中：

- [mutation_queue.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/mutation_queue.rs:56) 在 `operation().await` 期间持锁；
- [write.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/filesystem/write.rs:52) 把实际 truncate/write/sync 放进 `spawn_blocking`；
- [write.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/filesystem/write.rs:116) 在 queue 内等待这个 blocking task；
- `edit` 也使用相同模式：[edit.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/filesystem/edit.rs:518)。

而 `agent-core` 在操作取消或 tool deadline 到达时，会直接退出 `tokio::select!` 并丢弃 tool future：[nodes.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/agent-core/src/agent/turn/nodes.rs:928)。

Rust 的 `spawn_blocking` 一旦真正开始运行，丢弃 `JoinHandle` 不会终止 blocking closure。因此可发生：

1. write/edit 已进入 blocking truncate/write；
2. operation 被取消；
3. 外层 tool future 被丢弃；
4. mutation queue guard 随 future 一起释放；
5. blocking write 仍在后台运行；
6. 下一次对同一路径的 mutation 获得 queue lock；
7. 两个 truncate/write/sync 并发操作同一文件。

这违背了 mutation queue 的核心不变量，有实际数据损坏风险。另一个次生问题是，future 在 [mutation_queue.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/mutation_queue.rs:69) 之前被丢弃时不会执行 `cleanup_queue`，被取消过的路径可能永久留在静态 `HashMap` 中，形成按路径增长的内存泄漏。

建议：

- mutation queue 的所有权必须覆盖 blocking closure 的真实生命周期，而不是覆盖 `JoinHandle.await` 的 future 生命周期；
- mutation 开始后可以将其定义为一个明确的 atomic phase：即使上层取消，也要在后台 write 完成并释放 fencing 后才能允许下一次 mutation；
- 或将 queue guard 以及文件写入全部移入同一个 blocking owner；
- 为“write 已开始后取消”和“edit 已开始后 deadline”增加确定性并发测试。

严重级别：高。

---

### 2. Self-healing edit 的 check command 无 timeout、无输出上限，且等待期间不响应取消

`SelfHealingEditRunner` 只在步骤之间轮询 cancellation：

- [runner.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/operations/self_healing_edit/runner.rs:690)
- 特别是直接等待 `ctx.run_check().await`，直到命令结束才再次检查 cancellation：[runner.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/operations/self_healing_edit/runner.rs:704)。

实际 check runner 使用：

```rust
Command::output().await
```

见 [runner.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/operations/self_healing_edit/runner.rs:729)。

这里有三个问题：

- 没有 timeout；
- 没有在 await 期间 select cancellation token；
- `output()` 会把完整 stdout/stderr 全部缓存在内存，完全没有上限。

因此：

- `with_check_command("tail -f ...")`、等待输入或死循环的命令可以永久占有 session write operation；
- 大量输出的命令可以持续膨胀内存；
- runtime shutdown 虽然请求取消，但随后会无限等待 active operation drain：[connection.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/runtime/facade/connection.rs:121)，因此可能永久卡在 `shutdown().await`。

这还与 operation contract 中“所有 Async operation 都是 Cancellable”的声明不完全一致。

建议不要另做一套 check process runner，而是抽取与 `bash` 共用的 bounded process execution primitive，统一具备：

- timeout；
- cancellation；
- bounded streaming stdout/stderr；
- Unix process-group / Windows Job Object 回收；
- 超限时的明确诊断；
- shutdown grace period。

严重级别：高。

---

### 3. Bash 正常 timeout 会杀 process group，但 operation cancellation 可能只杀直接 shell，遗留子进程

正常的 bash 内部 timeout 分支会调用 `terminate_child_process_tree`：

- [shell.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/shell.rs:386)
- [shell.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/shell.rs:456)。

Unix 上命令也被放进独立 process group：[shell.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/shell.rs:342)。

但是 operation cancellation 走的是 bash tool 最外层的 `tokio::select!`：

- [shell.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/shell.rs:503)。

取消分支直接返回错误，从而丢弃正在运行的 `bash_execute_real` future。此时只能依赖 `Child::kill_on_drop(true)`。`kill_on_drop` 面向直接 child，不等价于显式地向 Unix process group 发送信号；Windows 上也没有 Job Object 管理 descendants。

所以类似以下命令被取消后，后台进程可能继续存在：

```sh
long_running_child &
wait
```

影响包括：

- operation 已显示 aborted，但子进程仍修改文件或占用端口；
- runtime shutdown 完成后仍遗留工作负载；
- tool authorization 的作用周期与真实副作用周期不一致。

建议让 cancellation 进入 `bash_execute_real` 内部，并在退出前显式执行与 timeout 相同的 process-tree termination；不要用“丢弃 future + kill_on_drop”充当完整的进程树协议。

严重级别：高。

---

### 4. Workspace `--all-features` 构建已经失败，公开 DTO 演进没有同步到可选 consumer

`coding-agent` 的 transcript DTO 现在要求：

- User 增加 `started_at`；
- Assistant 增加 `model_id`、`completed_at`。

定义见 [context.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/runtime/facade/context.rs:352)。

但 `desktop-devtools` fixture 仍按旧结构构造：

- [native_replay.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/desktop/src/app/devtools/native_replay.rs:749)
- [native_replay.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/desktop/src/app/devtools/native_replay.rs:758)
- [native_replay.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/desktop/src/app/devtools/native_replay.rs:781)。

实际验证结果：

```text
cargo test --workspace --all-features
error[E0063]: missing field `started_at`
error[E0063]: missing fields `completed_at` and `model_id`
```

默认 `cargo test --workspace` 通过，因此这是可选 feature 的集成断裂，不影响默认构建，但说明“stable facade”目前只检查了 module 暴露边界，没有完整守住 workspace consumer compatibility。

公开 DTO 全部使用 public fields 和 struct literal，也会让以后新增字段成为高频破坏性改动。建议：

- 立即修正 `desktop-devtools` fixture；
- CI 增加 `cargo check/test --workspace --all-features`；
- 对稳定 DTO 使用 constructor/builder；
- 不希望下游 struct literal 构造的 DTO，考虑 private fields 或 `#[non_exhaustive]`；
- 对 transcript fixture 提供集中 test factory，避免每个 consumer 手写所有字段。

严重级别：高，但属于构建/集成问题，不是默认运行时问题。

## 中优先级问题

### 5. Edit fuzzy matching 的唯一性判断使用了不同归一化空间，可能误改第一个匹配项

`apply_edits` 一旦任一 edit 需要 fuzzy match，就把整个文件归一化成 `base`；但是唯一性计数仍使用原始 `e.old_text`：

- fuzzy base 构造：[edit.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/filesystem/edit.rs:136)
- fuzzy search：[edit.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/filesystem/edit.rs:145)
- occurrence count：[edit.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/filesystem/edit.rs:149)。

例如文件中有两处 ASCII `"x"`，而传入的 `oldText` 是 typographic `“x”`：

- fuzzy normalization 会把 `“x”` 转成 `"x"`；
- search 能找到第一处；
- `count_occurrences(base, "“x”")` 却得到 0；
- 唯一性保护不会报错；
- 第一处会被静默修改。

唯一性判断应对 `normalize_for_fuzzy(old_text)` 计数，并使用与最终 replacement offsets 完全相同的文本空间。还应覆盖 Unicode NFKC、curly quote、non-breaking space、trailing whitespace 和多候选测试。

严重级别：中。

---

### 6. Edit 对非 UTF-8 文件使用 lossy conversion，可能在局部编辑时破坏文件其他内容

[edit.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/filesystem/edit.rs:521) 使用：

```rust
String::from_utf8_lossy(&raw)
```

之后重新写回整个文件。任何非法 UTF-8 byte sequence 都会被替换为 U+FFFD，即使它根本不在目标 replacement 范围内。

对代码文件来说，更安全的行为是：

- 非 UTF-8 直接拒绝，并提示使用 binary-aware 工具；
- 或在 byte offsets 上执行 exact replacement；
- 不能用 lossy decode 后全文件覆写。

严重级别：中，主要表现为非常隐蔽的文件损坏。

---

### 7. Grep 的 `context` 参数没有最大值，存在整数溢出

工具 schema 只声明 `context` 为 number，没有 maximum。执行时可将 `u64::MAX` 转成 `usize::MAX`，随后直接执行：

```rust
line_index + context
```

见 [grep.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/filesystem/grep.rs:145)。

在 debug build 中，当匹配不在第一行且 `context == usize::MAX` 时会 panic；release build 中会 wrap，产生错误的 context 范围。

`read` 已经专门修复并测试了相似的 `offset + limit` 溢出，但 grep 没有同步使用 saturating arithmetic。建议：

- schema 增加合理 `maximum`；
- runtime 再做一次 cap；
- 改成 `line_index.saturating_add(context)`；
- `limit * 2` 等提示运算也使用 saturating multiplication。

严重级别：中偏低。

---

### 8. Client projection 虽然 bounded，但构造 projection 前已经完整 replay/分配整个 transcript

`transcript_snapshot()` 明确 hydrate 完整 transcript：[connection.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/runtime/facade/connection.rs:12)。

persistent session 会先 replay 并把全部 transcript 收集成 `Vec`：

- [service.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/session/service.rs:2107)
- [service.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/session/service.rs:2126)。

只有把这个完整 `Vec` 交给 client projection 后，才按 10,000 items / 32 MiB 截断：

- [projection.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/runtime/client/projection.rs:888)
- [projection.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/runtime/client/projection.rs:909)。

因此 projection 的最终状态是 bounded 的，但初始化过程不是 bounded 的。长生命周期 session 可能造成明显的启动延迟和瞬时内存峰值。

建议把 bounded policy 下沉到 replay/hydration：

- 按 active leaf 反向读取最近 N 项或 byte budget；
- 同时返回 `omitted_items` / continuation cursor；
- export 等确需完整 replay 的场景走独立 API；
- UI bootstrap 不应先物化完整 transcript 再截断。

严重级别：中。

## 明确存在的功能未闭环

这些不是通过推断得出的，代码中已经明确标记：

1. Headless `read` 遇到图片只返回“omitted”，不输出 image content：[read.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/tools/filesystem/read.rs:141)。

2. `CodingAgentCapabilities::switch_session` 永远返回 Unsupported，理由就是“not exposed yet”：[context.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/runtime/facade/context.rs:510)。

3. 以下 settings 会被解析、merge，但最终只发出“不受 Rust runtime 支持”的 warning：[settings.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/config/settings.rs:513)：

   - `collapse_changelog`
   - `transport`
   - `npm_command`
   - `http_proxy`
   - `websocket_connect_timeout_ms`
   - `warnings.anthropic_extra_usage`

这些配置项属于“兼容性占位”还是“承诺要支持的功能”，目前缺少显式的 debt/roadmap 标注。按仓库的工作原则，建议建立可追踪债务，而不是无限期保留“recognized but ignored”。

4. `test-support` feature 只转发了依赖 feature，但 crate 自己的 `test_support` 是 `pub(crate)`；外部 integration consumer 无法使用它。如果它只服务本 crate，feature 名称略有误导；如果目标是帮助下游测试，则实现不完整。

## 架构评价

### 做得好的部分

1. Public API 有明确边界

README 明确要求只依赖 `coding_agent::api`，并有 [api_contract.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/tests/api_contract.rs:1) 防止新增公开 root module。这比多数同体量 Rust crate 的 facade 管理更有纪律。

2. Operation contract 是很好的中心模型

每种 operation 的：

- dispatch mode；
- session/runtime access；
- capacity；
- durability；
- cancellation；
- child policy；
- terminal evidence

都集中在 `OperationDescriptor` 中，并有 exhaustiveness tests。这个方向是正确的，避免了 admission、dispatch、finalization 各维护一份彼此漂移的规则。

3. 持久化可靠性设计扎实

session event log、transaction writer、durable outbox、finalization decision、partial commit/recovery 被明确建模，而不是笼统地返回 I/O error。`FinalizationDecision` 与 `FinalizationCommitResult::{Committed, DefinitelyFailed, InDoubt}` 的区分尤其有价值。

4. Capability 与 authorization 有明确绑定

filesystem target 在 authorization 阶段绑定成 handle/fingerprint，执行阶段消费绑定，而不是授权一个字符串路径后重新解析。这很好地降低了普通 TOCTOU 和 path substitution 风险。

5. Client snapshot/reconnect 语义较完整

cursor、generation、ack、replay、live lag、fresh snapshot recovery 都有显式类型。对 Desktop/CLI 这类长期运行适配器，这是成熟的产品运行时设计。

### 主要架构问题

1. 当前所谓“分层”更多是目录分类，不是单向依赖

依赖方向存在大量往返：

- `runtime` 依赖 `operations`；
- `operations` 依赖 `runtime`、`services`、`session`；
- `session` 又依赖 `operations`、`runtime`、`services`；
- `services` 依赖 `operations`、`runtime`、`session`；
- `runtime::facade` 同时聚合所有模块的类型。

典型例子是 [session/service.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/session/service.rs:10)：一个 session persistence service 同时知道 product events、prompt outcome、self-healing edit、finalization、event service、workspace migration 和 export。

这意味着：

- 修改一个 operation outcome 往往需要同步 session event、replay、product event、client projection 和 public DTO；
- 很难独立测试某一层；
- `facade` 实际上成了全局类型交换站；
- 新功能很容易沿用现有模式，继续扩大环状依赖。

2. 核心文件过大，已经出现多个“隐性 aggregate”

整个 crate 约 51,699 行 Rust，其中：

- `session/service.rs`：3,227 行；
- `runtime/snapshot.rs`：1,938 行；
- `runtime/client/connection.rs`：1,667 行；
- `operations/prompt/context.rs`：1,653 行；
- `session/repository.rs`：1,579 行；
- `services/event.rs`：1,558 行；
- `runtime/client/projection.rs`：1,524 行；
- `runtime/capability.rs`：1,429 行。

这些文件通常同时承担 command handling、state transition、projection、validation、DTO conversion 和 diagnostics。其问题不是单纯“文件太长”，而是修改一个领域状态机时很难确定完整影响面。

3. 同一个事实存在多套表现形式，转换散落

一次 prompt/tool 生命周期大致经过：

```text
AgentEvent
  → ProductEventDraft/ProductEvent
  → SessionEventEnvelope
  → SessionReplay/TranscriptItem
  → CodingAgentSessionTranscriptItem
  → CodingAgentClientProjection
```

事件溯源场景需要多种 representation，本身没有问题；问题在于转换逻辑分散在 `services/event.rs`、`session/service.rs`、`session/replay.rs`、`runtime/client/context_fold.rs`、`runtime/client/projection.rs`。缺少集中 schema/compatibility tests 时，很容易出现这次 `started_at/model_id` 一类的 consumer 漂移。

4. Async 边界仍混入同步阻塞协议

[transaction.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/session/transaction.rs:176) 使用 bounded `sync_channel` 把写命令交给独立线程，但调用端通过 `response.recv()` 同步等待：[transaction.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/session/transaction.rs:207)。

它不会和 writer thread 本身死锁，但从 async operation 调用时会阻塞 Tokio worker。专用 multi-thread runtime 下通常可接受；current-thread runtime 或磁盘异常缓慢时会冻结同线程上的事件处理。既然 crate public API 只要求“active Tokio runtime”，这里最好用 async reply channel 或明确要求 multi-thread runtime。

5. “repository authority 不泄漏”的边界并不完全一致

`CodingAgentSession` 仍公开 `session_storage_path()`：[connection.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/runtime/facade/connection.rs:4)，`CodingAgentSessionSummary` 也公开 `session_dir`：[context.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/runtime/facade/context.rs:262)。

代码注释称这是 legacy adapter protocol，但它仍在稳定 facade 可见。这是一个已经泄漏出去的基础设施细节，会限制以后更换存储布局或采用非文件系统 repository。

## 测试与工程状态

执行结果如下：

- `cargo test -p coding-agent --all-features`：通过。
  - 31 个 unit tests；
  - 1 个 integration test；
  - 7 个 doctests。
- `cargo test --workspace`：通过。
- `cargo test --workspace --all-features`：失败，原因是 `desktop-devtools` transcript fixture 未同步新 DTO fields。
- `cargo clippy -p coding-agent --all-targets --all-features -- -D warnings`：失败，当前错误是 [redaction.rs](/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src/redaction.rs:135) 的 `repeat(1)`。
- `cargo fmt --all -- --check`：失败，workspace 内有较多既存格式差异，包括 `coding-agent`。
- 工作树保持未修改。

51.7k 行实现只有 32 个 crate-local tests，数量本身不能直接代表质量，但覆盖分布很不均匀：

- operation contract 有较好表驱动测试；
- read/diff/redaction 有少量边界测试；
- session repository/transaction、authorization、event outbox、client projection、shell、write/edit cancellation 基本缺少直接回归测试；
- `tests/fixtures/client_projection/cross-adapter-events.json` 当前没有被任何测试引用。

所以目前测试更像“少数关键不变量 + 大量上层 UI 行为测试”，对 crash consistency、取消竞态、reconnect gap、partial commit 和 projection compatibility 的保护仍不够。

## 建议的收敛顺序

第一阶段应先处理 correctness，不做大规模架构迁移：

1. 修复 write/edit cancellation fencing，确保 blocking mutation 完成前同路径 queue 不可重入。
2. 抽取统一的 `ProcessRunner`，让 bash 与 self-healing check 共用 timeout、cancel、output budget 和 process-tree teardown。
3. 修复 fuzzy uniqueness、拒绝 lossy edit、限制 grep numeric args。
4. 修复 `desktop-devtools`，把 workspace all-features 纳入 CI。
5. 为上述每项添加可确定复现的回归测试。

第二阶段再处理结构：

1. 把 operation/session/event 的中立 domain contracts 从 `runtime::facade` 和具体 runner 中抽离。
2. 将 `session/service.rs` 拆成：
   - session commands；
   - session queries/hydration；
   - finalization/recovery；
   - persistence adapter。
3. 将 `runtime/snapshot.rs` 拆成：
   - client registry；
   - submission state machine；
   - lifecycle/capability state；
   - reconnect buffer。
4. 让 `services` 面向小型 ports/traits，而不是直接引用具体 `SessionService`、`PromptTurnContext` 和 `EventService`。
5. 为每个状态机建立 transition-table tests、fault injection tests 和 cancellation tests。
6. 最后删除或实现已识别但不支持的 settings，关闭兼容性债务。

整体判断是：这个架构值得继续演进，不需要推倒重写。它最有价值的部分——typed operation contract、capability snapshot、event/outbox/recovery 语义——应该保留；需要收敛的是执行资源生命周期、异步取消语义、跨层依赖和测试策略。