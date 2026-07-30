# desktop 原生桌面适配器结构精简重构计划

> 状态：执行中（DSK-700 已完成，下一项 DSK-701）
> 决策日期：2026-07-31
> 最近更新：2026-07-31
> 调研基线 commit：`7766b06974b861565dcc31bf0aa011c7ba6643e6`
> 正式执行基线：`01920b86761bec633d326493055f0658d29a3485`（开始执行时工作区干净）
> 适用范围：`crates/desktop`、相关 Desktop gate 脚本、`docs/architecture.md`
> 兼容策略：允许破坏 `desktop` 内部模块、类型、测试路径与私有协议；不保留长期兼容层
> 总原则：先收敛状态权威，再移动文件；一个事实只有一个 owner；每个 Phase 可验证；最终清零迁移债务

## 一、执行摘要

本计划把 `desktop` 从「以 `NativeShell` 为隐式 application layer 的 GPUI 大对象」收敛为：

```text
bootstrap / composition root
            │
            ▼
     GPUI UI adapter
       NativeShell
            │ UiIntent / DesktopEvent
            ▼
   application reducer
   ├── DesktopState
   ├── WorkspaceStore
   ├── CommandTracker
   ├── UiChangeSet
   └── DesktopEffect
       │              │
       ▼              ▼
runtime backend    platform adapters
       │
       ▼
 coding-agent API
```

核心变化不是继续拆 Pane，而是改变状态所有权：

| 当前 | 目标 |
| --- | --- |
| `NativeShell` 同时拥有 runtime、workspace、业务交互、child entities、focus、preferences | `NativeShell` 只做 GPUI 组装、事件转发、根布局和 effect 执行 |
| `Deref/DerefMut` 把 active workspace 伪装成 Shell 字段 | workspace 访问全部显式通过 `WorkspaceStore` |
| Home 使用字符串 `"home"` 充当 session key | `WorkspaceKey::Home` / `WorkspaceKey::Session(SessionId)` |
| command ID 在 Shell 全局生成，pending intent 分散在 per-workspace ledger | 单一全局 `CommandTracker<CommandId, PendingCommand>` |
| 一个 runtime update 在 Shell、commands、projection、completion 多处匹配 | 一个 application reducer 完整解释一次 |
| 多套 dirty bool 和 `notify_*` 分散 | reducer 返回唯一 `UiChangeSet`，Shell 统一刷新 |
| UI、review projection、process launch、preferences storage 混装 | application / UI / runtime / platform 单向分层 |
| dependency test 锁死 child 文件名集合 | dependency test 只守卫依赖方向和 authority |

本轮不拆新的 crate。`desktop` 的公共面只有 `DesktopApplicationOptions + run()`，内部代码尚无独立
发布或复用需求；此时拆 crate 会迫使内部类型公开化，却不能解决状态权威混乱。只有在 runtime backend
被第二个 GUI/Web adapter 复用、需要脱离 GPUI 独立编译，或实测构建时间证明有收益时，才另立任务评估。

## 二、已核实的基线

### 2.1 规模

| 区域 | 当前规模 | 说明 |
| --- | ---: | --- |
| `crates/desktop` Rust 总行数 | 39,863 | 含 unit/integration tests |
| `app/native_shell.rs` | 11,150 | 约 5,244 行生产代码，约 5,906 行内嵌测试 |
| `app/native_shell` 子树 | 21,345 | 超过 crate 一半，含 conversation/pane/tests |
| `runtime` 子树 | 8,222 | 含 3,859 行 `runtime/tests.rs` |
| `NativeShell` | 四十余字段 | 多个不同生命周期被平铺在一个 struct |
| `DesktopRuntimeCommand` | 18 variants | command admission 与发送接口存在重复 forwarding |
| `DesktopRuntimeUpdate` | 23 variants | 多处 match 共同解释一次更新 |
| `runtime/bridge.rs` 的 `try_*` | 41 个 | 正式 handle 与 test bridge 存在两套接口 |

### 2.2 当前健康资产，必须保留

- `desktop` 只依赖 `coding-agent` 产品 facade，不直接依赖 `ai`、`agent-core`、`tui`。
- crate 公共 API 保持为 `DesktopApplicationOptions` 与 `run()` 两项。
- runtime 与 UI 使用 typed command/update、bounded priority/data channel 和显式 shutdown owner。
- `DesktopProjection` 基于 `CodingAgentClientProjection`，支持 snapshot/event、resync 与 typed delta。
- conversation model/layout/viewport/render cache 基本为可脱离 GPUI 测试的纯逻辑。
- child entities 已采用 `ViewModel + Event`，可作为 UI 边界继续演进。
- dependency boundary、visual golden、GPUI interaction、headless/native performance gate 已存在。
- accessibility、responsive layout、reduced motion 与 bounded rendering 约束不因重构降低。

### 2.3 根因

1. `NativeShell` 实现 `Deref/DerefMut<Target = SessionWorkspace>`，root 与 active workspace 的字段命名空间被合并。
2. `active_workspace` 被物理移出 `workspaces`，切换时需要 swap 和 Home 特判。
3. `HOME_COMPOSER_SESSION_KEY = "home"` 同时承担 UI surface 与 session identity。
4. command ID 是 Shell 全局权威，pending intent 却是 workspace 局部权威。
5. `poll_runtime`、`reconcile_direct_update`、`ProjectionCommandCompletions`、`DesktopProjection::apply` 共同解释 update。
6. `commands.rs` 和 `project_catalog_controller.rs` 通过 `use super::*` 访问整个 Shell 私有面。
7. view-model 构建集中在 root，导致 leaf view 虽不导入 authority，root 仍知道所有 feature 细节。
8. `preferences.rs`、`file_review.rs` 分别混合纯模型与 I/O/process 实现。
9. exact child-module inventory test 把文件布局误当成稳定架构契约。

## 三、不可破坏的产品约束

允许激进删除内部代码，不代表允许悄悄改变产品语义。以下约束必须保持：

- `coding-agent` 仍是 session、operation、authorization、recovery 与 durable product facts 的唯一权威。
- Desktop 不从 cwd 字符串猜测 project/session identity，不共享一个可变 product owner 给多个 session。
- 每个 runtime update 保留 session identity、command identity 与 sequence/resync 语义。
- priority/data channel 的有界性、overflow/resync 策略与 shutdown/join 语义不退化。
- pending command 必须按 typed owner 和 intent 完成，过期/错配 response fail closed。
- Home、Project、Projectless、durable Session 行为保持，迁移后不再靠字符串哨兵表达。
- selective dirty routing 保留；不得用全量 child refresh 掩盖 reducer 设计问题。
- 文件 review、外部编辑器、preferences 原子写与 symlink 防护不降低。
- 现有 visual fixtures、focus trap/restore、keyboard navigation、accessibility roles 保持。
- 现有 bounded text、transcript、review、cache、attachment 和 queue 限制保持。
- 不通过批量更新 golden 隐藏非预期视觉回归。

## 四、成功标准

### 4.1 状态权威

- `NativeShell` 不再实现 `Deref/DerefMut`。
- 不存在 Home/session 字符串 sentinel。
- active workspace 由 typed key 引用，不在 map 内外物理换入换出。
- command ID、owner、intent、pending/completed 状态只有一个 `CommandTracker` 权威。
- `SessionWorkspace` 不再持有 command ledger。
- 删除 `command_owner_session_id`、`complete_workspace_command`、`reserve_with_id` 工作流。

### 4.2 更新与副作用

- 每个 `DesktopRuntimeUpdate` variant 只在一个 application reducer 中做完整业务解释。
- `DesktopProjection` 只处理 projection-relevant 输入，不 match catalog/editor/纯 acknowledgement。
- reducer 不依赖 `gpui::Context`、`Window`、filesystem、process 或 thread。
- reducer 返回 `Transition { changes, effects }`，不直接调用 runtime/platform/UI。
- 所有 UI refresh 由一个 `refresh_views(UiChangeSet)` 入口执行。
- 所有外部副作用都能在 `DesktopEffect` 中枚举并追踪 completion。

### 4.3 UI 与模块

- `NativeShell` 生产代码目标为 800～1,200 行；行数不是唯一 Gate，但其中不得再有业务 reducer。
- `NativeShell` 字段收敛为 `controller`、`views`、`ui`、`subscriptions` 等少量聚合。
- child event 全部先转换为 `UiIntent`。
- feature presenter 为无副作用函数，读取 state 并生成 ViewModel。
- leaf UI 不导入 runtime client、command tracker、preference store。
- 生产代码零 `use super::*`。
- application 层零 GPUI import；runtime 层零 GPUI/UI import；platform 层不依赖 UI。

### 4.4 可验证性

- reducer 的 workspace、command completion、runtime update、effect 与 dirty routing 有纯单元测试。
- GPUI test 只承担 hit test、focus、render、entity notification、responsive 与 accessibility。
- native shell 不再内嵌约 6,000 行测试。
- dependency boundary 守卫依赖方向，不锁死文件名列表。
- `cargo test -p desktop --all-targets` 全绿，无新增 ignored。
- visual golden compare、headless performance gate、native performance gate 全部通过。
- `docs/architecture.md` 与最终目录和数据流一致。

## 五、目标类型与数据流

### 5.1 Workspace identity 与 store

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkspaceKey {
    Home,
    Session(SessionId),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SessionId(Arc<str>);

struct WorkspaceStore {
    active: WorkspaceKey,
    entries: HashMap<WorkspaceKey, WorkspaceState>,
}
```

最低 API：

```rust
impl WorkspaceStore {
    fn active_key(&self) -> &WorkspaceKey;
    fn active(&self) -> &WorkspaceState;
    fn active_mut(&mut self) -> &mut WorkspaceState;
    fn get(&self, key: &WorkspaceKey) -> Option<&WorkspaceState>;
    fn get_mut(&mut self, key: &WorkspaceKey) -> Option<&mut WorkspaceState>;
    fn insert(&mut self, key: WorkspaceKey, state: WorkspaceState);
    fn activate(&mut self, key: &WorkspaceKey) -> Result<WorkspaceActivation, WorkspaceError>;
    fn remove_session(&mut self, id: &SessionId) -> Option<WorkspaceState>;
}
```

`WorkspaceState` 保留 per-workspace 的 product/presentation 状态：

- project snapshot；
- optional `DesktopProjection`；
- composer draft/submission/attachments；
- conversation controller；
- inspector selection 与 file review；
- thinking selection/hint；
- workspace-local notice（若确认确属 workspace scope）。

它不再拥有全局 command sequence、runtime client、preferences writer、GPUI entity 或 focus handle。

### 5.2 单一 CommandTracker

```rust
struct CommandTracker {
    next_id: u64,
    pending: HashMap<u64, PendingCommand>,
    capacity: usize,
}

struct PendingCommand {
    owner: WorkspaceKey,
    intent: CommandIntent,
}
```

最低 API：

```rust
impl CommandTracker {
    fn reserve(&mut self, owner: WorkspaceKey, intent: CommandIntent)
        -> Result<u64, CommandAdmissionError>;
    fn pending(&self, command_id: u64) -> Option<&PendingCommand>;
    fn complete(&mut self, command_id: u64, expected: &CommandIntent)
        -> Result<PendingCommand, CommandCompletionError>;
    fn reject(&mut self, command_id: u64) -> Option<PendingCommand>;
    fn cancel_owner(&mut self, owner: &WorkspaceKey) -> Vec<PendingCommand>;
}
```

要求：

- command ID 只在这里递增；overflow/ID exhaustion typed fail closed。
- completion 先按 ID 定位，再验证 intent/owner，不遍历 workspace。
- close workspace 时显式处理其 pending commands。
- runtime update 中携带的 session identity 与 tracker owner 不一致时触发 issue/resync，不静默接收。

### 5.3 Application reducer

```rust
enum DesktopEvent {
    Ui(UiIntent),
    Runtime(DesktopRuntimeUpdate),
    Platform(PlatformResult),
    Timer(DesktopTimer),
}

struct Transition {
    changes: UiChangeSet,
    effects: Vec<DesktopEffect>,
}

struct DesktopController {
    state: DesktopState,
}

impl DesktopController {
    fn reduce(&mut self, event: DesktopEvent) -> Transition;
}
```

`reduce()` 内部可按 feature 调用私有纯函数，但外部只有一个事件入口：

```text
reduce_ui_intent
reduce_runtime_update
reduce_platform_result
reduce_timer
```

禁止重新创建多个可以独立解释同一 `DesktopRuntimeUpdate` 的 reducer。feature reducer 只能接收 root
reducer 已分类后的窄事件。

### 5.4 Effect

```rust
enum DesktopEffect {
    SendRuntime(RuntimeRequest),
    PersistPreferences(DesktopPreferences),
    PickProjectDirectory(PickerRequestId),
    PickAttachments(PickerRequestId),
    WriteClipboard(Arc<str>),
    OpenExternalEditor(ExternalEditorRequest),
    ArmTimer(DesktopTimer, Duration),
    RequestWindowClose,
}
```

原则：

- Effect 只描述跨边界动作，不把普通状态更新包装成 effect。
- 异步 effect 必须携带 request identity，结果以 `PlatformResult` 回到 reducer。
- runtime command 的 command ID 在 effect 产生前已由 `CommandTracker` 注册。
- effect executor 失败必须回送 typed result，不能直接从 GPUI callback 修改 application state。
- 不为每个 effect 创建 trait；生产上确有第二实现时再抽象。

### 5.5 UiChangeSet

```rust
#[derive(Debug, Default, Clone, Copy)]
struct UiChangeSet {
    shell: bool,
    sessions: bool,
    header: bool,
    conversation: bool,
    composer: bool,
    inspector: bool,
    modal: bool,
    drawer: bool,
    toast: bool,
}
```

要求：

- 支持 `merge`，批量 runtime update 只在尾部刷新一次。
- 现有 `DesktopProjectionDelta` / `ProjectionDirtyRouting` 转换为 `UiChangeSet` 的一个来源。
- catalog、workspace switch、command completion、preferences、focus/modal 等变化也汇入同一类型。
- 只有 UI adapter 能把 `UiChangeSet` 转成 entity `set_view_model` / `cx.notify()`。

### 5.6 精简后的 NativeShell

```rust
struct NativeShell {
    controller: DesktopController,
    runtime: RuntimeConnection,
    views: ShellViews,
    ui: ShellUiState,
    subscriptions: Vec<Subscription>,
}
```

如果 effect executor 独立成对象，`runtime` 可归入 `DesktopEffectExecutor`；最终字段组织以生命周期清晰为准，
不要求机械地恰好四个字段。

`NativeShell` 仅保留：

- GPUI child entity 创建与 subscription wiring；
- runtime event stream 有界 poll；
- `dispatch(DesktopEvent)`；
- `execute_effects()`；
- `refresh_views()`；
- focus/modal/drawer/panel resize 等纯 UI adapter 协调；
- root `Render`。

## 六、目标目录

```text
crates/desktop/src/
├── lib.rs
├── bootstrap.rs
├── application/
│   ├── mod.rs
│   ├── state.rs
│   ├── workspace.rs
│   ├── commands.rs
│   ├── reducer.rs
│   ├── change_set.rs
│   └── effect.rs
├── runtime/
│   ├── mod.rs
│   ├── protocol.rs
│   ├── client.rs
│   └── worker/
│       ├── mod.rs
│       └── dispatch.rs
├── ui/
│   ├── shell/
│   │   ├── mod.rs
│   │   ├── state.rs
│   │   ├── layout.rs
│   │   └── overlays.rs
│   ├── sessions/
│   │   ├── view.rs
│   │   └── catalog.rs
│   ├── conversation/
│   │   ├── model.rs
│   │   ├── controller.rs
│   │   ├── view.rs
│   │   ├── layout.rs
│   │   ├── viewport.rs
│   │   └── render_cache.rs
│   ├── composer.rs
│   ├── inspector/
│   │   ├── view.rs
│   │   └── review.rs
│   ├── home.rs
│   └── components/
│       ├── controls.rs
│       ├── style.rs
│       ├── brand.rs
│       └── streaming_text.rs
├── platform/
│   ├── preferences/
│   │   ├── model.rs
│   │   └── store.rs
│   ├── workspace.rs
│   ├── external_editor.rs
│   ├── file_picker.rs
│   ├── clipboard.rs
│   └── assets.rs
└── devtools/
    └── native_replay.rs
```

这是一张职责地图，不是要求预先创建全部空文件。任务执行时只创建已经承接真实代码的模块；最终不允许
空目录、只做 re-export 的临时模块或为目录对称制造的空壳。

## 七、Gate 分级

### Gate A：每个任务

```bash
cargo fmt --check
cargo check -p desktop --all-targets
cargo clippy -p desktop --all-targets -- -D warnings
cargo test -p desktop --test dependency_boundary
cargo test -p desktop --lib
git diff --check
```

如果 DSK-700 发现当前存在既有 clippy warning，必须在 DSK-700 清理，或记录精确 warning 并在
DSK-701 前清零；Phase 1 起不允许使用“既有 warning”豁免。

### Gate B：每个 Phase

```bash
cargo test -p desktop --all-targets
```

按变更范围附加：

- 改 UI render/layout/view model：`scripts/desktop-visual-golden.sh` compare。
- 改 conversation/render cache/refresh routing：`scripts/desktop-perf-gate.sh`。
- 改 runtime channel/poll/shutdown：运行 runtime ordering/shutdown 定向测试。
- 改 focus/modal/drawer：运行对应 GPUI interaction tests。

### Gate C：最终验收

```bash
cargo fmt --check
cargo clippy -p desktop --all-targets -- -D warnings
cargo test --workspace --all-targets
scripts/desktop-perf-gate.sh
scripts/desktop-visual-golden.sh
scripts/desktop-native-perf-gate.sh
```

`desktop-visual-golden.sh` 需要 X11 `DISPLAY`；`desktop-native-perf-gate.sh` 需要 `DISPLAY` 或
`WAYLAND_DISPLAY`。无法在当前环境执行时不能假定通过，必须在有图形环境的同一最终 commit 上补跑并记录结果。

## 八、任务总览

| Phase | 任务 | 目标 | 依赖 |
| --- | --- | --- | --- |
| 0 | DSK-700 | 冻结基线、测试/性能清单 | 无 |
| 0 | DSK-701 | 将边界测试从文件清单升级为依赖方向守卫 | DSK-700 |
| 1 | DSK-710 | 删除 `NativeShell` 的隐式 `Deref/DerefMut` | DSK-701 |
| 1 | DSK-711 | 引入 typed `WorkspaceKey/WorkspaceStore`，删除 Home sentinel/swap | DSK-710 |
| 2 | DSK-720 | 建立单一全局 `CommandTracker` | DSK-711 |
| 2 | DSK-721 | 收敛 runtime command admission/client 接口 | DSK-720 |
| 3 | DSK-730 | 建立 `DesktopEvent/Transition/Effect/UiChangeSet` | DSK-721 |
| 3 | DSK-731 | 把 runtime update reconciliation 移入单一 reducer | DSK-730 |
| 3 | DSK-732 | 收窄 `DesktopProjection` 输入并统一 dirty routing | DSK-731 |
| 3 | DSK-733 | UI/platform 异步结果全部回流 reducer | DSK-732 |
| 4 | DSK-740 | 聚合 `ShellViews/ShellUiState`，瘦身 Shell 字段 | DSK-733 |
| 4 | DSK-741 | 下放 feature presenter/view-model builder | DSK-740 |
| 4 | DSK-742 | child event 统一转换为 `UiIntent` | DSK-741 |
| 4 | DSK-743 | 收敛 root render、focus、modal、drawer 与 refresh | DSK-742 |
| 5 | DSK-750 | 拆分 preferences model/store/workspace platform | DSK-743 |
| 5 | DSK-751 | 拆分 review presentation 与 external-editor platform | DSK-750 |
| 5 | DSK-752 | 整理 runtime protocol/client/worker | DSK-751 |
| 5 | DSK-753 | 按 feature 重组 UI 目录，合并碎片模块 | DSK-752 |
| 6 | DSK-760 | 迁移/降级 native shell 大型测试 | DSK-753 |
| 6 | DSK-761 | 拆分 runtime tests 与 fixture | DSK-760 |
| 6 | DSK-762 | 隔离 native replay/perf devtools | DSK-761 |
| 7 | DSK-770 | 删除兼容层、旧模块与执行债务 | DSK-762 |
| 7 | DSK-771 | 最终 Gate、结构指标与架构文档 | DSK-770 |

## 九、详细执行任务

### Phase 0：冻结基线与架构守卫

#### DSK-700：冻结真实基线

目标：在任何结构修改前记录可复现的行为、性能、视觉和工作区状态。

工作：

1. 先处理或提交当前未提交修改；不得把它们混入本计划的第一个重构 commit。
2. 记录实际执行基线 commit、Rust toolchain、操作系统、图形后端。
3. 记录：
   - `cargo test -p desktop --all-targets` 的通过/ignored 数；
   - clippy warning 数；
   - 39,863 等代码规模指标的最新值；
   - `native_shell.rs` 生产/测试行数；
   - visual golden compare 结果；
   - headless/native perf 指标与日志路径。
4. 为关键行为建立测试映射：workspace switch、command completion、resync、picker、preferences、focus、modal、review。
5. 确认所有 dirty working-tree 文件的归属；不覆盖用户未提交内容。

产物：在本文“执行记录”追加实际基线，不创建第二份临时计划。

Gate：完整 Gate B；有图形环境时同时跑 Gate C 的两个图形脚本。

建议 commit：`docs(desktop): freeze architecture refactor baseline`

#### DSK-701：重写 dependency boundary 守卫

主要文件：`crates/desktop/tests/dependency_boundary.rs`。

工作：

- 保留：公共 API 两项、只依赖 `coding-agent` facade、runtime 禁止 GPUI、gate artifacts 存在。
- 删除 `native_shell_has_one_explicit_child_module_graph` 的 exact filename 集合。
- 增加生产代码禁止 `use super::*` 的 AST 守卫。
- 增加 application 层禁止 `gpui`、`std::fs`、`std::process`、thread/tokio owner 的守卫。
- 增加 leaf UI 禁止引用 runtime client、command tracker、preference store 的守卫。
- 增加 platform 禁止依赖 `ui`，runtime 禁止依赖 `ui/application presentation type` 的守卫。
- 守卫按 identifier/import/manifest 结构判断，不搜索注释字符串。
- 初始目录尚未形成时，守卫允许目标目录不存在；后续任务创建目录后自动生效。

删除：对 legacy module filename 与现有 child module 全集的硬编码。

Gate：Gate A；对守卫做一次变异验证，例如临时让 runtime import `gpui` 并确认测试失败，随后撤销变异。

建议 commit：`test(desktop): enforce authority dependency directions`

### Phase 1：显式化 workspace 所有权

#### DSK-710：删除 `NativeShell` 的 `Deref/DerefMut`

主要文件：`app/native_shell.rs`、`app/native_shell/commands.rs`、`project_catalog_controller.rs`。

工作：

- 删除 `impl Deref for NativeShell` 与 `impl DerefMut for NativeShell`。
- 在不改变现有 storage 结构的前提下，把所有隐式访问改成显式：
  `self.active_workspace.project`、`self.active_workspace.projection` 等。
- 为频繁的只读/可变访问增加少量具名 helper；helper 必须表达 owner，不得复刻 `Deref`。
- `commands.rs` 改为显式 import；禁止继续用 wildcard 隐藏字段来源。
- 区分 Shell-global notice 与 workspace-local notice，暂不搬迁但记录所有调用点。

删除：`std::ops::{Deref, DerefMut}` import 与两个 impl。

完成标准：

- `rg 'impl Deref.*NativeShell|impl DerefMut.*NativeShell'` 零结果；
- 所有 workspace 字段访问从源码可见 owner；
- 行为和视觉不变，不更新 golden。

Gate：Gate A + workspace/session 定向测试。

建议 commit：`refactor(desktop): make active workspace ownership explicit`

#### DSK-711：typed WorkspaceKey 与稳定 WorkspaceStore

主要文件：新 `application/workspace.rs`、`app/native_shell.rs`、project catalog/controller tests。

工作：

- 引入 `WorkspaceKey::{Home, Session(SessionId)}`。
- 引入 Desktop 内部 `SessionId` newtype；与 `coding-agent` DTO 在 runtime/application 边界转换。
- 引入 `WorkspaceStore { active, entries }`。
- Home 作为普通 typed entry 插入 store。
- active workspace 不再从 map 中 remove/swap。
- 改写 create/open/close/activate/home navigation。
- 明确关闭 active session 后的 fallback：优先 Home；不得靠 map iteration 顺序决定。
- 明确 background runtime update 只修改目标 entry，不触碰 active UI state；需要时仅标记 sessions/toast。
- 删除所有 `HOME_COMPOSER_SESSION_KEY` 分支和 string comparison。

删除：

- `HOME_COMPOSER_SESSION_KEY`；
- `swap_active_workspace` 及其 map 搬运逻辑；
- Home 可被当作 durable session 的测试 fixture/特殊分支。

测试：

- Home ↔ durable session 切换保留各自 draft、attachments、conversation viewport。
- background session update 不污染 active session。
- close active/background session 的确定性 fallback。
- 真实 session ID 为 `"home"` 时不与 Home surface 冲突。
- store capacity 与 eviction/拒绝策略保持 typed、确定。

Gate：Gate A + Gate B；视觉不应变化。

建议 commit：`refactor(desktop): replace workspace sentinel with typed store`

### Phase 2：单一 command authority

#### DSK-720：全局 CommandTracker

主要文件：`command_ledger.rs`、`native_shell/commands.rs`、新 `application/commands.rs`、runtime tests。

工作：

- 将 `next_command_id`、pending commands 与 capacity 合并进 `CommandTracker`。
- `PendingCommand` 同时持有 typed owner 与 intent。
- reserve 返回 ID 后再产生 runtime effect；发送失败必须回滚同一个 pending entry。
- completion 先按 ID 定位，再校验 expected intent、update session 与 owner。
- rejection、authorization、recovery、selection、review、external editor 全部复用同一 completion API。
- close workspace 时处理 pending commands：取消并产生 bounded notice；不得遗留幽灵 completion。
- 为 command ID exhaustion、capacity full、stale ID、intent mismatch、owner mismatch 建纯测试。

删除：

- `SessionWorkspace.command_ledger`；
- `NativeShell.next_command_id`；
- `command_owner_session_id`；
- `complete_workspace_command`；
- `reserve_with_id`；
- 遍历 workspaces 查 command owner 的代码。

完成标准：command ID 与 pending intent 各只有一个存储位置；`rg 'command_ledger'` 只剩迁移后模块名或零结果。

Gate：Gate A + command queue/admission/rejection/resync 定向测试。

建议 commit：`refactor(desktop): centralize pending command authority`

#### DSK-721：收敛 runtime command client 与 admission

主要文件：`runtime/bridge.rs`、`runtime/protocol.rs`、runtime tests。

工作：

- 生产代码只保留一个可 clone 的 `RuntimeCommandClient`。
- Bridge 只负责 bootstrap 后拆分为 command client、event stream、shutdown owner。
- 消除 `DesktopRuntimeBridge` test-only `try_*` 与正式 handle `try_*` 的两套 forwarding。
- validation 选择单一权威：typed command constructors 或 command client，不能两边重复。
- test harness 直接观察 protocol command，不要求 Bridge 镜像全部生产方法。
- 保留 bounded `try_send`、typed admission error 与 debug redaction。
- 命名统一：`Bridge` 表示连接所有权，`Client` 表示 command side，`EventStream` 表示 update side。

删除：重复 forwarding、仅为测试复刻的 command façade、重复 validation。

完成标准：每个 command 的验证逻辑只有一份；Bridge 不再是第二个 command client。

Gate：Gate A + runtime command queue、closed/full、redaction tests。

建议 commit：`refactor(desktop): unify runtime command admission client`

### Phase 3：单一 application reducer

#### DSK-730：建立 application contract

主要文件：新 `application/{mod,state,reducer,effect,change_set}.rs`。

工作：

- 定义 `DesktopEvent`、`UiIntent`、`PlatformResult`、`DesktopTimer`。
- 定义 `Transition`、`DesktopEffect`、`UiChangeSet`。
- 建立 `DesktopState`，先容纳 `WorkspaceStore`、`CommandTracker`、catalog、preferences model 等非 GPUI 状态。
- `DesktopController::reduce` 先以现有逻辑 delegation 方式接入，不允许双写 state。
- effect identity 必须 typed；picker/timer/editor result 不可只凭“当前 active workspace”关联。
- 为 `UiChangeSet::merge` 和 effect/result identity 建纯测试。

约束：本任务只建立可用 contract，不预创建空 feature reducer，不引入通用 event bus。

Gate：Gate A；application 目录的 dependency guard 生效。

建议 commit：`refactor(desktop): introduce application transition contract`

#### DSK-731：迁移 runtime update reconciliation

主要文件：`native_shell.rs`、`native_shell/commands.rs`、`application/reducer.rs`。

工作：

- 把 `runtime_update_session_id` 逻辑移入 root runtime reducer。
- 把 `reconcile_direct_update` 全部 branch 移入 reducer。
- 把 `ProjectionCommandCompletions::{capture,reconcile}` 合并进同一个 update branch。
- 把 `poll_runtime` 中 Prompt accepted/rejected/started/finished、control、authorization、runtime failed/stopped 的状态转移移入 reducer。
- background/foreground routing 使用 `WorkspaceKey`，不再依赖 active state 隐式字段。
- 每个 update branch 同时决定 command completion、state mutation、notice、projection event、changes/effects。
- `poll_runtime` 只负责 bounded batch read、逐项 reduce、merge transition、安排下一次 poll。

删除：

- `DirectCommandUpdate`；
- `ProjectionCommandCompletions`；
- `native_shell/commands.rs` 中访问整个 Shell 的 reconciliation；
- Shell 内分散的 update-specific dirty bool。

测试：为 23 个 update variants 建 coverage table；新增 variant 未登记时编译或测试失败。

Gate：Gate A + runtime/projection 全套 unit tests + Gate B。

建议 commit：`refactor(desktop): reduce runtime updates through one authority`

#### DSK-732：收窄 DesktopProjection 与统一 dirty routing

主要文件：`projection.rs`、`native_shell/update.rs`、`application/change_set.rs`、conversation controller。

工作：

- 定义窄 `ProjectionEvent` 或等价 typed input。
- application reducer 从 `DesktopRuntimeUpdate` 提取 projection-relevant payload。
- `DesktopProjection::apply` 不再接收 catalog/editor/纯 acknowledgement variants。
- 保留 product client projection 的 duplicate/gap/resync 语义。
- 把 `ProjectionDirtyRouting` 合并为 `DesktopProjectionDelta -> UiChangeSet` 转换。
- conversation controller 只接收 projection delta 和目标 workspace，不接触 root Shell。
- batch updates 合并 `UiChangeSet`，每 frame 最多刷新一次对应 child。

删除：`DesktopProjection::apply` 中所有 `NoChange` 纯协议 arm、旧 `native_shell/update.rs`。

Gate：Gate A + projection gap/duplicate/resync/replacement tests + headless performance gate。

建议 commit：`refactor(desktop): narrow projection events and unify invalidation`

#### DSK-733：所有异步结果回流 reducer

主要文件：composer/project picker、clipboard、file review、external editor、preferences scheduling、timer callbacks。

工作：

- project-directory picker、attachment picker 用 request ID 关联 workspace。
- picker callback 只 dispatch `PlatformResult`，不直接调用 `set_project_directory`/修改 attachments。
- clipboard、external editor、preferences writer error 转为 effect result/typed notice。
- telemetry、conversation height refresh、toast expiry 等 timer 使用 typed timer identity。
- window close/shutdown effect 明确 owner；Drop 只做兜底，不承载主业务路径。
- 清理所有 async callback 中基于“当前 active workspace”推断原请求 owner 的逻辑。

完成标准：跨 await/thread/channel 的状态变更都重新进入 reducer；没有第二条隐藏状态通道。

Gate：Gate A + picker race、switch-during-picker、writer error、timer stale result tests。

建议 commit：`refactor(desktop): route platform completions through reducer`

### Phase 4：瘦身 NativeShell 与 UI feature

#### DSK-740：ShellViews 与 ShellUiState

主要文件：`app/native_shell.rs`、新 `ui/shell/{mod,state}.rs`。

工作：

- 把所有 child `Entity<T>` 聚合为 `ShellViews`。
- 把 focus handles、modal/drawer/palette、panel resize、input modality、announcement 等聚合为 `ShellUiState`。
- runtime/event/shutdown owner 归入明确 connection/executor 类型。
- `NativeShellInit` 改为 composition-root 输入，不暴露可由 state 派生的重复字段。
- subscriptions 统一由 `ShellViews::subscribe` 或 root wiring 构建。
- 聚合不是空壳：每个聚合拥有自己的 invariant/helper；禁止只为减少字段数套单字段 wrapper。

Gate：Gate A + focus/modal/drawer interaction tests。

建议 commit：`refactor(desktop): group shell view and ui lifecycles`

#### DSK-741：下放 presenter 与 ViewModel builder

主要文件：sessions/header/composer/inspector/conversation/root modal/center drawer modules。

工作：

- 把 `sessions_pane_view_model`、`composer_pane_view_model`、`inspector_pane_view_model`、
  `conversation_pane_view_model`、`conversation_header_view_model` 移到对应 feature presenter。
- presenter 输入为 `&DesktopState`、`&WorkspaceState`、`&ShellUiState` 的最窄组合。
- presenter 不发送命令、不修改 state、不读 filesystem/process、不持有 GPUI context。
- 共享 label/status projection 放入 UI components/presentation helper，不回流 application。
- 去除 root 对每个 ViewModel 字段构造细节的了解。

Gate：Gate A + ViewModel snapshot/equality tests；visual golden compare 必须无变化。

建议 commit：`refactor(desktop): move view presentation into features`

#### DSK-742：统一 child event -> UiIntent

主要文件：所有 Pane/Event、NativeShell subscriptions。

工作：

- 为 sessions、header、composer、conversation、inspector、modal、drawer 定义 typed `UiIntent` mapping。
- subscription closure 只记录必要的原始 GPUI 信息并 dispatch intent。
- select/copy/toggle/recovery/review/submit/rename/model/profile/thinking 等动作不再直接调用 root 业务方法。
- GPUI-specific measurement/focus event 可以先归一化为 typed payload，再进 reducer/UI state handler。
- 删除 root 中仅用于转发 child event 的 `on_*`/feature action 方法。

约束：不引入字符串 action 名；继续使用 typed enum/action。

Gate：Gate A + keyboard/action/pane interaction tests。

建议 commit：`refactor(desktop): dispatch typed intents from child views`

#### DSK-743：收敛 root Render、refresh、focus 与 overlays

主要文件：`ui/shell/mod.rs`、layout、root modal、center drawer、toast host。

工作：

- 建立唯一 `refresh_views(UiChangeSet)`。
- root `Render` 只组合 layout regions 与 overlay hosts，不构建 feature ViewModel。
- focus/modal/drawer 保留在 UI adapter，但 state transition 有单一入口。
- panel resize 只写 UI state/preferences intent，不直接操作 store。
- 合并 root modal/drawer/toast 的重复 host coordination；保留 modal 与 drawer 不同焦点语义。
- 将 `NativeShell` 生产区控制到目标范围；如果仍超过 1,200 行，按剩余职责继续拆，而不是按行数机械切割。

删除：分散的 `notify_*`、重复 `set_view_model` 序列、root 中已迁移 presenter/业务 handler。

Gate：Gate A + Gate B + visual golden + headless performance。

建议 commit：`refactor(desktop): reduce native shell to gpui adapter`

### Phase 5：platform/runtime/UI 目录收敛

#### DSK-750：preferences model/store/workspace 拆分

主要文件：`preferences.rs`、bootstrap、新 `platform/preferences/*`、`platform/workspace.rs`。

工作：

- `model.rs`：schema、defaults、normalization、thinking-level map。
- `store.rs`：load/save、atomic write、symlink/permission、writer thread。
- `workspace.rs`：scratch workspace ID/path 创建与安全校验。
- bootstrap 负责组合 store/writer/workspace resolver。
- application 只持有 normalized preferences model 和 writer effect，不知道文件路径/线程。
- 保留 schema version 与不可信输入 normalization 行为。

Gate：Gate A + preferences corruption/oversize/symlink/roundtrip/scratch tests。

建议 commit：`refactor(desktop): separate preference model from storage`

#### DSK-751：review presentation 与 external-editor platform

主要文件：`file_review.rs`、inspector、runtime protocol/driver、新 platform external editor。

工作：

- review document、diff rows、clipboard export 归入 `ui/inspector/review.rs` 或无 GPUI presentation model。
- editor config/validation/invocation/process launch 归入 `platform/external_editor.rs`。
- runtime protocol 不再依赖 review UI 模块。
- 外部编辑器启动失败以 typed platform result 回 reducer。
- 保留 bounded path/line/render/clipboard limits 和安全 process args。

删除：混合 presentation/process 的原 `file_review.rs`；若无内容则删除文件而非保留 re-export 壳。

Gate：Gate A + review projection、clipboard bound、editor validation/launch error tests。

建议 commit：`refactor(desktop): isolate file review and editor adapters`

#### DSK-752：runtime protocol/client/worker 整理

主要文件：`runtime.rs`、`runtime/{bridge,protocol,driver,dispatch}.rs`。

工作：

- `runtime/mod.rs` 只声明/re-export Desktop 内部稳定 contract。
- `client.rs` 承载 bootstrap、command client、event stream、shutdown owner。
- `worker/` 承载 runtime state、session owner、event pump、dispatch。
- protocol 只含 DTO、validation/limits 与 kind labels，不执行 process/UI/storage。
- 检查 driver/dispatch 的职责边界；只在确有独立状态机时保留两个文件。
- 删除旧路径 re-export，不保留 deprecated module alias。

Gate：Gate A + runtime tests + shutdown/panic/overflow/reconnect tests。

建议 commit：`refactor(desktop): organize runtime client and worker boundaries`

#### DSK-753：按 feature 重组 UI

主要移动：

- `conversation/*` + `native_shell/conversation_*` → `ui/conversation/`；
- sessions pane + catalog controller → `ui/sessions/`；
- inspector pane + review → `ui/inspector/`；
- controls/style/brand/streaming text → `ui/components/`；
- shell layout/focus/navigation/overlay → `ui/shell/`；
- Home/Skills 按真实职责合并或保留单文件。

工作规则：

- 本任务以移动/import 修正为主，不同时重写算法。
- `center_navigation.rs`、旧 `update.rs` 等不足以独立表达概念的碎片模块合并到 owner。
- 纯模型不因目录位于 `ui/` 就引入 GPUI。
- 不创建 `model.rs/controller.rs/view.rs` 空模板；只搬真实代码。
- 移动完成后删除全部旧路径和空目录。

Gate：Gate A + Gate B；visual golden compare。

建议 commit：`refactor(desktop): organize presentation by feature`

### Phase 6：测试与 devtools 收敛

#### DSK-760：拆出 native shell 大型测试并下沉纯逻辑

工作：

- `native_shell.rs` 只保留 `#[cfg(test)] mod tests;`。
- 测试按 `workspace`、`commands`、`runtime_updates`、`focus`、`responsive`、`overlays`、`fixtures` 分类。
- workspace/command/reducer/dirty routing 测试下沉 application，不启动 GPUI。
- GPUI tests 只保留 hit-test、focus、render/entity、responsive、accessibility。
- 合并重复 fixture builder；不恢复通用 test-only 后门。
- 删除仅验证私有函数调用顺序、对行为无约束的脆弱测试。

完成标准：native shell 主文件无大型内嵌测试；测试总行为覆盖不降低，运行时间应下降或记录原因。

Gate：Gate A + Gate B + visual golden。

建议 commit：`test(desktop): move shell behavior into focused suites`

#### DSK-761：拆分 runtime tests

将 3,859 行单文件按状态机概念拆为：

```text
runtime/tests/
├── admission.rs
├── ordering.rs
├── overflow.rs
├── reconnect.rs
├── recovery.rs
├── shutdown.rs
└── fixtures.rs
```

规则：

- fixture 只承载构造数据，不封装 assertion。
- 每个测试文件对应一个 runtime invariant。
- 不通过 public visibility 扩大生产 API；测试作为模块子树访问 `pub(super)`/private owner。
- 删除重复 setup 和相同 race 场景的镜像测试，保留边界值与不同不变量。

Gate：Gate A + runtime test 全量。

建议 commit：`test(desktop): organize runtime invariants by state machine`

#### DSK-762：隔离 native replay/performance devtools

主要文件：`native_perf.rs`、visual fixtures、scripts 中的测试路径。

工作：

- 将 replay request/spec/fixture 归入 `devtools/native_replay.rs`。
- production 默认路径不持有 fixture authority；只有显式 replay env 才进入 devtools。
- 更新性能脚本中的测试全限定路径。
- 保留真实生产 Render/event path，禁止为 replay 创建第二套 UI。
- 检查 fixture-only enum/安装器的 `cfg`/可见性，能只在 test/devtools 编译的就不进入普通路径。

Gate：Gate A + 两个 performance gate + visual golden。

建议 commit：`refactor(desktop): isolate native replay tooling`

### Phase 7：删除遗留与最终收敛

#### DSK-770：零债务清理

必须删除：

- 旧 `app/native_shell.rs` 中已迁移的 compatibility forwarding；
- 旧 `command_ledger.rs`、`native_shell/commands.rs`、`native_shell/update.rs`（若已无真实职责）；
- deprecated module alias/re-export；
- `HOME_COMPOSER_SESSION_KEY` 与相关 fixture；
- `NativeShell` Deref helper；
- Bridge test command façade；
- exact child module graph test；
- 旧 preferences/file_review 混合模块；
- 空目录、空 `mod.rs`、单纯 re-export 壳；
- 临时 `TODO(DSK-*)`、dual-write、fallback 分支、feature flag。

执行全文审计：

```bash
rg -n 'TODO\(DSK-|HOME_COMPOSER_SESSION_KEY|impl Deref.*NativeShell|use super::\*' crates/desktop
rg -n 'command_owner_session_id|complete_workspace_command|reserve_with_id' crates/desktop
```

所有结果必须为零，或在本文最终记录中逐项说明为什么是新的稳定概念；不能把未完成项移出本计划。

Gate：Gate A + Gate B。

建议 commit：`refactor(desktop): remove migration debt and legacy modules`

#### DSK-771：最终架构、性能与文档验收

工作：

1. 执行 Gate C 并记录结果。
2. 对比基线与最终结构指标：
   - desktop 总行数；
   - production/test 行数；
   - `NativeShell` 行数/字段/方法；
   - runtime client `try_*`/validation 重复数；
   - production wildcard imports；
   - application 的 GPUI imports；
   - test 数与 ignored 数；
   - headless/native perf 与 RSS。
3. 更新 `docs/architecture.md` 的 desktop 目录、数据流、测试路径和 dependency rules。
4. 把本文状态改为“已完成”，逐 task 填写实际偏差与 Gate。
5. 检查 workspace 未包含无关改动、生成 artifact 或未评审 golden。

完成标准：本节成功标准全部满足；没有执行债务；文档与代码相符。

建议 commit：`docs(desktop): finalize adapter architecture refactor`

## 十、删除优先清单

本计划明确允许并鼓励删除错误结构。以下内容不保留兼容：

| 旧结构 | 删除时点 | 替代 |
| --- | --- | --- |
| `NativeShell` 的 `Deref/DerefMut` | DSK-710 | 显式 workspace owner |
| `HOME_COMPOSER_SESSION_KEY` | DSK-711 | `WorkspaceKey::Home` |
| active workspace map swap | DSK-711 | stable `WorkspaceStore` + active key |
| per-workspace `DesktopCommandLedger` | DSK-720 | global `CommandTracker` |
| `command_owner_session_id` | DSK-720 | `pending[command_id].owner` |
| Bridge/Handle 双份 command API | DSK-721 | 单一 `RuntimeCommandClient` |
| `DirectCommandUpdate` | DSK-731 | root runtime reducer |
| `ProjectionCommandCompletions` | DSK-731 | 同一 reducer branch 完成 command |
| projection 的纯 `NoChange` protocol arms | DSK-732 | 窄 `ProjectionEvent` |
| 分散 `notify_*` | DSK-743 | `refresh_views(UiChangeSet)` |
| preferences 混合 model/storage 文件 | DSK-750 | platform preferences 子模块 |
| review/process 混合文件 | DSK-751 | inspector review + platform editor |
| exact child module inventory test | DSK-701 | dependency-direction guards |
| 旧路径 re-export/compat alias | DSK-770 | 无；调用点一次性迁移 |

## 十一、执行债务协议

允许单个任务内部短暂存在迁移债务，但必须遵守：

1. 债务在引入时写入下表，包含删除任务和精确路径。
2. 债务不能跨越约定删除任务后继续存在。
3. 禁止用 `deprecated`、feature flag、dual-write、fallback adapter 把债务永久化。
4. Phase 7 开始前所有债务必须有明确删除 commit；DSK-770 后表必须为空。

| 债务 ID | 引入任务 | 临时内容 | 路径 | 删除任务 | 状态 |
| --- | --- | --- | --- | --- | --- |
| — | — | 当前无计划内债务 | — | — | — |

## 十二、风险与处置

| 风险 | 处置 |
| --- | --- |
| 当前工作区已有未提交的 desktop 修改 | DSK-700 先冻结/提交；计划任务不覆盖不明修改 |
| 删除 Deref 后改动面巨大 | 单独 DSK-710，只显式化 owner，不同时改 storage 语义 |
| WorkspaceStore 重构破坏 background update | typed key + foreground/background 定向测试；active 不移出 entries |
| command completion 与 workspace switch race | global tracker 记录 owner；completion 不依赖当前 active workspace |
| reducer 变成新的 God Object | root 只做一次分类；feature reducer 接收窄事件，禁止重复解释 RuntimeUpdate |
| Effect 系统过度抽象 | 只枚举跨边界副作用，不给每种 effect 建 trait/manager |
| selective refresh 漏通知 | 唯一 `UiChangeSet` + transition tests + native interaction/perf gate |
| 文件搬迁与逻辑重构混在一起难审查 | Phase 1～4 先改变职责，DSK-753 后期纯搬迁 |
| visual golden 被无意更新 | 默认只 compare；更新必须走 `--review` 与 review note 流程 |
| performance 因 presenter/reducer clone 回退 | ViewModel 使用 Arc/共享 DTO；每 Phase 跑适用 perf gate |
| platform 拆分后在 GUI 线程执行阻塞 I/O | effect executor 保持后台执行；completion 回 reducer |
| 为了测试扩大生产可见性 | 测试作为子模块；禁止新增 test-only 公共 façade |
| 计划结束仍残留兼容层 | DSK-770 全文审计，债务表必须清空 |

## 十三、明确不做

- 不在本轮拆 `desktop-core` / `desktop-ui` / `desktop-runtime` 新 crate。
- 不改变 `coding-agent` 的产品权威、event sourcing 或公开产品语义。
- 不重做视觉设计、主题、交互文案或 shortcut 产品规格。
- 不以“文件小于 N 行”为唯一目标，不机械制造 `Manager`、trait、event bus。
- 不让 child Pane 直接持有 runtime client。
- 不为 mock 给每个 service 定义 trait。
- 不保留旧私有模块路径、deprecated alias 或兼容 feature。
- 不通过全量刷新 child entities 简化 dirty routing。
- 不通过更新 golden 掩盖结构重构造成的视觉变化。
- 不为了提高测试数恢复已经失去不变量价值的旧测试；测试必须对应明确风险。

## 十四、执行记录

### DSK-700 实际基线

正式执行基线为 `01920b86761bec633d326493055f0658d29a3485`。开始执行时工作区干净，
分支 `main` 相对 `origin/main` ahead 10；调研时的 dirty worktree 已由此前提交收敛，未混入本计划修改。

环境：LMDE 7（Debian trixie）、Linux `6.12.95+deb13-amd64`、
`x86_64-unknown-linux-gnu`、Rust/Cargo `1.96.0`、X11 `DISPLAY=:0`。

| 项目 | 基线 | 最终 | 变化 |
| --- | ---: | ---: | ---: |
| desktop Rust 总行数 | 39,863 | — | — |
| `native_shell.rs` 总行数 | 11,150 | — | — |
| `native_shell.rs` 生产代码 | 5,245 | — | — |
| `native_shell.rs` 内嵌测试 | 5,905 | — | — |
| desktop tests passed | 298（lib 289 + boundary 9） | — | — |
| ignored | 5（均为显式 release performance gate） | — | — |
| clippy warnings | 初始 1；清理后 0 | — | — |
| native frame P95/P99 | 4,724 / 4,994 µs | — | — |
| input-to-post-render P95 | 8,414 µs | — | — |
| RSS absolute/steady growth | 152,715,264 / 163,840 bytes | — | — |

Gate 与性能记录：

- `cargo test -p desktop --all-targets`：通过，298 passed、5 ignored、0 failed；最慢 GPUI 用例约 68 秒。
- `cargo clippy -p desktop --all-targets -- -D warnings`：通过。基线清理删除 1 个未使用样式 helper，
  并修复 2 个 `coding-agent` attribute lint 与 2 个无谓字符串分配 lint；没有使用 warning 豁免。
- `scripts/desktop-perf-gate.sh`：通过，日志 `target/desktop-perf/latest.log`。10k block headless
  CPU frame P95 2,662 µs、input roundtrip P95 5,753 µs、input-change-to-render P95 352 µs；
  window RSS growth 24,281,088 bytes。
- `scripts/desktop-native-perf-gate.sh`：通过，日志 `target/desktop-perf/native-latest.log`。200 frame
  GPU/present P95/P99 4,724/4,994 µs；50 input sample P95/P99 8,414/16,663 µs；最终 RSS
  152,715,264 bytes、steady growth 163,840 bytes；production Markdown P95 135 µs。
- `scripts/desktop-visual-golden.sh`：初次 compare 发现此前 4 个已提交 conversation UI commit 未同步 golden；
  按 `--review` 检查全部 20 组 before/after/diff 后，用 manifest + review note 安装基线。12 组原本
  pixel-identical，8 组变化均局限于已提交的 conversation presentation；安装后复跑 20 组 RMSE 全为 0。
  review 说明保存在 `crates/desktop/tests/goldens/native/REVIEW.md`。

关键行为测试映射：

| 风险 | 主要基线测试 |
| --- | --- |
| workspace switch / owner 隔离 | `background_workspace_advances_silently_and_switching_restores_scoped_state`、`closing_a_background_workspace_removes_only_its_owner`、`typed_navigation_switches_skills_session_and_home_without_runtime_commands` |
| command completion / stale identity | `terminal_and_projection_completion_are_identity_bound`、`stale_or_mismatched_completion_cannot_remove_another_intent`、`first_session_change_rekeys_the_home_workspace_and_completes_its_command` |
| gap / resync / reconnect | `desktop_projection_rejects_gaps_and_association_mismatches_atomically`、`reconnect_state_machine_handles_gap_lag_and_exhaustion_deterministically`、`create_and_resync_update_local_state_without_loading_the_catalog` |
| picker ownership / bounds | `composer_picker_attaches_bounded_paths_and_forwards_them_with_the_prompt`、`project_directory_picker_failures_are_bounded_and_do_not_replace_selection`、`project_directory_menu_chooses_replaces_cancels_and_clears` |
| preferences storage / safety | `preferences_round_trip_and_normalize_untrusted_geometry`、`background_writer_coalesces_without_blocking_the_caller`、`symbolic_link_file_and_directory_are_rejected` |
| focus / modal / responsive | `modal_traps_focus_and_restores_visible_owner`、`native_shell_authorization_smoke_traps_focus_and_submits_a_typed_decision`、`responsive_drawers_preserve_conversation_geometry_scroll_and_owner_focus` |
| review / external editor | `unified_diff_is_bounded_and_marks_collapsed_unchanged_ranges`、`editor_configuration_rejects_shells_nuls_and_argument_pressure`、`native_shell_inspector_smoke_submits_recovery_and_file_review_commands` |

### 任务状态

| 任务 | 状态 | commit | Gate/偏差 |
| --- | --- | --- | --- |
| DSK-700 | 已完成 | 待提交 | Gate B、clippy、headless/native perf、reviewed visual compare 全通过；同步 stale conversation goldens |
| DSK-701 | 待执行 | — | — |
| DSK-710 | 待执行 | — | — |
| DSK-711 | 待执行 | — | — |
| DSK-720 | 待执行 | — | — |
| DSK-721 | 待执行 | — | — |
| DSK-730 | 待执行 | — | — |
| DSK-731 | 待执行 | — | — |
| DSK-732 | 待执行 | — | — |
| DSK-733 | 待执行 | — | — |
| DSK-740 | 待执行 | — | — |
| DSK-741 | 待执行 | — | — |
| DSK-742 | 待执行 | — | — |
| DSK-743 | 待执行 | — | — |
| DSK-750 | 待执行 | — | — |
| DSK-751 | 待执行 | — | — |
| DSK-752 | 待执行 | — | — |
| DSK-753 | 待执行 | — | — |
| DSK-760 | 待执行 | — | — |
| DSK-761 | 待执行 | — | — |
| DSK-762 | 待执行 | — | — |
| DSK-770 | 待执行 | — | — |
| DSK-771 | 待执行 | — | — |

---

<sub>文档版本：1.0 | 调研基线：7766b06（dirty worktree）</sub>
