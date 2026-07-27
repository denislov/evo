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

- [x] 已为 runtime batch/wait、projection apply、preview sanitize、row height/layout、row prepare、view render、input handler 和最新 change→ComposerPane render 建立 opt-in tracing；release gate 另覆盖完整 GPUI headless CPU frame/input roundtrip，并直接调用 `TextViewState::markdown` 门禁真实解析器 P95；opt-in native gate 覆盖真实 draw+GPU/present P95/P99，以及 50 个真实 InputState 模拟按键从 dispatch 到首个 post-render callback 的保守上界。
- [x] production settling/final 消息行已用透明 Element 包装真实 `TextView::markdown` request-layout；`EVO_DESKTOP_MARKDOWN_TRACE=1` 为每次实际挂载/解析输出 session-scoped state key、phase、bytes 与 parse→layout completion，native gate 强制采样并门禁 P95 ≤ 150ms，默认路径无 trace-state/timer/log 开销。
- [ ] 包含物理按键、OS 输入队列和显示扫描的 click-to-photon latency 仍需外部光学/光电观测合格样本；production 黑白翻转 replay、应用配对日志和 fail-closed CSV validator 已完成。
- [x] release replay fixture 可重复生成 1、100、1,000、10,000 条历史消息并输出 hydration/prepare 数据。
- [x] release fixture 覆盖 10、50、200 次 streaming revision 回放。
- [x] 从初始提交 `a39615f` 的同一 10 MiB/10k fixture 记录五次 release 中位数：hydration 1.739ms、30,015 allocations/3,939,583B、滚动准备 P95 181µs、Composer edit P95 1µs；可复现 patch 与原始样本记录在性能基线文档。
- [x] 历史能力审计已完成：初始提交没有 NativeShell frame replay、RSS probe 或锁文件；基线文档明确记录不可重建边界，不通过移植当前 render harness 伪造“改造前”full-tree/GPU frame 与 RSS 数据。

验收：`scripts/desktop-perf-gate.sh` 串行执行 headless fixture、真实 GPUI Markdown parser 矩阵并写入 `target/desktop-perf/latest.log`；`scripts/desktop-native-perf-gate.sh` 在交互式 display 上执行相同 10k fixture、断言真实 GPUI draw/present P95/P99、production row Markdown completion、原生窗口 RSS，并对 50 个输入配对样本断言 dispatch→post-render P95 ≤ 50ms，结果写入 `target/desktop-perf/native-latest.log`；该内部上界覆盖应用渲染与 present submit，物理 click-to-photon 流程见 `docs/desktop-external-performance.md`。

#### DESK-002 消除 Conversation 焦点几何跳变

- [x] 确认根因：focused 分支动态调用 `border_1()`。
- [x] 删除动态几何样式。
- [x] 使用已有 header divider 的颜色变化表达键盘焦点，焦点样式不参与布局。
- [x] 鼠标操作和键盘 focus-visible 语义分离。
- [x] 增加源码不变量/纯逻辑回归测试。
- [x] 增加真实 NativeShell 的 GPUI bounds 组件测试，覆盖聚焦前后和宽/中/窄三档窗口。

验收：Conversation 聚焦前后 bounds、viewport 宽高和换行完全一致；不再出现完整紫色外框。

#### DESK-003 统一布局尺寸来源

- [x] 删除虚假的固定 `COMPOSER_HEIGHT`；`ShellLayout` 只描述稳定 workspace，Composer 的 `88..236px` auto-grow 由 GPUI 和共享 min/max token 管理。
- [x] 将 sidebar、composer、status、user/assistant content max width 等尺寸收进统一 design/layout tokens。
- [x] 窗口 resize 时只在宽度 bucket 改变后使消息高度缓存失效。

验收：纯布局模型与 GPUI 实际布局使用相同 token；760px sessions 和 1080px context 临界点的前后值同时通过纯模型与真实 GPUI bounds 回归，无双重 composer 高度来源。

### Phase 1：流式渲染与滚动

#### DESK-004 引入 StreamingText 双阶段渲染

- [x] 最小路径：未完成内容绕过带 200ms 防抖的 Markdown，以轻量可换行文本呈现；完成态再进入最终 Markdown。
- [x] 新增轻量 `StreamingText` 组件，以 typed revision phase 选择 plain、settling Markdown 或 final Markdown。
- [x] streaming delivery 在 runtime 按 16ms 有界窗口合并，active revision 由 `StreamingText` 的 plain wrapping path 呈现。
- [x] 100ms 无新内容时自动进入 revision-bound Settling Markdown；持续流式阶段保持轻量纯文本，quiet/final 阶段才把 bounded 内容交给稳定 keyed `TextView::markdown`，避免每个 delta 都触发 Markdown 路径。
- [x] terminal 后正文/detail 使用 revision-bound、session-scoped keyed Markdown identity；final revision 只安全清洗一次并冻结缓存，解析生命周期交给 `TextView::markdown` 的组件状态管理，release parser matrix 直接门禁其同步解析成本。
- [x] row cache 单调接受 revision，过期 revision/result 不得覆盖当前内容、phase 或 final state。
- [x] 正文、Reasoning 和 Tool output/detail 均使用同一 `StreamingTextPhase` revision 协议。

验收：连续输出由 16ms runtime coalescing 驱动且不依赖 200ms Markdown debounce；100ms quiet transition、stale revision rejection 和 Streaming→Settling→Final 均有确定时钟测试；源码契约断言 streaming 使用 plain wrapping、settling/final 使用稳定 keyed `TextView::markdown`，代码块 Copy 组件测试与 production screenshot golden 证明 Markdown/代码操作外观不变。上游组件没有公开 parser completion callback，因此生产逐行 completion tracing 不作为已完成能力；真实解析成本由独立 release matrix 覆盖。

#### DESK-005 使用稳定 ConversationItemKey

- [x] Markdown keyed state 从列表 index 改为现有稳定 block/message/tool ID。
- [x] `ConversationItemKey` 以 session + typed item kind + row ID 区分 durable（含 diagnostic）、submitted、live message 和 live tool。
- [x] `TextView::markdown`、row element、hover group、layout cache 和 selection 均从 typed key 派生稳定身份，不使用列表 index。
- [x] cache key 纳入 session ID；session 切换和 transcript 头部淘汰后不复用错误的 TextView state。

验收：切换 session、前部 eviction 和 live→durable reconciliation 后内容与 selection 身份正确；同 row ID 的跨 session、durable/live 身份隔离均有自动化断言。

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

- [x] 根 UI 使用 GPUI 跨平台 `.SystemUIFont`，启动失败面也保持相同字体继承。
- [x] code、command、path、ID 和 telemetry 局部使用 `monospace`；Sessions ID、Inspector 数据、Status 数据和 authorization details 均已显式限定。
- [x] 统一 canvas/surface/elevated 三层中性色，并为 user、assistant、reasoning、tool、diagnostic、summary 提供低饱和语义 surface。
- [x] 蓝色用于 action/focus，低饱和紫色仅用于 Reasoning，黄色仅用于 running/warning，红色仅用于 failure/destructive；语义角色与 WCAG 文本对比均有测试。
- [x] 减少嵌套完整边框：普通消息卡和 Palette row 使用固定宽度左侧标记，区域层级主要使用 surface、间距和 divider。

验收：正文阅读层级明确；focus、selection、reasoning、warning 和 error 不再共用同一视觉语义。

#### DESK-013 重构消息组件

- [x] Assistant 默认无完整彩色外框，使用中性 surface 与左侧状态标记，内容最大宽度 960px。
- [x] User 右对齐，宽度上限为可用区域的 70%，同时受 920px 上限约束。
- [x] Reasoning 默认折叠并区分 streaming/completed，折叠高度参与虚拟列表布局；AI 层的 `ThinkingStart/ThinkingEnd(content_index)` 已投影为持久化 `message.reasoning.started/completed`，按消息内独立分段累加时长，正常 Message 完成会稳定补齐遗漏的结束事件，绝不以整条 Assistant 时长冒充 Reasoning duration。
- [x] Tool 默认展示 `name · duration`，status 保留为独立状态标签，output/arguments 按需展开；duration 由持久化 `tool.call.started` 与 completed/failed/cancelled 终止事件的 RFC3339 `created_at` 计算并投影为 `duration_millis`，运行中、时间缺失/无效/倒置时不展示。
- [x] Diagnostic/Error 保留红色高辨识度左侧边框，普通消息不复用 failure 语义。
- [x] hover/focus 使用绝对定位稳定槽显示 Copy、More 且不改变行高；最终 Markdown code block 通过组件公开 action 接口提供 32px `Copy code` 并复制精确代码内容。

Tool duration 兼容契约：历史 transcript 与运行中 Tool 使用 `None`；public facade 使用可选 `duration_millis`，RPC 以 additive `durationMillis` 输出；product transcript sanitizer 必须原样保留权威值。desktop 使用稳定单位（`ms`、0.1 秒、分秒）格式化，单位跨界采用舍入后的下一档，避免 `60.0 s` 到 `1m 00s` 的瞬时跳变。

Reasoning duration 兼容契约：历史 transcript、没有独立 Thinking 生命周期以及仍在 streaming 的消息使用 `None`；实时投影使用单调时钟统计，持久化回放使用 reasoning 事件 envelope 的 RFC3339 `created_at`，多个 `content_index` 分段只累加各自开放区间，不包含正文或工具调用间隙。public facade 使用可选 `reasoning_duration_millis`，product event 使用 additive `reasoning_duration_millis`，RPC 使用 additive `reasoningDurationMillis`；desktop 仅在 completed 后复用 Tool 的稳定单位格式，避免流式计时导致额外重排。

验收：普通对话、Reasoning、工具和错误的主次关系一眼可辨；Reasoning 分段、Tool 成功/失败/取消的时长口径一致，缺少权威证据时不猜测。

#### DESK-014 重构 Composer 操作模型

- [x] Idle 使用单一 Send 主按钮。
- [x] Running 使用 `Steer now` / `Queue next` 模式选择和单一主按钮；键盘主提交也服从当前 session 的模式。
- [x] 每个 session 保存模式和草稿，切换 session 时精确保存并恢复，缓存数量保持有界。
- [x] 明确 waiting-for-start、submitting、authorization-required 和 rejected 状态文案，authorization 状态变化会定向 notify Composer。
- [x] 继续由 GPUI InputState 管理 IME marked text；desktop 仅消费组件提交事件和显式 Ctrl/Cmd+Enter，不在 Change 事件中触发提交。

验收：用户无需理解内部 command 类型即可预测发送结果。

#### DESK-015 Sessions 与 Inspector 重构

- [x] Sessions 使用 Current task/Recent task 语义标签展示相对时间和运行状态，ID 降为辅助信息，不重复显示 active ID 卡。
- [x] catalog 首次自动加载并每 15 秒增量刷新，增加搜索并保持 product catalog 的最近更新时间降序。
- [x] Inspector 提供 Changes、Task、Usage、Runtime typed section；默认 Changes。
- [x] diagnostics/recoveries 仅在 Runtime section 且存在内容时展示，不再以零计数占永久空间。
- [x] sidebar 宽度写入兼容旧配置的 preferences，支持覆盖式拖动热区、双击重置和既有窄窗口 drawer。

验收：主界面默认只展示完成当前编码任务所需的信息。

#### DESK-016 键盘、焦点和可访问性

- [x] `Ctrl/Cmd+Tab` 切换区域，root 不再绑定 plain Tab/Shift+Tab，交给 Composer/标准控件导航。
- [x] Conversation 支持 Up/Down 有界选择消息、Ctrl/Cmd+C 复制和 Space 展开/收起详情。
- [x] Overlay 通过 `FocusState::restore_after_overlay` 关闭后恢复原 focus owner；owner 在响应式布局中消失时安全回退 Composer。
- [x] root capture 输入模态，鼠标 focus 与 keyboard focus-visible 分离且不改变区域几何。
- [x] 对比度、reduced motion、文本 label/tooltip 与 32x32 primary control hit target 均有真实 headless bounds 门禁。
- [x] 锁定具备 AccessKit 支持的 GPUI/gpui-component 版本组合；Application、Navigation、Main、Complementary、Status、Log/List/ListItem、Form、TabList/TabPanel、Dialog/AlertDialog 及控件角色均写入真实 accessibility node，并补齐 label/description、selected、position/set-size 和 active-descendant。真实 `accesskit::Node` 元数据测试、语义映射契约测试以及既有键盘/overlay focus 测试共同门禁。

验收：仅键盘可完成新建 session、发送、切换区域、复制消息、审查文件和授权决策。

### Phase 4：验证与发布门禁

#### DESK-017 性能回放与基准

- [x] release gate 覆盖 1/100/1,000/10,000 条消息和 10/50/200 次增量 row revision。
- [x] release gate 覆盖 256KB Markdown、512KB+ Reasoning、1MB Bash output、表格、代码块、CJK 和 Emoji，并分别记录 bounded-preview sanitize 与真实 `TextViewState::markdown` parser P95。
- [x] release hydration 已输出 allocation count/cumulative bytes/retained bytes 线性曲线；Linux `/proc`、macOS Mach 和 Windows working-set probe 已接入同一 RSS before/after/growth 门禁；10,000-block NativeShell 已输出 headless CPU frame/input roundtrip P95、额外 window/component RSS，并通过 production binary 输出 native draw+GPU/present P95/P99 与 InputState dispatch→post-render P95/P99。
- [x] production binary 已直接输出 Linux/macOS/Windows 原生进程 RSS before/warmup/after、startup/steady growth；native gate 门禁总 RSS ≤ 256MiB、200 帧 steady growth ≤ 64MiB，headless 10k component-tree 的 64MiB 独立门禁保持不变。
- [x] Linux/macOS Bash 与 Windows PowerShell headless/native gate、物理 Space→全屏黑白 replay、应用 sample pairing 和 external CSV P95 validator 已就绪；执行与归档契约见 `docs/desktop-external-performance.md`。
- [ ] 补齐包含物理输入、OS 输入队列和显示扫描的外部 click-to-photon 合格样本，以及 macOS/Windows production 原生窗口 memory 合格样本。
- [x] `scripts/desktop-perf-gate.sh` 提供串行、可重复的本地 release benchmark，并断言 headless full-tree CPU frame、真实 InputState roundtrip、frame preparation、Composer edit、allocation/RSS 和 final parse 预算；基线记录在 `docs/desktop-performance-baseline.md`。
- [x] `scripts/desktop-native-perf-gate.sh` 与 PowerShell 等价入口启动无 runtime 的确定性 production replay，预热 20 帧后门禁 200 个 GPUI draw+present 样本的 P95 ≤ 16.7ms、P99 ≤ 33ms、50 个 InputState dispatch→post-render 配对样本的 P95 ≤ 50ms、production Markdown completion P95 ≤ 150ms 与 production RSS；Bash 入口无 display 时明确拒绝运行。

#### DESK-018 组件、截图和端到端回归

- [x] Focus bounds 组件测试。
- [x] Stable key/session switch 测试。
- [x] Following/Paused scroll anchor 测试。
- [x] Streaming→Final Markdown revision 测试。
- [x] 宽、中、窄窗口截图 golden tests。
- [x] Authorization、recovery、file review 和 command palette 端到端 smoke test。
- [x] `scripts/release-api-snapshots.sh` 锁定 `agent-core`、`ai`、`coding-agent`、`desktop`、`tui` 的公开边界与产品事件 inventory，防止 desktop 发布验证绕过上游契约漂移。

自动化证据：`native_shell_focus_and_responsive_bounds_are_stable` 在真实 NativeShell/GPUI 组件树上验证焦点零几何位移与三档响应式 bounds；`session_scoped_cache_keys_prevent_cross_session_state_reuse`、`paused_anchor_survives_growth_insertion_and_eviction_above_it` 和 `streaming_to_final_revision_sanitizes_once_and_freezes_final_state` 覆盖 identity、scroll anchor 与 revision；四条 NativeShell smoke test 通过真实 action/subview event 到达 overlay focus 和 runtime command queue。GPUI 0.2.2 的 headless `VisualTestContext` 不提供像素捕获接口，因此 `scripts/desktop-visual-golden.sh` 启动 production binary 的确定性 visual replay，在 X11 下验证目标窗口为活动窗口后只捕获该窗口，并对 wide/medium/narrow 三档 committed PNG 执行尺寸与归一化 RMSE 门禁；操作与更新流程见 `docs/desktop-visual-goldens.md`。

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
desktop.list.height_update
desktop.list.layout
desktop.render.prepare_rows
desktop.render
desktop.input.change
desktop.input.to_render
release: markdown_parser_p95_us
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
| GPUI 与组件版本漂移导致类型分叉或 accessibility 回退 | manifest 固定 gpui-component revision，提交 `Cargo.lock` 固定 GPUI/component 完整 commit，并用 dependency boundary 测试校验单一依赖来源与锁定值 |
| 依赖库 Markdown 行为限制 | live path 只渲染纯文本；settling/final 以稳定 revision key 做一次整段解析，直接复用组件的首帧稳定高度路径，并由 code-copy headless 测试及三档 production golden 门禁 |

每个阶段必须保持可回退：runtime 协议和产品事件不变，新的 view model、stream renderer 和视觉组件均在 desktop adapter 内演进。

## 9. 当前推进状态

| 任务 | 状态 | 备注 |
|---|---|---|
| DESK-001 | 进行中 | 初始提交同 fixture 的 hydration/allocation/scroll/input 五次中位数、历史不可重建边界、release replay/streaming fixture、主要 CPU tracing、production 逐行 Markdown completion、uncached GPUI headless CPU frame、native GPU/present P95/P99 与输入内部上界已完成；待外部 click-to-photon 合格样本 |
| DESK-002 | 完成 | 已移除动态面板边框，改用已有 header divider 和标题颜色；回归测试已通过 |
| DESK-003 | 完成 | ShellLayout 改为稳定 workspace 模型；sidebar/composer/status/content width 共享 token，响应式临界点与 width bucket 均有回归 |
| DESK-004 | 完成 | StreamingText 三态 revision 协议、16ms 合批、100ms settling、stale rejection、live 纯文本与 settling/final 稳定 keyed Markdown 路径均已完成；真实解析成本由 release matrix 门禁 |
| DESK-005 | 完成 | typed ConversationItemKey 已统一 cache/Markdown/row element/hover/layout/selection 身份，并覆盖 session 与 durable/live 隔离 |
| DESK-006 | 完成 | 基于真实 offset 的迟滞状态机、streaming unseen 计数和无布局占位的浮动 pill 已完成；GUI 像素门禁归入 DESK-017 |
| DESK-007 | 完成 | 文本保持 16ms 批次，行高约 15Hz；Following 底部锚定、Paused offset compensation 和 Unicode display width 已完成 |
| DESK-008 | 完成 | typed delta 已贯穿 Projection→NativeShell；product event 只增量同步 dirty overlay，replace 路径保留全量重建 |
| DESK-009 | 完成 | revision-aware bounded row cache 已接入 NativeShell；sanitized `Arc<str>`、稳定 Markdown state key 和 measured height 可复用 |
| DESK-010 | 完成 | 持久 row/height/size、sequence→单 index 更新、bounded 结构回退、15Hz 单行补刷和 67ms resize debounce 已完成 |
| DESK-011 | 完成 | 所有计划区域 Entity 已拆分；token 流隔离到 Conversation 子树，usage-only telemetry 限制为 4Hz，typed command/authorization/recovery 边界测试通过 |
| DESK-012 | 完成 | 系统 UI 字体与局部 monospace 已分层；中性 surface、蓝色 focus、紫色 reasoning 及 warning/danger 语义已解耦，嵌套边框已减少 |
| DESK-013 | 完成 | Assistant/User 宽度、Reasoning/Tool 折叠、稳定 Copy/More 槽、bounded live copy、Markdown Copy code、持久化 Tool duration 与独立可回放的 Reasoning 分段 duration 均已完成 |
| DESK-014 | 完成 | Idle/Running 均为单主操作；每 session draft/mode、pending/authorization/rejected 文案与 InputState IME 边界已落实 |
| DESK-015 | 完成 | Sessions 搜索/相对时间/自动刷新与 Inspector 按需分区已完成；sidebar 宽度可持久化、拖动和双击复位，窄屏继续使用 drawer |
| DESK-016 | 完成 | 区域/消息键盘导航、overlay focus restore、focus-visible、32x32 primary hit-target 与 AccessKit accessibility tree 已完成；真实 node 元数据、语义映射、modal focus 及依赖锁定均有自动门禁 |
| DESK-017 | 进行中 | release scale/content/streaming/allocation、三平台 production resident-memory probe、Linux hydration/headless/native RSS、10k NativeShell headless frame、production Markdown completion、native GPU/present、InputState dispatch→post-render、Bash/PowerShell 与物理测量工具链均已完成；待外部 click-to-photon 与 macOS/Windows production 原生窗口样本 |
| DESK-018 | 完成 | Focus bounds、session key、scroll anchor、Streaming→Final、四条关键流程 smoke、production wide/medium/narrow screenshot golden 与跨 crate release API snapshot 均已覆盖 |

## 10. 完成定义

本计划仅在以下条件全部满足后视为完成：

- 所有 DESK 任务完成或有明确批准的范围调整。
- 成功标准中的交互与性能指标有可重复证据。
- 宽、中、窄三种布局通过截图和键盘回归。
- 10,000 条消息压力场景下无持续 frame degradation。
- 产品事件、authorization、recovery、file review 和 session 生命周期无功能回退。
- 架构文档、用户可见快捷键和开发者性能说明同步更新。
