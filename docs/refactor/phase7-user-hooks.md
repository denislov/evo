# Phase 7 / ARC-710：User hooks

> 状态：完成
> 前序：ARC-700（`extension-host` 骨架：版本化事件 DTO、host 生命周期、
> trust、budget、diagnostics、shutdown；`on_event` 槽位留给本 ARC）
> 目标：在 ARC-700 骨架上实现真正的 user hooks —— 事件覆盖
> session/prompt/tool/permission/stop/subagent/compaction/merge 八类；
> matcher（event/tool/path/profile + 确定优先级）；command runner（沙箱
> 进程、超时、输出限流、结构化结果）；Observe/Tool/Stop 三类 gate 的
> blocking 与 fail-open/fail-closed 策略；project hooks 共用 folder trust、
> 首次启用展示来源与能力；coding-agent 真实接入。

## 架构决策

### 事件模型（`event.rs` 扩展，不新建 DTO 家族）

ARC-710 在 ARC-700 的 `ExtensionEventPayload` 上**扩展字段与变体**，保持
同一版本化纪律：

- 新增 merge 事件：`merge_proposed` / `merge_applied`（`MergeProposed` /
  `MergeApplied` 别名，payload 带 proposalId / childWorktree / appliedEntries）。
  至此八类事件全部覆盖：session（session_start/end）、prompt
  （user_prompt_submit）、tool（pre/post_tool_use）、permission
  （permission_denied）、stop、subagent（subagent_start/stop）、compaction
  （pre/post_compact）、merge。
- tool 类 payload（pre/post_tool_use、permission_denied）新增可选 `path`
  字段（`#[serde(default)]` + `skip_serializing_if`，向后兼容）——matcher
  的 path 条件数据源；产品从 tool arguments 的 `path` 字段提取。
- 新增 `truncate_json_payload`：hook 事件信封经环境变量注入子进程，超
  `MAX_HOOK_PAYLOAD_BYTES`（128 KB，Grok `MAX_PAYLOAD_SIZE` 同量级）的
  JSON 值截断为字符串并标记，防止 `ARG_MAX` 越界。

### matcher（`matcher.rs`）

- 每个 hook **绑定一个事件**（`HookSpec::event`，即最严格的 event 条件）；
  matcher 对 tool / path / profile 三个维度做次级过滤，条件间 AND。
- tool / profile 条件复用 Grok 的 simple-vs-regex 语义：只含
  `[A-Za-z0-9_|]` 是精确名或 `|` 列表（避免 `^a|b|c$` 锚定错误），其余
  是非锚定正则；空模式 / `*` 匹配一切；空白模式是匹配不到任何东西的正则
  （不是 match-all，防止 deny gate 变成 deny-all）。
- path 条件是**前缀匹配**（`src/` 匹配 `src/main.rs`）：面向「目录子树」
  直觉，不引入 glob 转义复杂度。文档明示该语义。
- 缺省值 = 通配（fail-open，与 Grok `matcher_allows` 一致）；非法正则
  编译失败 → 配置层记录并**跳过该 hook**（fail closed，不静默放宽为
  match-all）。
- `MatchContext::from_event` 从事件信封提取 tool/path/profile（tool 来自
  tool 类 payload；profile 来自 SessionStart.agent_type /
  SubagentStart/Stop.subagent_type）。

### 确定优先级（`hook.rs` + `dispatcher.rs`）

- `HookSpec::priority: i32`（默认 0）。执行顺序：**priority 降序，同优先
  级按 hook 名称字典序升序**（`sort_hooks`，稳定、可预测）。该排序是
  dispatcher 的唯一执行顺序来源。
- 冲突规则（transition 测试钉死）：
  - Tool gate：按序咨询，**首个 deny 短路**（后续 hook 不再执行）。
    deny 优先于 allow：存在任一 deny 即拒绝，与执行顺序无关。
  - Stop gate：按序**全部执行**；首个 `continue: false` 生效
    （force_stop wins，后续丢弃），block 与 additional_context 全量保留。
  - Observe gate：按序全部执行，结果只进诊断。

### runner（`runner.rs`）

- 复用 workspace-runtime 的 `ProcessSpec` / `run`：Shell（元字符路由
  `sh -c`）或 Direct 程序；**每个 hook 进程必须携带
  `SandboxProfile::product_default(workspace_root)`**。平台能力不足
  （`SandboxCapability` 探测，`fs_supported == false`）时**不 spawn**，
  返回 `SandboxUnsupported`（Tool gate 据此 fail-closed；Observe/Stop
  fail-open）。
- **相对命令解析（本次修复，规则固定）**：
  - direct 分支：相对路径相对扩展目录解析（`command_path()`）。
  - shell 分支（`sh -c`）：若命令**第一 token 是相对路径**（含 `/` 或
    以 `.` 开头、非绝对路径），执行前把该 token 替换为相对扩展目录
    解析的**绝对路径**，其余命令文本（含前导空白）逐字节不变
    （`shell_command()`）。**绝不对 PATH 命令做解析**（`echo hi` 第一
    token 无 `/` 不以 `.` 开头 → 原样）；`$VAR` / `~` 开头、管道 /
    重定向开头的命令同样原样（shell 负责展开与语义）。修复前 `sh -c`
    在 workspace_root 下执行原始命令，`bin/format.sh --write` 会 127
    失败。
- 事件信封经**环境变量**注入（`EVO_HOOK_EVENT` / `EVO_HOOK_NAME` /
  `EVO_SESSION_ID` / `EVO_WORKSPACE_ROOT`）。与 Grok 的 stdin 注入不同：
  Evo 的 `ProcessSpec` 固定 `stdin = null`，env 是唯一不破坏共享进程契约
  的通道。环境是白名单（PATH/HOME/... + 注入变量），注入变量最后写入
  （覆盖同名白名单值），hook 无法伪造身份信号。
- 输出按 `OutputBudget`（64 KB / 2000 行）截断：洪泛不丢进程、不爆内存，
  截断显式报告为 `OutputLimited`（`OutputLimited` 只出现在进程本身完成
  时；超时/取消分别有 `TimedOut` / `Cancelled`）。**截断输出不驱动 gate
  决策（本次修复）**：`interpret_tool` / `interpret_stop` 在
  `output_limited` 时直接返回 `OutputLimited`，不再解析截断 stdout——
  洪泛后残留的 JSON 尾部不能产生 allow / deny / block（dispatcher 对
  Tool gate 按 fail-open 放行、Stop gate 按无信号处理，Observe 行为
  不变）。
- 结构化结果 `HookRunOutcome`：`Success` / `ToolDecision{allow,reason}` /
  `StopSignals{block, force_stop, additional_context}` / `TimedOut` /
  `Cancelled` / `OutputLimited` / `Failed`（崩溃、非零退出、非法决策
  JSON、信号终止）/ `SpawnFailed`（命令不存在）/ `SandboxUnsupported`。
- 决策协议（stdout JSON，Tool / Stop gate）：Tool 用
  `{"decision": "allow"|"deny", "reason"}`，exit 0=allow、exit 2=deny
  （stderr 作 reason 兜底）；Stop 用 `{"decision": "block"|"approve",
  "continue", "stopReason", "hookSpecificOutput.additionalContext"}`，
  exit 2+stderr=block。JSON 决策优先于退出码（与 Grok 一致）；未知
  decision 值是错误（typo 显式暴露，不静默 fail-open）。
- 超时 = `min(spec.timeout_secs, budget.max_run_secs)`（spec 声明受预算
  封顶；未声明用预算；预算 0=不限 → 默认 30s）。**预算取值 per-extension
  优先**（`HookSpec::budget`，host 装配时注入：全局合并预算作默认、
  manifest config 覆盖，见 `phase7-extension-host.md`）；未装配时用全局
  预算。**`RunDurationSecs` 维度由此强制**。

### gate（`dispatcher.rs`）

- 架构分工避免双跑：**host 事件通道只派发 Observe gate 事件**（session/
  prompt/post_tool_use/permission/subagent/compact/merge），串行 + budget
  记账；**gate 事件**（pre_tool_use / stop / subagent_stop）在 host 通道
  只记账，hook 执行由产品经 `HookGate::evaluate_tool/evaluate_stop` 同步
  驱动（一次事件只跑一次 hook）。
- 策略矩阵（`HookGate` transition 测试钉死）：

  | 结果 \ gate        | Observe                  | Tool                          | Stop                    |
  |--------------------|--------------------------|-------------------------------|-------------------------|
  | 无匹配 / allow     | 忽略                     | Allow                         | 无信号                  |
  | deny / block       | —                        | **Block（阻塞工具）**         | Block（阻止停止）       |
  | force_stop         | —                        | —                             | **ForceStop**           |
  | 崩溃 / 非法 JSON   | 记录                     | fail-open（放行）             | fail-open（正常停止）   |
  | 超时 / 取消 / spawn | 记录                     | fail-open（放行）             | fail-open（正常停止）   |
  | **输出截断（OutputLimited）** | 记录（且报告 OutputLimited） | **fail-open（放行）** | **无信号（正常停止）** |
  | **sandbox 不支持** | 记录                     | **fail-closed（拒绝工具）**   | fail-open（正常停止）   |

  唯一 fail-closed 类别是 sandbox 环境性失败：平台不能提供沙箱时 hook
  无法在安全边界内运行，Tool gate 拒绝工具调用（ARC-610「平台不支持时
  fail closed 或明确请求降级授权，不能静默变成 unrestricted」）。普通
  执行失败一律 fail-open（与 Grok 一致：hook 超时/崩溃不得阻塞正常工具
  调用；induced-failure bypass 不在威胁模型内）。
- `HookGate` 由 `ExtensionHost::gate()` 暴露（`Arc`），产品在 agent loop
  内同步 await 评估。

### host 集成（`host/mod.rs`）

- dispatch 槽位从同步 `FnMut` 升级为 `Fn(Arc<HostShared>, Event) ->
  BoxFuture`（`DispatchHandler`）；每个事件在独立 tokio task 中执行，
  panic 被 `JoinError` 捕获（fail closed，不传播）。
- `HostShared` 增加 `CancellationToken`：**shutdown 先取消在途 hook
  进程**（`cancel_in_flight`），再发 watch 信号；dispatch 退出 select 后
  drain 已入队事件。在途 hook 被终止（`Cancelled`），shutdown 不被长
  hook 阻塞。
- host `new` 时从启用扩展的 manifest `hooks` 数组解析 `HookSpec`
  （容错：坏 hook 记录 `hook_invalid` 诊断并跳过），构建 `HookRegistry`。

### coding-agent 真实接入

- `CodingAgentSessionOptions::with_extension_host_options(...)` 启用真实
  host（默认 Noop，既有行为不变）。`LiveExtensionHostPort` 适配器：
  `ExtensionHost::new` + `start`，`Drop` 触发 shutdown。
- `ExtensionHostView` trait 扩展为真实接口：`submit` / `notify_shutdown` /
  `join_shutdown` / `first_enables` / `gate`；`ExtensionEventSink` trait
  （submit + hook_gate）穿透到 operation 层，无 host 时 no-op。
- 事件发出点：
  - **session**：`from_services` / `from_transient` 打开时
    `session_start`；`shutdown_internal` 关闭时 `session_end` + host
    shutdown + join。
  - **prompt**：`operations::prompt` 的 `run_inner` 提交
    `user_prompt_submit`（Text invocation 带 prompt）。
  - **tool**：`services::runtime` 的 `before_tool_call` 桥接 —— Tool gate
    deny / sandbox 拒绝 → `BeforeToolCallResult { block: true, reason }`
    （**真实生效**）；allow 与失败 fail-open → 继续走产品 authorization
    （原 hook 槽位被链在 user hook 之后）。`after_tool_call` 提交
    `post_tool_use`（结果按摘要 JSON：isError/terminate/details，遵守
    「事件不携带输出内容」骨架约定）。
  - **permission**：`AuthorizationService` deny 决策点（Ask 用户拒绝 +
    Plan 自动拒绝）提交 `permission_denied`（**真实生效**）。
  - **stop**：`should_stop_after_turn` 桥接 —— 提交 Stop 事件 +
    `HookGate::evaluate_stop`；`wants_continuation`（block/context）→
    继续（`Some(false)`）；force_stop / 无信号 / 失败（fail-open）→
    正常停止（`Some(true)`）（**真实生效**）。
  - **merge**：`merge_worktree` 提交 `merge_proposed` / `merge_applied`。
  - **subagent / compaction**：事件 DTO 与 matcher 已就绪；产品接线点
    （`delegation::execution` / `operations::compaction`）**登记债务**——
    两处都是纯函数式服务组合，穿透 sink 需改多层签名，成本高于收益，
    且不影响八类事件的 DTO/派发完整性。
- project hooks：`extension_host_service` 装配时若未显式指定
  `project_dirs`，默认取 `project_root/.evo/extensions`；trust 判定复用
  `TrustStore`（folder trust 单一权威）。首次启用（NotDecided）的扩展
  经 `first_enables()` 在 session 打开时以诊断事件展示
  （extension_id / name / source / source_dir / capabilities）；**产品
  放行确认路径**（交互式 UI）登记债务（Phase 9 CLI/Desktop）。

## 与 Grok 参考实现的差异

1. **事件通道**：Grok stdin 注入信封；Evo 走环境变量（`ProcessSpec`
   固定 stdin=null）。
2. **matcher 维度**：Grok 只有 tool 条件；Evo 增加 path（前缀语义）与
   profile 条件。
3. **优先级**：Grok 按配置顺序执行；Evo 有 `priority` + name 字典序的
   确定排序。
4. **runner 安全**：Grok 无 sandbox；Evo 强制 `SandboxProfile` + 能力
   探测，sandbox 失败是唯一 fail-closed 类别。
5. **gate 结构**：Grok dispatcher 直接聚合；Evo 拆 host 通道（Observe）
   与 `HookGate`（产品同步调用），避免事件双跑。
6. **生命周期**：Grok load-and-fire；Evo host 有 shutdown 取消在途 hook、
   panic fail-closed、事件逐条独立 task。
7. **HTTP runner**：Grok 有完整 http runner；Evo **本任务不做**（见债务）。

## 完成定义核对

- `cargo test -p extension-host --all-features`：115 lib + 9 集成全绿。
- `cargo test -p coding-agent --all-features`：222 lib + 2 api_contract +
  2 module_layering + 7 doc 全绿（含新增 hooks_tests 5 项 + permission
  事件 1 项）。
- `cargo test -p agent-core --all-features`：77 + 1 全绿（hooks 类型签名
  未改动）。
- `cargo clippy -p extension-host -p coding-agent -p agent-core
  --all-targets --all-features -- -D warnings` 通过；`cargo fmt --all
  -- --check` 通过。
- 无未登记 TODO、无 dead code；生产文件 ≤900 行（max 833）、测试
  ≤1200 行（max 609）。
- 状态机（gate/matcher/lifecycle）transition-table 测试：
  `tool_gate_transition_table` / `stop_gate_transition_table` /
  `tool_gate_deny_short_circuits_later_hooks` /
  `stop_first_force_stop_wins_later_signals_dropped` /
  `sort_applies_priority_then_name`；异步 shutdown/cancel/panic：
  `host_shutdown_cancels_in_flight_hook_and_drains` /
  `cancellation_returns_cancelled` / `dispatch_panic_*`。
- architecture-gate：`extension-host → workspace-runtime`、
  `coding-agent → extension-host` 依赖边已登记；dispatch.rs 债务基线
  更新至 925（Phase 10 清偿）。

## 债务登记

### HTTP runner（验证债务，非阻塞；最迟 Phase 10 清偿）

执行状态：待清偿；最迟在 Phase 10 Final Gate 前完成并删除本债务。

- 本 ARC 只实现 command runner；`HandlerType::Http` 不在本任务开放。
- 理由：SSRF 安全 fetch 管线（scheme/redirect/DNS/IP 每跳重验证、loopback
  与 cloud metadata 拦截、内容长度与读取字节限制）目前在 `ai` crate 内部
  （Phase 6 / ARC-620）。extension-host 引入 `ai` 依赖会破坏目标依赖图
  （`extension-host → tool-runtime + workspace-runtime`）。下沉共享 HTTP
  安全层是 Phase 8+ 工作。
- 清偿证据：共享 HTTP fetch 层（独立 crate 或 ai 对外暴露的安全 fetch
  契约）存在且 extension-host 只依赖该层；`HandlerType::Http` + URL/SSRF
  校验测试；`run_http_hook` 与 `hook.http` 集成测试（超时/重定向/回环
  拒绝/输出限流）。

### 其余债务

| id | 清偿期限 | 说明 |
| --- | --- | --- |
| subagent 产品接线 | 不迟于 ARC-730 | `delegation::execution::execute_agent/execute_team` 提交 `subagent_start/stop`；需要把 `ExtensionEventSink` 穿透纯函数式服务组合（多层签名），收益/成本比低，事件 DTO 与 matcher 已就绪。 |
| compaction 产品接线 | 不迟于 ARC-730 | `operations::compaction` 提交 `pre/post_compact`；同上原因。 |
| 首次启用确认路径 | Phase 9 | `first_enables()` 已展示来源与能力（诊断事件）；交互式放行 UI 由 CLI/Desktop 完成。 |
| Stop gate additional_context 注入 | 不迟于 ARC-730 | block 语义已生效（继续运行）；`additionalContext` 回填模型上下文需扩展 agent-core 消息流，评估后登记。 |
| end-to-end agent 循环测试（FauxProvider 驱动 bash deny） | ARC-730 | gate 语义层测试完整；产品桥接（before_tool_call 链、should_stop_after_turn）由 hooks_tests + 代码审查覆盖，完整 agent 循环测试并入 ARC-730 跨域矩阵。 |
| dispatch.rs 行数债务基线 | Phase 10 | 918 → 925（session_identity + merge 接线），已登记 oversized-rust-debt.tsv。 |

## 遗留问题

- **既有并行 flaky（非本任务引入）**：`coding-agent` 的 session writer
  锁（`SESSION_WRITER_REGISTRY` + `event-journal` flock）在**高并行测试
  负载**下存在随机竞态：`durable_rewind_restores_workspace_tracker_...`
  与 `test_support::temp_session_env_repairs_a_partial_commit_on_reopen`
  等既有测试在 `--test-threads=16` 全量并行时偶发
  `journal already has a writer in another process`（约 2/10 轮次，
  **排除本 ARC 全部新增测试后同样出现**）。单独运行（含
  `--test-threads=1`）稳定全绿。根因在 session 事务层的全局 writer
  注册表与 flock 生命周期，修复超出本 ARC 范围；本 ARC 的 hooks_tests
  只是增加了并行负载、改变调度窗口。已用 `flavor = "current_thread"`
  降低 hooks_tests 自身的线程占用。

## 后续

- ARC-720：`max_concurrent_extensions` 强制（MCP 并发启用，见
  `phase7-mcp-adapter.md`「并发上限」；本次已完成）；manifest
  capabilities 驱动的 MCP 注册。
- ARC-730：跨域矩阵（untrusted hook、超时、输出洪泛、非法 JSON、进程
  崩溃、重连风暴、hunk 归因）；本任务的 runner/gate 测试是其基础。

## Hook 修改自动归因（ARC-730，本次修复）

- **注入 seam**（`extension-host/src/hook_lifecycle.rs`）：[`HookLifecycle`]
  trait（`before` / `after`，携带 event + hook spec；`after` 附运行结果），
  由 `ExtensionHostOptions.hook_lifecycle` 注入、`dispatch_observe` 在每个
  Observe hook 执行前后调用；默认 `NoopHookLifecycle`（行为不变），观察
  失败不阻断 hook 执行。extension-host 不依赖 change-tracker（归因逻辑
  在产品侧）。
- **自动归因**（`coding-agent/src/services/hook_attribution.rs`）：
  [`HookEditAttribution`] 实现该观察点——before 记录 tracker 已知文件
  （files 快照 ∪ facts 历史）的磁盘基线；after 对比磁盘，对 revision
  变化的文件生成 `ChangeReceipt` 并 `record_receipt(ChangeSource::HookEdit)`
  （fingerprint 与 accept/reject 同源、diff 有界）。**修复**：此前 hook
  修改不归因（ARC-730 的 hunk review 闭环未实现，测试手工构造 receipt）。
- **因果窗口语义**（ARC-410）：receipt 与 fs event 按
  `(path, after_exists, after_revision)` 双向匹配消费——hook 在窗口内
  归因时，先到或后到的 fs event 都被消费（归因 `HookEdit`）；窗口过期
  已归因 `ExternalEdit`/`ExternalEditOnAgentFile` 时不重新归因（先到
  先得，失败落 `hook_attribution_failed` 诊断）；tracker 未知的新文件
  由既有外部修改语义兜底。
- **handle 生命周期**：`ReviewService` 与 host 共享 handle 槽（tracker
  懒启动时填充、rewind 停用时清空）——观察点不长期持有 handle，避免
  watch channel 永不关闭导致 rewind 的 projection task join 悬挂。
- 测试：`services/hook_attribution_tests.rs`（5 项：未绑定 no-op、自动
  归因、accept 后再归因、窗口过期保持外部归因、窗口内消费 fs event）；
  `hooks_e2e_tests.rs::hook_edit_is_attributed_and_reviewable_end_to_end`
  （真实 hook 进程写文件 → 自动 `HookEdit` → review 可见 → accept/reject
  生效，不再手工构造 receipt）；extension-host 侧 seam 测试
  `dispatcher/tests_dispatcher.rs::hook_lifecycle_*`。
