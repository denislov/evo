# Phase 7 / ARC-730：Extension tests 与收尾接线

> 状态：完成
> 前序：ARC-700（extension-host 骨架）、ARC-710（user hooks）、ARC-720
> （MCP adapter）
> 目标：清偿 ARC-710 债务（subagent/compaction 产品接线、Stop gate
> additional_context 注入、end-to-end agent 循环测试），补齐 master plan
> ARC-730 清单的跨域测试矩阵（untrusted hook、timeout、输出洪泛、非法
> JSON、进程崩溃、重连风暴、MCP 热更新/取消/401 refresh/shutdown、
> extension 修改文件的来源归因与 hunk review）。

## 架构决策与接线点

### subagent 事件接线（`delegation::execution`）

- 接线点：`execute_agent` / `execute_team`（`crates/coding-agent/src/
  operations/delegation/execution.rs`）——所有子代 agent 执行（delegation
  tool 路径、approve 路径、folded 路径、agent/team invocation 内部委托）
  的唯一汇聚点，在此发出 `subagent_start`（child 实际启动后）与
  `subagent_stop`（phase=Gate，stop_reason 按成功 `completed` / 失败
  error 文本）。
- 穿透载体：`services::ports::ExtensionEventDispatch`（新类型）——把
  sink + 会话身份（session id / workspace root）打包成单个参数穿透
  纯函数式服务组合，避免每层加三个参数。`PromptTurnContext` 新增
  `extension_events_dispatch()` 访问器（由既有的 extension_events +
  extension_workspace_root + session_id 组装）。
- 参数链：`install_tool_executor`（从 context 取）→ `execute_tool_request_
  with_pending` → `execute_agent/execute_team`；`approve` 与
  `agent_invocation::run` / `team_invocation::run` 由
  `application/operation/{dispatch,execution}.rs` 从
  `runtime_host.session_identity()` + `extension_host.sink()` 构造后传入。
- 子代内部的嵌套委托经 context 的 `with_extension_events` 继续传递
  （`AgentInvocationContext` / `AgentTeamContext` 新增字段）。

### compaction 事件接线（`operations::compaction`）

- 接线点：`compaction::run`——`run_typed` 之前发 `pre_compact`
  （`entries_removed` = first-kept 条目（最后一个 id 条目）之前的
  transcript 条目数，与 `select_compaction_range` 语义一致）；成功后发
  `post_compact { resumed: true }`，失败路径发 `post_compact { resumed:
  false }`（Observe gate，产品行为不变）。
- 同一 `ExtensionEventDispatch` 参数；dispatch.rs 构造。

### Stop gate additional_context 注入（agent-core 消息流扩展）

- ARC-710 债务要求扩展 agent-core 消息流。`ShouldStopAfterTurnHook` 的
  返回类型从 `bool` 升级为 `ShouldStopAfterTurnResult { should_stop,
  additional_context }`（旧 bool 语义保留在 `should_stop` 字段内；
  `ShouldStopAfterTurnResult::stop()` / `continue_with(...)` 构造器）。
  这是 agent-core 公共 hook 签名的扩展——workspace 内唯一使用方是
  coding-agent（`services/runtime.rs` stop 桥接），已同步更新。
- 注入点：`agent/turn/nodes.rs::should_stop_after_turn`——`should_stop
  == false` 且 additional_context 非空时，按序把上下文作为
  `AgentMessage::UserText`（`hook_additional_context_{turn}_{index}`）
  追加进 `ctx.messages`，下一轮 provider 请求可见。
- coding-agent 桥接：`should_stop_after_turn` 闭包返回
  `should_stop: !decision.wants_continuation()` +
  `additional_context: decision.additional_context`（blocks 不入模型
  上下文——blocks 是给用户的意见，additionalContext 才是模型反馈）。

### 发现的 ARC-720 缺陷（本次测试暴露并修复）

- `mcp/transport.rs` 的 `request()`：`Ok(Ok(Err(_)))`（服务器显式返回
  JSON-RPC error response，如 401 `-32001`）与 `Ok(Err(_))`（响应通道
  消失）被合并映射为 `TransportClosed`，导致 `call_tool` 的
  `error.is_unauthorized()` 分支永远不可达——**401 → refresh → retry
  管线实际是死代码**。修复：`Ok(Ok(Err(error)))` 原样上抛错误，
  `Ok(Err(_))` 才映射 `TransportClosed`。ARC-720 的 `mcp_lifecycle`
  无 401 测试所以未被发现；本 ARC 的新增 401 集成测试直接覆盖该修复。

## 测试矩阵

### master plan ARC-730 清单逐项对照

| 清单项 | 覆盖状态 | 位置 | 覆盖内容 |
| --- | --- | --- | --- |
| untrusted hook | **本次新增** | `extension-host/tests/hook_runner.rs::untrusted_and_pending_extensions_never_run_hooks` | trust 三态：Trusted 执行 / Untrusted 不执行 + `extension_untrusted` 诊断 / NotDecided 不执行 + first-enable 请求；事件到达只跑可信 hook |
| timeout | ARC-710 已覆盖 | `hook_runner.rs::timeout_kills_process_tree`、`mcp_lifecycle.rs::per_tool_timeout_applies` | hook 进程树超时终止；MCP per-tool timeout |
| 输出洪泛 | ARC-710/720 已覆盖 | `hook_runner.rs::output_flood_is_truncated_not_lost`、`mcp_lifecycle.rs::output_flood_does_not_kill_transport` | hook 输出按 budget 截断报 `OutputLimited`；MCP 洪泛不杀 transport |
| 非法 JSON | ARC-710/720 已覆盖 | `runner/tests_interpret.rs`（`malformed_json_falls_back_to_exit_code`、`tool_unknown_decision_is_an_error` 等）、`mcp_lifecycle.rs::bad_json_lines_are_skipped_not_fatal` | hook 决策 JSON 解析失败回退退出码 / 未知 decision 显式报错；MCP 坏行跳过不 collapse transport |
| 进程崩溃 | ARC-710/720 已覆盖 | `hook_runner.rs::crashed_hook_reports_failed`、`mcp_lifecycle.rs::process_crash_triggers_reconnect_and_recovers` | hook 非零退出/信号 → Failed；MCP 子进程崩溃 → 重连恢复 |
| 重连风暴 | **本次新增** | `mcp_lifecycle.rs::reconnect_storm_stays_bounded_and_recovers` | crash-every-call 服务器：5 轮 Ready→crash→退避重连循环，断言退避封顶（单循环 ≤ max_backoff + slack）、总时长有界、shutdown 不被风暴阻塞 |
| MCP 工具列表热更新 | ARC-720 已覆盖 | `mcp_lifecycle.rs::tools_list_changed_refreshes_cache` | list_changed 通知 → 重新发现 + 版本递增 |
| 调用取消 | ARC-720 已覆盖 | `mcp_lifecycle.rs::call_cancellation_applies`、`shutdown_cancels_in_flight_call_and_terminates_child` | 调用方取消贯通；host shutdown 取消在途调用 |
| OAuth 401 refresh | **本次新增** | `mcp_lifecycle.rs::oauth_401_refreshes_token_and_retries_once`（成功）、`oauth_401_refresh_failure_falls_back_to_device_flow_and_surfaces_error`（失败）、`http_oauth_refresh_injects_refreshed_token_into_retry`（HTTP + 动态注入）、`http_static_authorization_is_used_without_store_token`、`http_dynamic_credentials_override_static_authorization`、`stdio_path_ignores_credential_headers` | 401 → refresh（mock token 端点）→ retry 成功且凭据轮换落库；refresh 失败 → device flow 兜底失败 → 结构化 `Unauthorized` + `mcp_refresh_failed` 诊断。**HTTP transport 按请求注入凭据**（修复：refresh 后新 token 从未进入请求）：fake HTTP server 记录每个请求的 `Authorization`，断言首次调用带旧 token、refresh 后重试带新 token；静态配置的 Authorization 在无动态凭据时使用、有动态凭据时被覆盖；stdio 不注入（现有 `oauth_401_refreshes_token_and_retries_once` 回归覆盖） |
| session shutdown | ARC-710/720 已覆盖 | `mcp_lifecycle.rs::shutdown_cancels_in_flight_call_and_terminates_child`、`hook_runner.rs::host_shutdown_cancels_in_flight_hook_and_drains` | shutdown 顺序 + 在途 hook/调用取消 |
| extension 修改文件归因 + hunk review | **本次修复（自动归因）** | `coding-agent/.../hooks_e2e_tests.rs::hook_edit_is_attributed_and_reviewable_end_to_end` + `services/hook_attribution_tests.rs`（5 项） | 真实 hook 进程写工作区文件 → host 注入的 [`HookLifecycle`] 观察点自动归因（before 记录基线 / after 对比磁盘并 `record_receipt(ChangeSource::HookEdit)`）→ `list_changes` 可见（source=hook_edit）→ `open_change` 可读 → `accept_hunk` 生效（accepted 变更离开 review 列表）→ 第二次 extension 修改 → `reject_hunk` 生效（文件回退 accepted baseline）。**修复**：原先测试手工构造 receipt，不验证产品自动归因；现已删除手工构造段落，全部由观察点自动归因（含 accept 后再归因、因果窗口消费 fs event、窗口过期保持外部归因等语义，见 `hook_attribution.rs` 文档）。extension-host 侧 observer 注入 seam 测试见 `dispatcher/tests_dispatcher.rs::hook_lifecycle_*`（before/after 顺序、失败不阻断） |
| agent 循环 e2e（ARC-710 债务②） | **本次新增** | `hooks_e2e_tests.rs::agent_loop_fires_observe_hooks_and_tool_gate_blocks_bash`、`stop_gate_block_continues_loop_and_injects_additional_context` | FauxProvider 驱动真实 session：Observe 事件（session_start/user_prompt_submit）到达 host 并执行 hook；Tool gate deny 真实阻塞工具（工具未执行、turn 正常结束、hook_run 诊断含 deny）；Stop gate block 让工具 turn 后的循环继续（2 个 provider 调用都被消费）且 `additionalContext` 注入第二次 provider 请求的消息流 |

### subagent / compaction 接线测试（本次新增）

| 位置 | 覆盖内容 |
| --- | --- |
| `operations/delegation/extension_events_tests.rs::delegated_agent_emits_subagent_start_and_stop_events` | 真实 `execute_agent`（writer 子代在 managed worktree 跑一轮）：sink 收到 `SubagentStart{subagent_type: "writer"}` + `SubagentStop{phase: Gate, stop_reason: "completed"}` |
| `operations/delegation/extension_events_tests.rs::delegation_without_extension_host_stays_noop` | 无 host（`ExtensionEventDispatch::none()`）时接线 no-op，子代照常完成 |
| `hooks_e2e_tests.rs::manual_compaction_fires_pre_and_post_compact_hooks` | 持久 session：prompt（写入 transcript）→ `Compact` 操作（FauxProvider 摘要）→ `pre_compact` + `post_compact` Observe hook 真实执行 |

### 行数 / 既有测试影响

- 生产文件：`team_invocation/runner.rs` 908 行（+12）与
  `dispatch.rs` 965 行（+40）——已更新
  `scripts/architecture/oversized-rust-debt.tsv` 基线（Phase 10 清偿，
  与既有 dispatch.rs 债务同批）。其余生产文件均 ≤900。
- 测试文件 ≤1200（max `hooks_e2e_tests.rs` 732）。
- 既有测试全部保持通过（唯一偶发失败为 ARC-710 已登记的
  `durable_rewind_restores_workspace_tracker_...` /
  `temp_session_env_repairs_a_partial_commit_on_reopen` session writer
  并行竞态，独立运行与 `--test-threads=1` 稳定全绿；本 ARC 新增测试
  全部 `current_thread` flavor，不加剧该竞态）。

## 债务清偿记录

| 债务（ARC-710/720 登记） | 清偿期限 | 状态 | 说明 |
| --- | --- | --- | --- |
| subagent 产品接线 | 不迟于 ARC-730 | **已清偿** | `execute_agent/execute_team` 发出 `subagent_start/stop`；`ExtensionEventDispatch` 单参数穿透全部调用链 |
| compaction 产品接线 | 不迟于 ARC-730 | **已清偿** | `compaction::run` 发出 `pre/post_compact`（entries_removed 与 range 选择语义一致） |
| Stop gate additional_context 注入 | 不迟于 ARC-730 | **已清偿** | agent-core `ShouldStopAfterTurnResult` + 消息流注入；coding-agent 桥接回填 |
| end-to-end agent 循环测试 | ARC-730 | **已清偿** | FauxProvider 驱动：Observe 事件、Tool gate deny 阻塞、Stop gate block 续跑 + context 注入 |
| 首次启用确认路径 | Phase 9 | **保持开放** | `first_enables()` 展示来源与能力；交互式放行 UI 属 CLI/Desktop（不变） |
| HTTP runner（ARC-710） | 最迟 Phase 10 | **保持开放** | 需共享 HTTP fetch 层下沉（不变） |
| HTTP SSE / streaming（ARC-720） | 出现真实需求时 | **保持开放** | HTTP 下 list_changed 不可达（不变） |
| 跨进程 credential flock（ARC-720） | 不迟于 Phase 10 | **保持开放** | 单进程 Mutex 串行化（不变） |
| tool 调用请求级取消 stdio（ARC-720） | 不迟于 Phase 10 | **保持开放** | 无 `notifications/cancelled`（不变） |
| dispatch.rs 行数债务基线 | Phase 10 | 基线更新 | 925 → 965；新增 `team_invocation/runner.rs` 908 |
| （新登记）MCP 401 响应在 transport 层被折叠为 TransportClosed | — | **已修复** | `Ok(Ok(Err(error)))` 原样上抛（见上），无遗留债务 |

## 完成定义核对

- `cargo test -p extension-host --all-features`：156 lib + 10 hook_runner
  + 16 mcp_lifecycle 全绿。
- `cargo test -p coding-agent --all-features`：231 lib + 2 api_contract +
  2 module_layering + 7 doc/example 全绿（新增 hooks_e2e_tests 4 项 +
  extension_events_tests 2 项）。
- `cargo test -p change-tracker --all-features`、`cargo test -p agent-core
  --all-features` 全绿（agent-core hook 类型扩展未破坏既有测试）。
- `cargo clippy -p extension-host -p coding-agent -p change-tracker
  -p agent-core --all-targets --all-features -- -D warnings` 通过；
  `cargo fmt --all -- --check` 通过。
- `scripts/architecture-gate.sh` 通过（21 依赖边不变；oversized 债务
  基线更新）。
- 无未登记 TODO、无 dead code；生产文件 ≤900（2 个已登记债务基线）、
  测试 ≤1200。
- 不破坏其他 crate 现有 API：agent-core 的 `ShouldStopAfterTurnHook`
  返回类型升级为 `ShouldStopAfterTurnResult`（workspace 内唯一使用方
  coding-agent 已同步；旧 bool 语义保留在 `should_stop` 字段）。

## 遗留问题

- 既有并行 flaky（非本任务引入，见 ARC-710 遗留问题）：session writer
  锁竞态，`--test-threads=1` 稳定全绿。
- `subagent_stop` 为 gate 事件（host 通道只记账）；产品侧对
  `subagent_stop` 的同步 gate 评估（`HookGate` 无对应入口）未接——与
  ARC-710 设计一致（subagent_stop 的 gate 语义待真实使用场景再驱动）。
- provenance：本 ARC 全部测试与接线为 Evo 自研（无 Grok 参考移植），
  不登记 `provenance/grok-build.md`。

## 后续

- Phase 8：code-intelligence / LSP（extension-host 的共享 HTTP fetch
  层下沉后清偿 HTTP runner 债务）。
- Phase 9：首次启用确认路径（CLI/Desktop）。
