# Evo 架构设计文档

> 版本：0.7.2 | 语言：Rust (Edition 2024)

---

## 1. 项目概览

**Evo** 是一个用 Rust 编写的多模型、多界面的 AI 编程助手。它允许开发者通过终端或桌面应用与多种大语言模型（Claude、GPT、Gemini、Mistral 等）进行编码交互。

### 核心能力

- **多 AI 提供商**：Anthropic (Claude)、OpenAI (GPT/Codex)、Google (Gemini)、Mistral、Azure OpenAI
- **多界面形态**：交互式 TUI（全屏）、无头打印模式、JSON 模式、RPC 模式（JSONL stdio）、原生桌面应用（gpui）
- **代码操作工具**：read、write、edit、grep、find、ls、bash 执行
- **代理系统**：Agent/Team 配置文件、委托、压缩（对话摘要）、思考级别控制
- **会话管理**：持久会话日志、事件溯源架构、快照/重放
- **授权系统**：工具调用风险分级、交互式确认、基于范围的授权
- **自愈编辑**：运行时编辑失败的自动修复

### 部分关键设计约束

- **版本锁定**：所有 6 个 crate 通过 `workspace.package.version` 共享同一版本号，确保版本一致性
- **稳定性承诺**：每个 crate 通过 `api` 命名空间暴露分类的稳定公共 API；多数 crate 使用
  `api.rs`，`coding-agent` 在 `lib.rs` 内联定义该门面，实现细节完全私有
- **零循环依赖**：依赖方向严格单向：`ai` ← `agent-core` ← `coding-agent` ← `cli`/`desktop`；`tui` 作为独立通用组件库被 `cli` 依赖

---

## 2. 分层架构总览

```
┌──────────────────────────────────────────────────────────────┐
│                     UI 适配器层（Presentation）                │
│  ┌──────────────┐  ┌──────────────────┐  ┌────────────────┐  │
│  │  CLI (cli)   │  │ Desktop (desktop) │  │ 未来第三方客户端  │  │
│  │ • TUI 全屏   │  │ • gpui GUI       │  │                │  │
│  │ • 无头模式   │  │ • 原生 Shell      │  │                │  │
│  │ • 无头模式   │  │ • 外部编辑器       │  │                │  │
│  │ • RPC 模式   │  │                   │  │                │  │
│  └──────┬───────┘  └────────┬─────────┘  └───────┬────────┘  │
│         │                   │                    │            │
│  ┌──────┴───────────────────┴────────────────────┴────────┐  │
│  │              coding-agent（产品层 / Product Layer）       │  │
│  │  • 会话生命周期管理  • 事件溯源持久化  • 操作调度引擎      │  │
│  │  • 工具授权与审查    • 代理/团队配置文件  • 客户端投映     │  │
│  │  • 配置/主题管理     • 自愈编辑         • 压缩策略        │  │
│  └──────────┬──────────────────────────┬───────────────────┘  │
│             │                          │                      │
│  ┌──────────┴──────────┐   ┌───────────┴──────────────────┐  │
│  │    agent-core       │   │          tui                 │  │
│  │  （代理运行时层）     │   │    （通用终端 UI 组件库）     │  │
│  │  • Agent 回合引擎    │   │  • 终端能力协商              │  │
│  │  • 钩子系统          │   │  • 组件系统 (Markdown/Editor) │  │
│  │  • 压缩/摘要         │   │  • 输入/按键绑定              │  │
│  │  • 文件系统/Shell    │   │  • 渲染/样式引擎              │  │
│  │  • 资源加载器        │   │  • iTerm2/Kitty 图像协议     │  │
│  └──────────┬──────────┘   └──────────────────────────────┘  │
│             │                                                  │
│  ┌──────────┴──────────────────────┐                          │
│  │            ai                   │                          │
│  │     （AI 提供商抽象层）           │                          │
│  │  • 提供商注册与路由              │                          │
│  │  • 模型目录 (100+ 模型)          │                          │
│  │  • 协议无关的请求/响应类型        │                          │
│  │  • HTTP 传输与 SSE 流解析        │                          │
│  │  • 跨提供商兼容性适配             │                          │
│  └─────────────────────────────────┘                          │
└──────────────────────────────────────────────────────────────┘
```

**依赖方向**（严格单向，不可逆）：

```
ai  ←  agent-core  ←  coding-agent  ←  cli
                                    ←  desktop
                         tui  ←  cli
```

---

## 3. Crate 详细设计

### 3.1 `ai` — AI 提供商抽象层

**定位**：整个架构的最底层，封装与 AI 服务商通信的全部细节。

```
crates/ai/src/
├── lib.rs               # 私有模块声明 + 公开 api 模块
├── api.rs               # 分类的稳定公共 API（7 个子领域）
├── client.rs            # AiClient：ProviderRegistry 持有者
├── compatibility.rs     # 跨提供商兼容性配置
├── model/               # 模型元数据与目录
│   ├── catalog.rs       # 模型目录查询接口
│   └── generated.json   # 100+ 模型定义（~19k 行）
├── providers/           # 内置提供商实现
│   ├── anthropic/       # Claude Messages API
│   ├── openai/completions/  # Completions API
│   ├── openai/responses/    # OpenAI Responses 请求转换
│   ├── responses/       # Responses SSE 共享解析器
│   ├── openai_codex_responses/
│   ├── azure_openai_responses/
│   ├── deepseek/        # DeepSeek Responses API
│   ├── google/          # Gemini
│   └── mistral/
├── protocol/            # 协议无关的消息/请求/工具类型
│   ├── stream.rs        # EventStream、增量 JSON 解析
│   └── hooks.rs         # 提供商请求/响应钩子
├── registry/            # 提供商注册表
│   ├── env.rs           # 基于环境变量的 API Key 解析
│   └── resolver.rs      # AuthResolver 实现
└── transport/           # HTTP 传输层
    ├── error.rs         # ProviderError/ProviderErrorKind
    ├── retry.rs         # 重试策略（解析 Retry-After）
    └── sse.rs           # SSE 流解析
```

**核心抽象**：

- **`ApiProvider` trait**：统一流式调用接口，每个提供商实现 `stream(context, options) -> EventStream`
- **`AiClient`**：持有 `ProviderRegistry` + `AuthResolver`，通过 `stream_model()` 路由请求到对应提供商
- **`ProviderRegistry`**：基于 HashMap 的注册表，按 API 名称路由（如 `"anthropic"`、`"openai"`）

**公共 API 分类**：

| 类别 | 说明 |
|---|---|
| `api::model` | 模型元数据、目录查询、成本计算 |
| `api::conversation` | 协议无关的 Message/Context/Usage 类型 |
| `api::stream` | EventStream、流式 JSON 解析 |
| `api::hooks` | 提供商请求/响应钩子 |
| `api::client` | AiClient 构造 |
| `api::auth` | 基于环境变量的认证解析 |
| `api::provider` | 提供商注册合同 |
| `api::error` | ProviderError 错误分类 |
| `api::transport` | 重试配置、HTTP 策略 |
| `api::compatibility` | 跨提供商兼容性配置 |

---

### 3.2 `agent-core` — 代理运行时核心

**定位**：提供中性、低级的代理运行时，不包含任何产品策略或适配器逻辑。

```
crates/agent-core/src/
├── lib.rs              # 私有模块 + 公开 api 模块
├── api.rs              # 分类稳定 API（6 个子领域）
├── agent/              # 代理运行时
│   ├── runtime.rs      # Agent：Arc<RwLock<AgentState>> 线程安全运行时
│   ├── turn/           # 回合引擎（状态机）
│   │   ├── runtime.rs  # AgentTurnRunner：有界状态机
│   │   ├── context.rs  # AgentTurnContext
│   │   └── nodes.rs    # 每个状态的执行节点
│   └── types/          # AgentConfig/AgentMessage/AgentEvent 等
├── compaction/         # 压缩与摘要
│   ├── estimate.rs     # Token 估算（基于字符的启发式）
│   ├── prepare.rs      # 压缩判定
│   └── summarize.rs    # 摘要生成
├── context/            # AgentMessage → Provider Context 转换
├── execution/          # 执行环境抽象
│   ├── capture.rs      # Shell 输出捕获
│   └── truncate.rs     # 输出截断
├── hooks/              # 代理生命周期钩子（7 种）
├── resources/          # 技能和提示模板加载器
└── transcript/         # 会话记录和树投映
```

**核心设计：回合引擎状态机**

代理的每一次"思考+行动"循环配置为一个有限的确定状态机：

```
Start
  → DrainQueuedInput (处理积压输入)
  → CompactRuntimeContext (压缩决策)
  → PrepareProviderRequest (构建 AI 请求)
  → ApplyProviderHook (应用提供钩子)
  → ProviderStream (流式接收 AI 响应)
  → DecideAfterAssistant (决定下一步)
  → ExecuteTools (执行工具调用)
  → PrepareNextTurn (准备下一个回合)
  → (循环回 Start 或 Finish)
```

**安全设计**：最大 9 个合法状态，TURN_STATE_VISIT_FUSE = 16（熔断器），防止死循环。

**核心抽象**：

| 抽象 | 说明 |
|---|---|
| `Agent` | 线程安全的代理运行时，`Arc<RwLock<AgentState>>` |
| `AgentConfig` | 代理配置（工具列表、系统提示、压缩设置等） |
| `AgentMessage` | 代理消息（用户消息、工具结果等） |
| `AgentEvent` | 代理事件（流增量、错误、使用量等） |
| `AgentHooks` | 7 种生命周期钩子（BeforeToolCall/AfterToolCall 等） |
| `FileSystem` / `Shell` | 文件系统和 Shell 抽象接口 |

**公共 API 分类**：

| 类别 | 说明 |
|---|---|
| `api::agent` | Agent 运行时、配置、事件、钩子 |
| `api::tool` | 工具定义、执行上下文、输出 |
| `api::execution` | 文件系统/Shell 抽象契约 |
| `api::resources` | 技能/提示模板加载 |
| `api::compaction` | Token 估算、压缩/摘要 |
| `api::transcript` | 会话记录、树投映 |

---

### 3.3 `coding-agent` — 产品层

**定位**：产品策略、会话事实和适配器边界的承载层。CLI/Desktop 只依赖公开 facade，
不直接操作 repository、provider、tool 或 outbox。

生产代码采用五层单向依赖；`tests/module_layering.rs` 解析 Rust AST，阻止反向引用和
layer cycle：

```text
L4 api / adapters   app、runtime/facade、lib.rs 中的 api::*
        |
        v
L3 application      application、operations、services、session、events、tools、
                    runtime（facade 除外）、domain/projection、resources
        |
        v
L2 domain           authorization、config、profiles、theme、workspace
        |
        v
L1 platform         fs/process/io/time ports、mutex policy
        |
        v
L0 kernel           ids、operation/control/capability values、errors、limits
```

高层可以直接依赖任意更低层，但低层不能引用高层。`domain/projection` 名称表达的是
事实投影用途；由于它跨多种 representation 做集成转换，依赖守卫将它归为 L3。
有状态 service 通过 `SessionWriter`、`EventSink`、`CapabilityQuery` 等窄 port 协作，
composition root 才装配具体实现。

**API 边界**：crate root 唯一公共模块是 `coding_agent::api`，并按 `embedding`、
`settings`、`authorization`、`runtime`、`error`、`review`、`operation`、`event`、
`client`、`view` 分类。其他源码模块都是实现细节。会话仓储通过
`SessionStorageHandle` 暴露明确的“打开事件日志”和“取得导出路径”操作，不向适配器
泄漏可任意拼接的 session directory。

**核心设计：事件溯源架构**

所有产品级状态变更都以不可变事件的形式持久化和分发：

```
用户操作 → CodingAgentOperation → 处理 → ProductEvent → EventService 持久化
                                                      → 客户端投映更新
                                                      → UI 事件桥接
```

公开的 `ProductEventKind` 顶层 family 为 Session、Agent、Team、Message、Tool、Runtime、
Delegation、Workflow、Diagnostic、Capability。跨 representation 的转换集中在
`domain/projection/`，并由覆盖全部 family 的 golden fixture 固定 wire round-trip。

**取消语义**：取消是 cooperative request，不是任意位置的线程中断。prompt 的
`abort` 必须经 operation-scoped control handle 提交；丢弃 operation task 只会分离
join handle。异步阶段会观察 cancellation token，但文件 mutation 一旦把
`MutationGuard` 转移到 blocking closure，write/truncate 与 `sync_all` 会在 fence 内
完成，调用方 future 被取消也不会提前释放 fence。关闭流程因此必须停止新准入、请求
shutdown、abort/join active operation、消费终态，再 drain session。

**有界 hydration 契约**：普通打开从 active event log 尾部以 64 KiB 分块反向扫描，
最多物化 10,000 个事件或 32 MiB，并在视图中返回 `omitted_items` 与 opaque
continuation。该路径只修复 torn tail，不创建 writer、不读取 outbox、也不做全量
startup replay；客户端 projection 仍保留相同数量/字节预算作为二次防线。只有显式
`SessionExport` API 可以触发完整 replay，适配器不能把 UI bootstrap 当作完整归档。

**公共 API 分类**（10 个子领域）：

| 类别 | 说明 |
|---|---|
| `api::embedding` | 供第三方客户端嵌入使用的构建 API |
| `api::settings` | 有界的产品运行时和适配器展现设置 |
| `api::authorization` | 工具调用授权请求与决策 |
| `api::runtime` | 会话生命周期和运行时入口点 |
| `api::error` | 安全、有界的适配器错误类型 |
| `api::review` | 文件审查请求/响应 |
| `api::operation` | 操作命令和结果 |
| `api::event` | 可持续和实时的产品事件契约 |
| `api::client` | 客户端连接、提交、快照、恢复 |
| `api::view` | 只读视图和展现 DTO |

---

### 3.4 `cli` — 命令行界面

**定位**：面向终端的用户界面，支持多种运行模式。

```
crates/cli/src/
├── main.rs            # 入口：解析参数 → 按 CliMode 调度
├── cli/               # CLI 工具
│   ├── args.rs        # 参数解析
│   ├── headless.rs    # 无头/打印/JSON 模式
│   ├── list_models.rs # 模型列表输出
│   └── io.rs          # I/O 工具
├── interactive/       # 交互式 TUI（25 个模块）
│   ├── app.rs         # TUI 应用入口
│   ├── loop.rs        # 主事件循环
│   ├── render.rs      # UI 渲染
│   ├── event_bridge.rs # 产品事件 → UI 事件桥接
│   ├── transcript.rs  # 对话树展示
│   ├── input.rs       # 用户输入处理
│   ├── slash.rs       # 斜杠命令
│   ├── commands.rs    # 命令分发
│   ├── syntax.rs      # 语法高亮（syntect）
│   └── theme.rs       # TUI 主题
├── rpc/               # RPC 模式（JSONL stdio）
└── protocol/          # RPC 命令/事件类型
```

**运行模式**：

| 模式 | 说明 | 入口 |
|---|---|---|
| `CliMode::Rpc` | JSONL stdio 协议，供外部工具调用 | `rpc::run_rpc_mode_stdio()` |
| `CliMode::Interactive`（默认）| 全屏 TUI | `interactive::run_interactive_mode()` |
| `CliMode::Print` | 无头模式，打印纯文本结果 | `cli::headless::run()` |
| `CliMode::Json` | 无头模式，输出 JSON 结果 | `cli::headless::run()` |
| `CliMode::ListModels` | 列出可用模型 | `cli::list_models::list_models_output()` |

**关键流程**：

```
main()
  → parse_args()           # 解析命令行参数
  → CliMode::Rpc?          # 判断运行模式
  → stdin 非 TTY?          # 读取管道输入
  → CodingAgentStartup     # 创建产品启动上下文
  → 路由到对应模式处理器
```

---

### 3.5 `tui` — 通用终端 UI 组件库

**定位**：独立于 `coding-agent` 产品逻辑的纯终端 UI 工具库，不包含任何产品级状态。

```
crates/tui/src/
├── lib.rs              # 私有模块 + api.rs
├── api.rs              # 分类 API（5 个子领域）
├── terminal/           # 终端能力检测、颜色、图像协议
├── input/              # 标准化输入事件、按键绑定
├── component/          # 通用组件（Editor/Markdown/SelectList 等）
├── editing/            # 编辑历史（KillRing/UndoStack）
├── fuzzy/              # 模糊匹配
├── render/             # 渲染调度器、表面、ANSI 绘制
└── theme/              # 终端调色板
```

**公共 API 分类**：

| 类别 | 说明 |
|---|---|
| `api::terminal` | 终端颜色、iTerm2/Kitty 图像协议 |
| `api::input` | 标准化输入事件、按键绑定、自动补全 |
| `api::component` | 通用 UI 组件（16 种） |
| `api::render` | 渲染调度器、表面、布局 |
| `api::theme` | 浅色/深色主题调色板 |

---

### 3.6 `desktop` — 原生桌面应用

**定位**：基于 Zed `gpui` 的原生 GUI 适配器。它以 application reducer 作为桌面状态
权威，通过 typed effect 隔离 runtime/platform 副作用，并以 `UiChangeSet` 驱动选择性刷新。

```
crates/desktop/src/
├── lib.rs                              # 唯一公开面：DesktopApplicationOptions + run()
├── main.rs                             # 桌面应用二进制入口
├── app.rs                              # GPUI bootstrap、窗口与 runtime 生命周期
├── app/
│   ├── native_shell.rs                 # composition root 与 UiIntent 总入口
│   ├── native_shell/                   # runtime/command/platform/review adapters
│   │   └── tests/                      # GPUI shell 行为 suites 与 fixtures
│   └── devtools/native_replay.rs       # desktop-devtools feature 下的视觉/性能 replay
├── application/                        # 纯状态、reducer、command tracker、effect/change set
├── runtime/
│   ├── protocol.rs                     # typed command/update 与唯一 admission validation
│   ├── client.rs                       # connection/client/event-stream/shutdown owners
│   ├── worker/                         # session owner、dispatch 与优先级事件泵
│   └── tests/                          # admission/ordering/overflow/reconnect/recovery/shutdown
├── platform/
│   ├── preferences/store.rs            # 偏好 I/O 与 background writer
│   ├── external_editor.rs              # 安全的外部进程启动
│   └── workspace.rs                    # workspace/path 解析
├── preferences/model.rs                # 无 I/O 的 DesktopPreferences model
├── ui/
│   ├── conversation/                   # pane、composer、Markdown、layout、cache、viewport
│   ├── sessions/                       # session catalog presentation 与 pane
│   ├── inspector/                      # review presentation 与 inspector pane
│   ├── shell/                          # layout、focus、drawer、modal、toast presentation
│   └── components/                     # brand、controls、style、streaming text
├── projection.rs                       # 产品 snapshot/event → DesktopProjection
├── actions.rs                          # typed action、key context 与 Command Palette
└── assets.rs                           # 字体与静态资源
```

**核心架构模式**：

```
GPUI child event
      │
      ▼
   UiIntent → NativeShell（presentation/navigation/command adapter）
                  │                              │
                  ▼                              ▼
           refresh_views                 RuntimeCommandClient
            (UiChangeSet)                        │
                  │                      bounded command queue
                  ▼                              ▼
            GPUI entities                  runtime worker
                                                  │
                                  bounded priority/data update channels
                                                  │
                                                  ▼
runtime/platform/async/timer result → DesktopEvent → DesktopController::reduce
                                                      │
                                           Transition { changes, effects }
                                                ┌─────┴─────┐
                                                ▼           ▼
                                         refresh_views  execute_effect
```

`DesktopState` 聚合稳定的 `WorkspaceStore`、全局 `CommandTracker`、catalog 与 runtime
状态；runtime update、platform result、command completion 与 timer 的状态决策由 application
reducer 统一解释。一次 reducer transition 同时给出区域级 `UiChangeSet` 和可枚举的
`DesktopEffect`。composition root 执行 effect，将 completion 重新包装成 `DesktopEvent` 回流
reducer，不会在 callback 中平行解释同一更新。所有 entity notification 都经过唯一的
`refresh_views(UiChangeSet)` 路径。

每个已打开 session 在 runtime 和 GUI 两侧都有独立 owner。`DesktopPromptTarget::New` 携带
`CodingAgentWorkspaceSelection`、model 与 profile，用于原子创建新的 runtime workspace；
`DesktopPromptTarget::Existing` 只携带 durable `session_id`。`RuntimeCommandClient` 是唯一可
clone 的 command-side API；`DesktopRuntimeEventStream` 和 shutdown guard 分别拥有 update 与
关闭生命周期。command admission 使用 bounded `try_send` 和 typed error，prompt/path validation
只由 `runtime/protocol.rs` 解释。priority 与 data update channel 分离，GUI 每次 poll 有明确预算，
gap、lag、overflow、reconnect 与 shutdown 都由 runtime 状态机测试固定。

目录依赖与 authority 规则如下：

- `application` 不依赖 GPUI、filesystem、process、thread/Tokio owner，也不执行外部副作用。
- `runtime` 不依赖 GPUI、`ui` 或 presentation type；`platform` 不依赖 `ui`。
- leaf UI 不持有 runtime client、command tracker 或 preference store；child event 先变为
  `UiIntent`，presenter 只读取 state 并生成 ViewModel。
- `app/native_shell` 是允许连接 application、runtime、platform 与 GPUI 的 composition root，
  不是第二套 reducer；`NativeShell` 只聚合 connection、application state、skills、views 与 UI state。
- 生产模块禁止 wildcard import。上述规则由 `tests/dependency_boundary.rs` 的 AST/import/manifest
  守卫验证，不通过固定文件清单维持。

`ShellLayout` 对 Home、Skills 与已有 Session 使用同一三列几何：

```text
┌─ Sessions（docked） ─┬──────────── Center Header ────────────┬─ Inspector（docked） ┐
│ 独立 panel           │ Model | Thinking | Profile | toggles │ 独立 header/resize    │
│                      ├──────────── Center Body ──────────────┤                      │
│                      │ Home / Skills / Conversation         │                      │
│                      │ Composer + CenterDrawerHost           │                      │
│                      │ drawer 只覆盖此区域，不覆盖 Header     │                      │
└──────────────────────┴──────────────────────────────────────┴──────────────────────┘
```

Root modal 与 center drawer 是不同 host：授权、Command Palette 和全文查看使用带焦点
trap 的 modal；Sessions/Inspector drawer 是非 modal 的 center-body 覆盖层。Escape、
outside-click 和 drawer close 统一恢复打开前的可见焦点 owner。

#### Desktop 键盘快捷键

下列为用户可见的稳定绑定；`Ctrl/Cmd` 表示 Linux/Windows 使用 Ctrl、macOS 使用 Cmd。

| 操作 | 快捷键 |
| --- | --- |
| 打开 Command Palette | `Ctrl/Cmd+K` |
| 打开 changed-file review | `Ctrl/Cmd+P` |
| 新建 session | `Ctrl/Cmd+N` |
| 聚焦 Composer | `Ctrl/Cmd+L` |
| 提交 Composer | `Ctrl/Cmd+Enter` |
| 中止当前 operation | `Ctrl/Cmd+Esc` |
| 显示/隐藏 Inspector | `Ctrl/Cmd+\` |
| 在可见区域间前进/后退 | `Ctrl/Cmd+Tab` / `Ctrl/Cmd+Shift+Tab` |
| 跳到最新输出 | `End` |
| 层级关闭 popup、drawer 或 modal | `Escape` |
| Conversation 选择上一条/下一条 | `↑` / `↓` |
| 展开或折叠选中项详情 | `Space` |
| 复制选中的 conversation block | `Ctrl/Cmd+C` |
| 授权：拒绝/允许一次/本 operation 允许 | `1` / `2` / `3` |

#### Desktop accessibility 契约

- Application、Navigation、Main、Log、Form、Complementary、Status、Dialog 和
  AlertDialog 使用真实 AccessKit role；可选择行同步 `selected`、`position-in-set` 与
  `size-of-set`。
- icon-only action 必须具有 tooltip 与完整 accessible label；短标签或 Unicode-safe
  ellipsis 只影响可见文本，项目路径、模型/配置身份仍保留在 tooltip/label 中。
- Pointer hover 不冒充 keyboard focus；键盘输入触发可见 focus ring，hover-only 工具仍
  保持在 tab order 中。关键状态同时使用文字、形状或长度，不以颜色作为唯一信息载体。
- Modal 抢占并封闭焦点；drawer 保留 Center Header selector 可点击，关闭时恢复原焦点。
- 20 张 native fixture 固定覆盖三档 responsive/idle/session、Sidebar 与 Inspector drawer、
  production Model/Thinking popup、non-reasoning fallback、Project/long path、catalog
  unloaded/loading/ready/error/empty、authorization、keyboard focus、no-color 与 reduced-motion；
  GPUI interaction tests 负责真实 hit-test 与 focus restore，golden 不替代行为断言。

多项目工作区最终验收保持 runtime 与 presentation 的单向边界：visual replay 只安装 typed
catalog/drawer/home-path/model-capability fixture，并通过生产 GPUI event 打开 popup；它不持有
credential、command dispatch 或新的 session owner。reduced-motion 会停止 busy icon 动画，
同时保留 disabled、accessible label 与局部 loading 文本语义。

---

## 4. 核心数据流

### 4.1 用户提交提示 → AI 响应 → 工具执行

```
┌─────────────────────────────────────────────────────┐
│                   UI 适配器                          │
│  cli / desktop 接收用户输入                           │
└──────────┬──────────────────────────────────────────┘
           │ submit(PromptInvocation)
           ▼
┌─────────────────────────────────────────────────────┐
│              coding-agent (产品层)                    │
│  CodingAgentSession::submit()                        │
│    → CodingAgentOperation::AgentInvocation            │
│    → 事件溯源持久化                                   │
│    → 授权检查 (ToolAuthorizationService)             │
│    → CodingAgentOperationTask 异步执行                │
└──────────┬──────────────────────────────────────────┘
           │ 创建 Agent 并提交
           ▼
┌─────────────────────────────────────────────────────┐
│              agent-core (运行时)                      │
│  Agent::submit_messages()                             │
│    → AgentTurnRunner::run_state() ← 状态机循环        │
│      → CompactRuntimeContext (压缩决策)               │
│      → PrepareProviderRequest (构建请求)              │
│      → ProviderStream (流式接收 LLM 响应)             │
│      → ExecuteTools (执行工具调用)                    │
│      → PrepareNextTurn (分析输出)                     │
│    → yield AgentEvent (流增量)                        │
└──────────┬──────────────────────────────────────────┘
           │ 调用 AI 提供商
           ▼
┌─────────────────────────────────────────────────────┐
│                   ai (传输层)                         │
│  AiClient::stream_model()                             │
│    → ProviderRegistry 路由                            │
│    → ApiProvider::stream() → HTTP + SSE              │
│    → 流解析、错误分类、重试                            │
└─────────────────────────────────────────────────────┘
```

### 4.2 事件传播路径

```
AgentEvent (agent-core)
    │
    ▼
ProductEvent (coding-agent)
    │
    ├──→ EventService 持久化（事件溯源）
    │
    ├──→ ClientProjection 更新（客户端状态投映）
    │
    └──→ UI 适配器事件桥接
         │
         ├──  CLI: event_bridge.rs → UiEvent
         │    → render.rs 渲染更新
         │
         └──  Desktop: mpsc channel
              → DesktopRuntimeUpdate → GUI 更新
```

---

## 5. 关键设计模式

### 5.1 外观模式（Facade）

每个 crate 通过 `api` 命名空间暴露稳定、分类的公共 API，实现模块标记为 `pub(crate)` 或保持私有。
多数 crate 将门面放在 `api.rs`；`coding-agent` 的门面内联在 `lib.rs`：

```
crate::api::<category>    ←  公共消费者
crate::<private_module>   ←  仅 crate 内部访问
```

这种模式确保了 API 稳定性和实现灵活性。

### 5.2 基于 Trait 的多态性

关键抽象均通过 Trait 定义，支持注入和可测试性：

- **`ApiProvider`**：AI 提供商通信
- **`FileSystem` / `Shell`**：文件系统和 Shell 操作
- **`ProviderAuthResolver`**：认证方案

单元测试通过 `#[cfg(test)]` 私有 fixture 或窄 port fake 注入模拟实现。

### 5.3 事件溯源（Event Sourcing）

核心设计原则：所有产品级状态变更都记录为不可变的领域事件。

- 事件类型：`CodingAgentProductEvent`（10 个顶层 family）
- 事件持久化：`EventService` → 文件系统
- 状态重建：重放事件流到 `ClientProjection`
- 协议版本管理：`PRODUCT_EVENT_PROTOCOL_VERSION`

### 5.4 Actor 模式

- **`Agent`** 使用 `Arc<RwLock<AgentState>>` 实现线程安全的内部可变性
- **`CodingAgentSession`** 作为中心 Session Actor，管理所有操作和事件
- **DesktopRuntime** 通过 `mpsc` 通道与 GUI 线程通信

### 5.5 状态机模式

代理回合执行是基于有界状态机的确定性循环：

- 9 个合法状态，16 步熔断器
- 每步输出 `AgentEvent` 流事件
- 防止无限循环、死锁等非正常行为

### 5.6 适配器模式

`coding-agent` 作为产品核心，通过严格的适配器契约（`api::runtime`、`api::embedding`、`api::client`）向 `cli` 和 `desktop` 暴露能力，确保：
- UI 适配器不能绕过产品策略
- 产品层独立于任何特定的 UI 框架
- 第三方客户端可以通过相同的 API 嵌入

---

## 6. 外部集成

### 6.1 AI 提供商

| 提供商 | API 端点 | 传输层 |
|---|---|---|
| Anthropic (Claude) | `api.anthropic.com` | Messages API |
| OpenAI (GPT/Codex) | `api.openai.com` | Completions / Responses API |
| Azure OpenAI | 自定义实例 | Responses API |
| DeepSeek | `api.deepseek.com` | Responses API |
| Google (Gemini) | Generative AI API | Gemini API |
| Mistral | `api.mistral.ai` | Conversations API |

### 6.2 关键依赖

| 用途 | 依赖 |
|---|---|
| HTTP 客户端 | `reqwest`（TLS） |
| 异步运行时 | `tokio`（多线程）、`futures` |
| GUI 框架 | `gpui`（Zed 框架） |
| 终端控制 | `crossterm` |
| Markdown 渲染 | `pulldown-cmark` |
| 语法高亮 | `syntect` |
| 序列化 | `serde` + `serde_json` + `serde_yaml` + `toml` |
| 图像处理 | `image`（PNG/JPEG/GIF/WebP） |
| UUID | `uuid`（v7，时间排序） |
| 文件系统沙箱 | `cap-std` |
| 文件监视 | `notify` |
| 加密 | `ring`、`sha2` |

---

## 7. 配置与部署

### 7.1 配置目录

```
~/.evo/  (或 $EVO_DIR)
├── settings.toml     # 全局设置
├── auth.toml         # API Key 配置
├── agents/           # Agent 配置文件 (TOML)
└── teams/            # Team 配置文件 (TOML)

<cwd>/.evo/
└── settings.toml     # 项目本地设置（覆盖全局）
```

### 7.2 认证配置

通过环境变量配置 API Key：
- `ANTHROPIC_API_KEY`
- `OPENAI_API_KEY`
- `GOOGLE_API_KEY`
- `MISTRAL_API_KEY`
- `AZURE_OPENAI_API_KEY`

`auth.toml` 可用于持久化存储（按提供商组织）。

### 7.3 构建与发布

```toml
[workspace]
members = ["crates/agent-core", "crates/ai", "crates/cli", "crates/coding-agent", "crates/desktop", "crates/tui"]

[workspace.package]
version = "0.7.2"
```

- **二进制 1**：`crates/cli/src/main.rs` → 名称 `coding-agent`
- **二进制 2**：`crates/desktop/src/main.rs` → 桌面应用
- **Rust Edition**：2024

---

## 8. 测试策略

### 8.1 测试层次

| 层次 | 说明 | 文件示例 |
|---|---|---|
| **单元测试** | 每个 crate 的内部测试 | `#[cfg(test)] mod tests` |
| **API 契约测试** | 独立集成测试读取 crate root，守卫只有 `api` 可公开 | `coding-agent/tests/api_contract.rs`、`tui/tests/api_contract.rs` |
| **跨模块集成测试** | 依赖、模块与 adapter 边界验证 | `desktop/tests/dependency_boundary.rs` |
| **共享 fixture** | 跨 adapter 的产品投映事件样本 | `coding-agent/tests/fixtures/client_projection/` |
| **RPC 协议测试** | JSONL 与 typed event 协议 | `cli/src/protocol/*_tests.rs` |
| **组件测试** | UI 组件行为 | `tui/tests/components.rs` 及其子模块 |
| **依赖/模块边界测试** | 解析 manifest 与 Rust AST，验证公开面、child module graph 和 authority 方向；不搜索实现字符串 | `desktop/tests/dependency_boundary.rs` |

Desktop 的测试按 owner 与风险拆分：

- application reducer、workspace、command completion、effect 与 dirty routing 使用纯 unit tests；
- `runtime/tests/` 按 admission、ordering、overflow、reconnect、recovery、shutdown 状态机组织，
  共享启动与 command fixture，不复制生产 façade；
- `app/native_shell/tests/` 只覆盖需要真实 GPUI entity/hit-test/focus/render/responsive 的行为；
- `scripts/desktop-perf-gate.sh` 覆盖 headless layout/input/Markdown，
  `scripts/desktop-native-perf-gate.sh` 覆盖原生窗口 frame/input/RSS；
- `scripts/desktop-visual-golden.sh` 默认只 compare 20 个 fixture。更新 golden 必须显式 review，
  结构重构不得用 golden update 掩盖视觉回归；
- replay fixture 与原生性能入口只在默认关闭的 `desktop-devtools` feature 中编译，默认生产构建
  不包含 fixture 安装 API。

### 8.2 Test-Support 机制

`coding-agent` 的 fixture 只在 crate unit test 构建中编译，不通过 Cargo feature 或
crate-root public module 暴露给下游：

```rust
#[cfg(test)]
pub(crate) mod test_support;
```

CLI/Desktop 使用自己的 adapter fixture；共享的产品投影输入则存放为 JSON fixture，
而不是依赖 `coding-agent` 的私有测试实现。`agent-core`、`ai` 和 `tui` 仍可为各自测试
目标保留独立 feature，这不构成 `coding-agent` 的公共契约。

### 8.3 运行测试

```bash
# 全部测试
cargo test --workspace

# 特定 crate 测试
cargo test -p coding-agent

# 验证全部 feature/target 组合
cargo test -p coding-agent --all-features --all-targets

# 无头运行（CI 友好）
cargo test --workspace --no-fail-fast
```

---

## 9. 架构决策记录（ADR）

### ADR-001：分层架构

**决策**：采用严格的 4 层架构（ai → agent-core → coding-agent → UI），依赖方向单向不可逆。

**理由**：
- 清晰的关注点分离：传输层、运行时、产品逻辑、UI 各自独立
- 可测试性：每一层可通过私有 fixture、窄 port fake 与 adapter fixture 独立测试
- 可替换性：UI 适配器可能变更（TUI → GUI → Web），产品层不变
- 第三方嵌入：`coding-agent` 通过 `api::embedding` 暴露稳定的嵌入 API

### ADR-002：事件溯源

**决策**：所有产品级状态变更通过不可变事件持久化，而非直接修改状态。

**理由**：
- 可审计性：完整的操作历史
- 可恢复性：从事件流重放重建状态
- 可扩展性：新消费者可订阅事件流
- 多客户端支持：多个 UI 客户端通过事件流同步状态

### ADR-003：API 模块模式

**决策**：每个 crate 通过 `api.rs` 暴露分类的稳定 API，私有模块不对外暴露。

**理由**：
- 明确的 API 合同：文档、测试、编译时守卫
- 版本稳定：实现可重构不影响 API 消费者
- 清晰的边界：工具和 linter 可以验证 API 边界不被违反

### ADR-004：基于 Trait 的执行环境

**决策**：使用 `FileSystem` 和 `Shell` Trait 抽象执行环境，而非直接调用 OS API。

**理由**：
- 测试性：内存文件系统用于测试，不创建真实文件
- 安全性：能力系统（cap-std）限制文件访问范围
- 可移植性：不同 OS 可以有不同的实现

---

## 10. 演进方向建议

### 短期
- 补充 `docs/` 目录（本文档为起点），添加 API 参考文档
- 添加更多集成测试覆盖 RPC 协议边界
- 完善模型目录的 `generated.json` 文档说明

### 中期
- 考虑将模型目录从嵌入 JSON 迁移为可运行时更新的配置
- 增强压缩策略（引入滑动窗口）
- WebSocket/HTTP 传输支持（为 Web 客户端铺路）

### 长期
- 事件流的外部消费者支持（WebHook）
- 多代理协作优化（并行工具执行）
- 插件系统（自定义工具提供者）

---

<sub>文档版本：1.0 | 对应代码版本：0.7.2</sub>
