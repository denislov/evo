# Phase 7 / ARC-700：抽取 `extension-host`

> 状态：完成
> 前序：Phase 6 Gate（长任务可查询和取消、provider resilience、web fetch SSRF）
> 目标：在稳定 tool runtime 之上开放外部扩展（user hooks、MCP provider），
> 不污染产品内核。本 ARC 只做 crate 骨架与 host 治理机制；事件派发
> （ARC-710）与 MCP adapter（ARC-720）由后续 ARC 完成，骨架为其预留
> 扩展点。

## 决策

### 版本化事件 DTO（`event.rs`）

- **信封 `ExtensionEvent` 带 `version` 字段**（`EXTENSION_EVENT_VERSION = 1`），
  与内部 `ProductEvent` 完全隔离。`version` 缺失按 1 读取
  （`#[serde(default = "default_event_version")]` 向后兼容）；
  host 在 `submit_event` 时校验版本，高于支持的版本拒绝（fail closed）。
- **payload 用 internally-tagged 变体**（`#[serde(tag = "kind")]`），`kind`
  值与 `ExtensionEventKind` 的 snake_case 一致。曾尝试 untagged +
  字段互斥方案，但 untagged 下「全可选字段变体会匹配任何对象」「未知
  字段默认忽略导致前序变体吃掉后序变体」两个问题无法在不牺牲字段级
  向后兼容的前提下解决（serde 的 `deny_unknown_fields` 不支持 variant
  级标注），因此放弃扁平 JSON，改用独立 payload 对象 + tag 判别：
  判别唯一、round-trip 可靠，代价是 JSON 多一层 `payload` 对象。
- **向后兼容约定**：可选字段用 `#[serde(default)]` + `skip_serializing_if`；
  新增 payload 变体必须给 `kind` 新值（不破坏既有变体反序列化）。
- **tool 相关 payload 复用 `tool-contract` 的 `ToolId`**（Phase 7 Gate：
  MCP 工具与内建工具共用同一 Tool contract）；非法工具名反序列化失败
  （fail closed）。
- 事件 kind 覆盖 ARC-710 的事件全集（session/prompt/tool/permission/
  stop/subagent/compaction），每个 payload 只承载骨架最小字段，
  业务字段由 ARC-710 扩展。
- 事件方向：产品 → 扩展（extension host 的 dispatch 槽位），事件不携带
  输出内容（输出走 cursor/预算通道，与 phase6 背景任务一致）。

### Config merge（`config.rs`）

- 来源优先级 `Managed > Project > Global`（`ExtensionSource` 同时用于
  discovery 记录与 config layer 优先级）。
- 合并规则（`merge_config_layers`，输入按高优先级在前）：
  - `enabled`：**AND** —— 任意层禁用则整体禁用（任何层可关掉扩展，
    安全优先）；
  - `budget` / `diagnostic_level`：**最高优先级层**覆盖 scalar；
  - `permissions`：所有层**并集**（低优先级层在前，保持确定性）。
- 实现：从低到高 fold `config.merged_with(&layer)`（`merged_with(self =
  低, higher)`）。首次实现曾把 fold 方向写反（低优先级覆盖高优先级），
  由 `merge_config_layers_applies_priority_order` 等测试捕获。
- 容错与 Grok 的 TOML layer 一致：无法解析的层记录
  `ExtensionError::InvalidConfig` 并跳过，其余层照常合并。
- host 的 `options.budget` 作为最低优先级「host-defaults」层参与合并，
  可被任何配置层覆盖。

### Trust 模型（`trust.rs`）

- 与 Grok 相同：项目级扩展复用「folder trust」单一权威，不建第二套信任
  数据库。骨架提供 `TrustStore` trait（ARC-710 由产品信任存储实现）+
  `InMemoryTrustStore`（测试/骨架用）。
- 判定边界：`Trusted`（精确匹配或位于信任目录之下）→ 启用；
  `Untrusted`（明确决定过且不信任，含祖先目录）→ 不启用 + 诊断；
  `NotDecided`（从未决定，首次启用）→ 不启用，产出 `EnableRequest`
  展示 DTO（来源 + 能力 + 预算），由产品决定放行（ARC-710 提供确认路径）。
- 与 Grok 差异：Grok 只有 legacy 迁移辅助函数；Evo 把「首次启用展示
  来源与能力」做成 DTO 契约（`CapabilityClaim` + `CapabilityRisk`），
  host 在 new 时自动收集 `first_enables`。

### Shutdown 顺序（`host.rs`）

确定性顺序（骨架阶段完整实现并测试）：
1. `handle.shutdown(reason)`：状态 `Running → Stopping`（新事件被拒，
   `submit_event` 返回 `ShuttingDown`），写 `host_shutdown_initiated`
   诊断，发 watch 信号；重复调用幂等。
2. dispatch task 退出 select 循环，**drain 已入队事件**（有界：
   `Stopping` 后新提交已被拒，drain 只消费缓冲；不丢已提交事件）。
3. 写 `host_shutdown` 收尾诊断（含 reason 与 handled 计数），
   状态 → `Stopped`。
4. `task.join().await` 返回 `HostExit`（reason / handled_events / panicked）。
- 所有 handle drop（无 shutdown）→ channel 关闭 → task 自行退出
  （`SendersDropped`）。
- **panic 策略**：dispatch 处理事件时的 panic 被 `catch_unwind` 捕获
  （fail closed：停止派发 + `dispatch_panic` 诊断），join 不向调用方
  传播 panic；`HostExit::panicked` 报告。测试直接注入 panic 闭包验证。

### Budget 维度（`budget.rs`）

- `ExtensionBudget` 四维：`max_calls_per_session`（事件/调用次数）、
  `max_output_bytes_per_session`（输出字节）、`max_run_secs`（单次运行
  时长，ARC-710 runner 输入）、`max_concurrent_extensions`（同时启用数，
  ARC-720 输入）。`0` = 不限。
- 骨架落地前两维记账（`BudgetTracker`，dispatch 事件时校验，超出 →
  `budget_exceeded` 诊断 + 丢弃该事件），后两维只提供类型与默认值。
- 与 Grok 差异：Grok 是 per-hook `timeout_ms`（stop gate 默认 600s）；
  Evo 是 per-extension 多维预算，per-session 维度与 phase6 背景任务的
  session 归属语义一致。

### Diagnostics（`diagnostic.rs`）

- 结构化 `DiagnosticRecord`（level + 稳定 code + 上下文 map），有界
  环形缓冲（`diagnostic_capacity`）+ 可选 `DiagnosticSink`（ARC-710 接
  产品事件/日志）。
- 稳定 code 集合：`extension_untrusted` / `extension_first_enable` /
  `budget_exceeded` / `dispatch_panic` / `host_shutdown_initiated` /
  `host_shutdown` / manifest 错误（`InvalidConfig`/`ParseFile` 由错误
  列表返回，不重复落诊断）。

### Discovery（`discovery.rs`）

- 与 Grok「目录内散落 JSON hook 文件」不同：每个扩展一个**目录**，
  目录下 `extension.json` manifest（name/version/description/
  capabilities/config）。该形状同时服务 ARC-710（runner 需要
  per-extension 配置）与 ARC-720（MCP server 声明 transport/tools）。
- 容错与 Grok 一致：目录不存在 → 空（不是错误）；坏 manifest 记录
  错误并继续；结果按目录名排序保证稳定。manifest 语义校验：
  name/version 非空、capabilities 名称唯一。

### 给 ARC-710 / ARC-720 的扩展点

- **dispatch 槽位**：`dispatch_loop` 的 `on_event` 参数（`FnMut`），
  生产用 `default_on_event`（budget 记账 + 诊断）；ARC-710 在此接
  runner（matcher 过滤 + gate 策略）。
- **`ExtensionHostOptions`**：`config_layers` / `budget` / trust_store /
  diagnostics 已可按需组合；ARC-710/720 直接扩展字段（保持向后兼容）。
- **manifest 的 `capabilities` + `EnableRequest`**：trust 放行后的
  能力声明；ARC-720 MCP 注册入口。
- **budget 维度**：`RunDurationSecs` / `ConcurrentExtensions` 留给
  ARC-710/720 强制。
- **coding-agent 端口**：`services::ports::ExtensionHostPort` +
  `NoopExtensionHostPort`（当前产品无 host，行为不变）+ RuntimeHost
  持有 `ExtensionHostService`，session 关闭路径已接
  `notify_shutdown`；ARC-710 换真实 host 适配器并扩展查询面。
  CLI/Desktop 不接线（后续 ARC）。

## 落点

| 变更 | 位置 |
| --- | --- |
| 新 crate | `crates/extension-host/`（加入 workspace members + `[workspace.dependencies]`） |
| 错误类型 | `crates/extension-host/src/error.rs` |
| 版本化事件 DTO + kind/payload | `crates/extension-host/src/event.rs` |
| config merge（source/layer/merge） | `crates/extension-host/src/config.rs` |
| discovery（manifest/扫描） | `crates/extension-host/src/discovery.rs` |
| folder trust + 首次启用 DTO | `crates/extension-host/src/trust.rs` |
| budget 类型 + 记账 | `crates/extension-host/src/budget.rs` |
| 结构化诊断 | `crates/extension-host/src/diagnostic.rs` |
| ExtensionHost/handle/task + dispatch | `crates/extension-host/src/host/mod.rs` |
| host 测试（lifecycle/shutdown/budget/panic） | `crates/extension-host/src/host/tests_host.rs` |
| 公开 API 清单 | `crates/extension-host/src/api.rs` |
| coding-agent 端口 + 空实现 + RuntimeHost 接线 | `crates/coding-agent/src/services/ports.rs`、`runtime/owners.rs`、`runtime/facade/lifecycle.rs`、`runtime/facade/connection.rs` |
| 设计文档 | `docs/refactor/phase7-extension-host.md`（本文件） |
| provenance 登记 | `docs/refactor/provenance/grok-build.md` |

## 验证

```text
cargo test -p extension-host --all-features
67 passed（lib 67）
- budget 7：默认值/记账/超限（call 与 bytes）/session 独立/reset/零=不限/kind 序列化
- config 8：scalar 覆盖/enabled AND/permissions 并集/优先级 order/空层默认/
  TOML 部分指定/坏层跳过/JSON round-trip
- diagnostic 6：顺序/环形丢弃/drain/sink/容量下限/round-trip
- discovery 8：空目录/缺失目录/多扩展排序/跳过非 manifest/坏 manifest 继续/
  语义校验/round-trip/最小 manifest
- event 11：golden 序列化/round-trip/全变体 round-trip/旧输入兼容/
  可选字段缺省/别名与未知拒绝/snake_case/kind tag 判别/版本校验/
  非法 ToolId fail closed/Display 一致性
- trust 7：三态边界/child 继承/ancestor 覆盖/sibling 不泄漏/未决定/
  canonical/EnableRequest DTO round-trip
- host 20：trust 过滤启用/未信任跳过/config 层合并/完整生命周期/
  shutdown 拒新事件且 drain/幂等 shutdown/全 handle drop 退出/二次 start 拒绝/
  join 后拒绝提交/版本拒绝/budget 超限诊断/输出字节预算/panic fail-closed/
  部分处理后 panic 计数/queued drain/join 有界/首次启用 DTO/phase round-trip

cargo test -p coding-agent --all-features
227 passed（216 lib + 2 ports + 2 api_contract + 7 doc/example；ARC-700 新增
ports 3 项：noop 端口无 host、noop notify 为 no-op、RuntimeHost 接线回归）

cargo clippy -p extension-host --all-targets --all-features -- -D warnings  通过
cargo clippy -p coding-agent --all-targets --all-features -- -D warnings  通过
cargo fmt --all  通过
```

## 与 Grok 参考实现的差异清单

1. **事件 DTO 版本化**：Grok `HookEventEnvelope` 无 version；Evo 带
   `version` + host 侧版本校验（fail closed）。
2. **payload 判别**：Grok untagged + 字段互斥（仅 Serialize）；Evo
   internally-tagged `kind`（双向 serde 可靠判别）。
3. **事件字段按 Evo 业务重设计**：不照抄 Grok 事件全集（无
   PostToolUseFailure/Notification/StopFailure 等骨架变体，payload 字段
   最小化）。
4. **discovery 形状**：Grok 目录内散落 JSON 文件；Evo 每扩展一个目录 +
   `extension.json` manifest。
5. **trust**：Grok 只有 legacy 迁移辅助 + disabled-hooks 文件；Evo 的
   `TrustStore` 抽象 + 三态判定 + `EnableRequest` 首次启用 DTO。
6. **budget**：Grok per-hook timeout_ms；Evo per-extension 多维
   per-session 预算。
7. **runner**：Grok `command`/`http` runner 完整实现（900+ 行）；Evo
   骨架不实现 runner，只留 `on_event` 槽位（ARC-710）。
8. **matcher**：Grok 完整 matcher（exact/regex/alias 展开）；Evo 骨架
   无 matcher（ARC-710 引入，依赖 `regex` 时再评估）。
9. **async 生命周期**：Grok 无 host 概念（load-and-fire）；Evo 的
   ExtensionHost/dispatch task/shutdown 顺序/panic 捕获是 Evo 自己的设计。
10. **编码约定**：所有移植参考均小步重写 + 文件头注释标注来源；
   未整文件复制（Grok 最大参考文件 1376 行，未直接拷贝）。

## 后续

- ARC-710：`on_event` 槽位接 runner；matcher；`TrustStore` 产品实现；
  coding-agent 端口换真实 host 适配器；`RunDurationSecs` 强制。
- ARC-720：`ConcurrentExtensions` 强制；manifest capabilities 驱动的
  MCP 注册。
- `session_end` 事件处理时可接 `BudgetTracker::reset_session`。
