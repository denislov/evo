# Desktop 多项目工作区与启动界面重构基线

> 任务：`DSK-600` 记录基线并锁定现状证据
> 记录日期：2026-07-30
> 基线分支：`main`
> 基线提交：`365ad01aa2e6ceee4663da36207b789a08f7bf56`
> 提交说明：`perf(desktop): stream Markdown rows through incremental parses`

## 1. 基线环境与工作区状态

- `rustc 1.96.0 (ac68faa20 2026-05-25)`
- `cargo 1.96.0 (30a34c682 2026-05-25)`
- `x86_64-unknown-linux-gnu`
- `Linux 6.12.95+deb13-amd64`
- Desktop visual/native perf 使用本机 X11 `DISPLAY=:0`；受限沙箱不能连接 display，最终 gate
  在获准的沙箱外显示环境执行。

开始 DSK-600 前的 dirty state：

```text
 M docs/desktop待机界面与多会话工作台.md
?? AGENTS.md
?? docs/desktop多项目工作区与启动界面重构计划.md
```

其中旧计划文档只有 4 行后续决策说明；`AGENTS.md` 与本轮主计划均为用户已有的未跟踪文件。
DSK-600 不修改或回退这些内容。本任务新增的行为测试和本基线文档不属于上述初始 dirty state。

## 2. 自动 `ListSessions` 现状证据

新增测试：

```text
app::native_shell::tests::baseline_idle_shell_automatically_requests_the_session_catalog
```

测试使用 `DesktopRuntimeBridge::instrumented_for_test` 捕获 NativeShell 启动后的 runtime command，
并严格断言唯一命令为：

```text
[DesktopRuntimeCommandKind::ListSessions]
```

定向验证：

```text
running 1 test
test app::native_shell::tests::baseline_idle_shell_automatically_requests_the_session_catalog ... ok
test result: ok. 1 passed; 0 failed
```

此测试有意锁定待删除的旧行为。`DSK-630` 必须把断言反转为“启动及静置不发送
`ListSessions`”，不能简单删除证据测试。

## 3. 测试、golden 与性能 gate 结果

### 3.1 `coding-agent` 全量测试

命令：

```bash
cargo test -p coding-agent
```

结果：**基线失败**。library tests 为 `760 passed`，`api_contract` 为 `14 passed`；随后
`boundaries` 为 `64 passed, 3 failed`，Cargo 因 integration target 失败停止后续 target。

三个失败均在 DSK-600 改动范围之外，且行为测试只修改 `crates/desktop`：

1. `session_mutating_operation_owners_require_frozen_write_capability`
   - `crates/coding-agent/src/runtime/dispatch.rs` 的 session-mutating operation entry 计数不符；
     实际 `6`，允许值 `5`。
2. `durable_operation_paths_consume_admitted_identity_without_regeneration`
   - `crates/coding-agent/src/operations/session_naming.rs:61` 仍调用
     `ids.next_root_operation_id()`。
3. `final_receiver_aware_compatibility_absence_and_retained_api_guard`
   - `CodingAgentSession::list_overviews_internal` 尚未登记到 public/pub(crate) method ledger。

后续产品层任务不能把这三个既有失败误判为 workspace scope 重构引入的回归；在完整计划收敛前仍需
清偿。

### 3.2 Desktop 全量测试

命令：

```bash
cargo test -p desktop
```

结果：**通过**。

- Desktop library：`243 passed, 0 failed, 5 ignored`；5 项 ignored 均为独立 release perf gate。
- `dependency_boundary`：`17 passed, 0 failed`。
- main/doc tests：无测试项，均通过。

### 3.3 Visual golden

命令：

```bash
scripts/desktop-visual-golden.sh
```

结果：**通过**。以下 10 个 fixture 均与已审阅 golden 尺寸一致，normalized RMSE 为 `0`，预算
为 `0.015`：

- `wide`、`medium`、`narrow`
- `wide-idle`、`medium-idle`、`narrow-idle`
- `wide-authorization`
- `wide-reduced-motion`
- `wide-keyboard-focus`
- `wide-no-color`

本任务未执行 `--update`，未改写任何 golden。

### 3.4 Native window performance gate

命令：

```bash
scripts/desktop-native-perf-gate.sh
```

结果：**通过**。

| 指标 | 基线 | 预算 |
| --- | ---: | ---: |
| GPU/present frame P95 | 5,277 µs | 16,700 µs |
| GPU/present frame P99 | 5,743 µs | 33,000 µs |
| input dispatch → post-render P95 | 8,343 µs | 50,000 µs |
| input dispatch → post-render P99 | 8,414 µs | 仅记录 |
| native RSS after replay | 153,346,048 B | 268,435,456 B |
| native steady RSS growth | 40,960 B | 67,108,864 B |
| Markdown parse → layout P95 | 157 µs | 150,000 µs |

### 3.5 Release/headless performance gate

命令：

```bash
scripts/desktop-perf-gate.sh
```

结果：**通过**，5 个 release ignored test 均被逐项精确执行。

关键基线：

| 指标 | 基线 |
| --- | ---: |
| 10 MiB fixture hydration | 14,462 µs |
| 10 MiB scroll render P95 | 200 µs |
| 10 MiB input P95 | 1 µs |
| 10k block hydration | 2,514 µs |
| headless CPU frame P95 | 3,023 µs |
| headless input roundtrip P95 | 5,767 µs |
| input change → render P95 | 423 µs |
| headless window RSS growth | 25,460,736 B |
| Markdown 256 KiB parser P95 | 84,592 µs |

`desktop-click-to-photon.sh` 不属于上述自动 gate，且真实结果必须由外部传感器提供。本任务未用
X11 drive smoke 或 GPUI 内部时间冒充物理 click-to-photon 基线。

## 4. Catalog 自动加载与刷新删除清单

下列行号基于本任务完成时源码。后续移动代码时以符号和语义为准，不能只按行号机械删除。

### 4.1 `request_session_catalog` 调用点

| 位置 | 当前触发 | DSK-630 处理 |
| --- | --- | --- |
| `native_shell.rs:604` | `SessionsPaneEvent::Refresh` | 保留，作为唯一显式 user command |
| `native_shell.rs:731` | `NativeShell::new` 启动异步请求 | 删除 |
| `native_shell.rs:2176` | 窄屏 Sessions drawer 打开 | 删除 |
| `native_shell.rs:3356` | 键盘聚焦不可见 Sessions panel | 删除 |
| `commands.rs:29` | session close 成功 | 改为本地增量删除，不做完整 list |
| `commands.rs:250` | resync 成功 | 删除隐式 list |
| `commands.rs:269` | create/open session projection 成功 | 改为已知结果增量维护，不做完整 list |
| `session_controller.rs:206` | 15 秒 timer 到期 | 连 timer 一并删除 |
| `session_controller.rs:292` | switch-next 时本地 catalog 为空 | 改为明确空态/提示，不隐式 list |

`request_session_catalog` 本体位于 `session_controller.rs:132`。它在 DSK-630 后仍可服务显式
Refresh，但 admission 失败不得安排后台 retry。

### 4.2 `schedule_session_catalog_refresh` 路径

| 位置 | 当前触发 | DSK-630 处理 |
| --- | --- | --- |
| `session_controller.rs:156` | `ListSessions` admission 失败 | 删除 retry |
| `session_controller.rs:192-213` | deadline/timer 实现 | 整段删除 |
| `session_controller.rs:208` | active operation 时递归延期 | 删除 |
| `commands.rs:102` | `SessionsListed` 成功 | 删除周期刷新 |

同时删除：

- `SESSION_CATALOG_REFRESH_INTERVAL`（`session_controller.rs:7`）；
- `SessionController.refresh_deadline`；
- `schedule_refresh` / `take_scheduled_refresh`；
- `catalog_refresh_has_one_deadline_and_keeps_recent_order` 中只验证 timer/deadline 的部分；
- 成功结果生成的 `Loaded N session(s).` 全局 notice/toast。

## 5. 其他结构性删除与迁移清单

### 5.1 Idle layout 特例

- 定义：`ShellLayout::resolve_idle`，`shell.rs:180-188`。
- 产品 caller：`NativeShell::resolve_layout`，`native_shell.rs:1111-1114`；当
  `projection.is_none()` 时无条件绕过 panel preference。
- 旧行为测试：`idle_layout_hides_session_panels_and_gives_home_the_full_workspace`，
  `shell.rs:607-616`。

后续三栏 shell 任务应删除 `resolve_idle` 作为独立列模型；保留 `ShellLayout.idle` 仅用于 center
body/focus 语义，Home 也必须经正常 `resolve_with_panel_widths` 解析 Sidebar preference。

### 5.2 `OverlayHost` children

当前 `overlay_host.rs:406-414` 的全屏 root host 同时挂载：

1. `narrow_context_overlay`（Inspector）；
2. `narrow_sessions_overlay`（Sidebar）；
3. `command_palette_overlay`；
4. `full_message_overlay`；
5. `authorization_overlay`。

目标拆分：前两项迁到 `CenterDrawerHost`，只覆盖 center body；后三项留在
`RootModalHost`。当前 `overlay_surface` 使用 `absolute().size_full()`、scrim 与 focus trap，不能继续
复用于非模态 drawer。

### 5.3 固定 Thinking level 枚举

- 唯一 Desktop UI caller：`conversation_header.rs:338`，dropdown 直接 fold
  `DesktopThinkingLevel::ALL`。
- 结构守卫测试：`native_shell.rs:9005` 还明确要求该字符串存在。

`CAG-203` 暴露 capability 后，UI caller 应改为消费预计算的合法 options；相应结构守卫必须反转，
明确禁止 Desktop 直接遍历 `DesktopThinkingLevel::ALL`。

## 6. DSK-600 收口条件

- [x] main commit、初始 dirty state 与工具链已记录。
- [x] Desktop/coding-agent 全量测试结果已记录，既有失败已单独列明。
- [x] visual golden、native perf、release/headless perf 已运行并记录。
- [x] 启动自动发送 `ListSessions` 的行为测试已新增并通过。
- [x] catalog request/schedule、idle layout、OverlayHost、Thinking ALL 清单已锁定。
- [x] 未更新 golden，未在基线任务中顺手修改后续架构。
