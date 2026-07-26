# Desktop 交互、视觉与渲染性能优化任务计划

> 状态：执行中
>
> 创建日期：2026-07-27
>
> 范围：`crates/desktop` 原生 GPUI 客户端
> 目标：消除焦点与流式输出造成的视觉跳变，使长会话下的交互、渲染和滚动保持稳定，并将当前工程调试型界面演进为以对话和代码审查为核心的桌面产品界面。

---

## 1. 背景与结论

当前 desktop 已具备会话、流式对话、工具授权、文件审查、模型/配置选择、恢复操作和窄窗口 overlay 等主要能力，但交互和呈现仍偏向工程诊断壳。当前最影响体验的三个问题是：

1. Conversation 面板仅在聚焦时动态添加真实边框，边框参与 flex 布局，造成内容区缩小、文本重新换行和滚动跳变。
2. Runtime 已按 16ms 合并流事件，但 `gpui-component 0.5.1` 的 Markdown 更新还需等待 200ms 静默；连续输出时内容会短暂停住，再成块出现。
3. 每次流式刷新都会重建全量 overlay 投影和全部虚拟列表高度，并重新克隆、清洗和提交可见 Markdown；会话越长，成本越高。

本计划按以下顺序推进：

```text
稳定布局
  → 平滑流式文本
  → 正确滚动锚定
  → 增量投影和渲染缓存
  → 信息架构与视觉重构
  → 性能、可访问性和回归门禁
```

## 2. 成功标准

### 2.1 用户体验标准

- 在 Sessions、Conversation、Composer、Inspector 和 Status 之间切换焦点时，主区域 bounds 不发生变化。
- 连续流式输出期间，正文持续可见更新，不出现约 200ms 冻结后整块跳出的现象。
- 用户停留在底部时稳定跟随新内容；用户向上阅读历史时，新内容不改变当前阅读位置。
- 主要界面使用系统 sans-serif；仅代码、命令、路径、ID 和 telemetry 使用 monospace。
- Assistant、User、Reasoning、Tool 和 Diagnostic 有清晰且克制的视觉层级，不再依赖大量彩色完整边框。
- Sessions 面向任务导航，Inspector 面向变更审查；底层 runtime telemetry 不再与主任务争夺注意力。

### 2.2 性能标准

| 场景 | 目标 |
|---|---|
| 10,000 条历史消息、50 streaming deltas/s | frame p95 ≤ 16.7ms，p99 ≤ 33ms |
| 流事件到首次可见文本 | p95 ≤ 50ms |
| 连续流式文本更新间隔 | ≤ 50ms |
| Streaming 期间 Composer 输入延迟 | p95 ≤ 50ms |
| Terminal event 后最终 Markdown | 150ms 内完成 |
| Following 模式底部锚定误差 | ≤ 2px |
| Paused 模式阅读锚定误差 | ≤ 2px |
| Focus 切换几何位移 | 0px |
| 产品事件完整性 | sequence 无丢失；允许展示合并，不允许事件丢失 |

## 3. 约束与设计原则

- Runtime 事件交付必须保持无损；只能合并 UI 展示，不能丢弃产品事件。
- Authorization、failure、terminal、control 和 recovery 事件继续优先交付。
- 不通过扩大 runtime debounce 来掩盖 UI 性能问题。
- Focus、selection、warning、reasoning 和 error 使用不同的视觉语义。
- 任何 focus、hover、streaming 状态变化不得增减 border、padding、font weight 或其他会改变几何的属性。
- 使用稳定业务 ID 作为 GPUI keyed state，禁止使用列表 index 标识消息 Markdown 状态。
- 所有缓存必须有明确 revision 和内存上限，不能引入无界后台任务或解析队列。
- 响应式布局、键盘操作、IME、CJK/Emoji 和 reduced motion 是正式验收范围。

## 4. 目标信息架构

```text
NativeShell
├── TitleBar
│   ├── Sessions toggle
│   ├── Session title / cwd
│   ├── Operation status
│   └── Inspector toggle / More
├── Workspace
│   ├── Sessions
│   │   ├── Search
│   │   ├── Session rows
│   │   └── New session
│   ├── Conversation
│   │   ├── Transcript
│   │   ├── New-output indicator
│   │   └── Composer
│   └── Inspector
│       ├── Changes
│       ├── Task
│       ├── Usage
│       └── Runtime
├── StatusBar
└── OverlayHost
    ├── Authorization
    ├── Command palette
    └── Narrow-layout drawers
```

默认情况下 Conversation 是主工作区。Sessions 用于导航；Inspector 默认展示 Changes 或 Task，stream、sequence、generation 等指标收进 Runtime 折叠区。

## 5. 分阶段任务

### Phase 0：基线、可观测性和布局稳定性

#### DESK-001 建立优化基线

- [ ] 为 runtime batch 大小、等待时长、projection apply、view render、Markdown parse 和 list layout 建立 tracing/计时点。
- [ ] 准备可重复 replay fixture：1、100、1,000、10,000 条历史消息。
- [ ] 准备 10、50、200 events/s 的流式回放模式。
- [ ] 记录改造前 frame time、输入延迟、分配量和滚动行为。

验收：同一 fixture 可在本地重复执行并产生可比较数据。

#### DESK-002 消除 Conversation 焦点几何跳变

- [x] 确认根因：focused 分支动态调用 `border_1()`。
- [ ] 删除动态几何样式。
- [ ] 使用已有 header divider 的颜色变化表达键盘焦点，焦点样式不参与布局。
- [ ] 鼠标操作和键盘 focus-visible 语义分离；当前阶段先保证几何稳定，输入模态检测在 DESK-016 完成。
- [ ] 增加源码不变量/纯逻辑回归测试。
- [ ] 后续增加 GPUI bounds 组件测试。

验收：Conversation 聚焦前后 bounds、viewport 宽高和换行完全一致；不再出现完整紫色外框。

#### DESK-003 统一布局尺寸来源

- [ ] 消除 `ShellLayout::COMPOSER_HEIGHT` 与实际 auto-grow `88..236px` 的双重事实来源。
- [ ] 将 sidebar、composer、status、content max width 等尺寸收进统一 design/layout tokens。
- [ ] 窗口 resize 时只在宽度 bucket 改变后使消息高度缓存失效。

验收：纯布局模型与 GPUI 实际布局使用相同 token；窄窗口临界点无反复闪烁。

### Phase 1：流式渲染与滚动

#### DESK-004 引入 StreamingText 双阶段渲染

- [x] 最小路径：未完成内容绕过带 200ms 防抖的 Markdown，以轻量可换行文本呈现；完成态再进入最终 Markdown。
- [ ] 新增轻量 `StreamingText` 组件。
- [ ] streaming 阶段按 16–33ms 合并 append-only fragment，以 plain/rich-inline text 呈现。
- [ ] 80–120ms 无新内容时允许后台生成阶段性 Markdown。
- [ ] terminal 后执行一次最终 Markdown parse，并冻结结果。
- [ ] 过期 revision 的后台结果不得覆盖新内容。
- [ ] Reasoning 和 tool output 使用同一 revision 协议。

验收：连续输出期间可见更新间隔不超过 50ms；不存在依赖 200ms 静默的冻结。

#### DESK-005 使用稳定 ConversationItemKey

- [x] Markdown keyed state 从列表 index 改为现有稳定 block/message/tool ID。
- [ ] 为 durable、submitted、message、tool 和 diagnostic 定义稳定 typed key。
- [ ] `TextView::markdown`、row element 和 selection 使用稳定 key，不使用 index。
- [ ] session 切换和 transcript 头部淘汰后不复用错误的 TextView state。

验收：切换 session、前部 eviction 和 live→durable reconciliation 后内容与 selection 身份正确。

#### DESK-006 重写 follow-latest 状态机

- [x] 从 `ScrollHandle` 读取 offset 和 `max_offset`（由 content size 与 viewport size 得出）。
- [x] 使用迟滞阈值：Following 超过 48px 后暂停，Paused 回到 32px 内恢复。
- [x] 用户向上滚动超过阈值时进入 Paused。
- [x] Paused 时复用 `VirtualListScrollHandle`，保持当前像素 offset，不因追加尾部内容主动改写锚点。
- [x] 新 block 和同一 streaming row 的新 sequence 均累计 unseen count，显示 `↓ N new` 浮动 pill。
- [x] 用户滚回底部、点击 pill 或触发 End action 时恢复 Following 并清零 unseen count。
- [x] 状态由事件处理后的实际纵向 offset 决定；横向、零 delta 或底部轻微滚轮不会误暂停。

验收：纯状态机、负向 GPUI offset 和 streaming revision 已覆盖自动化测试；Following 与 Paused 的 ≤2px GUI 锚定误差并入 DESK-017 截图/交互回归。

#### DESK-007 限制流式高度抖动

- [x] 保留 16ms streaming 文本批次；行高通过独立状态以 67ms 间隔提交（约 15Hz），完成态立即 settle，并用定时补刷覆盖流中停顿。
- [x] Following 模式在每次布局解析后请求最后一项的 Bottom 锚定，包括仅由行高定时器触发的帧。
- [x] Paused 模式保存稳定 row key + intra-row pixel offset；锚点上方增长、插入和淘汰时对 `ScrollHandle` 做 offset compensation。
- [x] 使用 `unicode-width` display width 替换 UTF-8 `str.len()` 字节估算，并以 24px width bucket 抑制 resize 抖动。

验收：15Hz 提交、settle、width bucket、中文、Emoji、组合字符以及 grow/insert/evict 锚点补偿均有自动化测试；真实字体与代码块截图检查并入 DESK-017。

### Phase 2：增量投影、缓存和渲染隔离

#### DESK-008 Projection 返回 typed delta

- [x] 定义 `DesktopProjectionDelta` 和位集合 `ContextDirtyFlags`，映射共享 reducer 的全部 `CodingAgentClientProjectionArea`。
- [x] message/thinking/tool/usage/change/terminal 事件只返回对应 conversation/tool/context/terminal dirty 信息；NativeShell 只在 conversation/tool delta 时更新滚动状态，只在 change delta 时校验文件审查。
- [x] 删除 product event 后全量收集 messages/tools/diagnostics/recoveries 的路径，改为按 event sequence/业务 ID 更新单个 bounded overlay。
- [x] resync、session、metadata 和 recovery snapshot replace 保留显式 `full_replace` 与全量兼容视图重建路径。

验收：跨 adapter fixture 继续逐事件一致；测试计数器证明 product event 的 message/tool/diagnostic/recovery 增量更新不会增加 full view rebuild 次数，snapshot replace 才增加。

#### DESK-009 ConversationRowRenderCache

- [x] 每条 row 保存 source revision、sanitized revision、`Arc<str>`、稳定 Markdown state key、width bucket 和 measured height。
- [x] `bounded_markdown_preview` 对每个 text/detail 字段、每个 revision 最多执行一次；width bucket 改变只重新估高。
- [x] 完成消息的 sanitized text、GPUI keyed parse state 和高度保持冻结，revision 改变才失效。
- [x] 缓存同时受 transcript 数量（10,256 entries）和保留字节（40 MiB）上限约束，并在 session/row 消失后回收 stale entry。

验收：普通 render 只 clone `Arc`，不 clone 大字符串，不重新安全清洗未改变的消息；缓存命中、revision 冻结、width-only remeasure、streaming 更新和数量/字节淘汰均有自动化测试。

#### DESK-010 持久化虚拟列表尺寸

- [x] 将 `Rc<Vec<Size<Pixels>>>` 从 render 局部变量移入持久 list model。
- [x] product event sequence 贯穿到 row invalidation；append 只插入目标项，streaming 只更新业务 ID 对应的 row/height/size index，结构不一致时才回退到 bounded live-tail reconcile。
- [x] 宽度 bucket 改变时批量 invalidate；resize 使用 67ms trailing debounce。
- [x] 不在每帧为 10,000 条消息重新构造、比较和展开尺寸数组；普通 render 复用持久 row/height/size。

验收：固定可视区域下，一条 live row 更新不遍历全部历史消息；10,000 条 durable layout 的计数测试证明 `resolve_one` 只访问目标行，non-Clone row fixture 证明 indexed update 不复制历史前缀。

#### DESK-011 拆分 NativeShell 渲染实体

- [x] 拆出 TitleBar、Sessions、Conversation、Composer、Inspector、StatusBar、OverlayHost。
- [x] Conversation transcript 已迁入独立 `ConversationPane` Entity；持久 row/size 通过 `WeakEntity<NativeShell>` 零拷贝读取，selection 通过 typed child event 回传。
- [x] Sessions 已迁入独立 `SessionsPane` Entity；新建、刷新、打开会话通过 typed child event 回传，streaming token 不 notify 该区域。
- [x] Composer 已迁入独立 `ComposerPane` Entity；输入变化仅 notify Composer，Submit/Steer/Follow-up 通过 typed child event 进入原 command ledger 路径。
- [x] Inspector/Context 已迁入独立 `InspectorPane` Entity；file review、external editor、recovery 仅通过 typed child event 回到父级安全命令路径，conversation/tools-only delta 不触发 Inspector render。
- [x] StatusBar 已迁入独立 `StatusBar` Entity；model/profile/thinking 选择通过 typed child event 回传，conversation/tools/cursor-only delta 不触发 StatusBar render。
- [x] Conversation Header/TitleBar 已迁入独立 `ConversationHeader` Entity；Sessions/Context/Reload/Copy/Abort 通过 typed child event 回传，固定 divider focus indicator 不改变布局尺寸。
- [x] Command Palette、窄屏 Sessions/Context、Authorization 已统一迁入独立 `OverlayHost` Entity；focus trap 与 authorization command ledger 仍由 `NativeShell` 持有。
- [x] token 更新只 notify Conversation/live row；正文、空状态、滚动区和 follow-latest 均由 `ConversationPane` 持有，streaming-only delta 不再 notify `NativeShell` root。
- [x] usage、telemetry 等低优先级信息使用 250ms 合并窗口，最多 4Hz 更新；交互相关的 operations、changes、diagnostics、recovery 仍立即刷新。
- [x] 保持现有 typed command ledger、authorization 和 recovery 安全边界；子 Entity 只发 typed event，命令登记、身份校验和 focus trap 继续由 `NativeShell` 持有。

验收：streaming token 不触发 Sessions、Inspector、File Review、Palette 的重新 render。

### Phase 3：交互和视觉重构

#### DESK-012 字体、颜色和层级

- [ ] 根 UI 改用系统 sans-serif。
- [ ] code、command、path、ID 和 telemetry 局部使用 monospace。
- [ ] 统一 canvas/surface/elevated 三层中性色。
- [ ] 蓝色用于 action/focus，低饱和紫色仅用于 Reasoning，黄色仅用于 running/warning，红色仅用于 failure/destructive。
- [ ] 减少嵌套完整边框，以背景、间距和 divider 表达层级。

验收：正文阅读层级明确；focus、selection、reasoning、warning 和 error 不再共用同一视觉语义。

#### DESK-013 重构消息组件

- [ ] Assistant 默认无完整彩色外框，内容最大宽度 880–960px。
- [ ] User 右对齐，最大宽度 68%–72%。
- [ ] Reasoning 默认折叠，显示 streaming/duration 摘要。
- [ ] Tool 默认展示 `name · status · duration`，command/output/arguments 按需展开。
- [ ] Diagnostic/Error 保留高辨识度边框。
- [ ] hover/focus 时显示 Copy、More、Copy code 等操作且不改变行高。

验收：普通对话、Reasoning、工具和错误的主次关系一眼可辨。

#### DESK-014 重构 Composer 操作模型

- [ ] Idle 使用单一 Send 主按钮。
- [ ] Running 使用 `Steer now` / `Queue next` 模式选择和单一主按钮。
- [ ] 每个 session 保存模式和草稿。
- [ ] 明确 pending、authorization 和 rejected 状态文案。
- [ ] 正确处理 IME composition。

验收：用户无需理解内部 command 类型即可预测发送结果。

#### DESK-015 Sessions 与 Inspector 重构

- [ ] Sessions 展示语义名称、相对时间和运行状态，不重复显示 active ID 卡。
- [ ] 自动增量刷新，增加搜索和最近排序。
- [ ] Inspector 提供 Changes、Task、Usage、Runtime；默认 Changes/Task。
- [ ] 零 diagnostics/recoveries 不占永久空间。
- [ ] sidebar 支持持久化宽度、拖动调整、双击重置和窄窗口 drawer。

验收：主界面默认只展示完成当前编码任务所需的信息。

#### DESK-016 键盘、焦点和可访问性

- [ ] `Ctrl/Cmd+Tab` 切换区域，plain Tab 交给 Composer/标准控件导航。
- [ ] Conversation 支持上下选择消息和键盘消息操作。
- [ ] Overlay 关闭后恢复原 focus owner。
- [ ] 鼠标 focus 与 keyboard focus-visible 分离。
- [ ] 检查对比度、hit target、reduced motion 和 screen-reader label。

验收：仅键盘可完成新建 session、发送、切换区域、复制消息、审查文件和授权决策。

### Phase 4：验证与发布门禁

#### DESK-017 性能回放与基准

- [ ] 覆盖 1/100/1,000/10,000 条消息和 10/50/200 events/s。
- [ ] 覆盖 256KB Markdown、512KB Reasoning、大 Bash output、表格、代码块、CJK 和 Emoji。
- [ ] 输出 frame、input、parse、layout、allocation 和 memory 曲线。
- [ ] 将关键退化阈值加入 CI 或可重复的本地 benchmark。

#### DESK-018 组件、截图和端到端回归

- [ ] Focus bounds 组件测试。
- [ ] Stable key/session switch 测试。
- [ ] Following/Paused scroll anchor 测试。
- [ ] Streaming→Final Markdown revision 测试。
- [ ] 宽、中、窄窗口截图 golden tests。
- [ ] Authorization、recovery、file review 和 command palette 端到端 smoke test。

## 6. 建议 PR 顺序

| PR | 包含任务 | 可独立获得的收益 |
|---|---|---|
| PR 1 | DESK-002、DESK-005 的 key 基础、DESK-004 最小实现 | 消除紫框尺寸跳变和 200ms 流式冻结 |
| PR 2 | DESK-006、DESK-007 | 自动跟随和历史阅读稳定 |
| PR 3 | DESK-008、DESK-009、DESK-010 | 长会话性能由 O(N)/frame 转为 dirty-row 增量 |
| PR 4 | DESK-011 | 隔离重渲染范围，降低维护复杂度 |
| PR 5 | DESK-012、DESK-013、DESK-014 | 对话、工具、Reasoning 和 Composer 产品化 |
| PR 6 | DESK-015、DESK-016 | Sessions、Inspector、响应式和可访问性完善 |
| PR 7 | DESK-001、DESK-017、DESK-018 收口 | 性能数据、截图和发布门禁 |

## 7. 测试与观测矩阵

### 7.1 需要记录的 span/counter

```text
desktop.runtime.receive
desktop.runtime.batch_wait
desktop.runtime.batch_size
desktop.projection.apply
desktop.projection.dirty_rows
desktop.preview.sanitize
desktop.markdown.parse
desktop.list.height_update
desktop.list.layout
desktop.render
desktop.paint
desktop.input.latency
```

### 7.2 必测交互

- streaming 时输入、选择文字、复制、滚动、调整窗口、切换 sidebar。
- 用户停在底部、距离底部一个阈值内、向上阅读和手动回到底部。
- live submitted/message/tool 转换为 durable transcript。
- session 切换、session 删除/淘汰、resync 和 reconnect。
- authorization overlay 打开、决策 pending、失败、关闭和焦点恢复。
- Composer auto-grow、中文 IME、长行、粘贴大文本和 rejected submission。
- reduced motion、键盘 only、窄窗口 drawer。

## 8. 风险与回退策略

| 风险 | 控制措施 |
|---|---|
| Streaming plain text 与最终 Markdown 视觉差异过大 | 保留稳定前缀样式；最终 parse 原子替换；对代码 fence 增加最小状态机 |
| 增量 projection 漏更新 | 保留 snapshot/resync 全量路径；对 delta 与全量投影做测试期双算比对 |
| 高度缓存失真 | width bucket + revision；超出误差时使用真实测量回写 |
| Scroll offset compensation 符号/边界错误 | 纯状态机测试覆盖 append、grow、evict、resize、session switch |
| 拆分 Entity 破坏 focus/overlay | 先抽纯 view model，再逐区域迁移；保留 root typed actions |
| 依赖库 Markdown 行为限制 | live path 不依赖其 200ms debounce；最终 Markdown 可继续复用现有组件 |

每个阶段必须保持可回退：runtime 协议和产品事件不变，新的 view model、stream renderer 和视觉组件均在 desktop adapter 内演进。

## 9. 当前推进状态

| 任务 | 状态 | 备注 |
|---|---|---|
| DESK-001 | 待开始 | 先完成最小性能 fixture，再扩展完整基准 |
| DESK-002 | 完成 | 已移除动态面板边框，改用已有 header divider 和标题颜色；回归测试已通过 |
| DESK-003 | 待开始 | 依赖布局 token 整理 |
| DESK-004 | 进行中 | 最小双阶段路径已完成；后续抽取 StreamingText、revision 和后台 settling parse |
| DESK-005 | 进行中 | Markdown 已使用稳定业务 ID；typed key 和 row element 迁移待完成 |
| DESK-006 | 完成 | 基于真实 offset 的迟滞状态机、streaming unseen 计数和无布局占位的浮动 pill 已完成；GUI 像素门禁归入 DESK-017 |
| DESK-007 | 完成 | 文本保持 16ms 批次，行高约 15Hz；Following 底部锚定、Paused offset compensation 和 Unicode display width 已完成 |
| DESK-008 | 完成 | typed delta 已贯穿 Projection→NativeShell；product event 只增量同步 dirty overlay，replace 路径保留全量重建 |
| DESK-009 | 完成 | revision-aware bounded row cache 已接入 NativeShell；sanitized `Arc<str>`、稳定 Markdown state key 和 measured height 可复用 |
| DESK-010 | 完成 | 持久 row/height/size、sequence→单 index 更新、bounded 结构回退、15Hz 单行补刷和 67ms resize debounce 已完成 |
| DESK-011 | 完成 | 所有计划区域 Entity 已拆分；token 流隔离到 Conversation 子树，usage-only telemetry 限制为 4Hz，typed command/authorization/recovery 边界测试通过 |
| DESK-012～018 | 待开始 | DESK-011 完成后进入字体、颜色、消息组件和 Composer 视觉/交互重构 |

## 10. 完成定义

本计划仅在以下条件全部满足后视为完成：

- 所有 DESK 任务完成或有明确批准的范围调整。
- 成功标准中的交互与性能指标有可重复证据。
- 宽、中、窄三种布局通过截图和键盘回归。
- 10,000 条消息压力场景下无持续 frame degradation。
- 产品事件、authorization、recovery、file review 和 session 生命周期无功能回退。
- 架构文档、用户可见快捷键和开发者性能说明同步更新。
