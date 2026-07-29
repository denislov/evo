# Desktop 待机界面、多会话工作台与面板重排计划

> 状态：执行中（VUI-301 自动验收完成；CAG-101、CAG-102、CAG-103、CAG-104、CAG-105、CAG-106、DSK-501、DSK-502、DSK-503、DSK-504、DSK-511、DSK-512、DSK-513、VUI-302 实现完成）
> 基线：`main`（`32fdb25 docs(desktop): record composer visual lane completion`）
> 更新日期：2026-07-29
> 前置文档：[`desktop架构.md`](./desktop架构.md)（`DSK-*` / `VUI-*` 已完成批次）
> 原则：产品权威留在 `coding-agent`，Desktop 保持 adapter；增量 API 不破坏 CLI/TUI；每批可独立回退

## 一、目标

本计划解决五组已核实的问题：

1. **启动即建 session。** `crates/desktop/src/runtime/driver.rs:388` 无条件
   `context.create_session().await`，且 `crates/coding-agent/src/session/repository.rs:314`
   在持久化开启时立即 `fs::create_dir` + manifest + 空 event log + `sync_directory`。
   用户打开窗口再关闭，磁盘上就多一个空 session，并出现在 `ListSessions` 结果里。
2. **无法在没有会话的情况下呈现产品事实。** 待机界面所需的模型目录、设置、会话列表
   在存储层都不依赖 cwd 或 session，但公开入口都挂在 `CodingAgentEmbeddingContext` 上。
3. **单会话运行时。** Desktop 一次只能持有一个 `RuntimeState`，切换 session 会关闭上一个，
   用户无法让一个任务在后台继续跑。
4. **面板与控件布局不符合使用习惯。** 模型/Profile 是「Next」循环而非列表；thinking level
   埋在输入框且是 per-prompt 语义；底栏承载低价值信息；session 列表显示原始 id。
5. **Conversation 折叠展开导致整列表抖动。** `crates/desktop/src/conversation/layout.rs:120-127`
   在 `details_expanded` 变化时丢弃已测量高度，回退到估算值，产生两帧跳变；
   `layout.rs:256` 的锚定只在 follow-latest 被暂停时生效。

成功标准：

- 启动不产生任何 session 目录；用户不输入就退出时磁盘无残留；
- 待机界面完全由只读 API 驱动，不启动任何会话运行时；
- 同时最多 4 个会话运行时并存，后台会话持续投影、前台会话独占渲染；
- 模型、Profile、thinking level 通过顶栏下拉列表选择，thinking level 为 session 级状态；
- session 列表显示模型生成的名称，失败时显示「未命名」；
- 展开折叠消息时，被展开行之前的内容屏幕位置不变，之后的内容向下推移；
- 现有 command id 对账、ProductEvent 顺序、恢复、关闭与渲染性能语义不变。

## 二、已确认的架构事实

以下事实经代码核实，是本计划的依据，实施时不得凭印象推翻。

### 2.1 三层，不是「运行时 / 非运行时」两层

| 层 | 入口 | 实质 | 绑 cwd | 副作用 |
| --- | --- | --- | --- | --- |
| L1 项目上下文 | `CodingAgentEmbeddingContext::load(cwd)` | 同步的配置 + 资源解析，无线程、无网络、无写盘（`app/embedding.rs:270-299`） | 是 | 无 |
| L2 会话运行时 | `context.create_session()` / `open_session()` | event hub、client projection、authorization service、session coordinator，**写盘** | 继承 L1 | 有 |
| L3 客户端连接 | `session.connect(client_id)` | 投影订阅与提交 | 继承 | 无 |

Desktop 启动时同时做了 L1 + L2。本计划移除启动期的 L2，保留 L1。

### 2.2 session 存储本来就不绑 cwd

`crates/coding-agent/src/app/session.rs:31`：

```rust
pub fn resolve_session_dir(
    _cwd: &Path,                      // 参数被完全忽略
    requested_session_dir: Option<&str>,
    runtime_session_dir: Option<&Path>,
) -> Result<PathBuf, ApplicationError>
```

优先级为 `EVO_SESSION_DIR` > `EVO_DIR/sessions` > `~/.evo/sessions`（配合
`crates/coding-agent/src/config/paths.rs:23`）。每条 `CodingAgentSessionChoice`
自带 `cwd: String` 作为展示元数据（`app/session.rs:284`）。跨项目列出会话在存储层已经成立。

### 2.3 只读会话查询已存在，仅入口受限

`CodingAgentSessionQuery`（已在 `api::embedding` 导出）提供 `catalog()`、
`snapshot(id)`、`tree(id)`、`clone_session(id)`、`export_html(id, path)`，全部走
`CodingAgentSession::hydrate()`，不构造 runtime host、不写盘。

构造器只有两个：`disabled()`（`pub const`）与 `from_run_options()`（`pub(crate)`），
唯一公开路径是 `context.session_query()`（`app/embedding.rs:436`）。

`catalog_internal`（`app/session.rs:429-434`）对每一条会话调用 `hydrate()`，即读取完整
event log，上限 `MAX_SESSION_QUERY_CHOICES = 256`（`app/session.rs:16`）。

### 2.4 项目上下文支持整体替换

`app/embedding.rs:312-318`：

```rust
fn reload_local_resources_internal(&mut self) -> ... {
    let replacement = Self::load_internal(self.options.clone())?;
    *self = replacement;
}
```

因此「换 cwd 重建上下文」是既有模式，不需要新机制。

### 2.5 会话创建已支持调用方指定 id

`crates/coding-agent/src/session/service.rs:234-237`：

```rust
let session_id = match options.session_id() {
    Some(session_id) => normalize_session_id(session_id, "session id")?,
    None => ids.next_session_id(),
};
```

`CodingAgentSessionOptions::with_session_id` 是 `pub`。缺口仅在
`CodingAgentEmbeddingContext::create_session()` 未透出该参数。

### 2.6 manifest 没有会话名称字段

`crates/coding-agent/src/session/manifest.rs:14-29` 字段为 `schema` / `version` /
`session_id` / `created_at` / `updated_at` / `active_branch_id` / `active_leaf_id` /
`default_agent_profile_id` / `event_log` / `outbox_log`。

`CodingAgentSessionSummary`（`runtime/facade/context.rs:210-216`）与
`CodingAgentSessionChoice` 同样没有名称。`SetSessionTreeLabel`
（`runtime/session_coordinator.rs:428`）是给**树节点/分支**打标签，不是会话名称。

**会话名称是本计划中唯一需要改动持久化 schema 的能力。**

### 2.7 模型生成摘要的 operation 已存在

`crates/coding-agent/src/operations/branch_summary/` 是一条完整的「调用模型生成摘要并写回
session」的 operation。自动命名应复用它的基础设施，而不是从零实现。

### 2.8 附件能力在产品侧已具备

- `CodingAgentPromptImage::new(data, mime_type)`（`app/interactive.rs:117-130`），
  受 `MAX_INPUT_IMAGE_ENCODED_TOTAL_BYTES` 约束；
- `MAX_INPUT_IMAGES`、`block_images`、`auto_resize_images`（`app/prompt_input.rs:45-62,219`）；
- `append_file_reference`（`app/prompt_input.rs:169`）已实现「路径 → 文本引用 + 图片附件」；
- 模型能力位 `supports_images` 在 `CodingAgentModelChoice` 中。

缺口仅在 Desktop 传输层：`DesktopRuntimeCommand::SubmitPrompt` 目前只带
`prompt: String` 与 `thinking_level`。

### 2.9 运行时无全局可变单例

`crates/coding-agent/src/runtime/` 与 `src/services/` 中不存在全局可变状态
（仅 `services/redaction.rs` 的只读 regex `OnceLock`）。每个 `CodingAgentSession`
自带完整 `runtime_host`，client 容量限制是每会话独立的 registry。多实例并存结构上成立。

### 2.10 Desktop 侧已有一半的 per-session 结构

`crates/desktop/src/app/native_shell.rs` 中 `drafts`、`inspector_session_sections`、
`composer_running_modes` 已经是 `HashMap<String, _>` 按 session id 键控，并有
`reconcile_composer_session_state` / `reconcile_inspector_session_section_state`
负责切换时的保存与恢复。多工作台改造是把 `projection`、`command_ledger`、
`composer`、`file_review` 收进同一容器。

### 2.11 折叠抖动的确切根因

`crates/desktop/src/conversation/layout.rs:120-127`：

```rust
let details_changed = row.details_expanded != input.details_expanded;
if width_changed || phase_changed || details_changed || revision_changed {
    row.measured = None;              // 展开时丢弃测量高度
}
```

行高回退到 `estimate` → `v_virtual_list`（`conversation_pane.rs:116`）按新高度重排 →
下一帧真实测量到达 → 再排一次，形成两帧跳变。

叠加 `layout.rs:256`：`anchor_at` 只在 `paused_scroll_top.is_some()` 时调用，
即用户已暂停 follow-latest 才有锚定；在底部展开靠上的消息时完全没有锚定。

### 2.12 其他被本计划触及的现状

- 顶栏「Next model / Next profile」循环：`conversation_header.rs:216-243`。运行时早已接受
  显式 `SelectModel { model_id }` / `SelectSessionProfile { profile_id }`，循环是 UI 侧限制。
- 底栏承载：状态 glyph + label、changed file 计数、**notice 下拉**、命令面板提示
  （`status_bar.rs`）。`preference_notice` 是全应用唯一的瞬时反馈通道。
- `FocusTarget::Status` 是键盘焦点环的一站（`crates/desktop/src/shell.rs:190-202`）。
- Conversation block 使用圆角卡片 `bg(visual.surface)`，**hover 与 selection 也用 bg 表达**
  （`conversation_pane.rs:281-291`）。`DesktopActionRow` 已有「accent 竖条」模式
  （`desktop_controls.rs:467-476`），且注释说明它在无颜色模式下可读且不改变行高。
- Inspector tab 目前 `flex_1().min_w_0()` 四等分（`inspector_pane.rs:645-651`）。
- 技能来源为 `agent_dir/skills`（全局）+ `skills_dirs`（相对 cwd 解析）
  （`crates/coding-agent/src/resources/mod.rs:110-121`）。
- `crates/desktop/src/app/native_shell.rs` 的测试中存在**对源码文本本身的断言**
  （例如 `native_shell.rs:6541` 断言源码包含
  `"inspector_session_sections: HashMap<String, InspectorSection>"`）。结构改动会直接打断这类断言。

## 三、已确认的产品决策

| 决策 | 结论 |
| --- | --- |
| scratch 目录 | 固定根 `~/.evo/scratch/`，按**工作区 id** 分子目录，不按 session id。会话可 fork，工作区是稳定归属 |
| thinking level | 从 per-prompt 参数**提升为 session 级状态** |
| 自动命名模型 | 允许单独配置一个廉价模型；未配置时跟随会话当前选中模型 |
| 并存运行时上限 | **4**，超限直接拒绝新建并提示用户关闭 |
| 技能面板 | 只展示全局 skills，不展示项目级 skills |
| Inspector 文件编辑 | **本轮不做**，不纳入路线图 |

## 四、明确不做

- 不做多项目切换器（截图中的 `evo / pi2rust / 添加项目`）。它需要跨项目注册表与
  多 `EmbeddingContext` 生命周期管理，是独立一期；
- 不做 git 分支显示。`CodingAgentEmbeddingSnapshot` 不含任何 git 事实，Desktop 自行读
  `.git/HEAD` 会破坏 `crates/desktop/src/lib.rs:3` 的 adapter 分层原则；
- 不在 Inspector 内编辑文件。Desktop 当前对文件严格只读（`file_review.rs` 是 bounded 预览，
  外部编辑器仅做 revalidate 后移交）；引入写入会改变 Desktop 的分层定位；
- 不改 command ledger、projection reducer、reconnect 协议或 priority/data 双通道语义；
- 不把 coding-agent API 改动与 Desktop 结构改动、视觉改动放进同一提交；
- 不为已有 CLI/TUI 调用方改任何现有函数签名；本轮 coding-agent 侧全部为增量。

## 五、必须保持的既有决策

沿用 [`desktop架构.md`](./desktop架构.md) 第 3.1 节全部条款，并补充：

1. `DesktopProjection` 仍是产品状态唯一归并入口；多工作台是「N 个 projection」，
   不是「一个 projection 塞多会话」。
2. 待机态不得用哨兵 session id 伪造会话。必须用 `Option<DesktopProjection>`
   让编译器枚举全部需要决策的调用点。
3. 后台会话必须继续投影事件，但**不得触发 `cx.notify()`**；否则后台流式输出会持续重绘。
4. 会话创建仍由运行时线程原子完成，Desktop 不得出现「已建 session 但 prompt 未提交」的中间态。

## 六、执行顺序

任务族：

- `CAG-1xx`：`coding-agent` 增量 API（纯新增，不改现有签名）
- `DSK-5xx`：Desktop 结构（待机态、多工作台）
- `VUI-3xx`：Desktop 视觉与交互

依赖关系：

```text
VUI-301 ────────────────────────────────────► 可独立先做

CAG-101 ─┬─► CAG-102 ─┐
CAG-103 ─┤            ├─► DSK-503 ─┬─► DSK-504
CAG-104 ─┘            │            │
                      │            └─► DSK-511 ─► DSK-512 ─► DSK-513
DSK-501 ─► DSK-502 ───┘

CAG-105 ─► CAG-106 ─► VUI-306

DSK-503 ─► VUI-302 / VUI-303 / VUI-304 / VUI-305 / VUI-307 / VUI-308 / VUI-309 / VUI-310
```

---

### VUI-301：修复折叠展开导致的列表抖动

**优先级：P0**
**风险：低**
**范围：`crates/desktop/src/conversation/layout.rs`、`app/native_shell/conversation_controller.rs`、`app/native_shell/conversation_pane.rs`**
**依赖：无。可立即开工**

**目标**

展开或折叠一条消息时，该行之前的内容屏幕位置完全不变，之后的内容向下推移；
重复展开同一行不产生任何抖动。

**工作**

1. `ConversationRowHeight` 的 `measured: Option<f32>` 改为按 `details_expanded`
   分别缓存（折叠态测量值与展开态测量值各存一份）。`details_changed` 不再清空测量缓存，
   而是切换到对应变体；`width_changed` / `phase_changed` / `revision_changed`
   仍然清空**全部**变体。
2. 首次展开仍会经历一帧估算。改进 `estimated_height` 的计算，使展开态估算基于已知的
   detail 文本长度与当前 width bucket，而不是沿用折叠态估算。
3. 新增「toggle 锚定」路径：展开/折叠事件发生时，无条件以被切换行的顶部为锚点调用
   `anchor_at`，与 `follow_latest` 状态无关。现有的 `paused_scroll_top` 路径保持不变。
4. 若被切换行本身位于视口上方，锚定后该行仍应保持其屏幕位置（即上方内容不动）。

**不得改变**

- streaming 行的 `STREAMING_ROW_HEIGHT_INTERVAL` 节流语义；
- `TRANSCRIPT_COLLAPSED_PREVIEW_MAX_HEIGHT` 对折叠预览的封顶；
- follow-latest 的 pause/resume 双阈值滞回（`viewport.rs:6-7`）；
- 除折叠状态外任何测量拒绝条件（late prepaint rejection）。

**完成条件**

- 新增单元测试：同一行在 `collapsed → expanded → collapsed → expanded` 循环中，
  第二次及以后的展开不产生 `height_changed`；
- 新增单元测试：展开一行后，其之前所有行的累计偏移量不变；
- conversation 性能 gate 无回归；
- 七个 visual golden 无变化（本任务不改变静态截图形态）。

**完成记录（2026-07-28）**

- `ConversationRowHeight` 现分别保留 collapsed/expanded 两份真实测量；toggle 只切换
  当前变体，width bucket、streaming/text phase 或 source revision 改变时才同时清空
  两份缓存，late prepaint 的四项 identity 拒绝条件保持不变；
- expanded 首帧估算显式按当前 width bucket、主文本与 detail 文本重新计算；正常缓存
  宽度一致时继续复用 `ConversationRowRenderData::estimated_height`，没有把长文本扫描
  移入稳定帧热路径，collapsed preview 的 `680 px` 封顶保持；
- toggle 记录目标行顶部、当前 scroll top 与完整 row identity，并经现有 `anchor_at`
  路径解析新顶部；该路径优先于 session restore、paused anchor 与 follow-latest 对齐。
  后续真实 measurement 命中同一 identity 时继续保持 scroll top，目标行即使位于视口
  上方也不会因展开/折叠或二次测量移动；
- 新增双态循环测试，第二次 expanded measurement 明确返回
  `height_changed == false`；新增四行累计 offset 测试、width invalidation 测试，以及
  follow-latest 开启且目标行在视口上方的 controller 首帧 + measurement 集成测试；
- Desktop 全量 `193 passed / 5 ignored`，dependency boundary `16/16`，
  `cargo check -p desktop --all-targets`、fmt 与 `git diff --check` 通过；Clippy 仍只报
  `desktop架构.md` 已记录的 `field_reassign_with_default` 与 `large_enum_variant` 两项
  既有红灯，本任务没有新增 lint；
- headless gate：10k CPU frame P95 `3.083 ms`、input roundtrip P95 `5.778 ms`、
  Change-to-render P95 `361 us`、10 MiB hydration `14.789 ms`、10k-block hydration
  `2.728 ms`、scroll/render P95 `216 us`、streaming event P95 `5–22 us`；
- native gate：GPU present P95 `6.803 ms`、input-to-post-render P95 `8.353 ms`、
  steady RSS growth `45,056 bytes`、production Markdown completion P95 `116 us`，均低于
  既有预算；wide、medium、narrow、authorization、reduced-motion、keyboard-focus、
  no-color 七个 visual golden 全部 `RMSE=0`；
- 本任务没有以内部 post-render latency 冒充物理 photon 结果；计划测试矩阵中的
  `desktop-click-to-photon.sh` 仍需外部传感器提供至少 50 组配对样本，继续作为人工
  验收项保留，不阻塞后续代码任务推进。

**建议提交**

```text
fix(desktop): keep transcript rows anchored when details toggle
```

---

### CAG-101：`CodingAgentSessionQuery` 的 cwd-free 构造器

**优先级：P1**
**风险：低**
**范围：`crates/coding-agent/src/app/session.rs`、`src/lib.rs`**

**目标**

不加载任何 `EmbeddingContext`、不创建任何会话，即可查询全局会话根下的会话。

**工作**

1. 新增公开构造器，从全局会话根解析（复用 `resolve_session_dir` 的既有优先级：
   `EVO_SESSION_DIR` > `EVO_DIR/sessions` > `~/.evo/sessions`）。
   建议同时提供显式根路径的变体供测试使用。
2. 构造器需要的 cwd 与 default profile id 使用产品默认值，不从调用方索取。
3. `from_run_options` 与 `context.session_query()` 保持原样，CLI/TUI 路径零改动。

**不得改变**

- `catalog()` / `snapshot()` / `tree()` / `clone_session()` / `export_html()` 的签名与语义；
- `MAX_SESSION_QUERY_CHOICES = 256` 的边界；
- 持久化关闭时返回空目录而非报错的行为（`app/session.rs:418-423`）。

**完成条件**

- 新增测试：在临时 `EVO_DIR` 下创建两个会话，用新构造器（不经 `EmbeddingContext`）能列出两条；
- 新增测试：`EVO_SESSION_DIR` 覆盖生效；
- `cargo test -p coding-agent` 全绿，CLI/TUI 无改动。

**实现记录（2026-07-28）**

- 新增 `CodingAgentSessionQuery::global()`，复用 `resolve_session_dir` 保持
  `EVO_SESSION_DIR > EVO_DIR/sessions > ~/.evo/sessions`；新增
  `from_session_root(root)` 供受控嵌入与测试显式指定根路径；两者均不加载
  `CodingAgentEmbeddingContext`，现有 `from_run_options` 与 `context.session_query()`
  零改动；
- cwd-free 构造器保持 `CodingAgentSessionOptions::cwd == None`，这是底层“不过滤 cwd”
  的产品语义；若设为字面 `"."`，`SessionService::list` 会错误排除其他项目的会话。
  default profile 使用产品默认 `ProfileId("default")`，调用方只提供可选 session root；
- 新增临时 `EVO_DIR` 下两个持久会话的 global catalog 测试、
  `EVO_SESSION_DIR` 覆盖测试、显式缺失根只读且不创建目录测试；稳定 facade
  compile-pass fixture 同时锁定两个公开构造器；
- 直接相关 gate 已通过：coding-agent lib `737/737`、API boundary `14/14`、
  lib-only Clippy `-D warnings`、CLI binary `174/174`、TUI 全量 `140/140`、fmt 与
  `git diff --check`；
- 仓库当前缺少已被 `events_snapshot` 通过 `include_str!` 引用的
  `docs/product-event-contract.md`，因此 `cargo test -p coding-agent` 与 all-target
  Clippy 在编译该既有 integration target 时失败；CLI 全量另有 4 项既有
  `ai` ownership 文本边界失败。本提交未改这些文件，故不伪报“全绿”；CAG-101
  的实现与兼容性证据已闭环，但其“全量 gate 全绿”完成条件继续保留为待修复项。

**建议提交**

```text
feat(coding-agent): expose a context-free durable session query
```

---

### CAG-102：轻量会话摘要 API

**优先级：P1**
**风险：低**
**范围：`crates/coding-agent/src/app/session.rs`**
**依赖：CAG-101**

**目标**

待机界面首屏列出会话时，不为每条会话读取完整 event log。

**工作**

1. 新增「摘要级」目录查询：每个候选只读 manifest 与 `events.jsonl`
   的第一个有界、带 checksum 的 `SessionCreated` frame，返回 `session_id` /
   `created_at` / `updated_at` / `active_leaf_id` / `cwd` / `name`。`name`
   直接复用 CAG-105 已落盘的 manifest 字段，`cwd` 从创建 frame 恢复。
2. 现有 `catalog()` 保持不变，继续提供含 `entry_count` 的完整视图，供需要它的调用方使用。
3. 明确文档化两者差异：摘要不含 `entry_count`（该值需要读 event log）。

**不得改变**

- `catalog()` 的返回类型与行为；
- 256 条上限与 `truncated` 语义。

**完成条件**

- 新增测试：摘要查询在 N 条会话下只解码 N 个首 frame，
  后续 frame 即使损坏也不影响摘要；首 frame 损坏时跳过该会话；
- 新增测试：摘要与 `catalog()` 在共有字段上一致。

**实现记录（2026-07-28）**

- 新增 `CodingAgentSessionQuery::overviews()`、
  `CodingAgentSessionOverviewCatalog` 与 `CodingAgentSessionOverview`；公开 DTO
  仅包含列表展示所需的六个字段，不暴露会话目录、转录、usage
  或 `entry_count`，现有 `catalog()` 签名与完整 hydrate 行为不变；
- repository 复用单条 durable record 的 1 MiB 上限、UTF-8、checksum、
  序号与 session-id 校验，并要求首条为 `SessionCreated`。查询不读
  第二条及后续 frame；无效首 frame 仅跳过对应会话，不使整个目录失败；
- cwd-free 查询对排序后的前 256 个候选保持既有有界语义，不因某条
  首 frame 损坏而从第 257 条回填；cwd-filtered 查询则按有效首 frame
  恢复的 cwd 匹配后计算 `truncated`；
- 新增共有字段一致性、后续 frame 损坏仍可列出、首 frame 损坏跳过、
  disabled 空结果、256 条截断语义与稳定 facade compile-pass 覆盖。
- 直接相关 gate 已通过：coding-agent lib 串行 `750/750`、API boundary
  `14/14`、lib-only Clippy `-D warnings`、Desktop / CLI / TUI `cargo check`、
  CLI binary `174/174`、TUI 全量 `140/140`、fmt 与 `git diff --check`。CLI 全量
  仍有 4 个已记录的 `ai` ownership 文本边界失败，与本次新增查询无关。

**建议提交**

```text
feat(coding-agent): add a lightweight session overview query
```

---

### CAG-103：cwd-free 的模型、设置与凭证快照

**优先级：P1**
**风险：低**
**范围：`crates/coding-agent/src/app/embedding.rs`、`src/app/settings.rs`、`src/app/auth.rs`、`src/lib.rs`**

**目标**

待机界面在没有项目上下文时也能显示可选模型与当前全局配置。

**工作**

1. 新增自由函数：返回**已配置凭证**的模型目录。内部读全局 `AuthStore`
   （`~/.evo/auth.toml`）并复用 `configured_model_choices`（`app/startup.rs:220`）。
   `model_catalog()` / `model_catalog_entry_by_id()` 已是自由函数，无需改动。
2. 新增全局设置快照入口，返回仅由全局 `settings.toml` 解析出的
   `CodingAgentSettingsSnapshot`。文档中明确它**不含项目级合并结果**。
3. 新增全局凭证状态快照入口，复用 `CodingAgentAuthController::snapshot()` 的投影逻辑。
4. 新增全局技能列表入口，只解析 `agent_dir/skills`，不解析 `skills_dirs`。

**不得改变**

- `CodingAgentSettingsController::apply` 的写入路径与 scope 语义；
- `AuthStore` 与凭证材料保持内部类型，不得泄漏到公开 API。

**完成条件**

- 新增测试：无 `EmbeddingContext` 时可取得设置快照、凭证状态与全局技能列表；
- 新增测试：全局设置快照与「global + project 合并结果」在存在项目覆盖时确实不同，
  并以此固定语义；
- 公开 API 中无任何新的凭证明文暴露。

**实现记录（2026-07-28）**

- 新增 `configured_model_catalog()`：内部按 `EVO_DIR > ~/.evo` 解析全局
  `auth.toml`，复用 `configured_model_choices` 过滤已配置 provider，公开结果仍是
  `CodingAgentModelCatalogEntry`，不携带 API key、OAuth token、provider headers、
  compatibility 或 transport 配置；有效的全局 default provider/model 继续沿用既有
  selection 规则并排在目录首位，配置无效时安全回退到产品默认模型参与排序；
- 新增 `global_settings_snapshot()`：通过专用的 `load_global_settings` 只加载全局
  `settings.toml` 并投影既有 `CodingAgentSettingsSnapshot`。它不会读取当前目录，
  也不会合并任何项目 `.evo/settings.toml`；`CodingAgentSettingsController::apply`
  仍使用原有 Global scope 与写入路径，零改动；
- 新增 `global_auth_snapshot()`：全局 `AuthStore` 仅在 crate 内部存在，公开结果复用
  `CodingAgentAuthController::snapshot()`，只包含有界的 provider id、认证材料类型与
  `truncated` 标志；
- 新增 `global_skill_catalog()`：只把 `<global-config>/skills` 交给既有 skill loader，
  明确不解析项目 `.evo/skills`、全局/项目设置中的 `skills`（`skills_dirs`）或调用方
  路径；结果只公开名称、命令、截断后的描述与 model-invocable 标志，不公开正文和路径；
- 新增无 `CodingAgentEmbeddingContext` 的组合测试，覆盖设置、认证、模型与技能四个入口；
  另有 global/project 冲突测试锁定设置隔离语义，以及 credential/skill-body canary
  断言锁定公开 DTO 不泄漏明文。稳定 facade compile-pass fixture 同时锁定四个入口；
- 直接相关 gate 已通过：coding-agent lib `740/740`、API boundary `14/14`、lib-only
  Clippy `-D warnings`、Desktop `cargo check`、CLI/TUI all-target `cargo check`、fmt 与
  `git diff --check`；仓库既有的 `docs/product-event-contract.md` 缺失仍使
  `cargo test -p coding-agent` 在编译 `events_snapshot` integration target 时失败，
  与 CAG-101 记录的全量基线红灯相同，本任务未改该 target。

**建议提交**

```text
feat(coding-agent): expose global settings, auth, and model snapshots
```

---

### CAG-104：以指定 id 创建会话

**优先级：P1**
**风险：低**
**范围：`crates/coding-agent/src/app/embedding.rs`**

**目标**

支持工作区先分配标识、再据此创建会话，为 scratch 工作区目录提供确定路径。

**工作**

1. 在 `CodingAgentEmbeddingContext` 上新增「以指定 id 创建会话」的入口，
   将 id 透传至 `session_options_internal` 构造的 `CodingAgentSessionOptions`。
   底层 `SessionService::create` 已消费该字段（`session/service.rs:234-237`）。
2. id 冲突时返回明确的类型化错误（`repository.rs:306-312` 已有目录存在检查）。
3. 现有 `create_session()` 保持不变。

**完成条件**

- 新增测试：指定 id 创建后，`session_id` 与传入值一致（经 `normalize_session_id` 规范化）；
- 新增测试：重复 id 返回类型化错误且不产生半初始化目录；
- 持久化关闭时该入口返回 `UnsupportedCapability`。

**实现记录（2026-07-28）**

- 新增 `CodingAgentEmbeddingContext::create_session_with_id()`，仅在持久化开启时把调用方
  id 透传到 `session_options_internal()?.with_session_id(...)`，随后继续走既有
  `CodingAgentSession::create_internal` / `SessionService::create`；现有无参数
  `create_session()` 及其非持久化回退行为零改动；
- 入口保持 create-only：传入 id 由既有 service/repository 两层规范化与路径字符校验，
  不会退化为 open-or-create，也不会覆盖已有目录；持久化关闭时在任何目录写入前返回
  `unsupported_capability`；
- 新增 current-thread async 测试，覆盖带首尾空白的 id 规范化、manifest/events 初始化、
  重复 id 的 `Session/session` 类型化公开错误、会话根仍只有一个完整目录，以及关闭
  持久化时的 `Capability/unsupported_capability` 与零落盘；稳定 facade fixture 同时
  锁定新方法可由外部消费者调用。
- 直接相关 gate 已通过：coding-agent lib `742/742`、API boundary `14/14`、lib-only
  Clippy `-D warnings`、Desktop `cargo check`、fmt 与 `git diff --check`；全量
  `cargo test -p coding-agent` 仍受上文已记录的既有 product-event contract 文档缺失阻断。

**建议提交**

```text
feat(coding-agent): allow caller-assigned session identifiers
```

---

### CAG-105：会话名称持久化

**优先级：P1**
**风险：中**
**范围：`crates/coding-agent/src/session/manifest.rs`、`src/session/service.rs`、`src/runtime/facade/context.rs`、`src/app/session.rs`、`src/runtime/operation.rs`**

**目标**

会话拥有可持久化、可读取、可修改的名称。

**前置确认**

`CodingAgentSessionBootstrap` 中已存在 `initial_session_name` 字段
（`app/session.rs:69`，来源 `app/startup.rs:54`）。**开工第一步是确认它当前是否落盘**；
若已有落盘路径，本任务可显著缩减。

**工作**

1. `SessionManifest` 新增 `name: Option<String>`，`#[serde(skip_serializing_if)]`，
   并处理向后兼容：旧 manifest 反序列化为 `None`。评估是否需要 `SESSION_VERSION` 递增。
2. `CodingAgentSessionSummary` 与 `CodingAgentSessionChoice` 暴露 `name`。
3. 新增设置会话名称的 operation（与 `SetSessionTreeLabel` 并列，语义区分清楚：
   前者是会话，后者是树节点）。名称长度按现有 bounded 文本约定截断。
4. CAG-102 的摘要查询填入 `name`。

**不得改变**

- `SetSessionTreeLabel` 的既有语义与调用方；
- 会话目录布局、event log 与 outbox 格式。

**完成条件**

- 新增测试：旧版本 manifest（无 `name` 字段）可正常读取，名称为 `None`；
- 新增测试：设置名称后重新 hydrate 可读回；
- 新增测试：摘要查询无需读 event log 即可返回名称；
- 名称写入走既有 session write 路径，遵守 recovery/outbox 语义。

**实现记录（2026-07-28）**

- 前置审计确认 `initial_session_name` 原先只进入 operation factory / prompt 元数据，未参与
  `SessionService` 创建或 manifest 写入；现已由 bootstrap 与 embedding context 传入
  `CodingAgentSessionOptions`，并在新会话创建时与其余 manifest 字段原子落盘；open 路径
  不会用调用方的初始名称覆盖已有会话；
- `SessionManifest` 新增可选 `name`，创建、clone/fork、manifest patch、repository summary、
  `CodingAgentSessionSummary` 与 `CodingAgentSessionChoice` 全链路读取该字段；未提升
  `SESSION_VERSION`，因为字段带 serde 默认且 `None` 不序列化，旧 v1 manifest 可直接读为
  未命名会话；choice 的 `display_name()` 优先返回名称、无名称时仍降级到 session id；
- 新增独立的 `SetSessionName` 同步可变 operation 及类型化 outcome，不复用
  `SetSessionTreeLabel`；名称会 trim、空值清除，并以 Unicode 字符边界截断到 200 字符。
  写入继续通过单 writer 的 `commit_session_mutation`，携带 admitted operation id，因此失败
  保持既有 partial-commit 与 finalization/recovery 判定，同时没有新增 event log 或
  outbox 序列化格式；
- 新增测试覆盖：无 `name` 的旧 v1 manifest、初始名称落盘、重命名后重新 hydrate、Unicode
  长名称截断、manifest 写入失败保留 operation identity，以及把 `events.jsonl` 故意写成
  非法内容后，cwd-free 列表仍仅从 manifest 返回名称；CAG-102 后续摘要 DTO 可直接复用该
  repository summary 字段，cwd 契约仍按上文保持待确认；
- 直接相关 gate 已通过：coding-agent lib `747/747`、API boundary `14/14`、lib-only
  Clippy `-D warnings`、Desktop / CLI / TUI `cargo check`。全量
  `cargo test -p coding-agent` 仍受上文已记录的既有 product-event contract 文档缺失阻断。

**建议提交**

```text
feat(coding-agent): persist an optional session name in the manifest
```

---

### CAG-106：首轮对话后自动生成会话名称

**优先级：P2**
**风险：中**
**范围：`crates/coding-agent/src/operations/`、`src/runtime/dispatch.rs`、`src/session/`、`src/config/settings.rs`、`src/app/settings.rs`**
**依赖：CAG-105**

**目标**

会话完成第一轮对话后，由模型生成一个简短名称并写回 manifest。

**工作**

1. 复用 branch-summary 已验证的 scoped provider runtime 边界新增命名
   workflow：输入为首轮 user + assistant 内容，输出为受长度约束的单行名称。
2. 新增设置项：命名专用模型 id。**未配置时跟随会话当前选中模型**（已确认决策）。
3. 触发时机：首轮对话终态事件之后、且 `manifest.name.is_none()` 时后台触发一次。
   不阻塞任何用户可见路径。
4. 失败（模型错误、超时、输出为空、越界）时**不写入名称**，保持 `None`，
   由适配层显示「未命名」。失败必须产生一条 diagnostic 而非静默。
5. 名称生成消耗的 token 必须计入既有 usage 统计，不得旁路。

**不得改变**

- 首轮对话本身的 operation 生命周期与终态语义；
- prompt 的 terminal event drain 顺序。

**完成条件**

- 新增测试：首轮结束后名称被写入且只写一次；
- 新增测试：已有名称时不再触发；
- 新增测试：命名失败时会话仍可正常使用，名称保持 `None`，且产生 diagnostic；
- 新增测试：配置了命名模型时使用该模型，未配置时使用会话当前模型。

**实现记录（2026-07-28）**

- 新增 `operations/session_naming.rs`；`run_operation` 仅在主 prompt 的 durable
  terminal outbox 持久化且 submission guard 完成后派生 Tokio 任务。派生点
  只做 O(1) 的条件获取与 writer 句柄 clone，不追加 event-log replay、
  不改变 prompt terminal drain 顺序，模型请求与落盘均在后台执行；
- prompt 原本就会加载的 pre-turn replay 同时用于判定「尚无 user / completed
  assistant」，manifest 已有名称或后续轮次都不派生。正常产品
  operation factory 持有 settings snapshot 时启用该策略；缺少产品策略快照的
  底层测试 / legacy 调用保持原语义；
- 新增 `session_naming_model` 全局设置、公开 snapshot 字段与
  `SetSessionNamingModel`。未配置时 clone 当前 prompt 已解析的 model；
  配置时改用已注册的指定 model，仍经同一 scoped provider / model-capability
  边界；输入按 user / assistant 各 4,000 字符有界，输出必须是
  非空、单行且不超过 80 字符；
- 新增 durable `model.usage.recorded` event，命名成功或已返回 usage 的失败
  都以独立 `session_naming` operation 落盘并累加既有 session usage；该 usage
  不伪装为 assistant transcript message，也不覆盖最后对话 context-token
  水位。失败同时落盘 warning diagnostic 并发出 live diagnostic；
- 后台 writer 复用会话唯一 actor；「记录 usage + 仅当 manifest.name 仍为
  `None` 时写名称」在 actor 内原子判定，因此并发手工命名不会被覆盖，
  但已发生的模型 usage 仍会计账；
- 新增端到端测试覆盖首轮成功后命名且只命名一次、已有名称跳过、
  空输出保留 `None` / durable diagnostic / usage 且第二轮仍可用、配置模型
  覆盖与默认跟随，以及手工命名竞态下不覆盖但仍计费；
- 直接相关 gate 已通过：coding-agent lib 串行 `758/758`、API boundary
  `14/14`、lib-only Clippy `-D warnings`、Desktop / CLI / TUI all-target
  `cargo check`。全量 integration 仍保留上文已记录的既有基线阻断。

**建议提交**

```text
feat(coding-agent): auto-name sessions after the first exchange
```

---

### DSK-501：运行时启动不再创建会话

**优先级：P1**
**风险：中**
**范围：`crates/desktop/src/runtime/driver.rs`、`src/runtime/protocol.rs`、`src/app.rs`**

**目标**

运行时线程以 `session: None` 启动，窗口在没有任何会话的情况下正常打开。

**工作**

1. `run_runtime` 移除 `context.create_session().await`，以
   `RuntimeState { context, session: None }` 起步。
2. `ready` 通道改为传递**项目级**快照（`context.snapshot()` 即可构造），
   不再要求 `DesktopRuntimeHydratedSnapshot`。新增或调整协议 DTO 以表达
   「项目已就绪、会话可选」。
3. `app.rs` 的 bootstrap 轮询与 `DesktopProjection::new` 调用点相应调整。
4. 需要会话的命令在 `session.is_none()` 时继续返回既有的类型化拒绝
   （`DesktopBridgeError::Session { "desktop runtime has no idle session owner" }`）。
   项目级命令（`Reload`、`ListSessions`、`SelectModel`）在无会话时必须可用。

**不得改变**

- priority/data 双通道与队列容量；
- 既有命令的 command id 对账语义；
- `OpenSession` 从无会话态直接打开已有会话的路径。

**完成条件**

- 新增 runtime 测试：无会话启动后 `Reload` / `ListSessions` / `SelectModel` 成功，
  `Resync` / `ReviewChangedFile` / recovery 返回类型化拒绝；
- 新增测试：启动后不产生任何会话目录；
- 现有 27 项 runtime 定向测试通过。

**实现记录（2026-07-28）**

- `run_runtime` 现在只加载 `CodingAgentEmbeddingContext`，以 `session: None` 进入命令循环；
  ready 通道新增 `DesktopRuntimeReadySnapshot`，只携带项目快照，启动与关闭均不会调用
  `create_session` 或建立 session 目录；
- `DesktopRuntimeMetadataSnapshot.session` 改为可选。`Reload`、`ListSessions` 与
  `SelectModel` 在无会话态正常工作，metadata 只替换项目事实；完整 hydration、recovery、
  resync、文件审查等会话命令仍要求真实 session，并以 `code = "session"` 和既有精确消息
  `desktop runtime has no idle session owner` 拒绝；priority/data queue 与 command id 未改；
- 应用 bootstrap 在没有显式 session id 时打开持有运行时的 project-ready surface；传入
  `--session` 时先通过同一有界命令通道执行 `OpenSession`，拿到真实 hydrated snapshot 后
  再安装原 NativeShell，不创建中间会话。完整 Home、composer 与 sessionless shell 交互留给
  DSK-503；
- 新增测试覆盖无会话启动后的三类项目命令、三类会话拒绝、零 session 目录，以及从
  sessionless 状态直接打开已有会话；现有 runtime 定向集现为 `29/29`，Desktop lib
  `195 passed / 5 ignored` 与 dependency boundary `16/16` 通过。严格 Clippy 仍被两个
  与本任务无关的既有 lint 阻断：`native_perf.rs` 的 `field_reassign_with_default` 与
  `commands.rs` 的 `large_enum_variant`。

**建议提交**

```text
refactor(desktop): boot the runtime without an implicit session
```

---

### DSK-502：首次提交时原子创建会话

**优先级：P1**
**风险：中**
**范围：`crates/desktop/src/runtime/dispatch.rs`、`src/runtime/driver.rs`**
**依赖：DSK-501**

**目标**

`SubmitPrompt` 在无会话时于运行时线程内「先建会话、再启动 prompt」，
不产生「已建会话但未提交」的中间态。

**工作**

1. `dispatch_idle_command` 的 `SubmitPrompt` 分支：`session.is_none()` 时先创建会话
   （携带工作区 id，见 DSK-504），成功后立即 `start_prompt`。
2. 创建成功但 `start_prompt` 失败时，必须回滚到无会话态或明确报告会话已建立，
   二者择一并在协议中表达清楚，不得留下 UI 不知情的会话。
3. 新增 update 变体让 shell 知道「会话在本次提交中被创建」，以便安装 projection。
4. Shell 侧不得实现「等会话建好再提交」的两步状态机。

**完成条件**

- 新增测试：无会话时提交 prompt，得到会话创建 + prompt accepted 的有序更新；
- 新增测试：创建成功而启动失败时，状态可判定且无孤儿会话；
- 新增测试：已有会话时提交路径与改动前完全一致。

**实现记录（2026-07-28）**

- `SubmitPrompt` 在 idle state 没有 session 时，先在同一运行时线程创建会话并取得初始
  hydrated snapshot，随后立即走原 `start_prompt` 准备与 task 启动路径；已有 session 时
  仍返回原 `PromptAccepted`，未改变 prompt task、ProductEvent drain 或 terminal 顺序；
- 新增 `PromptAcceptedWithSession`，用一个 priority update 原子表达「会话已建立且 prompt
  已接受」，其中 snapshot 可直接建立产品投影；没有先发 `SessionChanged` 再等待
  `PromptAccepted`，因此 shell 不需要两步状态机；
- 创建会话本身失败时仍返回普通 `CommandRejected`，不产生 active owner。创建成功后
  `start_prompt` 失败则采用计划允许的「明确报告已建立会话」契约：
  `PromptRejectedWithSession` 始终携带真实 session metadata 与安全错误投影，并在 hydration
  成功时同时携带完整 snapshot。之所以不做假回滚，是因为 persistent `create_session`
  返回前已写 manifest/event log，而当前没有 delete API；即使 transcript/recovery hydration
  本身失败，UI 仍知道已建立的 session id 并可重新打开，不会形成不可见孤儿；
- DesktopProjection 与既有 NativeShell 已识别两种新 update：成功时按 prompt admission
  完成并原子替换 session，失败时保留草稿、安装 session 并显示类型化拒绝；待机 Home 的
  实际 composer 接线仍属于 DSK-503；
- 新增测试覆盖首次提交的创建 + 接受 + 完整 ProductEvent/terminal 投影、创建失败、创建后
  prompt 启动失败，以及已有 session 仍走旧路径。runtime 定向集为 `32/32`，dependency
  boundary `16/16`、Desktop lib `198 passed / 5 ignored` 通过。

**建议提交**

```text
feat(desktop): create the session atomically on first submission
```

---

### DSK-503：Shell 的无会话态与 Home 面板

**优先级：P1**
**风险：中高**
**范围：`crates/desktop/src/app/native_shell.rs`、新增 `app/native_shell/home_pane.rs`、`src/shell.rs`**
**依赖：DSK-501、CAG-101、CAG-102、CAG-103**

**目标**

窗口在无会话时呈现待机界面：大输入框、模型与 thinking 选择器、最近会话列表、全局技能。

**工作**

1. `NativeShell.projection` 改为 `Option<DesktopProjection>`，项目快照单独持有。
   逐个处理编译器报出的调用点，禁止使用哨兵会话 id 绕过。
2. 新增 `home_pane.rs`，在无会话时顶替 conversation 与 inspector 区域。
   Composer 直接复用（它已管理草稿与选择器）。
3. Home 的数据全部来自 CAG-101/102/103 的只读 API，不经过运行时会话。
4. 草稿迁移：待机态输入的文本在会话建立后必须落入新会话的草稿槽。
   复用 `reconcile_composer_session_state`，以一个「home」伪键参与。
5. `ShellLayout` 增加待机布局：面板隐藏、workspace 占满。该函数是纯函数，优先在此加测试。
6. 处理 `native_shell.rs` 中因结构改动而失效的源码文本断言，
   要么更新断言，要么将其改写为行为断言。

**不得改变**

- `ProjectionDirtyRouting` 的选择性通知机制；
- 既有 pane 的 bounded view model + typed event 模式；
- 键盘焦点环在有会话时的顺序。

**完成条件**

- 新增测试：无会话时六个 view model 构造不 panic 且不引用会话字段；
- 新增测试：待机态输入的草稿在会话建立后可见；
- 新增 visual golden：待机态 wide / medium / narrow；
- 全量 `cargo test -p desktop` 与 dependency boundary 通过。

**实现记录（2026-07-29）**

- `NativeShell` 现在分别持有始终可用的项目快照与
  `Option<DesktopProjection>`。编译器枚举出的会话调用点均按语义处理：待机态的
  view model 使用项目级默认值或空的有界集合，会话专属的文件审查、恢复、授权、复制
  与滚动动作安全退化；没有哨兵 projection，也没有用 `expect` 绕过产品状态分支；
- 新增独立 `HomePane`，以命名 main region 呈现当前模型 / thinking、最近会话与全局
  skills，并继续复用原 Composer 作为唯一输入与提交入口。最近会话经 CAG-102 的
  `session_query().overviews()` 获取，全局 skills 经 CAG-103 的
  `global_skill_catalog()` 获取，项目事实来自不创建 session 的 L1 ready snapshot；
  Home 不建立或连接任何会话运行时；
- shell 收到首次提交的 `PromptAcceptedWithSession` 或
  `PromptRejectedWithSession` 后才安装真实 projection；待机草稿使用仅限 Composer
  内部的 `home` 槽保存，并在首个真实 session id 建立时迁移后删除，runtime 从未看到
  该槽键。`ShellLayout::resolve_idle` 在 wide / medium / narrow 下隐藏两侧 session
  pane，让 Home 占满 workspace；已有会话的焦点顺序保持不变；
- 新增无会话六组 bounded view model + 三档真实 GPUI bounds 测试、Home 草稿迁移测试、
  idle layout 纯函数测试，以及 wide / medium / narrow 三张 idle golden；golden runner
  现在覆盖十档 fixture。十档自动比较均为 `RMSE=0`，review note 已记录三档布局与
  accessibility 结果；
- 本批验证通过：Desktop `201 passed / 5 ignored`，dependency boundary `16/16`，
  `cargo check -p desktop --all-targets`、严格 Clippy `-D warnings`、fmt 与
  `git diff --check` 全部通过。

**建议提交**

```text
feat(desktop): render an idle home surface without a session
```

---

### DSK-504：scratch 工作区

**优先级：P2**
**风险：中**
**范围：`crates/desktop/src/app/native_shell.rs`、`src/runtime/driver.rs`、`src/preferences.rs`**
**依赖：DSK-503、CAG-104**

**目标**

用户在待机界面不选择项目目录时，会话绑定到 `~/.evo/scratch/<工作区 id>`，
使 agent 仍可执行需要文件系统的任务。

**工作**

1. Desktop 生成工作区 id（与会话 id 解耦；一个工作区可容纳 fork 出的多个会话）。
   工作区 id 需持久化到偏好，使重开应用后仍能回到同一 scratch 目录。
2. 创建会话前建立 `~/.evo/scratch/<工作区 id>/`，以其为 cwd 加载 `EmbeddingContext`。
3. 明确该目录语义并在 UI 上标注，避免用户误以为文件写入了项目目录。
4. 该目录下的 `.evo/` 默认不存在，因此只使用全局配置 —— 与「技能面板只展示全局 skills」
   的决策自洽，实施时不要额外引入项目级解析。
5. 定义清理策略：空工作区（无会话、无文件）在何时可回收。

**完成条件**

- 新增测试：无项目待机态创建的会话，其 cwd 位于 scratch 工作区下；
- 新增测试：同一工作区重复进入复用同一目录；
- 新增测试：scratch 工作区不解析项目级 `.evo/settings.toml`。

**实施记录（2026-07-29）**

- Desktop 默认入口改为显式 projectless 模式，不再把进程 cwd 当成用户选择的项目；已有
  `DesktopApplicationOptions::new(cwd)` 仍保持显式项目启动语义。projectless 启动从产品解析的
  全局配置根（`EVO_DIR`，否则 `~/.evo`）创建并复用
  `scratch/<workspace-id>`，工作区 id 与 session id 解耦并持久化到
  `desktop/preferences.json`；id 仅允许有界的 ASCII 字母数字、`-`、`_`，scratch 根或工作区
  若是符号链接或非目录则拒绝启动，避免偏好内容把路径解析带出产品目录。
- coding-agent 以独立兼容提交 `7d4dcb0` 增加
  `CodingAgentEmbeddingOptions::with_global_config_only()` 与公开的全局配置根查询。scratch cwd
  仍是 agent 执行文件操作和写入 session manifest 的真实 cwd，但配置解析显式排除该目录及祖先的
  `.evo/settings.toml`、项目 skills、prompts、themes、profiles 与 `AGENTS.md`，只保留全局配置、
  全局资源和全局上下文；显式项目启动继续使用原有完整项目解析。
- Home 摘要行以文本标注 `Scratch workspace · <path>`，而非只靠颜色表达，避免将 agent 生成文件
  误认为写入进程目录或已选择项目；三档待机态 visual golden 已附 review note 更新，七个既有
  session / authorization / reduced-motion / keyboard-focus / no-color 夹具像素不变，独立复跑的
  十个夹具全部 `RMSE=0`。
- 清理策略采用保守保留：窗口关闭时绝不删除 scratch。即使目录当前为空，后续会话或 agent
  仍可能写入文件，且多窗口按同一持久化 id 复用该目录；仅在用户显式重置 Desktop 偏好时才允许
  回收该工作区，避免关闭竞态和隐式数据丢失。
- 测试覆盖首次 prompt 原子建会话后 durable overview 记录精确 scratch cwd、恶意 scratch
  `.evo/settings.toml` 不生效、工作区 id 首次创建与重复复用、非法 id 丢弃、旧版偏好兼容与
  projectless/显式项目选项语义。验证通过：Desktop `205 passed / 5 ignored`、dependency
  boundary `16/16`、coding-agent lib `759/759`、API boundary `14/14`、Desktop all-target
  check、Desktop 与 coding-agent lib 严格 Clippy、CLI/TUI all-target check、fmt 与
  `git diff --check`。coding-agent 全量 all-target 仍只受上文已记录的既有
  `docs/product-event-contract.md` 缺失阻断。

**建议提交**

```text
feat(desktop): bind projectless sessions to a scratch workspace
```

---

### DSK-511：运行时多会话路由

**优先级：P2**
**风险：高**
**范围：`crates/desktop/src/runtime/driver.rs`、`src/runtime/dispatch.rs`、`src/runtime/protocol.rs`**
**依赖：DSK-502**

**目标**

单个运行时线程同时持有至多 4 个会话，命令按会话路由，多个 prompt 可并行执行。

**架构选型**

采用「单 tokio current-thread + 多会话」：

```text
sessions: HashMap<SessionId, RuntimeState>
active:   HashMap<SessionId, ActivePrompt>
```

主循环的 `tokio::select!` 改为对全部 active prompt 的 `FuturesUnordered`。

不采用「每会话一个线程」（N 组 channel 使 Desktop 侧管理复杂度失控），
也不采用多线程 tokio（需重新审计全部单线程假设）。

**工作**

1. `DesktopRuntimeCommand` 全部变体携带目标 session id（项目级命令除外）。
2. 更新流携带来源 session id，供 shell 路由到对应工作台。
3. 会话数量硬上限 4；超限时 `CreateSession` / `OpenSession` 返回类型化拒绝。
4. 关闭单个会话的命令与既有 shutdown 语义对齐（先 drain terminal event 再释放 owner）。
5. 应用退出时按确定顺序关闭全部会话，沿用 `RUNTIME_SHUTDOWN_DEADLINE`。

**不得改变**

- 单个会话内部的事件顺序、ack、reconnect 与 recovery 语义；
- priority/data 双通道的传递保证。

**完成条件**

- 新增测试：两个会话各自跑 prompt，事件不串流、command id 不互相完成；
- 新增测试：第 5 个会话被拒绝且已有会话不受影响；
- 新增测试：关闭其中一个会话不影响另一个的 active prompt；
- 两组 performance gate 无回归。

**实施记录（2026-07-29）**

- runtime 线程仍是单个 Tokio current-thread executor；所有权从单一
  `RuntimeState.session + Option<ActivePrompt>` 改为按 session id 索引的 idle session 表和
  active prompt 表，主循环用 `FuturesUnordered` 同时等待最多 4 个 prompt 的 ProductEvent
  receiver 与 task completion。没有引入 per-session OS thread，也没有改变单个 session 内部的
  connect / prepare / run / ack / reconnect / recovery 链。
- 会话级 `DesktopRuntimeCommand` 现在携带目标 session id；`None` 仅作为现有单工作台调用面的
  兼容入口：无会话 Submit 仍原子建会话，已有唯一/聚焦会话时确定性解析，多会话且无可解析目标时
  返回 `session_target`，不猜测 HashMap 迭代结果。Create / Open 的在场 session 总数硬限制为 4，
  第 5 个返回类型化 `session_limit_reached`；显式 routed prompt / steer / follow-up / abort 已接到
  当前 shell projection，为 DSK-512 的多工作台调用面保留稳定协议。
- ProductEvent update envelope 独立携带来源 session id，不能再依赖产品事件 payload 本身一定有
  session id；priority / data 双通道只在同一 session 内比较 sequence，不会拿 A 的 sequence 与
  B 的 sequence 排序。每个 active prompt 独立维护 operation id、cursor、reconnect source、ack
  和 terminal drain，command id 的接受、启动与完成保持原关联。
- 新增 `CloseSession`：idle owner 直接 shutdown；active owner 先发 abort、在既有
  `RUNTIME_SHUTDOWN_DEADLINE` 内等待 task、排空并转发 terminal ProductEvent、detach client 后
  shutdown session。关闭失败返回拒绝而不伪报 `SessionClosed`；应用退出按排序后的 session id
  依次关闭全部 active 与 idle owner，因此一个会话关闭不影响其余 active prompt。
- 测试覆盖双 prompt 并行且事件/session/operation/command completion 不串流、第五会话拒绝后原有
  4 个仍在场、关闭 A 时 B 正常完成，以及跨 session 的 priority/data merge 不比较 sequence。
  验证通过：Desktop `209 passed / 5 ignored`、dependency boundary `16/16`、all-target check、
  严格 Clippy、fmt 与 `git diff --check`。`desktop-native-perf-gate.sh` 通过（frame p95
  `5732µs`、input p95 `8365µs`、steady RSS growth `151552B`、markdown p95 `115µs`）；
  `desktop-perf-gate.sh` 通过（10k blocks hydration `2515µs`、headless frame p95 `2807µs`、
  input roundtrip p95 `5648µs`，200 events/s streaming p95 `5µs`）。本任务不新增 UI surface；
  per-session projection、草稿、Inspector 与后台静默刷新仍明确属于 DSK-512。

**建议提交**

```text
feat(desktop): route runtime commands across concurrent sessions
```

---

### DSK-512：Shell 的会话工作台抽象

**优先级：P2**
**风险：高**
**范围：`crates/desktop/src/app/native_shell.rs` 及其子模块**
**依赖：DSK-511**

**目标**

Shell 持有多个会话工作台，前台渲染其一，后台持续投影。

**工作**

1. 提取 `SessionWorkspace`，收纳 `projection`、`composer`、`file_review`、
   `command_ledger`、`inspector_section`、`draft`、`composer_running_mode`、
   `thinking_level`（DSK 侧的 session 级状态）。
2. `NativeShell` 持有 `workspaces: HashMap<SessionId, SessionWorkspace>` 与 `active`。
   现有的三个 per-session `HashMap` 合并进工作台，删除
   `reconcile_composer_session_state` / `reconcile_inspector_session_section_state`
   这类「切换时搬运」的逻辑 —— 状态本就该长在工作台里。
3. 更新分发按 session id 路由到对应工作台。
   **后台工作台只更新数据，不得调用 `cx.notify()` 或 `notify_*_pane`。**
4. 切换 active 时一次性推送全部 pane view model。
5. `MAX_COMPOSER_SESSION_STATES` 等既有上限重新定义为工作台上限（4）。

**不得改变**

- 每个工作台内部的 dirty routing 判定条件；
- pane 的 bounded view model + typed event 边界。

**完成条件**

- 新增测试：后台会话收到事件后 projection 前进，但不触发 UI notify；
- 新增测试：切回后台会话时 transcript、草稿、Inspector 选中项与离开时一致；
- 新增测试：`file_review` 与 `command_ledger` 不跨工作台串扰；
- 全量测试与两组 performance gate 通过。

**实施记录（2026-07-29）**

- 提取 `SessionWorkspace`，把 `project / projection`、会话 notice、Conversation controller、
  Composer（含 draft 与 pending submission）、running mode、thinking selection、Inspector section、
  file review 与 typed command ledger 放到同一所有权边界。`NativeShell` 只保留一个前台
  `active_workspace` 与至多 3 个 parked workspace；既有 draft / running mode / Inspector 三张
  per-session `HashMap` 以及 Conversation controller 内的 session view 搬运缓存全部删除。
- update 先用显式 session id、hydrated/metadata snapshot 或全局唯一 command id 找到 owner；后台
  update 临时挂载目标 workspace，沿用原 projection/command/composer dirty 判定完成数据更新，再恢复
  前台 workspace 与本轮全部 pane/root dirty flags，因此不会调用 `cx.notify()` 或任一 pane notify。
  `SessionChanged` 则永久切换前台，并在同一轮推送完整 view model；Home 首次建会话保留原 draft 与
  pending command，不制造额外 Home workspace。
- command ledger 仍按 workspace 隔离且每个上限 32；为避免多 ledger 从 1 重复分配，shell 新增唯一的
  checked command-id sequence，再由 `reserve_with_id` 写入目标 workspace ledger。session catalog 仍是
  shell 全局事实，其 completion 可跨 parked workspace 精确完成发起方的 typed intent。
- 新增 GPUI 回归覆盖：后台 B 的 projection sequence 前进而 runtime UI notify 计数保持 0；A/B 切换
  恢复各自 transcript owner、draft 与 Inspector section；B 的 file review / ledger 不进入 A；四工作台
  上限拒绝第 5 个；Home→首会话完成原 Home ledger command 并保留草稿。源码边界断言同步改为
  per-workspace ledger/Workspace 所有权，移除旧搬运结构的假阳性文本断言。
- 验证通过：Desktop `210 passed / 5 ignored`、dependency boundary `16/16`、all-target check、严格
  Clippy、fmt 与 `git diff --check`。`desktop-native-perf-gate.sh` 通过（GPU/present frame p95
  `6826µs`、input p95 `8355µs`、steady RSS growth `53248B`、markdown p95 `116µs`）；
  `desktop-perf-gate.sh` 通过（10k blocks hydration `2600µs`、headless frame p95 `2741µs`、input
  roundtrip p95 `5718µs`，200 events/s streaming p95 `4µs`）。本任务没有改变 pane view model、typed
  event 边界或任何视觉 surface，因此不更新 golden。

**建议提交**

```text
refactor(desktop): own one workspace per concurrent session
```

---

### DSK-513：并存上限与会话运行状态

**优先级：P2**
**风险：中**
**范围：`crates/desktop/src/app/native_shell/sessions_pane.rs`、`session_controller.rs`**
**依赖：DSK-512**

**目标**

用户能看到哪些会话正在运行，并在达到上限时得到明确提示。

**工作**

1. 会话列表每一项显示自身运行状态点（运行中 / 等待授权 / 出错 / 空闲）。
   数据来自各工作台的 projection，不依赖 active 会话。
2. 达到 4 个上限时，新建/打开操作被拒绝并提示「请先关闭一个会话」。
3. 列表项提供关闭该会话的动作（trailing 工具位，宽度预留，遵循 `DesktopActionRow` 约定）。

**完成条件**

- 新增测试：后台会话运行中时列表状态点正确；
- 新增测试：上限拒绝有明确提示且不改变既有工作台；
- 三档 viewport 的 golden 更新并附 review note。

**实施记录（2026-07-29）**

- Sessions pane 的 bounded view model 新增按 session id 排序的 `SessionRuntimeState`，数据同时来自
  active 与 parked workspace 的 projection。每行独立映射 idle / running / authorization / warning /
  error 语义；后台 B 运行时不再借用前台 A 的状态。状态仍以 glyph、文本 accessible label 和颜色共同
  表达，无色模式不丢失含义。
- 工作台上限统一按「持有 projection 的运行会话」计数，待机 Home 不占名额。Create 与尚未打开的
  Open 在 4 个会话时均于发送 runtime command 前拒绝，保留既有 4 个 workspace，并明确提示
  `close one first`；已打开 workspace 仍可切回。加号在达到上限时同步禁用并给出同义 tooltip。
- 列表行新增 session-specific Close typed event/intent/runtime command。command id 仍由 shell 的 checked
  全局序列分配，并记在目标 workspace ledger；`SessionClosed` 只移除对应 workspace。关闭前台时按
  session id 稳定选择另一个 parked workspace，关闭最后一个会话则回到 Home，并保留可能仍待结算的
  全局 command ledger，因此迟到的 catalog completion 不会失去 owner。
- GPUI 的整行 `Button` 不能嵌套另一个 `Button`，因此关闭工具使用独立的固定宽度同级控件，避免被
  disabled 行吞掉事件。docked panel 为关闭工具稳定保留 36 px，并优先显示 session 名；窄屏 overlay
  额外保留 60 px relative-time 槽并展示 cwd/status detail。`DesktopActionRow` 的标题允许在预留尾部前
  安全收缩，不与关闭工具重叠。
- 新增 GPUI 回归覆盖后台 Running 状态、第五会话拒绝且不发 Open command、后台 Close command 的
  target-ledger 归属与仅移除 owner；全量验证为 Desktop `211 passed / 5 ignored`、dependency boundary
  `16/16`、all-target check、严格 Clippy、fmt 与 `git diff --check`。三档 viewport（含窄屏 Sessions
  dialog）及 authorization / reduced-motion / keyboard-focus / no-color 视觉变体已审图、附 review note，
  安装后 10 个 fixture 重放 RMSE 均为 `0`。
- 两组性能门通过：native GPU/present frame p95 `5245µs`、input p95 `8368µs`、steady RSS growth
  `20480B`、production Markdown p95 `119µs`；headless 10k hydration `2618µs`、frame p95 `2692µs`、
  input roundtrip p95 `5485µs`，200 events/s streaming p95 `4µs`。

**建议提交**

```text
feat(desktop): surface per-session run state and the workspace limit
```

---

### VUI-302：移除底栏并引入 toast 通知

**优先级：P2**
**风险：中**
**范围：删除 `app/native_shell/status_bar.rs`、新增 toast 宿主、`src/shell.rs`、`src/actions.rs`**
**依赖：DSK-503**

**目标**

移除底栏，同时不丢失任何现有反馈能力。

**工作**

1. **先建 toast，再删底栏。** `preference_notice` 是全应用唯一的瞬时反馈通道
   （会话创建、外部编辑器打开、偏好恢复、命令拒绝原因等均经由它），
   必须先有替代通道。
2. changed file 计数删除（Inspector 的 Changes tab 已含该信息且带 badge）。
3. 命令面板提示删除。
4. 从 `ShellLayout` 移除 status 区域，`FocusTarget::Status` 从 `focus_order()`
   （`shell.rs:190-202`）与 `is_visible()` 中移除，同步更新键盘导航测试与快捷键绑定。

**不得改变**

- 任何 notice 的文案与触发条件；
- 焦点环在其余区域的相对顺序。

**完成条件**

- 新增测试：每一处原先写入 `preference_notice` 的路径都能产生 toast；
- 键盘焦点环测试更新且通过；
- 三档 golden 更新并附 review note，说明反馈通道迁移。

**实施记录（2026-07-29）**

- 新增独立 `ToastHost` entity，并在删除底栏前将原 `preference_notice` 通道完整迁入。策略集中定义为
  最多堆叠 3 条、6 秒自动隐藏、支持逐条手动关闭；鼠标悬停或键盘焦点进入 host 时暂停倒计时，离开后
  按实际暂停时长顺延。每条使用 `Role::Status`、完整文案 accessible label 与独立的
  `Dismiss notification` 控件；host 只持有 bounded toast/来源队列，不读取 projection 或 shell。
- `SessionWorkspace` 以单一 `set_preference_notice` 入口维护单调 revision；所有原成员写入点保持原文案和
  原条件，仅机械迁移到该入口。ToastHost 按 session id + revision 去重，因此切回工作台不会重播旧通知，
  同一文案的两次真实触发仍会产生两条 toast；后台 workspace 的既有静默更新/切回恢复语义不变。
- 删除 `status_bar.rs` 及其 changed-file 计数、命令面板提示和常驻 notice 呈现；Home、Sessions 与 shell
  不再复制该反馈。`ShellLayout` 删除 30 px status rectangle，正常态与待机态 workspace 均占满窗口高度；
  `FocusTarget::Status` 从 enum、可见性、焦点顺序和 shell focus owner 中移除，其余区域相对顺序保持不变。
- 新增行为测试覆盖重复文案、三条上限与先进先出淘汰，源码边界测试确保原 notice owner 全部经过集中
  setter/ToastHost，响应式 GPUI 测试验证旧 status selector 消失、Composer 延伸到底部、ToastHost 在
  wide / medium / narrow 内有界。三档 standard golden 显式呈现堆叠 toast；全部 10 个 fixture 已人工
  审图、附 review note，安装后复播 RMSE 均为 `0`。
- 最终验证为 Desktop `212 passed / 5 ignored`、dependency boundary `16/16`、all-target check、严格
  Clippy、fmt 与 `git diff --check`。native 性能门：GPU/present frame p95 `6004µs`、input p95
  `8368µs`、steady RSS growth `163840B`、production Markdown p95 `117µs`；headless 门：10k blocks
  hydration `3261µs`、frame p95 `4450µs`、input roundtrip p95 `6382µs`，200 events/s streaming p95
  `5µs`。

**建议提交**

```text
refactor(desktop): replace the status bar with transient toasts
```

---

### VUI-303：顶栏模型与 Profile 完整下拉列表

**优先级：P2**
**风险：低**
**范围：`app/native_shell/conversation_header.rs`**
**依赖：DSK-503**

**目标**

用列表选择替代「Next model / Next profile」循环。

**工作**

1. 拆成两个独立选择器：模型、Profile。各自展开完整列表并标记当前项。
2. 列表数据来自项目快照的 `models` / `profiles`。
3. 事件改为携带显式 id；运行时侧无需改动
   （`SelectModel { model_id }` / `SelectSessionProfile { profile_id }` 早已支持）。
4. 待机态同样可用（模型来自 CAG-103 的 cwd-free 目录）。
5. 列表过长时需要滚动与可选的搜索，遵循既有 `DropdownMenu` primitive，不新造控件。

**不得改变**

- 选择器在运行中被禁用的既有条件；
- 窄视口下的降级显示逻辑（`conversation_header.rs:165-171`）。

**完成条件**

- 新增测试：选择非相邻模型可一步到位，产生正确的 command intent；
- 新增测试：待机态可选择模型；
- 三档 golden 更新并附 review note。

**建议提交**

```text
feat(desktop): select models and profiles from full dropdown lists
```

---

### VUI-304：thinking level 提升为会话级并移入顶栏

**优先级：P2**
**风险：中**
**范围：`app/native_shell/conversation_header.rs`、`composer_pane.rs`、`app/native_shell.rs`、`src/runtime/protocol.rs`**
**依赖：VUI-303**

**目标**

thinking level 成为会话级持久状态，在顶栏选择，不再是每条消息的临时参数。

**工作**

1. thinking level 移入 `SessionWorkspace`（或 DSK-512 前的等价 per-session 状态），
   随会话切换保持。
2. 顶栏新增选择器；从 composer 移除。
3. `SubmitPrompt` 继续携带 thinking level，但取值来自会话状态而非输入框临时选择。
4. 决定是否持久化到会话 manifest 或仅存于 Desktop 偏好。
   **建议先只存 Desktop 偏好**，避免与 CAG-105 的 schema 改动耦合。

**完成条件**

- 新增测试：切换会话后 thinking level 恢复为该会话的值；
- 新增测试：提交的 prompt 携带会话当前 thinking level；
- 三档 golden 更新并附 review note。

**建议提交**

```text
feat(desktop): promote thinking level to session-scoped state
```

---

### VUI-305：Composer 重排与附件上传

**优先级：P2**
**风险：中**
**范围：`app/native_shell/composer_pane.rs`、`src/runtime/protocol.rs`、`src/runtime/dispatch.rs`**
**依赖：VUI-304**

**目标**

发送按钮位于右下角，左下角为附件入口，支持图片与文件。

**工作**

1. 布局调整：输入区之下，左下 `+`（附件），右下发送。thinking 选择器已由 VUI-304 移走。
2. `DesktopRuntimeCommand::SubmitPrompt` 扩展为携带附件列表。
   附件必须按 Desktop 既有约定做 bounded 校验（与 `validate_prompt` 同级）。
3. 产品侧直接使用既有能力：`CodingAgentPromptImage::new`、`MAX_INPUT_IMAGES`、
   `block_images` / `auto_resize_images`、`append_file_reference`。
4. 当前模型 `supports_images` 为假时，附件入口需明确禁用并说明原因。
5. 超限行为按「待定问题 2」的结论实现。

**不得改变**

- composer 的 auto-grow 范围与 min/max 高度；
- Enter / Shift+Enter 的提交语义；
- 输入延迟探针（`InputRenderLatencyProbe`）的采样路径。

**完成条件**

- 新增测试：附件超限被拒绝且草稿不丢失；
- 新增测试：不支持图片的模型下附件入口禁用；
- click-to-photon 与 composer latency gate 无回归；
- 三档 golden 更新并附 review note。

**建议提交**

```text
feat(desktop): rearrange the composer and accept attachments
```

---

### VUI-306：会话列表显示名称

**优先级：P2**
**风险：低**
**范围：`app/native_shell/sessions_pane.rs`、`session_controller.rs`**
**依赖：CAG-105、CAG-106**

**目标**

列表显示会话名称，无名称时显示「未命名」，id 降级为次要信息。

**工作**

1. 主标题显示名称，`detail` 位显示时间或 id 前缀。
2. 名称为 `None` 时显示「未命名」。
3. 搜索同时匹配名称与 id。
4. 提供手动重命名入口（自动命名失败或用户不满意时的兜底）。

**完成条件**

- 新增测试：有名称与无名称两种会话的显示与搜索；
- 三档 golden 更新并附 review note。

**建议提交**

```text
feat(desktop): show session names in the sessions list
```

---

### VUI-307：Inspector Tab 横向滚动

**优先级：P3**
**风险：低**
**范围：`app/native_shell/inspector_pane.rs`**
**依赖：DSK-503**

**目标**

Tab 由四等分改为固定宽度 + 横向滚动，为未来扩展留出空间。

**工作**

1. `flex_1().min_w_0()`（`inspector_pane.rs:645-651`）改为固定宽度 tab +
   容器 `overflow_x_scroll`。
2. 选中 tab 在滚出视口时需自动滚入。
3. 键盘导航（左右方向键在 TabList 内移动）不得因滚动失效。
4. Runtime tab 的 badge 在滚动容器中位置正确。

**不得改变**

- `role(Role::TabList)` / `role(Role::TabPanel)` 与 aria 标签；
- 四个 section 的内容与 per-session 选中记忆。

**完成条件**

- 新增测试：最小 / 最大面板宽度下 tab 不换行、不裁切；
- 窄视口 overlay 形态正常；
- 三档 golden 更新并附 review note。

**建议提交**

```text
feat(desktop): make inspector tabs horizontally scrollable
```

---

### VUI-308：Conversation block 去卡片背景

**优先级：P3**
**风险：中**
**范围：`app/native_shell/conversation_pane.rs`**
**依赖：DSK-503**

**目标**

消息不再是圆角填充卡片，视觉更轻。

**工作**

1. 移除 block 的 `bg(visual.surface)` 与圆角卡片形态（`conversation_pane.rs:281-291`）。
2. **hover 与 selection 当前也用 bg 表达，必须改用其他载体。**
   复用 `DesktopActionRow` 的 accent 竖条模式（`desktop_controls.rs:467-476`）——
   该模式已经过验证：无颜色模式下可读，且不改变行高。
3. 角色区分（user / assistant / tool / reasoning）改用前导标记与文字色阶，不用底色块。
4. 嵌套的 detail 区域（`theme.elevated` 底色）保留，它承担的是「这是被折叠的次要内容」
   的层级语义，与 block 底色不同。

**不得改变**

- 行高与测量语义（本任务不得引入新的高度抖动，须与 VUI-301 的不变量共存）；
- keyboard focus 的可见性；
- 无颜色模式下的可读性。

**完成条件**

- 新增测试或 fixture：no-color 与 keyboard-focus 两个既有 golden 仍能区分选中与 hover；
- conversation 性能 gate 无回归；
- 三档 golden 更新并附 review note，明确说明选中/hover 的新表达方式。

**建议提交**

```text
style(desktop): drop the conversation block card background
```

---

### VUI-309：顶栏状态仅在非空闲时显示

**优先级：P3**
**风险：低**
**范围：`app/native_shell/conversation_header.rs`**
**依赖：DSK-513**

**目标**

常驻的 `idle` 不再占位；顶栏状态成为「有事才出现」的指示器。

**工作**

1. 顶栏状态区在 idle 时不渲染；运行中、等待授权、错误时渲染。
2. 状态消失/出现不得改变顶栏其余元素的位置（预留宽度或使用绝对定位）。
3. 会话级运行状态由 VUI-306/DSK-513 的列表状态点承担，两者不重复表达同一信息。

**完成条件**

- 新增测试：idle 与 running 两态下顶栏其余元素位置一致；
- 三档 golden 更新并附 review note。

**建议提交**

```text
style(desktop): show the header status only when not idle
```

---

### VUI-310：左侧面板加入「新建对话」与「技能」

**优先级：P3**
**风险：低**
**范围：`app/native_shell/sessions_pane.rs`**
**依赖：DSK-503、CAG-103**

**目标**

左侧面板承载三块内容：新建对话、全局技能、历史会话。

**工作**

1. 顶部「新建对话」入口，点击进入待机界面（不立即创建会话）。
2. 全局技能列表，数据来自 CAG-103 的全局技能入口。**只展示全局 skills**（已确认决策），
   不解析项目级 `skills_dirs`。
3. 历史会话列表保持既有整行交互与搜索。
4. 三块之间的分区在窄视口 overlay 形态下同样可用。

**完成条件**

- 新增测试：待机态与有会话态下面板三块内容均可渲染；
- 新增测试：点击新建对话不产生会话目录；
- 三档 golden 更新并附 review note。

**建议提交**

```text
feat(desktop): add new-conversation and skills sections to the sessions panel
```

---

## 七、测试与验收矩阵

沿用 [`desktop架构.md`](./desktop架构.md) 第七节全部条目，并补充：

| 不变量 | 主要测试/验证 |
| --- | --- |
| 启动不落盘 | 启动后会话根目录条目数不变 |
| 无会话态命令分类 | runtime idle-without-session 定向测试 |
| 首次提交原子建会话 | dispatch 定向测试（成功 / 建成但启动失败） |
| 待机态 view model 完整性 | shell 无会话态六个 view model 测试 |
| 草稿跨待机→会话迁移 | composer 定向测试 |
| scratch 工作区隔离 | cwd 断言 + 不解析项目 `.evo` |
| 多会话事件不串流 | runtime 双会话定向测试 |
| 后台会话不触发重绘 | notify 计数断言 |
| 工作台上限 4 | 第 5 个会话拒绝测试 |
| 会话名称向后兼容 | 旧 manifest 反序列化测试 |
| 自动命名失败降级 | 命名 operation 失败路径测试 |
| 折叠展开锚定 | layout 累计偏移与重复展开测试 |
| 附件边界 | prompt 附件校验测试 |
| 焦点环去除 Status | 键盘导航测试 |

每个任务至少执行：

```bash
cargo fmt --all -- --check
cargo test -p desktop
cargo test -p desktop --test dependency_boundary
cargo clippy -p desktop --all-targets -- -D warnings
git diff --check
```

`CAG-*` 任务额外执行：

```bash
cargo test -p coding-agent
cargo clippy -p coding-agent --all-targets -- -D warnings
cargo test -p cli
cargo test -p tui
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

`CAG-*` 与 `DSK-*` 不应更新 visual golden；截图变化先判定为回归。
`VUI-*` 预期产生视觉变化，必须执行：

```bash
scripts/desktop-visual-golden.sh --review
scripts/desktop-visual-golden.sh --update --review-note FILE
scripts/desktop-visual-golden.sh
```

review note 必须说明目标 surface、控件语义变化、wide/medium/narrow 结果与
accessibility 影响。

**新增视口要求：** DSK-503 起，golden 集合需增加待机态的 wide / medium / narrow
三档，共十档 fixture。

## 八、提交与评审策略

沿用 [`desktop架构.md`](./desktop架构.md) 第八节。补充两条：

1. `coding-agent` 的改动必须与 Desktop 改动**分开提交**，且每个 `CAG-*` 提交
   本身要能通过 CLI 与 TUI 的全量测试 —— 它们是这些 API 的既有消费者。
2. DSK-503 会打断 `native_shell.rs` 中的源码文本断言（如 `native_shell.rs:6541`）。
   处理这些断言的改动必须在**同一提交**内完成并说明原因，不得留下被注释掉的测试。

## 九、停止条件

出现以下情况立即停止并重新评估：

- 移除启动期会话后，出现任何「UI 认为有会话但运行时没有」的状态不一致；
- 多会话改造导致任一会话的 ProductEvent 顺序、ack 或 recovery 语义发生变化；
- 后台会话触发前台重绘，性能 gate 出现回归；
- 会话名称的 manifest schema 改动无法向后兼容旧会话；
- 自动命名 operation 影响首轮对话的终态语义或 usage 统计；
- `VUI-308` 去掉卡片底色后，no-color 模式下无法区分选中与未选中。

## 十、待定问题

以下问题不阻塞开工，但在对应任务开始前需要答复：

1. **待机界面的最近会话是否按项目分组？** 会话自带 `cwd`，分组技术上可行。
   影响 DSK-503、VUI-310。
2. **附件超出 `MAX_INPUT_IMAGES` 或字节上限时，拒绝还是自动压缩？**
   `auto_resize_images` 设置已存在。影响 VUI-305。
3. **已决策：toast 最多堆叠 3 条并停留 6 秒。** 支持手动关闭；鼠标悬停或键盘焦点位于 toast 内时
   暂停自动隐藏。VUI-302 已按此策略实现与验收。
4. **自动命名失败是否重试？** 还是一次失败即保持「未命名」，由用户手动改名。
   影响 CAG-106、VUI-306。
5. **已决策：上限 4 不包含待机态工作区。** 待机 Home 不持有 session projection，也不占运行会话
   名额；DSK-513 已按此语义实现与验收。
6. **本轮是否加入会话删除？** `coding-agent` 目前没有 delete API；一旦列表显示名称，
   用户会预期可以删除。若纳入，需新增 `CAG-107`。影响 DSK-513、VUI-306。

## 十一、后续（不在本轮）

- 多项目切换器与跨项目注册表；
- git 分支显示（需 `coding-agent` 侧暴露 git 事实）；
- Inspector 内的文件编辑（需重新定义 Desktop 的分层定位）；
- 会话 fork / tree 的图形化导航（`CodingAgentSessionTreeSnapshot` 已具备数据）。
