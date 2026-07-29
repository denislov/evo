# Desktop 多项目工作区、启动界面与运行时上下文重构计划

> 状态：实施中（DSK-600、CAG-201、CAG-202、CAG-203、CAG-204、DSK-610、DSK-611、DSK-612、DSK-613、DSK-630、DSK-631、DSK-640、DSK-641、DSK-642、VUI-401、DSK-650 已完成）
> 决策日期：2026-07-29
> 最近更新：2026-07-30
> 前置计划：[`desktop待机界面与多会话工作台.md`](./desktop待机界面与多会话工作台.md)
> 适用范围：`coding-agent` 产品层、`desktop` runtime/projection/native shell、Desktop 视觉资产与自动验收
> 总原则：允许破坏内部实现并进行结构性重构；公开产品契约保持类型安全；旧 session 必须可读取；不以兼容错误架构为目标

## 一、执行摘要

本计划把 Desktop 从「一个启动 cwd、一个共享项目上下文、若干 session UI」重构为真正的
**多项目工作台**：每个 session 拥有不可变的 workspace scope 与独立的
`CodingAgentEmbeddingContext`，Home composer 显式选择下一次新会话的项目目录，左侧导航按项目
组织历史 session，中间栏拥有自己的顶栏和工作区，右侧 Inspector 是独立第三栏。

本计划一次性落实以下已确认需求：

1. Desktop 保持三栏布局：左 Sidebar、中间 Workspace、右 Inspector；顶栏只属于中间栏。
2. 启动 Home 默认展开左 Sidebar；Home 不再隐藏正常导航。
3. Home 删除 Recent Sessions、Global Skills 和重复的 Model/Thinking/Project 摘要。
4. Home 中央使用巨大 Evo 矢量 logo、品牌文案和 composer。
5. Sidebar 固定提供 New conversation、Skills、Projects；历史 session 按工作目录分组为项目。
6. Session catalog 不在启动、定时器、panel 打开、session 切换或失败重试时自动加载；只允许用户手动刷新。
7. 模型下拉只显示具有可解析认证材料且支持文本的模型，并按 provider 分组。
8. Thinking 下拉由当前模型的产品级 capability 决定，不再固定遍历全部 level。
9. Inspector 在窄布局中只覆盖中间 body，不覆盖中间顶栏；Model、Thinking、Profile 始终可操作。
10. Composer 的 `+` 右侧增加项目目录选择器；未选择时显示 `无项目`。
11. 第一次发送时，项目选择参与构造 session 与 agent runtime；已有 session 的 cwd 不可变。
12. `无项目` 使用全局配置和受管理 scratch workspace，但在产品语义和历史分组中仍是 Projectless，
    不把 scratch 路径伪装成普通项目。

这不是一组孤立的 VUI 修改。核心重构是：

```text
旧：DesktopRuntime = 一个固定 EmbeddingContext + 多个共享该 context 的 session

新：DesktopRuntime
    ├── HomeContext（global catalog + 新会话选择）
    ├── SessionWorkspace A（WorkspaceScope + EmbeddingContext A + Session A）
    ├── SessionWorkspace B（WorkspaceScope + EmbeddingContext B + Session B）
    └── SessionWorkspace C（WorkspaceScope + EmbeddingContext C + Session C）
```

## 二、成功标准

全部完成时必须同时满足：

- Fresh/default Home 在可 dock 宽度下显示左 Sidebar，且用户手动关闭后的偏好仍被尊重。
- Home 中没有 Recent Sessions、Global Skills 卡片或重复的 Model/Thinking/Project 文本。
- 中间顶栏不跨左右栏；左右 panel toggle 分别位于中间顶栏两端。
- 宽屏为三栏 docked；中等/窄屏的 Sidebar/Inspector drawer 只覆盖中间 body。
- Inspector 打开时仍可点击 Model、Thinking、Profile 和 Inspector toggle。
- Sidebar 的 Projects 按 workspace scope 分组；同名目录可通过完整路径区分。
- Desktop 静置不会自行发出 `ListSessions`；不存在 15 秒 session catalog refresh timer。
- 手动刷新成功不弹全局 “Loaded N sessions” toast；失败提供局部 Retry 和错误反馈。
- 模型菜单没有未配置 provider/model 的大批 disabled 项。
- Thinking 菜单不会显示当前模型不支持的 level；模型切换不会留下非法 thinking 状态。
- Home composer 默认显示 `无项目`，目录 picker 只允许单选目录，并可恢复为 `无项目`。
- 选择项目后，session 持久化 scope、agent runtime cwd、工具 cwd、授权边界和 Inspector cwd 一致。
- 已有 session 中目录选择器为只读；不能在同一对话中从项目 A 切到项目 B。
- 多个并存 session 可以分别属于不同项目，不共享错误的配置、资源或 cwd。
- Projectless session 仅加载 global 配置，使用受管理 scratch cwd，并在 Sidebar 归入 `无项目`。
- 旧 session 可读取、可打开、可完成一次显式 workspace scope 迁移。
- CLI/TUI 不被迫依赖 Desktop DTO；`desktop` 不直接解释 `ai` provider compatibility。
- wide/medium/narrow、keyboard focus、no-color、reduced-motion 和 authorization golden 全部重新 review。

## 三、被本计划替代的旧决策

前置计划中的以下判断由本计划明确替代：

| 旧判断 | 新决定 |
| --- | --- |
| 多项目切换器不在本轮 | 多项目 workspace 是本轮核心架构，不再延期 |
| Desktop runtime 可共享一个 `EmbeddingContext` | 每个 session 必须拥有与其 workspace scope 对应的 context |
| Home 是隐藏左右 panel 的特殊 idle layout | Home 使用正常三栏 shell，仅 Inspector 默认关闭 |
| Session 列表是扁平历史 | Sidebar 按 workspace scope 组织 Projects → Sessions |
| Session catalog 可自动刷新 | catalog 严格按用户动作加载，不做定时刷新或自动重试 |
| Global Skills 直接铺在 Home/Sidebar | Home 不展示；Sidebar 提供 Skills 一级入口，中央使用独立 Skills surface |

以下既有决定继续有效：

- 启动不创建 durable session；第一次 prompt 才创建。
- Home 不使用伪造 session id 构造 `DesktopProjection`。
- 最多 4 个 session runtime 并存；Home 不占名额。
- 后台 session 继续推进 projection，但不能持续触发前台重绘。
- `DesktopProjection` 仍是单 session 产品事件的唯一归并入口。
- Desktop 仍是 adapter，不读取 provider secret，不自行解释模型 wire compatibility。

## 四、已经核实的现状与根因

### 4.1 Home 主动绕过 panel preference

`NativeShell::resolve_layout` 在 `projection.is_none()` 时调用 `ShellLayout::resolve_idle`；后者把
`sessions` 和 `context` 固定为 `None`。虽然 `DesktopPreferences` 与 `PanelVisibility::default()`
都把 Sidebar 设为 visible，Home 仍永远隐藏它。

实施结论：删除「idle 必须无 panel」这一布局特例。Idle 只影响 center body 内容，不再改变 shell
的列模型或区域焦点顺序。

> DSK-640 已清偿：`resolve_idle` 与 layout 的 idle 分支已删除，Home/Conversation 统一通过同一个
> panel-width-aware resolver 计算 `sidebar / center / inspector`，Home 在可 dock 宽度正常显示 Sidebar。

### 4.2 Home DTO 与视图直接承载 Recent/Skills

`HomePaneViewModel` 当前包含 `recent_sessions`、`omitted_sessions`、`global_skills`、catalog pending；
`HomePane::render` 明确生成 RECENT SESSIONS 与 GLOBAL SKILLS 两栏。

实施结论：这些字段和事件从 HomePane 删除。Home 只接收品牌/项目选择需要的 bounded presentation
数据；session 与 skills 导航由 Sidebar/Skills surface 独占。

### 4.3 Header 归属正确，但 drawer 挂载层错误

当前 `conversation_header` 已经位于 center workspace 内，这个归属应保留。问题在于
`OverlayHost` 是 application-root `absolute().size_full()`，窄 Inspector 又是
`top_0/right_0/bottom_0`，因此覆盖 center header。

实施结论：不引入全局顶栏。拆分 root modal host 与 center-body drawer host。

### 4.4 模型目录已有 configured 信息

`CodingAgentModelChoice` 已包含 provider、reasoning、supports_text、supports_images、configured、selected。
Desktop 当前仍渲染所有模型，仅把未配置模型 disabled 并附加 `not configured`。

实施结论：不新增 Desktop 认证判断；使用产品给出的 configured 事实过滤，并按 provider 分组。

### 4.5 Thinking capability 尚未按模型暴露

Desktop 当前固定遍历 `DesktopThinkingLevel::ALL`。`ai::Model` 有 `thinking_level_map`，
`CodingAgentApplicationStartup` 也能读取当前模型 mapping，但 `CodingAgentModelChoice` 只暴露
`reasoning: bool`，且没有 `can_disable`。

实施结论：在 `coding-agent` 增加 provider-neutral 的 thinking capability DTO；Desktop 只消费结果。

### 4.6 Catalog 存在真实的后台循环

现状同时包含：

- `NativeShell::new` 异步调用 `request_session_catalog`；
- panel 打开/键盘聚焦会调用 catalog；
- session create/open/close/resync 后调用 catalog；
- `SessionsListed` 成功后安排下一次 refresh；
- admission 失败后安排 retry；
- `SESSION_CATALOG_REFRESH_INTERVAL = 15s`；
- 成功结果通过 preference notice 生成 Loaded N sessions toast。

实施结论：删除 timer、deadline、所有隐式调用与成功 toast；仅保留显式 user command。

### 4.7 Composer 已有适合扩展的 bottom row 与 native picker

Composer bottom-left 当前包含附件 `+`；附件选择使用 `cx.prompt_for_paths(PathPromptOptions)`。
目录选择可复用同一平台接口，只需 `files=false/directories=true/multiple=false`。

实施结论：目录选择是 typed Composer event，不让 Pane 直接改 root/runtime。

### 4.8 Runtime 仍只有一个固定项目 context

`RuntimeState` 当前持有一个 `CodingAgentEmbeddingContext`，所有 session create/open/prompt operation
均通过该 context。Home 第一次提交只是 `session_id=None`，dispatch 用固定 context 创建 session，
然后用同一个 context 准备 prompt。

实施结论：仅给 `SubmitPrompt` 塞一个 `cwd` 字段是不完整且危险的。必须引入产品级 workspace
scope，并让每个 session context 与 session 生命周期绑定。

## 五、确认后的产品与交互决策

### 5.1 三栏 shell

```text
┌──────────────────┬──────────────────────────────────────────┬──────────────────┐
│ SidebarHeader    │ [Sidebar] Project  Model Thinking Profile│ InspectorHeader  │
│                  │                              [Inspector] │                  │
├──────────────────┼──────────────────────────────────────────┼──────────────────┤
│ New conversation │                                          │ Changes          │
│ Skills           │           Home / Conversation            │ Task             │
│ Projects         │                                          │ Usage            │
│                  │                 Composer                 │ Runtime          │
└──────────────────┴──────────────────────────────────────────┴──────────────────┘
```

- Sidebar 与 Inspector 是全高兄弟列，各自有 header。
- 中间顶栏只跨 center column。
- Sidebar toggle 位于 center header 最左端；Inspector toggle 位于最右端。
- panel 关闭后 center column 扩张；不移动 selectors 的相对顺序。
- Fresh/default Home：Sidebar 展开、Inspector 关闭。
- 用户手动 panel preference 被保存；Home 不再强制覆盖。

### 5.2 响应式规则

| 宽度 | Sidebar | Inspector | Drawer 规则 |
| --- | --- | --- | --- |
| Wide | 可 dock，默认开 | 可 dock，Home 默认关 | 无 drawer；三栏并列 |
| Medium | 优先 dock Sidebar | Inspector 为右 drawer | 只覆盖 center body，不覆盖 center header |
| Narrow | 两者均不 dock | 两者均不 dock | 同时最多一个 drawer；打开一侧先关闭另一侧 |

- Panel drawer 非模态，不使用阻断 center header 的全屏 scrim。
- 点击 drawer 外、Escape 或对应 toggle 关闭。
- Authorization、Command Palette、Full Message 等真正 modal 继续由 root modal host 承担。

### 5.3 Sidebar 信息架构

```text
EVO

+ New conversation
◇ Skills

PROJECTS                         ↻
▾ evo                            12
  Desktop UI polish          2h ago
  Runtime cleanup             1d ago
▸ pi2rust                         5

无项目                             2
```

- New conversation 与 Skills 是固定一级入口。
- Skills 打开独立 center surface；不在 Home 或 Sidebar 内展开技能卡片列表。
- Projects 标题提供唯一 catalog refresh 按钮。
- Project group 默认按最近 session 更新时间降序；内部 session 同序。
- 当前 project group 默认展开；其他折叠状态按稳定 group id 记忆。
- 标题显示 basename，辅助信息/tooltip 显示完整路径。
- 同名 basename 不合并；workspace scope 的稳定 id 是分组 key。
- Projectless session 统一进入 `无项目`。
- Legacy 且无法可靠归类的 session 进入 `旧会话`，不能丢弃。
- 搜索匹配 project 名、完整路径、session name、session id。

### 5.4 Home

Home center body 只包含：

1. 巨大 Evo wordmark；
2. 主文案 `Software evolves. Your agent should too.`；
3. 辅助文案 `Describe what you want to build, fix, or understand. Evo will plan, act, and adapt with you.`；
4. composer；
5. 必要的局部错误/加载状态。

删除：Recent Sessions、Global Skills、Model/Thinking/Project 重复摘要、卡片墙。

Composer placeholder 改为：`What do you want to build or improve?`

### 5.5 Evo logo：Evo Loop

- 采用可主题化 SVG/GPUI 原生 vector，不使用生成式 PNG。
- 完整 Home wordmark 使用小写 `evo` 连续视觉路径。
- `e` 表示 seed/initial state；`v` 左降右升；`o` 是开放、向内回旋且末端上扬的 feedback loop。
- 主体单色，最后上扬端点使用 accent；no-color 模式仍靠轮廓成立。
- 不使用脑、机器人、DNA、无限符号、AI 星芒等通用图形。
- 同时交付 full wordmark 与只保留开放 `o` 的 compact mark。
- 校验 16/24/32px compact mark，以及 200–360px Home wordmark。
- reduced-motion 模式无动画；首版不引入循环动画，避免品牌资产依赖时序状态。

### 5.6 Composer 项目选择器

```text
[+] [ProjectDirectoryIcon 无项目 ▾]                         [Submit]
[+] [ProjectDirectoryIcon ~/dev/pi2rust/evo ▾]              [Submit]
```

Home / New conversation：

- 默认 `无项目`；不自动复用进程 cwd 或上次项目。
- 点击打开单目录 picker；取消保持原状态。
- 选中后显示 compact path，tooltip/aria-label 显示完整路径。
- 菜单提供 `选择其他目录…` 与 `清除项目（无项目）`。
- submit admission 期间 disabled，防止 cwd 在命令入队后变化。
- 发送失败保留文本、附件、thinking、profile、model 与项目选择。

已有 session：

- 同一位置显示 session workspace，呈只读/locked 状态。
- tooltip：`项目目录在对话创建后固定。请新建对话以选择其他项目。`
- 不允许中途改变 cwd；要换项目必须 New conversation。

### 5.7 模型选择器

- 只显示 `configured && supports_text`。
- configured 的定义由产品层决定，包含可解析的 API key/OAuth/invocation credential，不在 Desktop 读 secret。
- provider 使用 non-interactive label；组间 separator。
- 行主文本为 model name；model id 是 metadata/tooltip；不逐行重复 provider。
- 当前 model 若刚失去认证，可作为单独 warning 状态显示，但不把全部未配置模型放回菜单。
- 空状态为 `No configured text models`，提供进入认证配置的明确动作（若本轮没有设置页，至少给出可执行说明）。

### 5.8 Thinking 选择器

产品层 DTO 至少表达：

```rust
pub struct CodingAgentThinkingCapability {
    pub supported: bool,
    pub can_disable: bool,
    pub explicit_levels: Vec<CodingAgentThinkingLevel>,
}
```

- 非 reasoning 模型隐藏 selector；不得显示无意义的七项菜单。
- reasoning 模型显示 `Auto` 加 capability 支持的显式 levels。
- `Off` 仅在 `can_disable` 时出现。
- `Auto` 是 UI 文案，对应现有 Default/未显式覆盖语义。
- 模型切换后若当前 level 不支持，必须在同一 typed selection transaction 中回退到 Auto。
- Desktop 不接触 provider-specific mapping string/null。

### 5.9 Catalog 手动刷新

- 初始未加载：`Projects are loaded on demand.` + `Load projects`。
- 点击 refresh 后按钮本地 spinner/disabled。
- 成功仅更新列表与 `Updated …` metadata；不弹成功 toast。
- 失败保留旧 catalog（若有），显示局部错误与 Retry；可同时发一次错误 toast。
- create/rename/close 使用已知结果增量维护 catalog，不发完整 ListSessions。
- 外部进程创建的 session 只有手动刷新后出现，这是明确产品语义。
- 打开 panel、聚焦 panel、Home render、idle timer、resync 不得隐式 list。

## 六、目标产品数据模型

### 6.1 Workspace scope 是产品事实

新增 provider-neutral、adapter-safe 类型：

```rust
pub enum CodingAgentWorkspaceScope {
    Project {
        cwd: PathBuf,
    },
    Projectless {
        workspace_id: String,
    },
    Legacy {
        cwd: Option<PathBuf>,
    },
}
```

约束：

- `Project.cwd` 在产品边界完成绝对化、存在性、directory 与 canonical/normalized 策略校验。
- `Projectless.workspace_id` 用于稳定定位受管理 scratch，不向 UI 展示 scratch 实际路径。
- `Legacy` 只存在于读取旧 session 的 migration boundary；新 session 禁止创建 Legacy。
- 提供 `workspace_group_id`、display name、display path 等安全 DTO，Desktop 不自行 hash/canonicalize。

### 6.2 持久化 schema

Session manifest/轻量 overview 增加显式 workspace scope record：

```text
workspace_scope = project(cwd) | projectless(workspace_id)
```

要求：

- Bump manifest schema/version，reader 向后兼容缺失字段。
- 新 `SessionCreated` record 同时记录 scope identity；旧 `cwd` 字段保留兼容读取，停止作为唯一分类依据。
- Overview 无需 hydrate 全 event log 即可返回 workspace scope。
- Projectless scratch 路径不作为 project cwd 对外展示。
- 旧 session 迁移：
  1. 已有显式 scope：直接使用；
  2. cwd 位于产品管理的历史 scratch root：迁移为 Projectless，并生成稳定 workspace id；
  3. 其他 cwd：迁移为 Project；
  4. cwd 缺失/非法：Legacy(None)，归入旧会话；
  5. 第一次成功 open/rename/显式 migration 时原子写回新 manifest。
- 不允许 Desktop 根据字符串前缀猜 scratch；迁移判断属于 `coding-agent`。

### 6.3 Runtime session owner

用统一 owner 替代 `RuntimeState.context + sessions HashMap` 的共享上下文：

```rust
struct RuntimeSessionWorkspace {
    scope: CodingAgentWorkspaceScope,
    context: CodingAgentEmbeddingContext,
    session: RuntimeSessionState,
}

enum RuntimeSessionState {
    Idle(CodingAgentSession),
    Active(ActivePrompt),
}

struct RuntimeState {
    home: HomeRuntimeContext,
    workspaces: HashMap<String, RuntimeSessionWorkspace>,
    focused_session_id: Option<String>,
}
```

如果 event-loop 所有权使 ActivePrompt 不能直接嵌入 owner，可保留单独 active map，但 context 必须按
session id 存储并通过一个 owner API 访问；禁止退回单一共享 context。

### 6.4 Home runtime context

Home 需要全局模型/认证/profile 基线，但没有 project：

```rust
struct HomeRuntimeContext {
    global_context: CodingAgentEmbeddingContext,
    selected_model_id: String,
    selected_profile_id: ProfileId,
    thinking: DesktopThinkingLevel,
}
```

- global context 使用 global-config-only 与受管理 scratch。
- 用户选择 Project(path) 后，第一次 submit 用该 path 创建新的 project context。
- Home 显式 model/profile 选择作为 override 传给新 context；不能被项目默认值静默改掉。
- 项目 context load 失败时不创建 session，Home draft 保留。
- 新 context load 可在用户选目录后预验证，但最终 submit 必须重新权威校验。

### 6.5 Typed prompt target

替换 `session_id: Option<String>` 的隐式含义：

```rust
enum DesktopPromptTarget {
    New {
        workspace: CodingAgentWorkspaceSelection,
        model_id: String,
        profile_id: String,
    },
    Existing {
        session_id: String,
    },
}
```

`DesktopRuntimeCommand::SubmitPrompt` 持有该 target。Admission 必须验证：

- Existing 不接受任何 cwd override；
- New 必须显式为 Project(path) 或 Projectless，不能用 `None` 猜语义；
- path 字节数、NUL、存在性、directory、权限错误有 typed error；
- debug 输出不包含 credential，路径按既有 Desktop error policy bounded；
- command id 对账和 rejection 必须把失败归还正确 Home workspace。

### 6.6 第一次 prompt 的事务边界

顺序必须固定：

1. 校验 typed target；
2. resolve workspace scope；
3. load 对应 `CodingAgentEmbeddingContext`；
4. 校验/绑定 selected model、profile、thinking；
5. 创建 session 并持久化 scope；
6. 用同一个 context prepare prompt/attachments；
7. 启动 operation；
8. 发布 `PromptAcceptedWithSession` 完整 snapshot；
9. Shell 把 Home draft 原子迁移为 session workspace。

Context load 或 session create 失败时不得出现 UI 幽灵 session。若 session 已持久化而 prompt start 失败，
沿用现有明确 rejection contract，但 update 必须带 session snapshot 和 scope，UI 明确展示已创建 session，
不得悄悄丢失。

### 6.7 打开历史 session

OpenSession 不再用 Home/shared context：

1. 通过 global session query 读取轻量 overview 与 workspace scope；
2. Project：以持久化 cwd load context；
3. Projectless：解析 workspace id 到 managed scratch，并 global-config-only load；
4. Legacy：执行 migration policy，失败则给 typed unavailable reason；
5. 用该 context open session；
6. context 与 session 一起进入 RuntimeSessionWorkspace。

目录已删除时不得把 session 从列表移除。Project group 保留，session 行显示 unavailable；用户可查看错误、
复制原路径，未来可提供 Relocate，但本轮不擅自重绑定 cwd。

## 七、状态所有权与模块边界

### 7.1 Desktop UI state

`SessionWorkspace` 增加或重命名为明确的 workspace state：

```rust
struct SessionWorkspace {
    project: CodingAgentEmbeddingSnapshot,
    projection: Option<DesktopProjection>,
    composer: ComposerState,
    composer_attachments: Vec<PathBuf>,
    composer_project: ComposerProjectSelection,
    thinking_selection: DesktopThinkingLevel,
    // existing bounded UI state...
}
```

- 仅 Home workspace 的 `composer_project` 可变。
- 安装 durable session snapshot 后从 session scope 得到 locked display state。
- UI 不能直接改 `project.cwd`。
- New conversation 创建干净 Home state；默认 Projectless。

### 7.2 Pane DTO

`ComposerPaneViewModel` 新增 bounded 字段：project label、完整 accessible path、editable、pending、locked。
`ComposerPaneEvent` 新增 ChooseProjectDirectory/ClearProjectDirectory。

`HomePaneViewModel` 删除 session/skills/catalog 字段，只保留 hero 所需内容。

`SessionsPaneViewModel` 不再暴露扁平 catalog；使用预分组、bounded `ProjectGroupViewModel`，Pane 不做
filesystem normalization。

`ConversationHeaderViewModel` 的 model option 变成 provider group；thinking option 由 capability 预计算。

### 7.3 Overlay owner

拆分：

```text
RootModalHost
  Authorization / CommandPalette / FullMessage

CenterDrawerHost
  NarrowSidebar / NarrowInspector
```

- CenterDrawerHost 位于 center header 之后、center body 的 relative container 内。
- drawer 不能捕获 header hit regions。
- root action blocker 只对真正 modal 生效；普通 drawer 不阻止 selector。
- focus restore 分别记录 drawer 与 modal，避免沿用一个 `active_overlay` 混淆语义。

## 八、执行任务与顺序

任务 ID 延续现有命名：`CAG-*` 产品层、`DSK-*` Desktop runtime/结构、`VUI-*` 视觉与交互、
`TST-*` 迁移与验收。以下顺序是依赖顺序，不建议为追求并行而打破。

### Phase 0：基线与删除清单

#### DSK-600：记录基线并锁定现状证据

> 状态：已完成。基线提交、dirty state、既有测试失败、golden/perf 数值与完整删除清单见
> [`desktop多项目工作区与启动界面重构基线.md`](./desktop多项目工作区与启动界面重构基线.md)。

工作：

- 记录当前 main commit、dirty state、desktop/coding-agent 全量测试、golden 与 perf gate。
- 增加行为测试证明当前会自动发 ListSessions，随后在 DSK-630 中反转断言。
- 列出所有 `request_session_catalog` 调用点、所有 `schedule_session_catalog_refresh` 路径、
  `resolve_idle` caller、OverlayHost children、`DesktopThinkingLevel::ALL` UI caller。

完成标准：基线失败与本轮引入失败可区分；没有用更新 golden 掩盖结构回归。

### Phase 1：产品级 workspace scope

#### CAG-201：新增 WorkspaceScope 公共契约

> 状态：已完成。产品层已新增 `Project` / `Projectless` / `Legacy` scope、仅允许新建前两类
> scope 的 selection、safe overview 与 resolved workspace DTO；Project 路径在解析时转为绝对规范路径，
> Projectless 仅解析到受管 scratch execution cwd，不把 scratch 暴露为 UI 项目身份。稳定 group id、
> typed path error、workspace id 越界/逃逸和 scratch symlink 防护均有定向测试。
>
> 债务清偿：CAG-202 已让持久化记录写入并迁移 typed scope，DSK-613 也已让 Desktop open
> 直接消费 product-owned open target，不再从 overview 的展示 cwd 反推 workspace；DSK-631 已让
> Desktop catalog 直接透传 typed workspace overview/migration，并删除旧
> `CodingAgentSessionOverview.cwd` 兼容字段。新 session selection 不允许构造 `Legacy`。
>
> Gate：格式、diff、Clippy、API compile-pass 与新增的 7 个 workspace tests 通过；`coding-agent`
> 767 个 library tests 全部通过，boundary 仍严格保持 DSK-600 登记的 3 个既有失败；TUI 全量通过。
> CLI 的 174 个单测通过，ownership boundary 扫描仍有 4 个与本任务改动无关的既有 `ai`
> ownership 失败，需在完整计划收敛前清偿。

主要文件：

- `crates/coding-agent/src/app/embedding.rs`
- `crates/coding-agent/src/app/session.rs`
- `crates/coding-agent/src/api.rs`
- runtime facade/session DTO

工作：

- 定义 Project/Projectless/Legacy scope、selection、safe overview DTO。
- 实现路径 normalization 与稳定 group id。
- 分离 execution cwd 和 UI project identity；Projectless 不暴露 scratch 为项目。
- API 只暴露必要路径事实，不暴露 session repository path。

完成标准：纯产品单测覆盖 scope equality、display、group id、path 错误、projectless scratch 解析。

#### CAG-202：持久化 workspace scope 与旧 session 迁移

> 状态：已完成。Session manifest 已升级到 v2，并以 tagged record 持久化 Project cwd 或
> Projectless workspace id；新 `SessionCreated` 同时写入 typed scope 与兼容 cwd。v2 overview
> 只读 manifest，完全不访问 event log；v1 overview 仅有界读取首个 checksummed
> `SessionCreated` frame，不 hydrate transcript。
>
> 迁移规则：历史 scratch 直属 workspace 迁移为 Projectless，其他合法绝对 cwd 迁移为
> Project；已删除项目保留 Project identity 并返回 bounded unavailable diagnostic；缺失、相对或
> 非法 cwd 投影为 `Legacy(None) + Unavailable`。后者保持 v1 可读取且不写入伪造 scope，保证任何
> 新 session writer 和迁移 writer 都不会持久化 Legacy。首次 open、rename 前的 open 以及公开的
> `CodingAgentSessionQuery::migrate_workspace` 均复用同一个 `ManifestPatch + update_manifest`
> 原子替换通道；写入失败时原 v1 manifest 逐字节不变。
>
> Gate：新增/更新后的 776 个 `coding-agent` library tests、14 个 API contract、17 个 event
> boundary tests、Clippy、TUI 全量与 Desktop compile 均通过；`coding-agent` boundary 严格保持
> DSK-600 的 3 个既有失败，CLI 仍严格保持 4 个既有 `ai` ownership 失败，没有新增回归。

工作：

- 扩展 manifest/creation record/overview；bump schema。
- 实现旧 cwd、历史 scratch、缺失 cwd 的迁移规则。
- 保持旧 manifest 可反序列化；新 writer 永不写 Legacy。
- 保证轻量 overview 不 hydrate 完整 transcript。
- 提供显式 migration outcome/diagnostic，便于 Desktop 显示 unavailable。

完成标准：旧 fixture、project fixture、projectless fixture、损坏/已删除目录 fixture 全覆盖；迁移写盘原子。

#### CAG-203：模型 thinking capability

> 状态：已完成。`CodingAgentThinkingCapability` 以 provider-neutral 的
> `supported + explicit_levels + can_disable` 描述模型能力，并同时进入配置无关 model catalog、configured
> catalog 与当前 embedding model selection snapshot。缺失 `thinkingLevelMap` 或 map 中缺失的
> level 沿用对应 API 的默认支持，显式 `Null` level 被剔除；非 reasoning 不支持 thinking，不能表达
> 显式 thinking 的 reasoning API 保留 Auto-only capability。Anthropic 与 Mistral 支持显式关闭，OpenAI Responses 的 Off 回落 Auto，
> OpenAI Completions 仅在 compatibility 明确支持 reasoning effort 或 DeepSeek thinking format 时
> 暴露显式 level，且只有 DeepSeek format 支持显式关闭。
>
> `sanitize_thinking_level` 只接收产品 `CodingAgentModelChoice`，返回合法的 `Explicit(level)` 或
> `AutoFallback`，Desktop 无需读取 `ai::Model`、provider compat 或 wire payload。Anthropic、OpenAI
> Responses、OpenAI DeepSeek compatibility、非 reasoning、显式 null mapping、无 mapping 与 sanitize
> fallback 均有定向测试。
>
> Gate：`coding-agent` 783 个 library tests（单线程全量）、14 个 API contract、Clippy、Desktop
> 243 个 tests 与 17 个 dependency boundary tests 全部通过；`coding-agent` boundary 仍严格保持
> DSK-600 登记的 3 个既有失败，没有新增回归。并发全量 library 首轮曾因同一 fixture 的
> `.writer.lock` 竞争失败 1 项，该项单独重跑和单线程全量均通过。

工作：

- 在 `CodingAgentModelChoice` 增加 provider-neutral capability。
- 从 reasoning、thinkingLevelMap 与 API compatibility 解析 explicit levels/can_disable。
- 当前 model selection snapshot 与 catalog 全部携带 capability。
- 增加 sanitize 函数：给定 model + requested level，返回合法 level 或 Auto fallback outcome。

完成标准：Anthropic/OpenAI/非 reasoning/包含 null mapping/无 mapping 模型均有定向测试；Desktop 无需依赖 `ai`。

#### CAG-204：按 scope 构建 EmbeddingContext

> 状态：已完成。新增 `CodingAgentEmbeddingOptions::for_workspace` 产品入口，在 context load
> 前解析 `CodingAgentWorkspaceSelection`，并把 canonical execution cwd、typed scope 和安全
> overview 固定到 options/snapshot。Project context 读取各自的项目 settings、skills、agent
> profiles 与 context files；Projectless context 强制 global-config-only，即使受管 scratch 下存在
> `.evo/settings.toml`、project skill 或 `AGENTS.md` 也不会加载。
>
> Durable session root 在 workspace 解析时由全局 settings、`EVO_SESSION_DIR` 或默认全局
> `sessions` 目录确定，项目 settings 不能按 cwd 重定向它。Direct session、bootstrap、headless
> prompt 与 session query/hydration 均通过同一个 resolved-workspace 转换点，不再把已解析 scope
> 退化为裸 `cwd`；model/profile override 继续由 typed options builder 叠加。
>
> 过渡债务：`CodingAgentEmbeddingOptions::new(cwd)` 与 snapshot 中可空的 `workspace` 暂时保留。
> DSK-611 已把 Desktop runtime 全部切到 typed workspace options，并在 runtime 边界拒绝无 workspace
> snapshot；产品层 raw-cwd 构造路径与可空 snapshot 字段仍须在完整计划收敛前删除。
>
> Gate：`coding-agent` 785 个 library tests（单线程全量）、14 个 API contract 与严格 Clippy
> 全部通过；Desktop 243 个 tests、17 个 dependency boundary tests 通过，5 个既有用例保持
> ignored。`coding-agent` boundary 为 64/67，仍严格保持 DSK-600 登记的 3 个既有失败，没有新增
> 回归。新增定向验收覆盖两个 Project context 同时加载时 settings/skills/context files 隔离、
> model/profile override、共享 session root、workspace-filtered overview，以及 Projectless 对 scratch
> `.evo` 的完全忽略。

工作：

- 提供从 WorkspaceSelection + model/profile override 构建 context 的产品入口。
- Project 读取项目配置/resources；Projectless global-config-only。
- 复用同一 session root，不因 cwd 改变 durable repository root。
- Context snapshot 返回最终 scope 与 cwd 事实。

完成标准：两个不同项目同时 load，不串 settings/skills/context files；Projectless 不读取 scratch `.evo`。

### Phase 2：Desktop runtime 多 context

#### DSK-610：Typed DesktopPromptTarget

> 状态：已完成。`DesktopRuntimeCommand::SubmitPrompt` 已删除
> `session_id: Option<String>`，统一持有 `DesktopPromptTarget`。`New` 必须显式携带
> `CodingAgentWorkspaceSelection + model_id + profile_id`，`Existing` 只能携带 session id，因而
> 已有 session 的 cwd/workspace override 在类型层面不可表达。Bridge/command handle 已删除
> `*_for_session` 平行 API，所有生产与测试 caller 均通过同一 typed submit 入口。
>
> Admission 在入队前统一验证 Existing session id、New model/profile id、16 KiB workspace path
> 上限、NUL、存在性、directory 与可用性，并把 workspace resolution failure 投影为 bounded typed
> error。Command Debug 仅保留 command kind、command id 与 target kind，不输出 prompt、附件、
> workspace path、model/profile/session selection。Test harness 直接捕获 `DesktopPromptTarget`，不再把
> `None` 解释为 New。
>
> Dispatch 中 `New` 即使在已有 session 打开时也会创建独立 session，并返回
> `PromptAcceptedWithSession`；`Existing` 只路由到显式 session。Desktop 启动 context 同时切换到
> `CodingAgentEmbeddingOptions::for_workspace`，Home submission 可以从 snapshot 构造真实 typed
> workspace target，而不是依赖 scratch cwd 猜 Projectless。
>
> 债务清偿：DSK-611 已建立 `RuntimeSessionWorkspace` owner，DSK-612 已让 `New` 中的
> workspace/model/profile/thinking 实际参与 scope resolve、context load、capability sanitize、session
> persistence 与 prompt start，并完成 post-create rejection 的强类型 snapshot/UI 迁移契约。
>
> Gate：Desktop 245 个 tests 通过、5 个既有用例保持 ignored，17 个 dependency boundary tests
> 全部通过；严格 Clippy、格式和结构删除审计通过。`coding-agent` boundary 为 64/67，仍严格保持
> DSK-600 登记的 3 个既有失败，没有新增回归。新增验收覆盖 typed target admission/debug redaction、
> harness 的完整 New/Existing payload，以及已有 session 存在时显式 New 仍创建新 session。

主要文件：

- `crates/desktop/src/runtime/protocol.rs`
- `crates/desktop/src/runtime/bridge.rs`
- `crates/desktop/src/runtime/dispatch.rs`
- runtime tests/harness

工作：

- 用 New/Existing target 替换 `session_id: Option` 的创建语义。
- 新 target 携带 workspace selection 与 Home model/profile selection。
- Bridge admission 做尺寸/组合验证；Existing cwd override 编译期不可表达。
- 更新 command kind、debug redaction、test harness 捕获。

完成标准：所有 caller 使用显式 target；不存在 `None` 代表 New 的隐式分支。

#### DSK-611：RuntimeSessionWorkspace owner

> 状态：已完成。`RuntimeState` 已删除全局唯一的 `context + sessions` 所有权，改为
> `HomeRuntimeContext + HashMap<session_id, RuntimeSessionWorkspace>`。Home owner 只保存启动界面的
> context 与可派生 session context 的 typed options；每个 idle workspace owner 独立持有最终
> `CodingAgentWorkspaceScope + CodingAgentEmbeddingContext + CodingAgentSession`。`ActivePrompt` 在运行期间
> 接管同一 scope/context/session，完成或失败后再把 session 放回原 owner，不会从当前焦点重新推断 cwd。
>
> `New` prompt 已实际消费 target 中的 workspace/model/profile，按目标 workspace 加载独立 context，
> 同时继承 Home 已冻结的 durable session root。Reload、model/profile selection 使用显式
> `DesktopRuntimeOwnerTarget::{Home, Session}`；review 与 external editor 必须携带明确 session id；
> create/start/recovery/rename/close 与 active command 均通过对应 workspace owner。最多 4 个 open
> session、command id、priority/data queue、shutdown/join 与 recovery 语义保持不变。
>
> 债务清偿：DSK-612 已清偿首次 prompt 的原子创建与失败恢复；DSK-613 已清偿 persisted session
> 从 Home options 建 context 的错误路径，历史 session 现在先从 durable manifest 取得完整 scope，
> 再建立目标 workspace context；DSK-630 已把 session query 收敛为严格用户驱动，DSK-631 已完成
> product-owned identity 驱动的项目分组 catalog。产品层 raw-cwd options 与可空 workspace snapshot
> 仍按 CAG-204 债务在计划收敛前删除。
>
> Gate：Desktop 246 个 tests 通过、5 个既有用例保持 ignored，17 个 dependency boundary tests
> 全部通过；严格 Clippy、格式、CodeGraph 与 `ast-grep outline` owner/call-path 审计通过。
> `coding-agent` boundary 为 64/67，仍严格保持 DSK-600 登记的 3 个既有失败，没有新增回归。
> 新增验收覆盖两个 Project prompt 同时运行时 cwd、resources、model/profile、context files 与 product
> event session id 隔离；Home/Session selection target 隔离；后台 workspace 静默推进并在切换后恢复；
> 关闭后台 owner、file review 显式路由及 runtime shutdown/join 均不影响其他 owner。

工作：

- 删除 `RuntimeState.context` 作为所有 session 的唯一 context。
- 建立 per-session scope/context/session owner。
- create/open/start_prompt/select model/profile/reload/review/external editor 全部路由到目标 owner。
- active prompt 完成后把 session 放回同一 context owner。
- 保留最多 4 session、command id、priority/data queue、shutdown/recovery 语义。

完成标准：两个项目 session 同时运行，事件、cwd、资源、model/profile 不串；后台 session 不触发前台 notify。

#### DSK-612：首次 prompt 的 scope-aware 原子创建

> 状态：已完成。New prompt 现在固定按第 6.6 节执行：Bridge admission 校验 typed target；runtime
> 重新 resolve workspace（覆盖 admission 后目录被删除的 TOCTOU）；按 resolved workspace load 独立
> context；由该 context 验证 model/profile，并用 `sanitize_thinking_level` 把不支持的显式 thinking
> 回落为 Auto；在持久化前验证 typed scope；创建 session 后直接从同一 owner 构造初始空 transcript、
> 空 recovery 的完整 hydrated snapshot；再由同一 context prepare prompt/attachments 并启动 operation；
> 只有 start 成功才发布 `PromptAcceptedWithSession`。
>
> Context load、workspace path 删除或 session create 在持久化前失败时只返回 `CommandRejected`，
> runtime 中没有 owner，session manifest 也不存在。Session 已持久化后的 prompt prepare/start 失败
> 必须返回 `PromptRejectedWithSession`；该 variant 已删除 `metadata + Option<snapshot>` 模糊形态，类型上
> 强制携带完整 `DesktopRuntimeHydratedSnapshot`，并保留 idle workspace owner。Prompt-start failure
> 注入点已移动到同 context prepare 成功之后，覆盖真实 post-persistence 阶段。
>
> Shell 收到 accepted/rejected snapshot 时，会把当前 Home `SessionWorkspace` 原地晋升为新 session
> owner，即使已有其他后台 session 也不会新建空 workspace。Pending command ledger、精确 draft、附件与
> thinking selection 因而一起迁移；rejection 会安装已创建 session、恢复 idle composer 并展示 typed
> issue，不存在 draft 留在隐藏 Home、session 显示在另一 workspace 的中间态。
>
> 完成标准验证：Project target 的 canonical cwd 同时进入 embedding snapshot、typed persisted session
> overview、同 context 的相对 attachment prepare、实际 builtin `bash pwd` 执行，以及 shell
> authorization 的 `ToolAuthorizationScope::Shell` 和 preview；Projectless 首次 prompt 继续使用 managed
> scratch execution cwd，并持久化 Projectless scope 而不对外暴露 scratch project path。
>
> Gate：`coding-agent` 785 个 library tests 与严格 Clippy 通过；Desktop 251 个 tests 通过、5 个既有
> 用例保持 ignored，18 个 dependency boundary tests 全部通过，严格 Clippy、格式、CodeGraph 与
> `ast-grep outline` transaction/owner 审计通过。`coding-agent` boundary 为 64/67，仍严格保持
> DSK-600 登记的 3 个既有失败，没有新增回归。定向验收分别覆盖 context load、admission 后 path
> 删除、session create、prompt prepare、prompt start 五类失败，以及有后台 session 时的 Home
> draft/attachment rejection 迁移、snapshot/context/scope 一致性、thinking capability fallback、
> Project/Projectless scope 和 tool/authorization cwd。

工作：

- 按第 6.6 节顺序重写 New target dispatch。
- Context load 失败、path 删除、session create 失败、prompt prepare 失败、prompt start 失败分别测试。
- Snapshot/project/context 必须来自同一个 owner。
- UI rejection 可恢复 Home draft 或明确安装已创建 session，不存在模糊中间态。

完成标准：所选 cwd 同时出现在 context、session scope、tool execution 和 authorization；无项目走 Projectless。

#### DSK-613：跨项目历史 session open

> 状态：已完成。产品层新增 `CodingAgentSessionOpenTarget` 与
> `CodingAgentSessionQuery::open_target`：按 product session id 读取 durable workspace identity，Legacy
> 可恢复时先原子迁移 manifest，再返回完整 `CodingAgentWorkspaceScope + migration/availability` 证据；
> 查询不 hydrate transcript，也不向 adapter 暴露 session repository path。Projectless 因而保留真实
> `workspace_id`，不再从 list overview 的展示字段反推 scratch。
>
> Desktop `OpenSession` 现在固定执行 `open_target -> for_workspace -> context load -> session open`。
> Project 使用持久化 canonical identity，Projectless 按持久化 id 恢复或重建 managed scratch，Legacy
> 无法恢复及已删除/不可用 Project 统一返回可继续操作的 `CommandRejected(workspace_unavailable)`；失败
> 不安装半初始化 owner。新 context 只继承 Home 已冻结的 product-global session root，不继承 Home 的
> workspace 配置或可变 model/profile selection。
>
> 每个历史 session 均建立独立 `RuntimeSessionWorkspace`，即使 scope 指向同一 Project 也不共享 mutable
> context。定向验收覆盖 Project A -> B -> A 重开后的 transcript、project settings、skills 与 cwd；同一
> Project 两个 owner 的 model/profile 独立变化；当前 Projectless scratch 删除后的受管恢复；v1 Legacy
> manifest 在 context load 前迁移；deleted Project 的有界诊断及 rejection 后 runtime 可继续接收命令。
>
> Gate：`coding-agent` 786 个 library tests、API contract 14/14 与严格 Clippy 通过；Desktop 255 个 tests
> 通过、5 个既有 release 性能用例保持 ignored，18 个 dependency boundary tests、严格 Clippy、格式、
> CodeGraph 与 `ast-grep-outline` API/owner 审计通过。`coding-agent` boundary 为 64/67，仍严格保持
> DSK-600 登记的 frozen write count、session naming operation id、session method ledger 三个既有失败，
> 没有新增回归。

工作：

- OpenSession 先读 scope，再构造 context，再打开 session。
- 支持 deleted project diagnostic、Legacy migration、Projectless scratch 恢复。
- 同一个项目可有多个 session owner；可以共享不可变 catalog 数据，但不能共享可变 runtime selection。

完成标准：从 Project A 切到 B 再回 A，三者 transcript/配置/目录保持正确；目录缺失有可恢复错误。

### Phase 3：Catalog 变为用户驱动

#### DSK-630：删除所有 session catalog 自动加载

> 状态：已完成。`SessionController` 已删除 15 秒 refresh interval、deadline、schedule/take API 与
> `NativeShell` timer；启动、静置、Sidebar toggle/focus、session create/open/close、resync 及
> admission failure 均不会再隐式发出 `ListSessions`。生产代码中唯一 catalog request caller 是
> `SessionsPaneEvent::Refresh`，重复点击继续由 typed command ledger 做 pending 去重；失败仍释放 pending
> 并显示错误 toast，成功只替换 catalog，不再生成 “Loaded N sessions” 全局提示，也不安排后续 timer。
>
> Create（包括 Home 首次 prompt 创建）、rename、close 改为本地增量 mutation：新建项置于 recent
> 首位并遵守 `MAX_DESKTOP_SESSION_CATALOG` 上限，rename 原位更新，close 直接删除；open 与 resync
> 只安装各自 projection/owner，不触发全量查询。Projectless/Legacy 本地新建项不把受管 scratch cwd
> 暴露成普通项目路径。删除自动 request 后，narrow Sidebar 打开改为显式通知 `SessionsPane` 更新
> view-model，保持 drawer close control 与焦点行为而不访问 runtime。
>
> 完成标准验证：instrumented GPUI 回归在启动后、静置 60 秒和打开 Sessions overlay 后均观测到
> 0 个 command；模拟点击 overflow 中的 Refresh 后恰好观测到 1 个 `ListSessions`，pending 期间再次
> 请求仍为 0；成功响应后再静置 60 秒仍为 0。另有定向测试覆盖刷新失败不重试、首次 prompt/create
> 本地插入、open/resync 零查询、close 本地删除、catalog recent order/rename/remove/bounds，以及
> responsive drawer 在纯本地 pane 通知下保持可用。
>
> Gate：Desktop library 257 个 tests 通过、5 个既有 release 性能用例保持 ignored，18 个 dependency
> boundary tests 与严格 Clippy 通过；`coding-agent` 786 个 library tests、API contract 14/14 与严格
> Clippy 通过；格式、diff、禁止符号扫描、CodeGraph call-path 与 `ast-grep-outline` 变更后结构审计通过。
> `coding-agent` boundary 为 64/67，仍严格保持 DSK-600 登记的 frozen write count、session naming
> operation id、session method ledger 三个既有失败，没有新增回归。

删除：

- `SESSION_CATALOG_REFRESH_INTERVAL`；
- `refresh_deadline`；
- `schedule_refresh` / `take_scheduled_refresh`；
- `schedule_session_catalog_refresh`；
- NativeShell 初始化自动 request；
- success/failure timer；
- panel toggle/focus 自动 request；
- create/open/close/resync 的完整 request；
- Loaded N sessions 成功 toast。

保留：

- 显式 Refresh event；
- pending 去重；
- 错误反馈；
- create/rename/close 的本地增量 catalog mutation。

完成标准：启动后和静置测试中 ListSessions command count 为 0；只有模拟点击 refresh 后变为 1。

#### DSK-631：ProjectCatalogController

> 状态：已完成。`SessionController` 已重构为 `ProjectCatalogController`；Desktop catalog DTO 不再
> 保存或推断展示 cwd，而是直接透传产品层 `CodingAgentWorkspaceOverview` +
> `CodingAgentWorkspaceMigration`，同时删除 `CodingAgentSessionOverview.cwd` 兼容字段。Controller
> 严格按 product-owned `group_id` 聚合项目：同名但 identity/path 不同的项目保持独立，Projectless
> 与 Legacy 使用各自 typed kind，不会把受管 scratch 伪装成项目。
>
> Catalog 现在显式维护 `NotLoaded / Loading / Ready / Error / Stale` 状态。首次刷新失败进入 Error；
> 已有服务端快照或本地增量数据时刷新失败进入 Stale；runtime admission/rejection/failure/stop 都会
> 结束 Loading，且 rejection 状态只保留安全 command kind/code，不复制 runtime 私有 message。
> 成功快照保留服务端 recent order，项目顺序由各组首次出现位置决定，组内顺序保持同一 recent
> order；create/rename/remove 继续本地增量更新并遵守目录上限与 omitted 计数。
>
> 搜索统一匹配 workspace identity/name/path 与 session id/name/updated time；匹配结果临时展开但不
> 修改 disclosure state。折叠 group id 集合与 catalog 快照完全分离，包含空快照在内的 refresh 都
> 不会重置用户折叠偏好。`SessionsPaneViewModel` 已消费 typed project groups 与 catalog state；本阶段
> 为保持 Phase 4/视觉任务边界仍将 groups 扁平渲染，真实 project row/tree 与 toggle event 留给后续
> pane 重构。
>
> 完成标准验证：4 个 Controller 纯逻辑测试覆盖同名项目、Projectless、Legacy、empty、omitted、
> workspace/session search、折叠状态跨空刷新、server/group recent order、insert/rename/remove 与
> bounds；纯逻辑与 GPUI/runtime 回归共同覆盖 NotLoaded→Loading→Ready、初次失败 Error、带旧数据
> 失败 Stale、admission/rejection 错误收敛，以及 typed workspace 从产品层到 Desktop 的端到端映射。
>
> Gate：Desktop library 261 个 tests 通过、5 个既有 release 性能用例保持 ignored，18 个 dependency
> boundary tests 与严格 Clippy 通过；`coding-agent` 786 个 library tests、API contract 14/14 与严格
> Clippy 通过；格式、diff、旧符号扫描、CodeGraph call-path 与 `ast-grep-outline` 变更后结构审计通过。
> `coding-agent` boundary 为 64/67，仍严格保持 DSK-600 登记的 frozen write count、session naming
> operation id、session method ledger 三个既有失败，没有新增回归。

工作：

- SessionController 重命名/重构为 ProjectCatalogController。
- 接收 product-owned scope/group id，生成稳定 project groups。
- 维护 loaded/not-loaded/loading/ready/error/stale 状态。
- 增量 insert/update/remove；保持 server recent order 与 group recent order。
- 项目折叠状态与 catalog 数据分离，避免 refresh 重置用户展开状态。

完成标准：同名项目、Projectless、Legacy、empty、omitted、search、增量变化均有纯逻辑测试。

### Phase 4：Shell 与 pane 结构

#### DSK-640：统一三栏 ShellLayout

> 状态：已完成。`ShellLayout` 已删除 `idle + sessions/workspace/context` 特例字段，改为显式持有
> `viewport / sidebar / center / center_header / center_body / inspector` bounds。Home 与 Conversation
> 均调用同一个 `resolve_with_panel_widths`，`NativeShell::resolve_layout` 不再读取 projection 或切换
> 几何算法。Center header 使用同一 `CENTER_HEADER_HEIGHT=48` 同时驱动纯 geometry 与 GPUI 实际高度；
> center body 成为独立容器，为 DSK-641 的非模态 drawer host 提供稳定挂载边界。
>
> Fresh `DesktopPreferences` 现在是 Sidebar open、Inspector closed；布局不再针对 Home 写死 panel
> visibility，因此已有或用户后续保存的 Inspector open/close preference 会原样生效。Sidebar/Inspector
> resize 仍使用既有持久化宽度、min/max clamp，并继续优先隐藏 Inspector、再隐藏 Sidebar，从而始终
> 保证 `MIN_CONVERSATION_WIDTH`。
>
> `FocusState` 已从 Sessions/Conversation/Context 粗粒度 owner 拆为
> `CenterHeader → Sidebar → CenterBody → Composer → Inspector` 稳定顺序；GPUI Header 与 center body
> 分别持有独立 focus handle，隐藏的响应式 panel 自动从 focus order 排除，overlay trap/restore 保持
> 既有语义。当前 narrow Sidebar/Inspector 仍由 root `OverlayHost` 承载，其 drawer/modal 状态拆分明确
> 留给 DSK-641，本任务未提前改变该边界。
>
> 完成标准验证：纯 geometry 测试覆盖 session 的 wide/medium/narrow、精确 breakpoint、可配置宽度
> clamp、center header/body 高度分割，以及 fresh Home 的 wide/medium/narrow Sidebar/Inspector bounds；
> GPUI 回归验证 Home Sidebar 在 dockable 宽度可见、fresh Inspector 关闭、显式 Inspector preference
> 在 Home 不被覆盖、实际 center header/body 坐标一致，以及 FocusNext 的 Composer→Inspector→Header
> 顺序不改变 panel geometry。
>
> Gate：Desktop library 264 个 tests 通过、5 个既有 release 性能用例保持 ignored，19 个 dependency
> boundary tests 与严格 all-target Clippy 通过；格式、diff、旧 idle layout/FocusTarget 符号扫描、
> CodeGraph call-path 与 `ast-grep-outline` 变更后结构审计通过。

工作：

- 删除或改写 `resolve_idle`；Home 与 Conversation 走同一列解析。
- `ShellLayout` 明确 sidebar/center/inspector bounds 与 center header/body bounds。
- Home 默认 Inspector closed，不写死覆盖用户后续 preference。
- 更新 FocusState：center header controls、Sidebar、center body、Composer、Inspector 顺序稳定。
- 保留 panel resize、min/max 与 conversation minimum width。

完成标准：wide/medium/narrow 纯 geometry 测试覆盖 Home 与 session；Home sidebar 在 dockable 宽度可见。

#### DSK-641：拆分 RootModalHost 与 CenterDrawerHost

> 状态：已完成。原 application-root `OverlayHost` 已收敛并重命名为 `RootModalHost`，只持有
> Command Palette、Authorization 与 Full Message 三类真正 modal；Sessions/Inspector Pane 由新的
> typed `CenterDrawerHost` 持有。Home 与 Conversation 的 `center-body` 都建立 `relative` 挂载边界，
> drawer 的 top/bottom/right/left 只相对 center body 计算，因此不会覆盖 center header。
>
> `NativeShell` 已删除 `DesktopOverlayKind`、`active_overlay` 与两组 narrow-open bool，改为独立的
> `active_modal: Option<DesktopModalKind>`、`active_drawer: Option<CenterDrawerKind>` 和
> `drawer_restore_focus`。Modal 继续通过 `FocusState::open_modal/close_modal` 严格 trap/restore；drawer
> 保留逻辑 root focus owner，切换另一侧 drawer 不覆盖原恢复点，点击外部、Escape 或对应 toggle
> 均关闭并恢复可见 owner。Drawer key context 只处理 Escape，不再把 Tab/Shift-Tab 路由到 modal
> focus trap；`root_action_blocked_by_modal` 只读取 `active_modal`，drawer 打开时 header selectors 与
> workspace actions 继续可操作。
>
> Authorization projection 出现时会先无恢复地关闭 active drawer，再进入 root modal；授权消失后
> modal 恢复 drawer 打开前的 root owner。GPUI 回归覆盖 medium Inspector drawer 与 center body/header
> 坐标、Model selector 在 drawer 打开时可点击、点击 drawer 外关闭、narrow Sessions drawer、
> conversation geometry/scroll 不变，以及 Authorization 实际挂载、关闭 drawer、modal focus trap 和
> 最终恢复 Composer。源码 ownership guard 与 dependency boundary 同时约束两个宿主不能重新混合
> Pane、projection、command ledger 或 authorization 职责。
>
> Gate：Desktop library 265 个 tests 通过、5 个既有 release 性能用例保持 ignored，19 个 dependency
> boundary tests 与严格 all-target Clippy 通过；格式、diff、旧 OverlayHost/active_overlay/narrow state
> 符号扫描、CodeGraph call-path 与 `ast-grep-outline` 变更后结构审计通过。

工作：

- Narrow Sidebar/Inspector 从 application-root OverlayHost 移出。
- Center body 建立 relative drawer host；header 位于其外。
- 分离 `active_modal` 与 `active_drawer` 状态/focus restore。
- Drawer 不触发 `root_action_blocked_by_overlay`；Modal 保持严格阻断。

完成标准：Inspector drawer 打开时 selector 可点击；authorization 出现时 drawer 被关闭并正确恢复焦点。

#### DSK-642：HomePane 精简与 Skills surface

> 状态：已完成。`HomePane` 已删除整个 ViewModel/EventEmitter 边界，不再接收 model、thinking、workspace、
> recent sessions、global skills、catalog/pending 等任何产品 DTO，也不再发出 open session 事件。当前
> Home 只渲染无状态的 `evo` 品牌锚点、确认后的主/辅助文案；Composer 仍由 composition root 独立挂载，
> 因而 Home render 不会查询 catalog、创建 projection、调用 runtime 或触碰 durable session。
>
> 新增独立 `SkillsPane` 与 `SkillsPaneViewModel`，只接收 bounded
> `Arc<[CodingAgentResourceCommand]>` 并在 center surface 展示完整全局 skills 列表。`SessionsPane` 已删除
> skills DTO、截断常量和内联 skill cards，只保留固定一级 `Skills` action；Skills surface 不挂载 Composer，
> 可从 Home 或已有 Conversation 打开，底层 session owner 保持不变。
>
> 新增 `CenterNavigationTarget::{NewConversation, Skills, Session}` typed contract，并以
> `SessionsPaneEvent::Navigate` 统一三个入口；`NativeShell::navigate_center` 是唯一 surface/session 路由点。
> 独立 `CenterSurface::{Primary, Skills}` 只描述展示 surface，不复制 projection/session id；从 Skills 点击
> 当前 session 会纯本地恢复 Conversation，点击其他 session 才进入既有 typed `OpenSession` 流程，点击
> New conversation 则恢复/创建仅用于 draft 的 Home workspace，但不发 runtime command 或创建 durable
> session。
>
> GPUI 定向回归覆盖 Home/Skills/Conversation 三个 surface、Sidebar 与 drawer 的固定 Skills action、Skills
> 列表只在独立 surface 挂载、Skills 不挂载 Composer、当前 session 纯本地返回，以及 Skills/Home 导航
> 全程零 runtime command。Dependency boundary 进一步禁止 Home 引入 catalog、projection、command ledger、
> NativeShell back-reference 或事件，禁止 SkillsPane 越过 bounded DTO，并禁止 SessionsPane 重新持有 skills。
>
> Gate：Desktop library 265 个 tests 通过、5 个既有 release 性能用例保持 ignored，19 个 dependency
> boundary tests 与严格 all-target/all-features Clippy 通过；格式、diff、CodeGraph typed call-path 与
> `ast-grep-outline` 变更后 Home/Skills/navigation owner 审计通过。

工作：

- HomePane DTO/render 删除 recent_sessions/global_skills/catalog pending。
- 新建独立 SkillsPane/route，展示全局 skills；不把 skills 逻辑留在 SessionsPane。
- New conversation/Skills/Project session 统一成 typed navigation event。
- Home 不创建 projection/runtime/session。

完成标准：dependency boundary 保持 Pane 不能任意读取 NativeShell；Home 无 catalog dependency。

### Phase 5：Composer project selector

#### VUI-401：ProjectDirectory control

> 状态：已完成。`DesktopIcon::ProjectDirectory` 已集中映射 Lucide `Folder`，新的
> `DesktopProjectDirectoryControl` 作为共享 compact control 持有 folder icon、label、tooltip、ARIA、
> 36 px hit target 与最大 280 px 宽度。控件显式区分 `Editable`、`Locked`、`Pending`：editable 保留
> selector caret 与 Button 键盘语义，locked/pending 以结构和 `Fixed`/`Pending` 文本双重表达状态，
> 因而不依赖颜色；长路径使用 Unicode-safe ellipsis，完整中文路径仍保留在 tooltip 与 accessibility label。
>
> Composer bottom-left 已改为 attachment + project selector，左侧容器可收缩、右侧 action 固定不缩。
> Home draft 显示 `无项目`，已有 session 从 typed workspace scope 显示不可变目录，提交 admission 与等待
> runtime 启动期间进入 pending。wide/narrow GPUI 回归同时约束 attachment、project selector、submit 的顺序、
> selector 宽度上限以及三个 action 的最小 hit target；editable 的实际 choose/clear event 与 directory-only
> picker 仍由下一项 DSK-650 接入，本项不在视觉 primitive 中提前持有 runtime/picker authority。
>
> Gate：Desktop library 268 个 tests 通过、5 个既有 release 性能用例保持 ignored，19 个 dependency
> boundary tests 与严格 all-target/all-features Clippy 通过；格式与 diff 检查通过。实际 visual review 已检查
> wide、narrow、idle 与 no-color 输出，确认中文长路径压缩、`无项目`、`Fixed` 和 hit target；未提前安装
> golden，因为最终 Home 品牌与 idle 构图仍由 VUI-411/VUI-412 收敛。CodeGraph 变更前 call-path 与
> `ast-grep-outline` 变更后结构审计确认 shared control、Composer DTO 与 shell scope derivation 的职责边界。

工作：

- `DesktopIcon` 新增语义项 `ProjectDirectory`，集中映射 Lucide folder asset。
- Composer bottom-left 变为 attachment + project selector。
- 新建可复用 compact selector/pill，支持 editable/locked/pending、ellipsis、tooltip、aria。
- Home 显示 `无项目`；existing session 显示 locked scope。
- narrow 下 project selector 先压缩 label，再保证 attachment 与 submit hit target 不缩水。

完成标准：键盘 Tab/Enter/Space、tooltip、no-color、长路径、中文文本、窄宽度均可用。

#### DSK-650：Project picker state 与事件

> 状态：已完成。`ComposerPaneEvent` 已新增 typed `ChooseProjectDirectory` / `ClearProjectDirectory`，
> editable control 使用同一个 keyboard-capable PopupMenu 提供“选择项目目录…”与“无项目”；Pane 只发事件，
> 不持有 `prompt_for_paths`、workspace selection 或 runtime target。`NativeShell` 是唯一 picker owner，调用
> `files=false`、`directories=true`、`multiple=false` 的原生 path prompt；取消保持原 selection，打开失败、
> 空结果和异常多选均给出 bounded notice，且不会覆盖既有选择。
>
> `SessionWorkspace` 已新增显式 `draft_workspace_selection`。Home 切换到历史 session 后仍完整保留 composer
> draft 与 project selection；首次 prompt 建立 session 后，下一次 New conversation 使用新的 managed
> Projectless selection。Desktop bootstrap 现在无论显式启动 cwd 与否都会准备并持久化稳定 scratch workspace
> identity，因此 clear/reset 不再从当前 session snapshot 或裸启动 cwd 反推 `无项目`。Home metadata template
> 与 per-workspace draft 分离保存，避免首个 Home 被 rekey 为 session 后丢失新会话的 Home context 基线。
>
> `SessionWorkspace::prompt_target` 在 admission 时 clone 当前 selection 到 owned `DesktopPromptTarget::New`；
> admission pending、等待 start 和 existing session 都在 shell guard 层拒绝后续 choose/clear，不只依赖 disabled
> UI。同步 validation 失败会完成 command ledger 并保留精确 draft/selection；已选目录随后被删除的回归确认不会
> 发出 runtime prompt。收到首个 `PromptAcceptedWithSession` 后 UI 立即从 pending 进入 durable locked scope。
>
> Gate：新增 GPUI 回归覆盖 directory-only options、中文目录、替换、取消、清除、picker failure、异常多选、
> 零 runtime command、历史 session 往返、immutable admission、pending mutation guard、目录删除后提交、成功
> 锁定和新 Projectless draft。Desktop library 274 个 tests 通过、5 个既有 release 性能用例保持 ignored，
> 19 个 dependency boundary tests 与严格 all-target/all-features Clippy 通过；格式与 diff 检查通过。完整
> wide/medium/narrow、idle、authorization、keyboard-focus、reduced-motion、no-color visual review 已生成，
> 并抽查 wide/narrow idle 与 no-color；golden 仍留给 VUI-411/VUI-412 的最终品牌构图统一安装。CodeGraph
> 变更前 call-path 与 `ast-grep-outline` 变更后 owner 审计确认 Pane、shared control、Home draft 和 shell picker
> authority 没有重新混合。

工作：

- ComposerPaneEvent 增加 choose/clear；Pane 不直接开 runtime command。
- NativeShell 调用 directory-only picker，处理 cancel/error。
- Home SessionWorkspace 保存 selection；切换历史 session 再返回 Home 时保留 draft。
- New conversation 建立全新 Projectless draft；成功 admission 后 scope 锁定。
- 发送时 clone 一份 immutable selection 进入 command，随后 UI disabled。

完成标准：picker options、取消、替换、清除、删除后提交、失败保留、成功锁定有 GPUI test。

### Phase 6：Sidebar Projects 与 Home 品牌

#### VUI-410：Sidebar 导航与 Projects 树

工作：

- SidebarHeader 放 compact Evo mark。
- 固定 New conversation 与 Skills 一级 action。
- Projects header 放 refresh、状态与可选 search。
- Project disclosure row + nested session rows；支持 current/running/error/available 状态。
- Session rename/close 继续存在，但不抢占主行宽度。
- unloaded/loading/error/empty/omitted/Legacy 有明确局部状态。

完成标准：宽 sidebar、最小 sidebar、drawer、键盘导航、屏幕阅读顺序和 4 个并存 runtime 状态通过。

#### VUI-411：Evo Loop 矢量资产

交付：

- full wordmark SVG/GPUI asset；
- compact mark；
- dark/light/monochrome token mapping；
- 16/24/32/200/360px visual fixtures；
- 资产设计说明与 path ownership。

完成标准：无 raster fallback、无嵌入文字字体依赖、no-color 仍可辨识、缩放无模糊。

#### VUI-412：Home hero

工作：

- 使用 logo、确认文案与新 placeholder。
- Hero 与 composer 在不同高度下保持合理间距；短窗口优先保证 composer 可见。
- 删除旧 Recent/Skills 两列和重复状态行。
- Home center content 不因 Sidebar refresh 重排。

完成标准：wide/medium/narrow idle golden 重新 review；Home 首屏视觉重心明确。

### Phase 7：Header selector 与 Inspector

#### VUI-420：Provider-grouped model menu

工作：

- ViewModel 预分组 configured text models。
- Popup menu 使用 provider label + separator；当前项 check。
- 处理 zero configured/current auth lost/long model names/scroll。
- 去掉逐项 `provider · id · not configured` 噪音。

完成标准：菜单中没有普通未配置项；provider group 顺序稳定；选择结果仍走 typed runtime selection。

#### VUI-421：Capability-driven Thinking menu

工作：

- 非 reasoning 隐藏 selector并重新分配 header 空间。
- reasoning 显示 Auto/Off（可用时）/explicit levels。
- 模型切换与 fallback 原子更新 header、preference 和 runtime context。
- fallback 使用局部辅助提示，不弹全局 toast。

完成标准：每个 fixture model 菜单精确匹配 capability；非法组合无法通过 UI 或 runtime admission。

#### VUI-422：Inspector dock/drawer 行为

工作：

- Docked Inspector 保持独立 header/resize。
- Drawer 只覆盖 center body；不覆盖 selector/toggle。
- 中间 header 最右 toggle 是主控制；drawer 自身 close 可作为窄屏辅助。
- drawer 外点击/Escape/切换另一 drawer 行为一致。

完成标准：medium/narrow 自动化 hit-test 证明 Profile dropdown 在 Inspector 打开时可用。

### Phase 8：清理、文档与发布

#### DSK-690：删除旧路径和源码文本断言债务

工作：

- 删除已无 caller 的 Home recent/skill DTO、session refresh timer、旧 overlay branches、固定 thinking menu builder。
- 更新 dependency boundary，使用行为/模块边界断言替代脆弱的源码字符串断言。
- 更新 `docs/architecture.md` 的 per-session context、workspace scope 与 Desktop shell 图。
- 更新 visual REVIEW、用户可见 keyboard shortcut 和 accessibility 文档。

完成标准：`rg` 不再找到旧 timer、Home Recent/Global sections、NarrowContext root overlay、固定 ALL menu 渲染。

## 九、测试与验收矩阵

### 9.1 `coding-agent`

| 不变量 | 测试 |
| --- | --- |
| Project scope 持久化 | create/open/overview roundtrip |
| Projectless 不泄漏 scratch 为项目 | public DTO snapshot |
| 旧 session 可读 | legacy manifest/event fixtures |
| 历史 scratch 迁移 | known scratch root fixture |
| 删除目录仍可列出 | overview + open unavailable diagnostic |
| 两项目资源隔离 | settings/AGENTS/skills/profile fixture |
| session root 与 cwd 解耦 | 两 cwd 同一 global session root |
| thinking capability 正确 | representative model table tests |
| 非法 thinking fallback | sanitize outcome tests |

### 9.2 Desktop runtime

| 不变量 | 测试 |
| --- | --- |
| 启动不 ListSessions | command harness count = 0 |
| 静置不自动刷新 | virtual timer/parked executor 后仍为 0 |
| 手动刷新只发一次 | click + pending dedupe |
| New target 显式 scope | protocol admission |
| Existing 禁止 cwd override | 类型/compile path + admission test |
| 所选 cwd 贯穿 agent | snapshot/session/tool/auth assertions |
| Projectless global-only | scratch local `.evo` 不生效 |
| 两项目同时运行 | dual active prompt event routing |
| Context 不串 model/profile/resources | per-owner snapshots |
| 打开历史 session 重建 context | A→B→A test |
| cwd 删除 | typed error, catalog retained |
| 首轮失败保留 draft | shell reconciliation |

### 9.3 Desktop pure layout/state

- Home wide sidebar visible、Inspector default closed。
- 用户 preference close 后 Home 尊重。
- Medium Inspector drawer bounds 从 center body 顶部开始。
- Narrow 同时最多一个 drawer。
- Drawer 与 Modal focus restore 不串。
- Project grouping：同名路径、Projectless、Legacy、omitted、search。
- Refresh 不重置 disclosure state。
- 长路径 compact label 与完整 accessible label。
- 模型 provider groups 与 thinking options snapshot。

### 9.4 GPUI interaction

- Composer folder control 可点击、Tab、Enter/Space。
- Picker `files=false/directories=true/multiple=false`。
- Cancel/clear/replace/locked/pending/error。
- Inspector drawer 打开时 Model/Thinking/Profile dropdown 可点击。
- Sidebar/Inspector toggles 在 docked/drawer 状态图标与 selected 状态正确。
- New conversation、Skills、Project、Session 的 focus/route 正确。
- Catalog refresh 成功无 toast，失败有局部 error。

### 9.5 Visual golden

必须 review：

- wide/medium/narrow idle；
- wide/medium/narrow session；
- medium/narrow Sidebar drawer；
- medium/narrow Inspector drawer；
- provider model menu；
- reasoning/non-reasoning thinking menu；
- Home Projectless/Project selected/long path；
- project catalog unloaded/loading/ready/error/empty；
- authorization；
- keyboard focus；
- no-color；
- reduced-motion。

### 9.6 Gate 命令

每个任务至少：

```bash
cargo fmt --all -- --check
git diff --check
```

`CAG-*`：

```bash
cargo test -p coding-agent
cargo clippy -p coding-agent --all-targets -- -D warnings
cargo test -p cli
cargo test -p tui
```

`DSK-*` / `VUI-*`：

```bash
cargo test -p desktop
cargo test -p desktop --test dependency_boundary
cargo clippy -p desktop --all-targets -- -D warnings
```

布局/Render：

```bash
scripts/desktop-visual-golden.sh --review
scripts/desktop-visual-golden.sh --update --review-note FILE
scripts/desktop-visual-golden.sh
```

Runtime/channel/conversation ownership：

```bash
scripts/desktop-native-perf-gate.sh
scripts/desktop-perf-gate.sh
```

物理输入验收继续遵循现有 click-to-photon 流程，不用 GPUI 内部时间冒充 photon 结果。

## 十、提交策略

推荐提交序列：

1. `feat(coding-agent): add workspace scope contracts`
2. `feat(coding-agent): persist and migrate workspace scopes`
3. `feat(coding-agent): expose model thinking capabilities`
4. `feat(coding-agent): build contexts from workspace selections`
5. `refactor(desktop-runtime): type new and existing prompt targets`
6. `refactor(desktop-runtime): own one embedding context per session`
7. `feat(desktop-runtime): open sessions across project scopes`
8. `refactor(desktop): make project catalog user-driven`
9. `refactor(desktop): unify home and session three-column layout`
10. `refactor(desktop): split center drawers from root modals`
11. `feat(desktop): add composer project directory selection`
12. `feat(desktop): group history by projects and add skills route`
13. `feat(desktop): add Evo Loop vector brand and home hero`
14. `feat(desktop): group configured models by provider`
15. `feat(desktop): derive thinking choices from model capability`
16. `feat(desktop): keep inspector below the center header`
17. `test(desktop): review multi-project visual and performance gates`
18. `docs: record multi-project desktop architecture`

规则：

- `coding-agent` 契约与 Desktop 消费分开提交；每个产品提交自身可通过 CLI/TUI。
- Schema migration 单独提交，便于审查和回滚。
- Runtime owner 重构提交不混入视觉变化。
- VUI 提交必须附 reviewed golden note。
- 不保留双实现 feature flag；新路径验证完成后直接删除旧共享 context/timer/overlay 路径。
- 不为减少 diff 而让 Desktop 继续解释 cwd/provider compatibility。

## 十一、风险与停止条件

遇到以下情况暂停后续 VUI，先修产品/runtime：

- 同一 session 的 persisted scope、context cwd、tool cwd、authorization cwd 任意不一致；
- 两个项目 session 共享可变 EmbeddingContext 或 selection；
- Projectless 加载了 scratch/project-local `.evo`；
- migration 会让旧 session 消失或不可恢复地改错 cwd；
- New prompt context load 失败后仍产生 durable session；
- 后台 session 事件错误进入前台 projection；
- 移除 catalog timer 后某条 UI 路径又偷偷发 ListSessions；
- Drawer 为了方便重新覆盖 center header；
- Thinking UI 展示产品 capability 未声明的 level；
- Visual golden 更新掩盖 hit target、焦点或无颜色语义退化。

以下不构成停止理由，可以直接做激进重构：

- `native_shell.rs` 大面积移动代码；
- 内部 DTO/enum/command variant 破坏性修改；
- 删除旧 timer、旧 overlay host 分支、旧 Home sections；
- manifest schema bump（前提是 reader 有迁移测试）；
- golden 大幅变化（前提是逐张 review）；
- 为建立 per-session owner 拆分 runtime driver/dispatch 模块。

## 十二、明确不在本轮追加的范围

为保持计划闭合，以下能力不随手加入：

- Git branch/worktree 管理 UI；本轮只按 workspace scope 分组。
- 项目 relocate；目录删除时先只读诊断，不允许静默重绑定。
- Session delete；close 与 delete 仍是不同产品操作。
- Inspector 内直接编辑文件。
- Logo 动画、启动动画或生成式 raster 品牌图。
- 自动扫描磁盘发现项目。
- 自动刷新 session catalog、文件系统 watcher 或后台 polling。
- 在 Desktop 实现完整国际化系统；`无项目` 作为已确认产品文本先落地，其余延续现有语言。

## 十三、最终删除审计

合并前执行代码与行为审计，确认以下旧概念已经消失：

- Home-specific “no panels” layout；
- Home Recent Sessions 与 Global Skills columns；
- 单一 `RuntimeState.context` 服务所有 session；
- `SubmitPrompt.session_id: Option` 表达 New/Existing；
- 15 秒 catalog refresh deadline/timer；
- panel open/focus 自动 list sessions；
- `SessionsListed` 成功 toast；
- model menu 中全部未配置 disabled entries；
- UI 固定遍历全部 thinking levels；
- Narrow Inspector 挂在 root `size_full` overlay；
- Desktop 用 cwd 字符串猜 Projectless；
- 已有 session 可变 cwd 的任何入口。

只有当删除审计、测试矩阵、reviewed golden 与文档同步全部完成，本计划才可标记完成。
