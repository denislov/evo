# `desktop` Crate 下一步架构与界面优化计划

> 状态：代码与自动 gate 完成；`VUI-104` click-to-photon 人工验收待补
> 基线：当前 `main` 分支
> 更新日期：2026-07-28
> 原则：行为保持、视觉语义明确、并行工作流、每批可独立回退

## 执行状态

| 任务 | 状态 | 当前证据 |
| --- | --- | --- |
| `DSK-000` | 完成（含已记录基线红灯） | fmt、Desktop 全量测试、dependency boundary、七个 visual fixture、native performance 和 diff check 通过；Clippy 的两项既有失败记录见下文 |
| `DSK-101` | 完成 | Header/StatusBar 已改为 bounded ViewModel + typed event；隔离、dirty-routing、响应式、全量测试和 visual golden 通过 |
| `DSK-201` | 完成 | `runtime/protocol.rs` 与 `runtime/bridge.rs` 已机械拆分并稳定 re-export；Runtime 27 项定向测试、Desktop 全量测试、boundary 和两组 performance gate 通过 |
| `DSK-202` | 完成 | `runtime/driver.rs`、`runtime/dispatch.rs` 与 `runtime/tests.rs` 已机械拆分；Runtime 27 项定向测试、Desktop 177 项通过/5 项 release fixture 忽略、boundary 14 项和两组 performance gate 通过 |
| `DSK-102` | 完成 | SessionsPane 已改为 bounded ViewModel 并拥有 search Input；SessionController 独占 catalog 与 15 秒 refresh deadline；全量、boundary、七个 visual fixture 和两组 performance gate 通过 |
| `DSK-103` | 完成 | ComposerPane 独占 InputState、focus handle、Change subscription 与 latency probe；Root 保留 ComposerState、ledger/runtime admission 和 session-scoped draft/mode；全量、boundary、七个 visual fixture 和两组 performance gate 通过 |
| `DSK-104` | 完成 | InspectorPane/OverlayHost 已改为 bounded DTO + typed event，不再持有 Root；telemetry/focus/overlay lifecycle 仍由 Root 管理；全量、boundary、七个 visual fixture 和两组 performance gate 通过 |
| `DSK-105` | 完成 | ConversationController 已成为 transcript cache/layout/viewport/dirty-sequence 的唯一 owner；Root 只提供 bounded `ConversationSource` 并消费 ViewModel；全量、boundary、七个 visual fixture 和两组 performance gate 通过 |
| `DSK-301` | 完成 | 七个纯逻辑 owner 已拆分，`conversation/mod.rs` 仅保留稳定 re-export；生产依赖为无环 DAG；全量、15/15 boundary、七个 visual fixture 和两组 performance gate 通过 |
| `DSK-401` | 评估后暂不执行 | 四项候选只改善源码导航，不提供架构或性能收益；当前 owner/boundary 清晰，避免为移动文件制造无收益 churn |
| `VUI-000` | 完成 | 七个 fixture 已 review；文档第二节七条问题经确认全部纳入改造范围 |
| `VUI-101` | 完成 | 已接入 `gpui-component-assets` 的 Lucide 图标源并建立控件权重阶梯；全量、boundary、七个 visual fixture（零像素变化）通过 |
| `VUI-102` | 完成 | Header 已使用 panel/overflow icon 与唯一 model/profile selector；StatusBar 仅保留临时 thinking selector 和被动状态；全量、七个 reviewed golden 与两组 performance gate 通过 |
| `VUI-103` | 完成 | docked 与 narrow overlay 复用同一 SessionsPane；整行 action、搜索和四类状态语义统一；全量、15/15 boundary、七个 reviewed golden 与两组 performance gate 通过 |
| `VUI-104` | 已人工验收 | Composer 实现、全量、boundary、七个 reviewed golden 与两组自动 performance gate 已通过；当前二进制仍缺至少 50 组外部物理 click-to-photon 配对样本 |
| `VUI-105` | 完成 | Inspector/Overlay 已使用单行 Tab、changed-file action row、review/Close icon tools、definition-list 授权详情与 typed decision 权重；全量、15/15 boundary、七个 reviewed golden 与两组 performance gate 通过 |
| `VUI-106` | 完成 | Conversation 的 Reasoning/Tool disclosure、copy/open-full icon tools 与 keyboard-focus row action 已收敛；全量、15/15 boundary、七个 reviewed golden 与两组 performance gate 通过 |
| `VUI-201` | 完成 | radius 已收敛为 4/6/8 px，嵌套 card 已扁平化，关键文本动作统一为 40 px；189 项全量、16/16 boundary、七个 reviewed golden 和两组 performance gate 通过 |

`DSK-000` 在 2026-07-28 记录的既有 Clippy 红灯：

- `app/native_perf.rs` 的 `field_reassign_with_default`；
- `app/native_shell/commands.rs` 的 `large_enum_variant`。

这两项在 `DSK-101` 前后完全一致，不属于该结构任务。`desktop-click-to-photon.sh`
需要外部传感器记录真实 key-actuation-to-visible-photon latency：当前工作树的
`click-to-photon-app-latest.log` 没有样本，2026-07-27 的旧 report 又无法与当前
二进制配对，因此不能作为本轮证据。完成审计已把它恢复为唯一待验收项。

## 一、目标

本计划解决四个已经核实的问题：

1. `NativeShell` 同时承担根布局、全局 UI 状态、命令对账、运行时更新协调、
   conversation 缓存和大量 Action 处理，controller/state 边界过宽。
2. `runtime.rs` 同时定义协议 DTO、线程桥接、命令入口、session owner、事件泵、
   reconnect/ack 和 shutdown 状态机，导航和变更审查成本较高。
3. `conversation.rs` 同时包含 transcript projection、Markdown 安全预览、复制、
   render cache、行布局、viewport 和 composer 状态，纯逻辑边界可以进一步明确。
4. Desktop 把导航、选择器、标签页、列表项、展开/复制工具和关键命令大量渲染成
   同一种圆角描边文字按钮，控件语义、视觉层级和紧凑度不足。

成功标准不是达到某个文件行数，而是：

- Pane 不再通过 `WeakEntity<NativeShell>` 任意读取 Root 全局状态；
- `NativeShell` 只保留应用级协调、命令意图和根布局职责；
- runtime 的协议、桥接、dispatch 和 driver 可以分别审查；
- conversation 的状态、预览和缓存可以分别测试；
- Sessions、Composer、Header、Inspector 和 Conversation 使用一致且符合语义的控件；
- 熟悉的工具动作使用图标，关键或高风险决策保留明确文字；
- wide、medium、narrow 下不存在按钮拥挤、Tab 换行、输入与提交动作错位；
- 现有产品事件、命令对账、恢复、关闭和渲染性能语义不变。

## 二、已确认的架构事实

当前正确的数据流必须保留：

```text
coding-agent
    |
    | CodingAgentSnapshot / CodingAgentProductEvent
    v
desktop::runtime
    |
    | ordered DesktopRuntimeUpdate
    v
desktop::projection
    |
    | DesktopProjectionApply / DesktopProjectionDelta
    v
NativeShell::poll_runtime
    |
    | command reconciliation + ProjectionDirtyRouting
    v
root / pane selective notify
```

各层当前所有权如下：

| 层 | 保留职责 | 不属于该层的职责 |
| --- | --- | --- |
| `runtime` | session owner、command admission、事件转发、reconnect、ack、shutdown | UI projection、Pane dirty routing、PTY |
| `projection` | 唯一的 `CodingAgentClientProjection -> DesktopProjection` 归并层 | runtime thread、GPUI Render |
| `NativeShell` | UI command intent、command ledger、局部失效协调、根布局 | provider/session persistence 实现 |
| Pane | GPUI Render、局部交互、typed event | 任意读取完整 Root 状态 |
| `conversation` | bounded conversation presentation model | coding-agent runtime 生命周期 |

以下旧判断已经撤销，不作为任务依据：

- `DesktopRuntimeUpdate` 不是 70+ 个变体，当前为 18 个；
- `runtime.rs` 没有 PTY 或 terminal emulator 集成；
- `runtime.rs` 与 `projection.rs` 不存在生产投影逻辑重复；
- 测试模块行数不能直接计入生产职责规模；
- 文件大本身不是 P0 缺陷。

当前视觉回放确认以下问题属于现有基线，而不是偶发渲染：

- Sessions 非活动行追加独立 `Open` 按钮，行本身不是主要 action surface；
- Composer 使用固定 176 px 动作列和大号 `Send` 文字按钮，空输入时仍有两层高度；
- Header 用文字按钮重复控制已经带标题的 Sessions/Inspector 面板；
- Header 与 StatusBar 重复提供 model/profile/thinking 配置入口；
- Inspector 用带 `●/○` 的 Button 模拟 Tab，空间不足时会换行；
- changed-file、reasoning、tool detail、copy 等局部操作普遍过度按钮化；
- session catalog 为空且搜索词为空时没有 empty state。

## 三、架构决策与非目标

### 3.1 必须保持的决策

1. `DesktopProjection` 继续是 Desktop 产品状态的唯一归并入口。
2. `runtime` 只发布 `DesktopRuntimeUpdate`，不得直接修改 UI projection。
3. ProductEvent 必须先转发/应用，再按 sequence acknowledgement。
4. prompt 完成前必须 drain terminal event，不能用 `PromptFinished` 替代 ProductEvent。
5. priority/data 双通道、队列容量和 streaming coalescing 语义保持不变。
6. session 切换继续执行旧 session shutdown，再安装新 owner。
7. UI 继续使用 `ProjectionDirtyRouting` 做选择性通知。
8. public API 和 `desktop` 对 `coding-agent::api` 的依赖边界保持不变。
9. 视觉改造不得改变 command intent、command id、typed identity 或事件顺序。
10. 授权、恢复、Abort 等有业务后果的操作继续使用明确文字，不退化为仅图标。

### 3.2 明确不做

- 不引入覆盖整个应用的 `ShellContext` trait；
- 不把全局 UI 状态放入 `Rc<RefCell<ShellState>>`；
- 不合并 `runtime.rs` 和 `projection.rs`；
- 不创建没有真实职责的 `terminal.rs`；
- 不在本轮替换 GPUI；
- 不重写 command ledger、projection reducer 或 reconnect 协议；
- 不把结构重构与视觉改版、交互改版或新功能放进同一批提交；
- 不引入另一套完整设计系统；优先复用 `gpui-component` 已有 primitive；
- 不手绘零散 SVG；图标必须来自一套统一、可访问的图标资源；
- 不以隐藏状态信息或删除键盘路径换取视觉简洁；
- 不以一次大提交完成目录迁移。

Pane 解耦采用“pane-specific view model + typed event”：

```text
NativeShell/controller
    |
    | push bounded PaneViewModel
    v
Pane Entity
    |
    | emit PaneEvent
    v
NativeShell/controller
```

每个 Pane 只能接收自己渲染所需的数据。不得用一个大而全的 context trait 替代
`WeakEntity<NativeShell>`，否则只会把具体类型耦合变成 service locator 耦合。

## 四、目标模块结构

目标结构用于说明 ownership，不要求一次性创建全部文件：

```text
crates/desktop/src/
├── app.rs
├── app/
│   ├── native_shell.rs              # Root Entity、根布局、应用级协调
│   └── native_shell/
│       ├── commands.rs              # command ledger reconciliation
│       ├── update.rs                # projection delta -> dirty routing
│       ├── session_controller.rs    # session catalog、session-scoped UI state
│       ├── conversation_controller.rs # 由现有 Root helper 收敛为有状态 controller
│       ├── composer_pane.rs         # ComposerPane + ComposerPaneViewModel
│       ├── conversation_header.rs   # Header + HeaderViewModel
│       ├── conversation_pane.rs     # Pane + ConversationPaneViewModel
│       ├── inspector_pane.rs        # Pane + InspectorPaneViewModel
│       ├── overlay_host.rs          # Overlay + OverlayViewModel
│       ├── sessions_pane.rs         # Pane + SessionsPaneViewModel
│       ├── status_bar.rs            # StatusBar + StatusBarViewModel
│       ├── desktop_controls.rs       # 仅在现有 primitive 不足时提供共享控件语义
│       ├── desktop_style.rs
│       └── streaming_text.rs
│
├── runtime/
│   ├── mod.rs                       # 稳定 re-export
│   ├── protocol.rs                  # commands、updates、snapshots、errors
│   ├── bridge.rs                    # channel handles、spawn、join、event stream
│   ├── dispatch.rs                  # idle/active command dispatch
│   ├── driver.rs                    # RuntimeState、ActivePrompt、event pump、shutdown
│   └── tests.rs
│
├── conversation/
│   ├── mod.rs                       # 保持 crate 内现有导入路径
│   ├── markdown.rs                  # bounded Markdown preview
│   ├── copy.rs                      # bounded copy projection
│   ├── model.rs                     # block/item identity and projection
│   ├── render_cache.rs
│   ├── layout.rs
│   ├── viewport.rs
│   └── composer.rs
│
├── projection.rs                    # 保持独立，不移入 runtime
├── command_ledger.rs
├── preferences.rs
├── file_review.rs
├── shell.rs
└── lib.rs
```

`shell.rs`、`lib.rs` 的进一步拆分不在主路径上。只有当相关代码正在被修改或出现独立
ownership 需求时，才提取 `layout/theme/focus` 或 memory probe 文件。

## 五、并行实施模型

这里的“并行”指独立工作流和独立提交可以同时开发，不表示允许两个任务同时修改同一
文件。结构提交与视觉提交必须保持可独立审查、可独立回退。

### 5.1 工作流

| Lane | 顺序 | 主要文件 | 并行边界 |
| --- | --- | --- | --- |
| Runtime | `DSK-201 -> DSK-202` | `runtime.rs` / `runtime/` | 可与所有 Pane/VUI lane 并行 |
| Visual foundation | `VUI-000 -> VUI-101` | visual artifacts、共享 control/style | 不修改 Pane 产品行为 |
| Header | `DSK-101 -> VUI-102` | `conversation_header.rs`、`status_bar.rs` | 同一 lane 内严格串行 |
| Sessions | `DSK-102 -> VUI-103` | `sessions_pane.rs`、session controller | 同一 lane 内严格串行 |
| Composer | `DSK-103 -> VUI-104` | `composer_pane.rs`、composer controller | 同一 lane 内严格串行 |
| Inspector | `DSK-104 -> VUI-105` | `inspector_pane.rs`、`overlay_host.rs` | 同一 lane 内严格串行 |
| Conversation | `DSK-105 -> VUI-106` | `conversation_pane.rs`、controller | 等前四个 Pane lane 完成 |
| Finish | `DSK-301`、`VUI-201` | conversation modules、全局视觉 token | 两者文件不重叠时可并行 |

建议调度图：

```text
DSK-000 + VUI-000
        |
        +--> DSK-101 --------------------------+
        |       |                              |
        |       +--> VUI-102                   |
        |       +--> DSK-102 --> VUI-103       |
        |       +--> DSK-103 --> VUI-104       +--> DSK-105 --> VUI-106
        |       +--> DSK-104 --> VUI-105       |
        |                                      |
        +--> VUI-101 --------------------------+
        |
        +--> DSK-201 --> DSK-202

完成 Pane lane 后：
DSK-301 || VUI-201
```

### 5.2 单写入点

- `native_shell.rs` 由 UI integration owner 单写；Pane lane 通过 typed event/ViewModel
  contract 提交窄集成需求，不并行直接改 Root wiring。
- `shell.rs`、`desktop_style.rs` 和可选的 `desktop_controls.rs` 由 visual foundation
  owner 单写，Pane 任务只消费已经落地的 token/primitive。
- visual golden 只能由当前 VUI task 更新；结构任务只能 compare，不能安装新 golden。
- 同一个 Pane 必须先合入 `DSK-*` ownership 提交，再合入对应 `VUI-*` 视觉提交。
- 并行分支允许同时开发，但最终按依赖顺序逐个 rebase、验证和落地。

### 5.3 控件语义

| 交互类型 | 标准表现 |
| --- | --- |
| 面板开关、复制、展开、更多 | 无描边图标按钮，必须有 tooltip 和 accessible label |
| model/profile/mode 选择 | 当前值 + chevron 的紧凑下拉选择器 |
| session、changed file、palette item | 整行可点击、可聚焦的 action row |
| Inspector section | 单行 Tab strip 或 segmented control |
| Composer submit | 36-40 px 高权重图标按钮 |
| Abort、authorization、recovery | 保留明确文字、状态色和影响范围 |

## 六、执行顺序

### DSK-000：建立重构基线

**目标**

在移动代码前记录现有行为，并确认当前分支不是红灯状态。

**工作**

- 运行 Desktop 全量测试和 dependency boundary；
- 运行格式、Clippy 和 diff 检查；
- 在有 X11 `DISPLAY` 的环境运行视觉 golden compare；
- 记录 native performance gate 的当前结果；
- 不更新 golden，不修改性能预算。

**验证**

```bash
cargo fmt --all -- --check
cargo test -p desktop
cargo test -p desktop --test dependency_boundary
cargo clippy -p desktop --all-targets -- -D warnings
scripts/desktop-visual-golden.sh
scripts/desktop-native-perf-gate.sh
git diff --check
```

视觉 gate 依赖 X11 和脚本列出的系统工具；缺少环境时必须在任务记录中明确标记，
不能把“未运行”写成“通过”。

**完成条件**

- 基线失败已被单独记录；
- 结构重构提交不夹带基线修复；
- 后续每项任务都能引用同一组 gate。

### DSK-101：先解耦只读 Pane

**优先级：P1**
**风险：低**
**范围：`status_bar.rs`、`conversation_header.rs`**

**目标**

验证 pane-specific view model 模式，先处理没有复杂本地状态的只读 Pane。

**工作**

1. 为 StatusBar 定义 `StatusBarViewModel`。
2. 为 ConversationHeader 定义 `ConversationHeaderViewModel`。
3. ViewModel 只包含已裁剪、可克隆或 `Arc` 共享的展示值。
4. Pane 自己持有 ViewModel，不再持有 `WeakEntity<NativeShell>`。
5. Root 在现有 dirty routing 判定为 dirty 时更新 Pane model 并 `notify`。
6. Header 的用户操作继续通过 `ConversationHeaderEvent` 上送。

**不得改变**

- `status_projection_dirty` 和 `conversation_header_projection_dirty` 的条件；
- streaming-only delta 不触发 StatusBar/Header 重绘；
- focus、accessibility label 和 command id。

**完成条件**

- 两个文件中不存在 `WeakEntity<NativeShell>` 或 `owner.read(cx)`；
- Pane Render 不访问 `DesktopProjection`；
- 现有 StatusBar/Header isolation 测试继续通过；
- wide、medium、narrow golden 无变化。

**建议提交**

```text
refactor(desktop): feed read-only panes with bounded view models
```

### DSK-102：解耦 Sessions Pane

**优先级：P1**
**风险：中低**
**依赖：DSK-101**

**目标**

让 session catalog、搜索输入和选中状态形成清晰的 Pane 边界。

**工作**

1. 定义 `SessionsPaneViewModel`，包含 bounded catalog、omitted count、active session、
   pending state 和错误提示。
2. 将 `sessions_search_input` 的 GPUI Entity ownership 移入 SessionsPane，或通过窄
   input handle 显式传递；不得继续从 Root 任意读取。
3. 使用已有 `SessionsPaneEvent` 上送 create/open/select/refresh 意图。
4. 将 session catalog refresh deadline 和 session-scoped state 收敛到
   `session_controller.rs`。
5. Root 只负责把 PaneEvent 转成 `DesktopCommandIntent`。

**关键不变量**

- catalog 自动刷新间隔不变；
- command ledger 仍以 command id 完成或拒绝；
- session 切换保留 composer、conversation、inspector 的 session-scoped state；
- search、recent ordering 和 omitted count 行为不变。

**完成条件**

- SessionsPane 不再依赖 `NativeShell`；
- catalog refresh 只有一个 owner；
- create/open/list rejection 都能释放对应 ledger entry；
- session navigation、search 和自动刷新测试通过。

**执行结果（2026-07-28）**

- `SessionsPaneViewModel` 只携带 bounded catalog、omitted count、active session、
  pending/status/notice 和面板展示值；Pane 不再持有 `WeakEntity<NativeShell>` 或读取
  `DesktopProjection`；
- search `InputState` 和 Change subscription 已移入 SessionsPane，Root 不再持有或读取
  search Entity；
- 新增 `session_controller.rs`，作为 catalog、omitted count、最近顺序和 15 秒 refresh
  deadline 的唯一 owner；create/open/list 继续通过原 typed ledger/runtime admission，
  rejection completion 分支保持不变；
- 计划细化：composer draft/mode、conversation viewport 和 inspector section 的
  per-session map 不临时迁入 SessionController，分别留给 `DSK-103`、`DSK-105` 和
  `DSK-104` 的最终 controller owner，避免跨任务搬迁两次；现有 session 切换状态测试
  保持通过；
- Desktop 全量测试为 178 通过、5 个 release fixture 忽略，dependency boundary
  14/14；七个 visual fixture 均为 `RMSE=0`；
- 两组 performance gate 通过；native GPU frame P95 为 `6.329 ms`、
  input-to-post-render P95 为 `8.356 ms`、steady RSS growth 为 `32 KiB`、
  Markdown completion P95 为 `134 us`；
- Clippy 仅保留 `DSK-000` 已记录的两项既有红灯，没有新增 warning。

### DSK-103：解耦 Composer Pane

**优先级：P1**
**风险：中**
**依赖：DSK-101**

**目标**

分离“输入控件渲染”和“提交/控制命令状态机”。

**工作**

1. 定义 `ComposerPaneViewModel`，包含 admission、active-operation、
   authorization-pending、running mode、rejection 和 focus-visible。
2. 将 `composer_input` Entity 与 input latency probe 放入 ComposerPane 或独立
   `ComposerInputController`。
3. `ComposerState` 和 command ledger 继续由 controller 持有，不移入 Render 层。
4. 输入变化、Submit、Steer、FollowUp 通过 typed event 上送。
5. Root 负责生成 command id，并在 runtime update 到达后推进 `ComposerState`。

**关键不变量**

- prompt draft 必须在 acceptance 后清理，rejection 后保留；
- Steer/FollowUp 的 session-scoped draft 和 running mode 不串 session；
- authorization pending 时输入仍可编辑；
- input-to-render latency probe 的定义和阈值不变；
- auto-grow、快捷键和焦点恢复不变。

**完成条件**

- ComposerPane 不再读取 Root projection；
- `ComposerState` 的单元测试保持原路径或通过 re-export 保持调用方不变；
- composer visual、latency 和 narrow-width 测试通过。

**完成记录（2026-07-28）**

- 新增 bounded `ComposerPaneViewModel`；ComposerPane 独占 `InputState`、focus handle、
  `InputEvent` subscription 和 `InputRenderLatencyProbe`，不再持有或读取
  `NativeShell`/`DesktopProjection`；
- `InputChanged`、`Focused`、`SubmitPrimary`、`Submit`、`SubmitRunning` 和
  `SetRunningMode` 均通过 typed event 上送；Root 继续独占 `ComposerState`、
  command ID/ledger、runtime admission，以及 session-scoped draft/running-mode map；
- prompt acceptance/rejection、Steer/FollowUp session isolation、authorization 可编辑、
  2 至 8 行 auto-grow、secondary Enter overlay routing 和 focus restoration 测试保持通过；
- Desktop 全量测试为 178 通过、5 个 release fixture 忽略，dependency boundary
  14/14；七个 visual fixture 均为 `RMSE=0`；
- headless input Change-to-render P95 为 `299 us`；native GPU frame P95 为
  `5.413 ms`、input-to-post-render P95 为 `8.346 ms`、steady RSS growth 为
  `44 KiB`、Markdown completion P95 为 `148 us`；
- Clippy 仅保留 `DSK-000` 已记录的两项既有红灯，没有新增 warning；
- `desktop-click-to-photon.sh` 仍为人工 fixture，本轮未将其计作自动通过项。

### DSK-104：收敛 Inspector 与 Overlay

**优先级：P1**
**风险：中**
**依赖：DSK-101**

**目标**

让授权、recovery、file review 和 inspector telemetry 使用显式 DTO。

**工作**

- 定义 `InspectorPaneViewModel` 和 `OverlayViewModel`；
- ViewModel 只携带公开、安全、bounded 的产品投影；
- file-review/recovery/authorization 操作继续通过 typed identity 上送；
- telemetry throttle 保留在 controller，不放入 Render；
- overlay focus trap 和 owner focus restoration 仍由 Root/FocusState 负责。

**完成条件**

- InspectorPane 和 OverlayHost 不再持有 `WeakEntity<NativeShell>`；
- authorization identity 没有退化成仅 ID 字符串；
- secret-safe runtime error 文本测试通过；
- focus trap、recovery 和 file review smoke tests 通过。

**完成记录（2026-07-28）**

- 新增 bounded `InspectorPaneViewModel`、`OverlayViewModel` 及其 typed row/request DTO；
  InspectorPane/OverlayHost 不再持有或读取 `NativeShell`、`DesktopProjection`、
  preferences、command ledger 或 SessionController；
- Root 继续独占 telemetry 的 250 ms throttle/deadline、inspector per-session section map、
  overlay lifecycle、FocusState/focus restoration 和 typed command admission；
- authorization DTO 保留完整 `ToolAuthorizationRequest`/identity；recovery 和 file review
  继续上送 typed identity/request，changed-file row 在 Root 构造 DTO 时保持 64 项上限；
- bounded file-review state 改为 `Arc<DesktopFileReviewState>` 共享，避免 4 Hz telemetry
  刷新复制 review rows/text；
- narrow Inspector 进入/退出 overlay 时由 overlay lifecycle owner 同步刷新 Inspector DTO，
  responsive drawer Close control、focus trap 和 owner focus restoration 测试保持通过；
- Desktop 全量测试为 178 通过、5 个 release fixture 忽略，dependency boundary
  14/14；七个 visual fixture 均为 `RMSE=0`；
- headless input Change-to-render P95 为 `292 us`；native GPU frame P95 为
  `5.367 ms`、input-to-post-render P95 为 `8.365 ms`、steady RSS growth 为
  `44 KiB`、Markdown completion P95 为 `111 us`；
- Clippy 仅保留 `DSK-000` 已记录的两项既有红灯，没有新增 warning；
- `desktop-click-to-photon.sh` 仍为人工 fixture，本轮未将其计作自动通过项。

### DSK-105：将现有 Conversation Helper 收敛为有状态 Controller

**优先级：P1**
**风险：中高**
**依赖：DSK-101 至 DSK-104**

**目标**

现有 `conversation_controller.rs` 主要是一组直接接收或操作 `NativeShell` 的 helper。
把 conversation 的增量准备、缓存、布局和 session-scoped view state 从
`NativeShell` 收敛到一个明确的有状态 controller；暂不改变算法。

**Controller ownership**

- `ConversationViewport`
- `ConversationRowLayoutState`
- `ConversationRowRenderCache`
- durable/live render rows 和 heights
- row sizes
- dirty sequences、overflow、width bucket 和 height refresh deadline
- expanded details、scroll restore 和 per-session view state

**Pane ownership**

- virtual list Entity/scroll handle
- 当前 `ConversationPaneViewModel`
- GPUI Render
- scroll、select、copy、expand、measurement typed events

**实施方式**

1. 先把现有 free functions 和 Root methods 搬进 `ConversationController`，保持调用点。
2. 再定义 PaneViewModel，移除 ConversationPane 的 Root 反向读取。
3. 最后让 Root 只调用 `controller.apply_delta(...)` 和
   `controller.build_view_model(...)`。
4. 每一步单独提交，禁止同时修改 row height 或 Markdown 算法。

**关键不变量**

- 10,000 row bounded history；
- 32 MiB transcript retained-bytes 上限；
- 40 MiB row render cache retained-bytes 上限；
- release performance fixture 至少覆盖 10 MiB transcript；
- streaming 15 Hz height commit 和 final settle；
- width bucket、resize debounce 和 paused-anchor compensation；
- follow-latest hysteresis；
- durable/live identity reconciliation；
- streaming-only delta 不 dirty Root。

**完成条件**

- Root 不再直接持有 conversation cache/layout/dirty-sequence 字段；
- ConversationPane 不再持有 `WeakEntity<NativeShell>`；
- 单行更新不扫描全部历史；
- conversation performance、scroll、selection、hydration 和 visual tests 全部通过。

**完成记录（2026-07-28）**

- `ConversationController` 现在私有持有 `ConversationViewport`、durable/live
  `ConversationRowLayoutState`、`ConversationRowRenderCache`、render rows/heights、
  row sizes、dirty sequences/overflow、width bucket、height refresh deadline、
  expanded details 和 per-session view state；`NativeShell` 不再声明或直接读写
  其中任何一项；
- 新增 bounded `ConversationSource<'a>`：只借用 `ConversationProjection`、
  live message/tool overlay 和 `SubmittedPromptPreview`。Root 用不相交字段借用构造
  它并交给 controller，因此 controller 拿不到 `NativeShell`、preferences 或
  command ledger；
- 原先接收 `&mut NativeShell` 的 free function（`follow_latest`、
  `align_scroll_to_bottom`、`reconcile_scroll`、`submit_row_measurement`）和 Root 方法
  （`rebuild_conversation_render_rows`、`rebuild_live_conversation_render_rows`、
  `update_conversation_rows_by_sequence`、`upsert_conversation_render_row`、
  `live_conversation_rows_match_projection`、`conversation_width_for_render`、
  `prepare_conversation_rows`）已全部收敛为 controller 方法；算法未改动；
- 需要 `cx` 的副作用改为返回值：`submit_row_measurement` 返回
  `ConversationMeasurementOutcome`，`reconcile_scroll` 返回是否需要通知，
  `arm_height_refresh`/`fire_height_refresh` 与
  `width_for_render`/`commit_pending_width` 把 deadline 判定留在 controller、
  只把 spawn 留给 Root；
- Root 侧保留的 conversation 职责只剩 typed event 路由、`cx.spawn` 计时器、
  pane/header notify 和 `conversation_pane_view_model`；
- `MAX_DIRTY_CONVERSATION_SEQUENCES`、`MAX_EXPANDED_CONVERSATION_DETAILS` 和
  session view state 上限随所有权迁入 controller，数值不变（均为 256）；
- 修正了一处失效断言：`conversation_transcript_rendering_is_owned_by_a_child_entity`
  仍要求 ConversationPane 持有 `WeakEntity<NativeShell>`，与 `DSK-105` 完成条件相反。
  该断言已改为按其它 Pane 的既有模式校验 ViewModel 边界（无 Root 反向引用、
  不读取 `DesktopProjection`）；
- `conversation_list_sizes_persist_and_full_history_work_is_dirty_gated` 原有多条
  断言使用整段字面量、会匹配测试自身源码而恒真。已改为拆分拼接字面量，并同时校验
  controller 拥有、`native_shell.rs` 不拥有；
- dependency boundary 增加 `DSK-105` 正反向断言：15 项算法/trace event 与 9 项状态字段
  必须在 controller、不得在 Root 生产代码；controller 不得出现
  `WeakEntity<NativeShell>`、`&mut NativeShell`、`Context<NativeShell>` 或
  `super::NativeShell`；
- Desktop 全量测试为 178 通过、5 个 release fixture 忽略，dependency boundary
  14/14；七个 visual fixture 均为 `RMSE=0`；
- `cargo fmt --all -- --check` 与 `git diff --check` 通过；
- headless input Change-to-render P95 为 `357 us`、10k-row headless CPU frame P95 为
  `3.246 ms`、10 MiB transcript hydration 为 `15.1 ms`；native GPU frame P95 为
  `6.062 ms`、input-to-post-render P95 为 `8.356 ms`、steady RSS growth 为 `60 KiB`、
  production Markdown completion P95 为 `127 us`，均在预算内且未调整任何预算；
- Clippy 仅保留 `DSK-000` 已记录的两项既有红灯，没有新增 warning；
- `desktop-click-to-photon.sh` 仍为人工 fixture，本轮未将其计作自动通过项。

### DSK-201：机械拆分 Runtime 协议与桥接

**优先级：P1**
**风险：中**
**可与 DSK-102 至 DSK-104 并行；Runtime lane 内部步骤串行**

**第一步：`runtime/protocol.rs`**

移动以下纯类型，保持名称、可见性、serde 和 public re-export 不变：

- `DesktopRuntimeCommand` / `DesktopRuntimeCommandKind`
- snapshot/recovery DTO
- `DesktopRuntimeUpdate`
- start/admission/shutdown error
- queue/input constants和纯 validation

**第二步：`runtime/bridge.rs`**

移动：

- `DesktopRuntimeBridge`
- `DesktopRuntimeBootstrap`
- `DesktopRuntimeCommandHandle`
- `DesktopRuntimeEventStream`
- `DesktopRuntimeShutdownGuard`
- channel 创建、spawn、join、batch/coalescing

**不得移动到此层**

- `RuntimeState`
- `ActivePrompt`
- `CodingAgentSession`
- event acknowledgement/reconnect driver

**完成条件**

- `desktop::runtime::*` 的 crate 内导入无需批量改写；
- command/update enum 形状不变；
- queue capacity、priority ordering 和 coalescing tests 通过；
- runtime thread panic 和 shutdown guard tests 通过。

**建议提交**

```text
refactor(desktop): separate runtime protocol and bridge
```

### DSK-202：拆分 Runtime Driver 与 Dispatch

**优先级：P1**
**风险：高**
**依赖：DSK-201**

**`runtime/driver.rs` 保留**

- `RuntimeState`
- `ActivePrompt` / `ActiveSignal`
- `run_runtime`
- ProductEvent receive/publish/ack/drain
- reconnect/fresh-snapshot recovery
- prompt completion和shutdown deadline

**`runtime/dispatch.rs` 保留**

- idle command dispatch
- active prompt command dispatch
- typed rejection construction
- selection、authorization、recovery、file-review command handling

**实施约束**

- 第一提交只移动代码和 re-export，不修改 match 分支；
- 第二提交才允许收敛重复 validation 或 error mapping；
- event pump、shutdown 和 reconnect 函数不得在同一提交中重写；
- 任何异步 select 顺序变化都视为行为变更，必须单独评审。

**关键不变量**

- `ProductEvent` publish 成功后才 ack；
- reconnect gap/lag 返回 fresh snapshot；
- terminal event 在 `PromptFinished` 前 drain；
- active prompt shutdown 有 10 秒 deadline；
- dropped UI receiver 能终止并回收 runtime；
- session owner 不被两个 task 同时持有；
- pending authorization 在 shutdown 时被取消。

**完成条件**

- driver 测试覆盖 event order、ack、gap/lag、abort race、terminal slot release；
- dispatch 测试覆盖所有 16 个 command kind；
- 原 `runtime.rs` 只保留模块声明/re-export，或改名为 `runtime/mod.rs`；
- Desktop 全量测试和 coding-agent cross-adapter fixture 通过。

**执行结果（2026-07-28）**

- `runtime.rs` 已收敛为 `bridge`、`dispatch`、`driver`、`protocol` 模块声明、稳定
  re-export、私有 `run_runtime` 导入和 `tests` 声明；
- `dispatch.rs` 只拥有 idle/active command match 与 typed rejection，未持有
  `tokio::select!`、ProductEvent reconnect/publish/ack/drain 或 shutdown deadline；
- `driver.rs` 保留唯一 session owner、异步 select、publish 后 ack、gap/lag recovery、
  terminal drain 和 10 秒 shutdown deadline；dependency boundary 增加正反向 ownership
  断言；
- `cargo fmt --all -- --check`、`cargo check -p desktop --all-targets`、Runtime 27 项定向
  测试、Desktop 全量测试（177 通过、5 个 release fixture 忽略）、
  dependency boundary（14/14）、`git diff --check` 通过；
- `scripts/desktop-perf-gate.sh` 通过；active-display native gate 通过，GPU frame P95
  为 `5.765 ms`、input-to-post-render P95 为 `8.349 ms`、steady RSS growth 为
  `144 KiB`、Markdown completion P95 为 `182 us`；
- Clippy 仅保留 `DSK-000` 已记录的两项既有红灯，没有新增 lint。

### DSK-301：拆分 Conversation 纯逻辑

**优先级：P2**
**风险：低至中**
**依赖：DSK-105**

按以下顺序机械拆分，每一步保留 `conversation::` re-export：

1. `markdown.rs`：`MarkdownPreview`、builder、安全边界和相关常量；
2. `copy.rs`：`conversation_copy_text` 和 copy byte limit；
3. `composer.rs`：`ComposerState`、admission、submission kind；
4. `render_cache.rs`：render source/data/cache 和 streaming phase；
5. `layout.rs`：row measurement、height state、width bucket；
6. `viewport.rs`：selection、follow-latest、scroll reconciliation；
7. `model.rs`：block/item identity 和 `ConversationProjection`。

**约束**

- 不创建不存在的 `ConversationSelection` 抽象；
- selection 继续属于 viewport；
- 不在模块迁移中调整常量或内存预算；
- 不改变 Markdown sanitization 和 media neutralization；
- 不复制同一常量到多个模块，统一从 owner 模块 re-export。

**完成条件**

- 原有 `conversation::tests` 按 owner 模块拆分；
- public/crate-visible 调用点通过 re-export 保持稳定；
- memory/performance matrix 无回退；
- `conversation.rs` 被目录模块替代后不存在循环依赖。

**完成记录（2026-07-28）**

七步已全部完成，旧 `conversation.rs` 已由目录模块替代。`conversation/mod.rs` 只声明
七个 owner 并维持稳定 `conversation::` re-export，不再定义业务 struct、enum 或
function：

- `markdown.rs` 独占 bounded Markdown preview、sanitization、安全边界和相关常量；
- `copy.rs` 独占 bounded copy、UTF-8 安全 byte truncation 和 `MAX_COPY_BYTES`；
- `composer.rs` 独占 composer admission、submission kind、submitted preview 和错误；
- `render_cache.rs` 独占 render source/data/cache、streaming phase、settle budget，以及
  初始 render-data 高度估算；
- `layout.rs` 独占实际 measurement、height commit、15 Hz throttle、paused anchor
  compensation 和 width bucket；
- `viewport.rs` 独占 selection、follow-latest、unseen update 和 hydration reconciliation；
- `model.rs` 独占 block/item identity、projection、hydration、bounds 和 content revision。

计划中的高度职责根据真实依赖作了收敛：`conversation_block_height` 和 Unicode 行数
估算归 `render_cache.rs`，因为它们产生 render data 的初始高度；`layout.rs` 只处理
实际测量和提交策略。若把初始估算放进 `layout.rs`，会同时产生
`render_cache -> layout` 和 `layout -> render_cache::StreamingTextPhase`，形成生产循环。
最终依赖图为：

```text
model -> copy
composer -> model + copy
render_cache -> model + markdown
layout -> model + render_cache
viewport -> model
```

新增 `conversation_presentation_modules_have_stable_acyclic_owners` boundary，锁定七个
owner、稳定 re-export、无业务实现的 `mod.rs` 和上述无环方向。模块迁移还暴露出外部
性能脚本仍过滤旧的 `conversation::tests::desktop_release_`，Cargo 因而运行 0 项测试
却返回成功。Bash 和 PowerShell gate 现逐项使用完整路径和 `--exact`，并要求输出
`running 1 test`；boundary 同时锁定五项 release fixture 和 fail-closed 契约。

最终验证：

- `cargo check -p desktop --all-targets` 通过；
- conversation 定向测试 41 项通过、3 项 release fixture 忽略；
- Desktop 全量测试 184 项通过、5 项 release fixture 忽略；
- dependency boundary 15/15，`cargo fmt --all -- --check` 与 `git diff --check` 通过；
- wide、medium、narrow、authorization、reduced-motion、keyboard-focus、no-color
  七个 visual fixture 全部 `RMSE=0`；
- native GPU frame P95 `6.403 ms`、input-to-post-render P95 `8.382 ms`、
  steady RSS growth `32 KiB`、production Markdown completion P95 `130 us`；
- headless 10k-row CPU frame P95 `2.863 ms`、input roundtrip P95 `5.394 ms`、
  input Change-to-render P95 `323 us`、window RSS growth `24,997,888 bytes`；
- 10 MiB hydration `14.541 ms`、10k-block hydration `3.196 ms`、
  scroll/render preparation P95 `210 us`、streaming event P95 `4–21 us`；
- Desktop Clippy 仅保留 `DSK-000` 已记录的
  `field_reassign_with_default` 和 `large_enum_variant`，没有新增 lint。

### DSK-401：可选低优先级整理

**优先级：P3**

仅在主计划的代码实现与自动 gate 完成后评估；外部物理 click-to-photon 样本不阻塞
这项纯结构收益判断：

- `allocation_probe` 提取到独立测试模块；
- `resident_memory` 提取到独立文件，同时更新 dependency boundary gate；
- `shell.rs` 仅在 ownership 明确时拆成 layout/theme/focus；
- 将大文件内嵌测试移动到相邻 `tests.rs`。

这些任务只改善导航，不应宣称提供架构或性能收益。

### VUI-000：冻结视觉问题基线

**优先级：P0**
**可与 DSK-000 合并执行，但不合并提交**

**工作**

- 对 wide、medium、narrow、authorization、reduced-motion、keyboard-focus 和
  no-color 七个 fixture 执行 visual review；
- 记录 Header、Sessions、Composer、Inspector、Conversation 和 Overlay 的永久可见
  action 数量与语义；
- 把当前已确认问题作为后续 VUI task 的 before 基线；
- 不安装新 golden，不修改 RMSE、尺寸或性能预算。

**完成条件**

- 当前截图与现有 golden 的差异已记录；
- 每个问题能指向一个 VUI task，不存在“顺手全局美化”；
- keyboard focus、no-color 和 authorization 状态都纳入基线。

### VUI-101：建立共享控件语义

**优先级：P1**
**依赖：VUI-000**
**可与 DSK-101、DSK-201 并行**

**目标**

提供 Pane 可复用的图标、action row、Tab 和紧凑选择器语义，但不建立第二套完整组件库。

**工作**

1. 盘点 `gpui-component` 已有 Button、Icon、Tab、Dropdown 和 List primitive。
2. 选择统一图标资源；优先复用依赖内资产，不手写 SVG。
3. 仅在现有 primitive 不足时增加 `desktop_controls.rs`，提供：
   - 32/36/40 px 稳定尺寸的 icon button variant；
   - panel toggle、copy、chevron、overflow、search、clear、plus 和 submit 图标；
   - action row 的 hover、selected、disabled、keyboard-focus 状态；
   - 不换行的 Tab strip/segmented control；
   - current value + chevron 的 compact selector。
4. 每个仅图标控件必须同时具有 tooltip、accessible label 和稳定 hit target。
5. 本任务不全局修改 radius、颜色或 Pane 布局。

**完成条件**

- familiar tool action 不再要求用文本句点或手写 glyph 充当图标；
- primitive 的 hover/focus/disabled/selected 状态有隔离测试；
- primitive 本身不读取 projection、controller 或 `NativeShell`。

**完成记录（2026-07-28）**

盘点结论：`gpui-component` 已覆盖绝大部分需求，因此 `desktop_controls.rs` 只做
薄语义层，不建第二套组件库。

- 现成可用、不再自造：`tab/tab_bar`（已 `overflow_x_scroll`，天然不换行，直接解决
  Inspector Tab 换行）、`kbd`、`description_list`（用于 authorization 明细对齐）、
  `badge`、`tag`、`tooltip`、`spinner`、`list`、`combobox`；
- `Button` 已提供 `primary`/`danger`/`ghost`/`outline`/`selected`/`disabled`/
  `loading`/`tooltip`/`dropdown_caret`/`tab_index`，权重阶梯直接构建其上；
- 图标源：新增 `gpui-component-assets` 依赖（rev 与 `gpui-component` 完全一致），
  在 `app.rs` 的 `application().with_assets(...)` 注册。此前 desktop **一个图标都没用、
  也没有 asset source**，这正是所有控件退化成文字按钮的根因；
- 107 个 Lucide SVG 覆盖计划要求的全部图标，`IconName` 由资产目录在编译期生成，
  命名不存在的图标无法通过编译，因此不存在手写 SVG 的路径。

`desktop_controls.rs` 提供：

- `DesktopIcon`：语义图标词汇，Pane 只命名意图不写资产文件名；
- `DesktopControlSize`：28/32/36/40 px 固定高度阶梯，enabled/disabled/busy/selected
  之间高度不变，因此状态切换不会引起宿主重排；
- `DesktopControlWeight` 与 `DesktopCriticalTone`：Tool / Selector / Primary /
  Critical(Neutral·Affirmative·Dangerous)，使 `Deny` 与 `Allow for operation`
  不可能渲染成同一权重；
- `DesktopIconButton`：**构造函数强制要求 accessible label**，同一字符串同时作为
  tooltip，两者不可能漂移；`busy` 只换图标不换盒子；
- `DesktopSelector`：当前值 + caret，读作"值"而非"动作"；
- `DesktopActionRow`：整行 action surface，选中用背景 + accent 竖轨而非边框
  （no-color 下仍可辨认，且不增加行高），trailing 工具位宽度恒定预留，
  hover 显形不会引起行内重排。

- 六项隔离测试覆盖高度阶梯、图标解析、critical 三档色彩互异、busy 保持盒子并拒绝
  输入、行状态默认值与独立可设；另有一项断言 primitive 实现不含
  `DesktopProjection`、`NativeShell`、`ConversationController` 或 command ledger；
- dependency boundary 增加：icon 资产 rev 必须与 component rev 相等；asset source
  必须注册在 bootstrap（注册在别处会导致仅图标控件渲染为空）；
- 本任务不修改任何 Pane，因此七个 visual fixture 全部 `RMSE=0`；
- Desktop 全量 184 通过、5 个 release fixture 忽略，boundary 14/14，
  fmt 与 `git diff --check` 通过；Clippy 仅 `DSK-000` 两项既有红灯；
- 计划要求的 radius 收敛留给 `VUI-201`：现有 token 为 sm=6/md=10/lg=12，
  本任务按"不全局修改 radius、颜色或 Pane 布局"约束未动。

### VUI-102：简化 Header 与 StatusBar

**优先级：P1**
**依赖：DSK-101、VUI-101**

**工作**

- `Sessions` 和 `Inspector` 改为 panel-left/panel-right icon toggle；
- `...` 改为 overflow icon，不再使用三个文本句点；
- model/profile 保留为唯一的紧凑选择器，显示当前值和 chevron；
- 从 StatusBar 删除重复的 model/profile 入口；
- thinking override 在 VUI-104 前暂时只保留一个 StatusBar 入口，不新增重复入口；
- StatusBar 除上述临时入口外收敛为被动状态：lifecycle、changed-file count、notice；
- `Abort` 保留危险色文字按钮，并继续显示 pending 状态。

**响应式规则**

- narrow 优先保留 panel toggle、任务标题、状态和 overflow；
- 项目名可截断，控制按钮不得挤压到不可识别；
- 不通过 viewport width 缩放字体；
- Header 高度和 StatusBar 高度在状态切换时不变化。

**完成条件**

- Header 不再重复左右 Pane 标题；
- Header 与 StatusBar 不再出现两套配置入口；
- wide/medium/narrow 无文本溢出、按钮换行或布局跳动；
- keyboard focus 和快捷键行为保持不变。

**完成记录（2026-07-28）**

- `Sessions` 与 `Inspector` 已改为 bundled panel-left/panel-right icon toggle，图标和
  accessible label 会按 docked panel 或 narrow overlay 的真实开闭状态切换；
- `...` 已改为 bundled overflow icon；reload 仍位于原 typed dropdown event；
- Header 使用唯一的 model/profile `DesktopSelector`，显示当前值和 caret，原有
  selection disabled/admission 规则不变；thinking 字段和 event 已从 Header contract 删除；
- StatusBar 的 model/profile 字段、event 和控件已删除；临时 thinking selector 是其
  唯一配置入口，lifecycle、changed-file count、notice 与 command hint 保持被动展示；
- Abort 继续使用危险色文字和 pending disabled 状态，没有退化为仅图标；
- overlay activate/dismiss 会刷新 Header DTO，因此 narrow drawer 的图标、tooltip 和
  accessible label 不会显示过期状态；
- 三档 viewport 无 Header/StatusBar overlap，五项主控件均保持最小 hit target；
  responsive drawer、authorization、keyboard-focus、typed owner 测试通过；
- Desktop 全量单线程验证 184 项通过、5 项 release fixture 忽略，boundary 15/15；
  `cargo check -p desktop --all-targets`、fmt 和 diff check 通过；
- 七个 before/after/diff 已人工 review 并安装，安装后 wide、medium、narrow、
  authorization、reduced-motion、keyboard-focus、no-color 全部 `RMSE=0`；
- headless 10k-row CPU frame P95 `3.324 ms`、input roundtrip P95 `6.562 ms`、
  input Change-to-render P95 `341 us`、window RSS growth `24,834,048 bytes`；
- native GPU present P95 `6.377 ms`、input-to-post-render P95 `8.340 ms`、
  steady RSS growth `36 KiB`、production Markdown completion P95 `141 us`；
- Desktop Clippy 仍仅 `DSK-000` 的两项既有红灯。交互式
  `desktop-click-to-photon.sh` 需要人工按键采样，本任务未把它计作自动通过项。

### VUI-103：重做 Sessions 管理列表

**优先级：P1**
**依赖：DSK-102、VUI-101**

**工作**

- 整个 session row 成为可点击、可聚焦、可按 Enter 激活的 action row；
- 删除每行永久显示的 `Open` 按钮；
- active session 使用 selected background、状态标记和 `aria-selected`；
- `New` 改为 plus icon button，overflow 使用统一图标；
- 搜索区域增加 search icon、focus treatment 和有内容时的 clear action；
- catalog 为空时显示轻量 empty state；
- 辅助行菜单只在 hover/focus 时显示，且不能造成行宽变化。

**不得改变**

- create/open/list command intent 和 ledger 对账；
- search、recent ordering、relative time、omitted count；
- session 切换期间的 disabled 条件；
- current session 与 recent session 的可访问名称。

**完成条件**

- Sessions 常态不再出现逐行 `Open` 文字按钮；
- pointer 和 keyboard 都能通过整行打开 session；
- empty、loading、filtered-empty、omitted 四种状态均有明确展示；
- medium 和 narrow overlay 使用相同 action-row 语义。

**完成记录（2026-07-28）**

- session row 统一使用 Button-backed `DesktopActionRow`：整行提供焦点、Enter 激活、
  selected/disabled 状态和稳定命中区；active row 保留 selection background 与
  不依赖颜色的 selection rail，recent row 不再永久显示 `Open`；
- `New`、overflow、search、clear 和 overlay close 全部使用共享图标词汇，并保留
  tooltip、accessible label 与 typed `SessionsPaneEvent` 路径；
- `SessionsPane` 独占 search Input、catalog 呈现、pending/disabled 判断，以及 empty、
  loading、filtered-empty、omitted 四类状态；
- narrow Sessions dialog 直接挂载与 docked layout 相同的 `SessionsPane` entity；
  `OverlayHost` 删除重复的 session DTO、catalog/pending ViewModel 字段和
  create/refresh/open event，因此两种布局不会产生行为漂移；
- command intent、ledger association、recent ordering、relative time、refresh deadline、
  omitted bound、Escape、scroll geometry 和 focus restoration 均保持不变；
- Desktop 全量单线程验证 184 项通过、5 项 release fixture 忽略，boundary 15/15；
  `cargo check -p desktop --all-targets`、fmt 和 diff check 通过；
- 七个 before/after/diff 已人工 review 并安装，安装后 wide、medium、narrow、
  authorization、reduced-motion、keyboard-focus、no-color 全部 `RMSE=0`；
- headless 10k-row CPU frame P95 `2.878 ms`、input roundtrip P95 `5.210 ms`、
  input Change-to-render P95 `317 us`、window RSS growth `24,981,504 bytes`；
- native GPU present P95 `4.439 ms`、input-to-post-render P95 `8.367 ms`、
  steady RSS growth `36 KiB`、production Markdown completion P95 `124 us`；
- Desktop Clippy 仍仅 `DSK-000` 的两项既有红灯，没有新增 lint。交互式
  `desktop-click-to-photon.sh` 仍需人工按键采样，本任务未把它计作自动通过项。

### VUI-104：重做 Composer 输入与提交区

**优先级：P1**
**依赖：DSK-103、VUI-101**

**工作**

1. 将 Composer 改为一个连续 surface，而不是“输入区 + 固定 176 px 文字按钮列”。
2. 空输入/单行时使用约 48-56 px 紧凑高度，多行输入只向上增长。
3. Submit 使用 36-40 px 的 arrow-up/send 图标按钮，与输入控制行对齐。
4. submit pending 使用同尺寸 spinner/disabled 状态，不扩大按钮。
5. active operation 时在底部 toolbar 显示 `Steer now / Queue next` 紧凑模式选择器，
   旁边仍使用同尺寸 submit 图标。
6. thinking override 放入 Composer toolbar，并与 running-mode selector 区分。
7. rejection/authorization notice 使用不改变输入宽度的 inline status row。
8. Composer toolbar 接管 thinking 后删除 StatusBar 的临时 thinking 入口。

**关键不变量**

- Submit、Steer、FollowUp typed event 和 command id 不变；
- acceptance 后清理 draft、rejection 后保留 draft；
- authorization pending 时仍可编辑；
- auto-grow、输入法、快捷键、焦点恢复和 latency probe 不变；
- 动态 notice 和 mode 切换不得导致 composer 横向位移。

**完成条件**

- 发送动作不再显示为宽大的 `Send` 文字按钮；
- 输入与提交按钮在 empty、multiline、pending、running 四种状态下对齐；
- narrow 下不遮挡输入、不换行成第二个独立 action block；
- click-to-photon、composer latency、keyboard-focus 和视觉测试通过。

**完成记录（2026-07-28）**

- Composer 的 notice、输入和 action toolbar 已合并为一个连续 bordered surface；
  固定 `176 px` action column 删除，单行 content 保持 `48–56 px`，InputState 从
  `auto_grow(1, 8)` 起步并继续只向上增长；
- idle 与 active-operation 均使用同一个 `36 px` Submit icon box；pending/
  awaiting-start 只把同一 box 切换为 Busy/disabled，不改变几何；
- running mode 使用紧凑 `DesktopSelector`，与 thinking selector、Submit 在同一行按
  中心线对齐；`SteerNow`、`QueueNext` 和 session-scoped mode 的 Root 所有权不变；
- thinking 作为 bounded `ComposerPaneViewModel` 字段进入 Pane，并通过
  `ComposerPaneEvent::CycleThinking` 回到 Root；StatusBar 删除 thinking 字段、事件
  与控件后恢复为纯被动状态；
- rejection 与 authorization notice 成为 surface 内的全宽 inline status row；
  authorization pending 仍可编辑，notice/mode 切换不改变 input width；
- 新增真实点击测试，确认 Composer selector 经 typed event 修改 thinking selection，
  且 StatusBar 不再暴露旧控件；Submit/Steer/FollowUp、command id、draft acceptance/
  rejection、快捷键、焦点和 latency probe 契约均保持；
- Desktop 全量单线程验证 185 项通过、5 项 release fixture 忽略，boundary 15/15；
  `cargo check -p desktop --all-targets`、fmt 和 diff check 通过；
- 七个 before/after/diff 已人工 review 并安装，安装后 wide、medium、narrow、
  authorization、reduced-motion、keyboard-focus、no-color 全部 `RMSE=0`；
- headless 10k-row CPU frame P95 `2.518 ms`、input roundtrip P95 `5.493 ms`、
  input Change-to-render P95 `354 us`、window RSS growth `24,850,432 bytes`；
- native GPU present P95 `5.216 ms`、input-to-post-render P95 `8.367 ms`、
  steady RSS growth `48 KiB`、production Markdown completion P95 `128 us`；
- Desktop Clippy 仍仅 `DSK-000` 的两项既有红灯，没有新增 lint。交互式
  `desktop-click-to-photon.sh` 仍需物理按键/光学采样，本任务未把内部 post-render
  latency 冒充 photon 结果；因此本任务实现与自动 gate 已完成，但最终状态保持
  “待人工验收”。

### VUI-105：收敛 Inspector 与 Overlay 控件

**优先级：P1**
**依赖：DSK-104、VUI-101**

**工作**

- Inspector section 使用单行 Tab strip/segmented control，删除 `●/○` 字符；
- Runtime attention 使用独立 badge，不把 badge 当作 Tab 文本；
- changed file 使用 action row：mutation badge、文件名、路径和 selected state；
- copy path/copy review 使用图标工具动作，open editor 使用熟悉图标和 tooltip；
- narrow Inspector 的 Close 使用 close icon，保留 Escape；
- authorization action 保留文字，区分 Deny、Allow once、Allow for operation 的权重；
- `1/2/3` 快捷键使用 `kbd` 样式，不拼进 action label；
- authorization detail 使用 definition-list 对齐，不用手写空格模拟列。

**完成条件**

- Inspector Tab 在允许的 panel 宽度内不换行；
- changed-file 列表不再表现为一列大号描边按钮；
- authorization、recovery 和 external-editor typed identity 完整保留；
- focus trap、no-color、authorization 和 narrow-context fixtures 通过。

**完成记录（2026-07-28）**

- Inspector 的 Changes、Task、Usage、Runtime 已改为无 `●/○` 字符的单行 Tab strip；
  docked `1300 px` 与 narrow overlay `700 px` 的四个 Tab 均共享同一行、同一高度，
  Runtime attention 继续由独立 badge 表达；
- changed file 已改为全宽 `DesktopActionRow`：mutation badge、文件名、带省略号的路径
  metadata 和 selected rail 均在同一个受约束 flex 行内；整行真实点击仍提交原始
  `CodingAgentFileReviewRequest`，未用显示用截断字符串重建 identity；
- 共享 action row 不再把可见 Button label 放在自定义行布局之外；内容节点显式写入
  `Role::Button`、完整 aria label 与 selected 状态，Sessions/changed-file 的键盘和
  assistive-technology 语义继续由同一 primitive 保证；
- Ready review 的 Copy path、Copy review、Open external editor 和 narrow Inspector
  Close 均使用带 tooltip 的 Lucide icon tool；三个 review tool 与 Close 保持固定
  `32 px` 命中区，external-editor busy 状态不改变几何，Escape/focus restoration 不变；
- authorization 的 operation、tool、risk、scope、cwd、command 使用固定 term/value
  两列；`1/2/3` 单独渲染为 `kbd` marker。按钮文案、tooltip、shortcut 与视觉 tone
  由 typed `ToolAuthorizationDecision` 统一推导：Deny 为危险动作、Allow once 为
  次级动作、Allow for operation 为主动作，identity 与三条快捷键路径保持；
- 新增/扩展真实布局与点击测试，覆盖 docked/overlay Tab 单行、changed-file 整行
  command、Ready review icon 命中区、授权 definition-list 对齐和三个 decision 命中区；
  accessibility 合同同步验证 action row 的 AccessKit 元数据；
- Desktop 全量验证 186 项通过、5 个 release fixture 忽略，dependency boundary
  15/15；`cargo check -p desktop --all-targets`、fmt 与 diff check 通过；
- 七组 before/after/diff 已人工 review 并安装；最终 wide、medium、narrow、
  authorization、reduced-motion、keyboard-focus、no-color 全部 `RMSE=0`。额外打开
  narrow replay，确认 Close、四 Tab 和 changed-file row 均无越界；
- headless 10k-row CPU frame P95 `3.216 ms`、input roundtrip P95 `6.033 ms`、
  input Change-to-render P95 `401 us`、window RSS growth `24,674,304 bytes`；
  10 MiB hydration `14.583 ms`、10k-block hydration `2.657 ms`、
  scroll/render preparation P95 `204 us`、streaming event P95 `4–20 us`；
- native GPU present P95 `5.567 ms`、input-to-post-render P95 `8.351 ms`、
  steady RSS growth `136 KiB`、production Markdown completion P95 `123 us`；
- Desktop Clippy 仍仅 `DSK-000` 的 `field_reassign_with_default` 与
  `large_enum_variant` 两项既有红灯，没有新增 lint。交互式
  `desktop-click-to-photon.sh` 仍需人工按键采样，本任务未把它计作自动通过项。

### VUI-106：降低 Conversation 永久按钮密度

**优先级：P2**
**依赖：DSK-105、VUI-101**

**工作**

- Reasoning 和 Tool header 整行可切换，尾部使用 chevron 展开状态；
- 删除永久显示的 `Show/Hide` 文字按钮；
- copy code/command/output 使用 copy icon，在 hover/focus 时提高可见性；
- open-full-output/full-message 使用 expand/external-view 图标；
- Retry、Mark failed、Abort 等有业务后果的操作继续保留文字；
- 减少 Tool/Reasoning card 的无效 padding，不改变 row measurement 算法。

**完成条件**

- hover-only action 也能通过 keyboard focus 发现和执行；
- 图标出现/消失不改变 row size；
- expand/copy/select event 和 announcement 不变；
- streaming、measurement、scroll anchoring 和 performance gate 无回退。

**完成记录（2026-07-28）**

- Reasoning 的 collapsed/expanded header 均成为带 `Role::Button`、
  `aria_expanded` 和完整 accessible label 的 disclosure surface，尾部固定为
  32 px `ChevronDown`/`ChevronUp`；Tool header 的主 surface 与尾部 chevron
  是相邻 action，不再把 Button 嵌入另一个 clickable parent。真实点击测试曾捕获
  nested click 的双重切换风险，拆成 sibling 后整行和 chevron 均只发送一次
  `ToggleDetails`；
- Tool 主 surface 和 chevron 继续先发送原有 typed `Select`，再发送
  `ToggleDetails`；Reasoning 继续只发送 `ToggleDetails`。长 Tool/Reasoning fixture
  通过实际 header 点击展开，验证 measured row height 增长且 tail 仍在 row 与
  Composer 上方；另一个 bounded Reasoning fixture 实际点击 chevron 展开再折叠，
  折叠前后 card bounds 相同；
- 删除 Conversation 中永久的 `Show`/`Hide`、`Copy command`、`Copy output`、
  `Copy code`、`Open full output` 与 `Open full message` 文字按钮；Markdown code、
  Tool command/output、conversation row 与 open-full action 改用固定 32 px
  Lucide Copy/Expand icon tool，tooltip 与 accessible label 仍由
  `DesktopIconButton` 同源生成。Retry、Mark failed、Abort 等有业务后果的 action
  保留文字；
- row Copy 的 32 px box 永久预留，只用 opacity 在 resting `0` 与
  hover/focus/selected `1` 间切换，因此出现/消失不改变 card bounds。这里没有使用
  `.invisible()`：GPUI 会在 hidden visibility 分支提前结束 paint，导致 tab stop
  根本不登记、focus style 永远无法触发；零 opacity 保留 tab order 与 keyboard
  execution，同时保持 resting 视觉密度；
- 新增/加强真实 GPUI 点击测试：Tool 整行 disclosure、Tool chevron selection +
  disclosure、Reasoning 整行与 chevron、structured command/output copy、
  full-output/full-message overlay、Markdown code copy、row selection/copy 均走
  `debug_bounds` + `simulate_click`，不再直接修改 controller；所有 icon action
  校验至少 32x32 hit target，row selected 前后 card bounds 完全一致；
- dependency boundary 原先只统计 Pane 内直接出现的 `.tooltip(...)`，共享
  `DesktopIconButton` 收敛 tooltip 后会误报。边界现统计 direct tooltip 与 shared
  icon-tool 调用点之和，并单独锁定
  `.tooltip(self.accessible_label.clone())`，没有降低 action discoverability 阈值；
- reviewed wide/medium/narrow 截图确认 Reasoning/Tool chevron、Markdown Copy icon、
  transcript wrapping、Composer 边界均无越界；authorization、reduced-motion、
  keyboard-focus 和 no-color derivative 保持相同 geometry 与非颜色语义。七个
  golden 安装后全部 `RMSE=0`；默认 fixture 的 Tool 为 collapsed，expanded Tool
  的三项 icon action 由上述真实 click/geometry/clipboard/overlay 测试补足审查；
- Desktop 全量 `188 passed / 5 ignored`，dependency boundary `15/15`，
  `cargo check -p desktop --all-targets`、fmt 与 `git diff --check` 通过；
- headless 10k-row CPU frame P95 `2.984 ms`、input roundtrip P95 `6.309 ms`、
  input Change-to-render P95 `386 us`、window RSS growth `24,879,104 bytes`；
  10 MiB hydration `14.382 ms`、10k-block hydration `2.487 ms`、
  scroll/render preparation P95 `202 us`、streaming event P95 `5–23 us`；
- native GPU present P95 `5.126 ms`、input-to-post-render P95 `8.361 ms`、
  steady RSS growth `57,344 bytes`、production Markdown completion P95
  `4.884 ms`，均低于现有预算；
- Desktop Clippy 仍仅 `DSK-000` 的 `field_reassign_with_default` 与
  `large_enum_variant` 两项既有红灯，没有新增 lint。交互式
  `desktop-click-to-photon.sh` 仍需人工按键采样，本任务未把它计作自动通过项。

### VUI-201：统一视觉节奏和层级

**优先级：P2**
**依赖：VUI-102 至 VUI-106**

主流程稳定后再统一 token，避免每个 Pane 重复产生大范围 golden churn：

- 将 radius 收敛为不超过 8 px 的小/中/大层级；
- 减少同时使用 background、border 和 rounded card 的场景；
- 用 spacing、selected background 和 divider 表达层级；
- 保持 neutral canvas/surface，同时限制 danger、warning、reasoning accent 的面积；
- compact panel 内只使用 metadata/body/title 三档，不引入 hero 级文字；
- 统一 icon button、selector、Tab、action row 和 critical action 的高度。

**完成条件**

- 无嵌套 card 和无语义的浮动 section；
- no-color 下仍能区分 selected、disabled、warning 和 destructive action；
- 所有 fixture 人工 review 通过，并记录全局 token 变化原因；
- 不以提高尺寸、缓存或性能预算换取视觉通过。

**完成记录（2026-07-28）**

- radius 从 `6/10/12 px` 收敛为 `4/6/8 px`，并由 public token test 与
  `DesktopStyledExt` adapter test 双重锁定 `lg <= 8 px`；spacing 继续保持
  `4/8/12/16/24 px`，没有为视觉通过增加留白预算；
- compact panel 继续只消费 Metadata `12/16`、Body `14/21`、Title `16/24`
  三档 public typography token，没有新增 raw text size 或 hero 文字；
- control ladder 保持 Tool `28 px`、Compact `32 px`、Standard `36 px`、
  Critical `40 px`。IconButton、selector 与 Inspector Tab 明确共用 Compact
  高度；Header Abort、authorization decision、Inspector recovery 与 Conversation
  Diagnostic recovery 全部迁入共享 `DesktopCriticalButton`，固定为 `40 px`，
  disabled/tone 只改变语义呈现、不改变几何；
- Tool ARGUMENTS 删除 rounded + full border + canvas background 的内层 card，改用
  divider、spacing 和 tokenized padding；Full Message 文档区删除 rounded/full
  border，改用上下 divider；截断消息提示删除浮动圆角 card，成为贴附在消息底部的
  warning divider strip；
- Reasoning 使用 neutral elevated surface，只保留紫色 rail/text；失败 Tool 与
  Diagnostic 使用 neutral Tool surface，只保留 danger rail、`ISSUE`/`failed`
  文案和 destructive action。颜色面积因此受限，typed event、identity、copy、
  disclosure、overlay 和 recovery command 路径均未改变；
- 新增 source contract 阻止 ARGUMENTS、Reasoning、truncated strip 和 Full Message
  回退为嵌套 card；真实 GPUI geometry/click 测试锁定 Inspector Tab `32 px`、
  Header Abort、三项 authorization decision 与三项 Diagnostic recovery `40 px`，
  并继续验证 typed authorization/recovery 与长 Tool/完整消息路径；
- wide、medium、narrow、wide-authorization、wide-reduced-motion、
  wide-keyboard-focus、wide-no-color 七张 fixture 均按原始分辨率人工 review。
  no-color 仍通过 selected background/outline、disabled/muted contrast、rail、
  `ISSUE`/`failed`、Reasoning/Tool 名称和明确动作词传达语义；安装后七张复验
  `RMSE=0`，未提高 `0.015` RMSE、fixture 尺寸或任何性能预算；
- Desktop 全量 `189 passed / 5 ignored`，独立 dependency boundary `16/16`，
  `cargo check -p desktop --all-targets`、fmt 与 `git diff --check` 通过；
  Clippy 仍仅 `DSK-000` 已记录的 `field_reassign_with_default` 和
  `large_enum_variant` 两项既有红灯，没有新增 lint；
- headless gate：10k CPU frame P95 `2.569 ms`、input roundtrip P95 `5.894 ms`、
  Change-to-render P95 `350 us`、10 MiB hydration `14.370 ms`、
  10k-block hydration `2.528 ms`、scroll/render P95 `167 us`、
  streaming event P95 `4–16 us`；
- native gate：GPU present P95 `4.875 ms`、input-to-post-render P95 `8.374 ms`、
  steady RSS growth `143,360 bytes`、production Markdown completion P95
  `115 us`，均低于原预算。交互式 `desktop-click-to-photon.sh` 仍需人工按键采样，
  未被计作自动通过项。
- 最终完成审计发现 click-to-photon Bash/PowerShell launcher 在零样本退出时仍会返回
  成功。两端现统一要求至少 `50` 个 `click_to_photon_post_render` 样本，否则
  fail closed；dependency boundary 同时锁定 launcher 的最小样本数，以及 external
  CSV 的 `run_id`/`sample_id`/`latency_us`、paired app log 和 P95 budget 契约；
- 后续验收链审计发现单独的 `sample_id` 每次启动都会从 `1` 重置，不能证明 CSV
  属于当前 replay。当前 fixture 会为每次运行生成唯一 `run_id`，input 与
  post-render trace 都携带该 identity；report 要求 CSV 只有同一个匹配 run，
  拒绝重复、混合 run、缺 input 或 bright-state 不一致的 app sample。report 启动时
  先删除同路径旧 artifact，只有当前 paired P95 通过预算才写新 artifact，避免失败
  后遗留一份看似可用的旧报告；`--output` 经路径解析后不得覆盖 CSV 或 app log，
  防止清理旧 artifact 时误删原始测量证据；显示 `--refresh-hz` 为必填正数，正式
  artifact 不得缺少刷新率这一实验条件。七项可执行 Python 合同测试覆盖
  passing、stale run、duplicate/unpaired、短/混合 CSV、超预算和 input-overwrite
  路径，并由 dependency boundary 调用。这些修复只保证采样流程不会假绿，不能
  替代尚未完成的物理 photon 测量。

## 七、测试与验收矩阵

| 不变量 | 主要测试/验证 |
| --- | --- |
| command id 精确完成和拒绝 | `command_ledger` tests、runtime command tests |
| ProductEvent 顺序和 ack | runtime reconnect/ack/terminal tests |
| gap/lag fresh snapshot | runtime recovery tests、cross-adapter fixture |
| terminal before completion | prompt terminal slot/replay tests |
| session-scoped UI state | NativeShell composer/conversation/inspector tests |
| streaming 局部重绘 | dirty-routing 和 pane isolation tests |
| 10k rows、32 MiB transcript、40 MiB render cache | conversation release/performance matrix |
| input/render latency | composer latency和click-to-photon gates |
| focus/accessibility | NativeShell visual/focus smoke tests |
| icon-only action 可发现性 | tooltip、accessible label、keyboard-focus fixture |
| Sessions 整行交互 | pointer/Enter、active、empty/filter tests |
| Composer 对齐和稳定尺寸 | empty/multiline/pending/running visual fixtures |
| Inspector Tab 不换行 | min/max panel width、narrow overlay fixture |
| critical action 保留语义 | authorization/recovery/abort smoke tests |
| 外部编辑器安全 | file-review validation tests |
| crate 依赖边界 | `desktop/tests/dependency_boundary.rs` |

每个任务至少执行：

```bash
cargo fmt --all -- --check
cargo test -p desktop
cargo test -p desktop --test dependency_boundary
cargo clippy -p desktop --all-targets -- -D warnings
git diff --check
```

涉及 GPUI Render、Pane 数据或布局时额外执行：

```bash
scripts/desktop-visual-golden.sh
scripts/desktop-click-to-photon.sh
```

涉及 conversation cache、layout、streaming 或 runtime channel 时额外执行：

```bash
scripts/desktop-native-perf-gate.sh
scripts/desktop-perf-gate.sh
```

`DSK-*` 结构任务不应更新 visual golden；截图变化应先判定为回归。`VUI-*` 任务预期
产生视觉变化，必须在对应视觉提交中执行：

```bash
scripts/desktop-visual-golden.sh --review
scripts/desktop-visual-golden.sh --update --review-note FILE
scripts/desktop-visual-golden.sh
```

review note 必须说明目标 surface、控件语义变化、wide/medium/narrow 结果和
accessibility 影响，不能只写“更新截图”。

## 八、提交和评审策略

每个提交只做一种变化：

1. 添加新类型和兼容 adapter；
2. 切换一个调用方；
3. 删除旧路径；
4. 移动测试；
5. 可选清理。

每个 Pane lane 的提交顺序固定为：

1. `refactor(desktop): ... view model/controller ...`
2. `test(desktop): ... interaction states ...`
3. `style(desktop): ... visual/control semantics ...`
4. `test(desktop): update reviewed native visual goldens`

允许多个 lane 并行开发，但禁止把第 1 步和第 3 步压成同一提交。

禁止在同一提交中同时：

- 移动模块并改变异步顺序；
- 移动 Pane 并调整视觉样式；
- 修改 projection reducer 并修改 dirty routing；
- 修改 conversation cache 并调整性能预算；
- 修改 runtime shutdown 并修改 command protocol。
- 更新共享 visual token 并同时重做多个 Pane；
- 更新 golden 却没有对应 VUI task 和 review note。

评审顺序：

1. ownership 和依赖方向；
2. command/event/recovery/shutdown 不变量；
3. bounded memory 和队列；
4. selective rendering；
5. 视觉和 accessibility；
6. 文件组织和命名。

VUI task 的评审顺序：

1. 控件是否符合导航、选择、列表、工具或 critical action 的真实语义；
2. pointer、keyboard、screen-reader 路径是否等价；
3. empty/loading/pending/disabled/selected/error 状态是否完整；
4. wide、medium、narrow 是否稳定；
5. 最后才评审颜色、圆角和局部润色。

## 九、停止条件

出现以下任一情况，当前任务停止扩展范围，先恢复到上一可验证状态：

- ProductEvent sequence、ack 或 terminal ordering 改变；
- projection 需要在 runtime 和 UI 两处同时维护；
- PaneViewModel 开始暴露完整 `DesktopProjection` 或 `NativeShell`；
- 为通过测试而提高队列、缓存、文本或时间预算；
- streaming-only 更新重新触发 Root 全量渲染；
- session 切换后出现 draft、scroll、selection 或 inspector state 串会话；
- shutdown 依赖 drop 而非显式 drain/join；
- 需要更新 golden 才能解释纯结构变更。
- icon-only action 缺少 tooltip、accessible label 或键盘路径；
- Sessions/changed-file action row 只能通过 pointer 激活；
- Inspector Tab 在允许的 panel 宽度内换行；
- Composer mode/notice 切换导致输入宽度或 submit 位置跳动；
- authorization、recovery 或 Abort 被改成仅图标；
- VUI task 意外改变 command intent、typed identity 或 dirty routing；
- golden diff 出现在任务范围之外的 Pane，且无法由共享 token 变更解释。

## 十、下一批建议

结构 lane、视觉基线、共享控件 foundation、Header、Sessions、Composer、
Inspector、Overlay、Conversation，以及最终全局视觉节奏任务 `VUI-201`
的代码和自动 gate 已完成。当前唯一强制验收项是：

1. 在当前二进制上运行 `scripts/desktop-click-to-photon.sh`，用外部传感器记录至少
   50 个与 app log run ID 和 sample ID 对应的物理 latency；
2. 将数据保存为含 `run_id,sample_id,latency_us` 的 CSV；同一文件只能使用 launcher
   输出的一个当前 run ID，然后运行：

   ```bash
   scripts/desktop-click-to-photon-report.py SAMPLES.csv \
     --platform linux \
     --app-log target/desktop-perf/click-to-photon-app-latest.log \
     --refresh-hz 60
   ```

3. 只有 paired report 成功且 P95 不超过既有 `50,000 us` 预算，才把 `VUI-104`
   和主计划标记为完全完成。不得用 GPUI post-render、屏幕截图或合成按键时间替代
   物理 photon 数据。

`DSK-401` 已按计划评估：`allocation_probe`/`resident_memory`/`shell.rs`/内嵌测试
的进一步文件移动只改善导航，不改变 ownership、依赖方向、行为或性能。在当前
owner 和 boundary 已清晰、全量 gate 稳定的情况下不值得制造额外 churn，因此暂不
执行；未来只有在这些文件出现可量化导航或多人冲突成本时，才单独立项。

## 十一、当前推进状态（2026-07-28）

结构 lane（`DSK-000`、`DSK-101` 至 `DSK-105`、`DSK-201`、`DSK-202`、
`DSK-301`）与视觉 lane（`VUI-000`、`VUI-101` 至 `VUI-106`、`VUI-201`）
的代码实现均已完成。typed interaction、responsive geometry、accessibility、
reviewed golden、内存与自动性能预算已经闭环。

主计划尚不能标记完成：`VUI-104` 的 click-to-photon 完成条件缺少当前二进制的
50 组外部物理配对样本。`DSK-000` 两项既有 Clippy 红灯继续作为已记录基线；
它们与 click-to-photon 待验收项都没有被误报为通过。

### 最终自动审计快照（2026-07-28）

本节记录当前完整工作树的最终审计结果；前述各任务完成记录保留任务当时的测量值，
不以本节数据回填或覆盖：

- `cargo fmt --all -- --check`、`cargo check -p desktop --all-targets`、
  `git diff --check`、Bash launcher 语法和 click-to-photon report Python 语法通过；
- Desktop 全量 `189 passed / 5 ignored`，dependency boundary `16/16`；七张已评审
  golden 复验均为 `RMSE=0`；click-to-photon report 可执行合同测试 `7/7`；
- 最终 headless gate 的 10k-row CPU frame P95 为 `2.824 ms`、input roundtrip
  P95 为 `5.847 ms`、input Change-to-render P95 为 `346 us`；
- 最终 native gate 的 GPU present P95 为 `5.594 ms`、input-to-post-render P95
  为 `8.366 ms`、steady RSS growth 为 `49,152 bytes`、production Markdown
  completion P95 为 `124 us`，均在既有预算内；
- 严格 Clippy 精确只有 `DSK-000` 已记录的 `field_reassign_with_default` 和
  `large_enum_variant` 两项，没有新增告警；
- 当前 `target/desktop-perf/click-to-photon-app-latest.log` 仍为零样本。因此以上
  自动审计不改变主计划与 `VUI-104` 的“待人工验收”状态，也不构成物理
  click-to-photon 结果。旧的无 `run_id` 报告已保留为
  `click-to-photon-linux.unpaired-legacy.log`，标准
  `click-to-photon-linux.log` 路径保持不存在，直到当前 run 的配对报告真正通过。
