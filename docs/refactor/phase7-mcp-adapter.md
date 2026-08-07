# Phase 7 / ARC-720：MCP provider adapter

> 状态：完成
> 前序：ARC-700（extension-host 骨架）、ARC-710（user hooks）
> 目标：MCP 作为 external tool provider，实现 tool registry adapter，
> **不进入 agent-core**。支持 stdio 与 HTTP transport；生命周期覆盖
> initialize、liveness、per-tool timeout、reconnect、tool/resource change；
> credential store、OAuth 与 refresh 走统一 auth seam；默认采用
> `search_tool` + `use_tool` meta tools，避免把大量 MCP schema 全塞入
> context。ACP transport 只有出现真实需求才加入（本任务不做）。

## 设计决策

### Wire（`mcp/wire.rs`）

- JSON-RPC 2.0 + MCP spec（2025-06-18 及之后常用能力）：`initialize` /
  `notifications/initialized` / `ping` / `tools/list` / `tools/call` /
  `notifications/tools/list_changed`。`resources/*` 不在最小集（见债务）。
- 手写 wire（不引入 rpc 框架），serde 严格解析：
  - 非法 JSON / 顶层非对象 / 缺 `jsonrpc` / **未知顶层字段** / 类型不符
    都产生结构化 [`WireError`]（fail closed，不静默吞掉；第三方服务器
    加未知顶层字段属 spec 违反，宁可显式拒绝）。
  - `params` / `result` 保持原始 `Value`，业务层按需取字段。
- 错误码沿用 JSON-RPC 保留码 + MCP 约定的 `-32001`（UNAUTHORIZED）；
  `is_unauthorized` 同时识别消息文本中的认证字样。

### Transport（`mcp/transport.rs`）

- **stdio**：子进程经 workspace-runtime 新增的
  [`PeerProcess`]（`process/peer.rs`）spawn——**同一 `SandboxProfile`
  强制边界**（能力探测 fail-closed、process-group 终止、kill-on-drop）。
  与 ARC-710 runner 同源：`prepare_sandbox` + `pre_exec` 复用，不另建
  spawn 路径。
  - framing 选择：**JSON lines**（每行一个 JSON-RPC 消息）。理由：
    xai-grok-mcp 的 stdio transport 即按行读取（`ResilientRwTransport`
    的 `read_until(b'\n')`）；MCP 新版 Content-Length framing 主要服务
    二进制 payload，本任务消息全为 JSON，行分隔最简单且可测。文档钉死
    该选择。
  - **单行解码失败跳过并继续读**（参考 grok `ResilientRwTransport`）：
    一个坏行不 collapse 整个 transport；通知行（无 id 有 method）静默
    忽略。stderr 由后台 drain task 丢弃（防止管道写满阻塞子进程）。
  - 读写分离：stdin / stdout / stderr 在 spawn 后经 `take_*` 独立持有
    （tokio 1.52 已移除 `take_owned`），读循环独占 stdout 按 id 分发
    响应、推送通知；写侧经 `Mutex<ChildStdin>`。
- **HTTP**：reqwest 直连用户显式配置的 endpoint（仅 http/https scheme，
  惰性连接、`POST` JSON-RPC 同步读响应）。
  - **信任边界（文档钉死）**：MCP endpoint 来自**显式配置**（server
    配置里写死），与 web_fetch 任意 URL 的 SSRF 管线不同——用户配置
    什么就连什么（等价于 proxy 配置的信任模型）；本模块只做最小校验
    （scheme 必须 http/https）。不引入 IP/loopback 拦截。
  - **stdio 同款信任模型**：`command` 也是用户显式配置（配置即信任，
    与 hook 的 folder trust 不同的维度——MCP 配置进 host options 不经
    discovery/trust 判定）。enabled=false / host 未启动 / host 已关闭
    的 server 一律不可调用（fail closed）。
  - HTTP 下收不到服务端通知（无 SSE）：`tools/list_changed` 热更新仅
    stdio 生效，HTTP 侧登记债务。
- 超时 / 取消：`request()` 内部 `select!` 超时与取消令牌；超时/取消后
  迟到响应按 id 丢弃；**transport 死亡时 `fail_all_pending`**——在途
  请求立即以 `TransportClosed` 失败，不悬挂到超时。

### Lifecycle 状态机（`mcp/state.rs` + `mcp/lifecycle.rs`）

- 状态机独立成纯决策层 `apply_event(state, event)`，transition 表测试
  逐个钉死（`transition_table_is_complete`）；非法转换显式拒绝
  （`TransitionError`，fail closed）。状态发布经
  `McpHost::server_state(name)` 查询。

  ```
  Disconnected ─► Connecting ─► Initializing ─► Ready
                   ▲  ▲              │              │
                   │  └── ConnectFailed → Failed(初始失败不重试)
                   │                                  │ liveness 失败
                   └──── Reconnecting ◄── ConnectFailed ┘
  任何状态 ── Shutdown ──► Terminated
  ```

- **initialize 握手**：`initialize`（协议版本协商，接受服务器返回的
  `protocolVersion`）→ `notifications/initialized` → `tools/list` →
  缓存工具 + bump 版本号（`tools_version` watch，无订阅者时保活
  receiver，send 不失败）。
- **工具转换**：`convert_tool` 校验 name 非空 ≤128、schema 缺省
  `{"type":"object"}`；非法条目**拒绝并诊断**（`mcp_tool_rejected`），
  其余继续（fail closed 于单条目，不整批失败）。
- **liveness**：`Ready` 下按 `ping_interval` 发 `ping`，`ping_timeout`
  内无响应判定死亡 → 重连。ping 超时/进程退出/transport 死三类事件
  统一走 `ConnectFailed`。
- **reconnect**：指数退避（`initial * 2^(n-1)` 封顶 `max`），重连后
  **重新 initialize + 重新发现工具**。区分「初始连接失败」（fail early
  → `Failed`，不重试）与「曾 Ready 后的失败」（退避重连）——
  `was_ready` 判定，避免 liveness 失败被误判为初始失败。
- **tools/list_changed**：`Ready` 下收到通知 → 重新 `tools/list` →
  更新缓存 + bump 版本（`McpHost::subscribe_tools_changed` 订阅）。
- **per-tool timeout / 取消**：`McpServerHandle::call_tool` 带 server
  配置的 `tool_timeout` 与调用方 `CancellationToken`（meta tool 把
  `ToolCallContext::cancel` 贯通进来）；host shutdown 经 `host_cancel`
  select 使在途调用立即失败。
- **shutdown 顺序**（`McpHost::shutdown`）：状态置 `Stopping` → 全局
  cancel（在途调用立即失败）→ 逐 server 关闭会话（stdio terminate
  子进程 + join 读循环）→ task 观察到取消后退出 → `Stopped`。host
  （`ExtensionHost`）在 dispatch task 退出后、`host_shutdown` 诊断前
  关闭 MCP host——join 回收时已是终态。

### Credential / OAuth seam（`mcp/credentials.rs` + `mcp/oauth.rs`）

- `McpCredentialStore` trait（get / set / remove，token 与 refresh token
  分离存储 [`McpCredentials`]）+ `FileCredentialStore` 默认实现：0600
  权限、tmp + rename 原子写、加载时收紧权限、坏文件按缺失处理不 panic、
  目录可注入（测试用临时目录）。单进程 Mutex 串行化（跨进程 flock
  登记债务）。
- **OAuth：RFC 8628 device flow**（与 xai-grok-mcp 的浏览器授权码流程
  不同——Evo 面向 CLI，无 callback server）：POST device authorization
  → 展示 verification URI + user code → 按 interval 轮询 token endpoint
  → 成功/拒绝/超时（`expires_in` 与 `flow_timeout` 取小）/取消。
- **401 后单次 refresh/retry**：`tools/call` 401 → 有 refresh token 则
  refresh 一次并重试一次；refresh 失败或仍 401 → 配置了 OAuth 则触发
  一次 device flow；**不无限重试**——每次失败都是结构化错误
  （`RpcError::Unauthorized` / `OAuthError`）。
- **refresh 后的新 token 立即进入请求（本次修复）**：`McpServerHandle`
  每次 `request`/`notify`（含握手、liveness、tools 发现）从 credential
  store 读取当前 access token，HTTP transport 按请求注入
  `Authorization: Bearer <token>`（`RpcSession::request_with_headers` /
  `notify_with_headers`，动态 header 覆盖静态配置的同名 header；无动态
  凭据时静态保底；stdio 不注入）。此前 refresh 只更新 store，请求仍用
  会话建立时的静态 headers——新 token 从未到达服务器。测试：fake
  HTTP MCP server（`src/bin/fake_mcp_http_server.rs`）记录每个请求的
  `Authorization`，断言 401 → refresh → 重试携带新 token。
- 可测性：`OAuthRuntime` 注入 mock HTTP 客户端、poll_interval、
  flow_timeout、verification presenter；测试用本地 TcpListener mock
  端点。

### Meta tools（`mcp/meta.rs`）

- 默认采用 `mcp_search` / `mcp_use` 两个 DynamicTool（`ToolId` 沿用
  产品 snake_case 命名惯例，前缀 `mcp_` 标识 provider 归属）：
  - `mcp_search`：列出/搜索已发现工具（server/name/description 子串
    匹配、可选 server 过滤），返回 JSON 数组；`read_only`。
  - `mcp_use`：`tool = "<server>/<name>"` + `arguments` 对象，转发
    `tools/call`；取消/超时贯通（`ToolCallContext::cancel` +
    `tool_timeout`）；401 自动走 refresh/device-flow 恢复；`isError`
    → `terminate`。
- **不把 MCP inputSchema 塞进模型 context**：模型只看到两个静态工具
  声明；运行时经 search 发现、use 调用。
- 与 Grok 差异：Grok 把每个 MCP 工具直接注册为 ToolBridge 条目（全部
  schema 进 context）；Evo 按 master plan 第六节采用 meta tool 形态。

### coding-agent 接入（最小接法）

- `ExtensionHostOptions` 增加 `mcp: Option<McpHost>`（ARC-700 预留的
  options 扩展字段；不引入 manifest capabilities 承载 transport 配置
  的复杂度——capabilities 是能力声明，MCP server 配置走 options 直给）。
  单一实例：调用方构造一次 `McpHost`，同一份 options 同时用于
  `ApplicationRunOptions`（app 层生成 meta tools）与
  `CodingAgentSessionOptions`（session 装配 host）。
- `ExtensionHost::start` 启动 MCP host（失败即 host 启动失败）；
  dispatch task 退出时确定性关闭；`ExtensionHost::mcp()` /
  `mcp_meta_tools()` 暴露。
- `ApplicationRunOptions.extension_host_options`（默认 `None`）：
  `resolve_application_context` 在配置了 MCP 时把 meta tools 追加进
  工具列表（`mcp_meta_tools_tests.rs` 钉死：有 MCP 追加 search+use、
  无 MCP 行为不变、显式工具不被覆盖）。
- **默认（无 MCP 配置）行为与现在完全一致**；agent-core 零改动。

## 并发上限（ARC-720 `ConcurrentExtensions`，本次强制）

- `McpHost` 持有 `max_concurrent_extensions`（`set_max_concurrent_extensions`；
  默认与 `ExtensionBudget::default()` 一致为 32；`0` = 不限制）。此前
  `max_concurrent_extensions` 只是配置字段，MCP 并发上限未被强制。
- `start()` 时启用的 server 数超过上限：**只启动前 N 个**（按 configs
  顺序），其余不 spawn（状态保持 `Disconnected`、调用返回 not connected）
  并发出 `mcp_concurrency_limit` 诊断。
- 预算接线：`ExtensionHost::new` 在 config merge 后把合并预算的
  `max_concurrent_extensions` 应用到 `options.mcp`（同一份
  `ExtensionBudget` 同时约束 hooks 与 MCP）。
- 测试：`mcp_lifecycle.rs::mcp_concurrency_limit_starts_only_the_first_servers`
  （上限 2、3 个 server → 只启动 2 个 + 1 条超限诊断）、
  `mcp_concurrency_limit_zero_means_unlimited`（0 不限制）、既有 13 项
  mcp_lifecycle 测试（默认上限 32 不受影响）回归。

## 落点

| 变更 | 位置 |
| --- | --- |
| MCP wire（JSON-RPC 2.0 严格解析） | `crates/extension-host/src/mcp/wire.rs` |
| transport（stdio 子进程 + HTTP） | `crates/extension-host/src/mcp/transport.rs` |
| lifecycle（server 状态机 / McpHost 装配） | `crates/extension-host/src/mcp/lifecycle.rs` |
| server 调用句柄（OAuth 动态 header 注入） | `crates/extension-host/src/mcp/server_handle.rs`（本次拆分） |
| 状态机纯决策层 + transition 表 | `crates/extension-host/src/mcp/state.rs` |
| credential store（trait + 文件实现） | `crates/extension-host/src/mcp/credentials.rs` |
| OAuth device flow + refresh | `crates/extension-host/src/mcp/oauth.rs` |
| meta tools（search / use） | `crates/extension-host/src/mcp/meta.rs` |
| 公开 API 汇总 | `crates/extension-host/src/mcp/mod.rs`、`src/api.rs` |
| host 集成（options.mcp + 生命周期） | `crates/extension-host/src/host/mod.rs` |
| 集成测试（fake stdio server） | `crates/extension-host/tests/mcp_lifecycle.rs` |
| 测试辅助二进制 | `crates/extension-host/src/bin/fake_mcp_server.rs` |
| 交互式子进程 spawn（sandbox 强制） | `crates/workspace-runtime/src/process/peer.rs` |
| coding-agent 装配（meta tools 注入） | `crates/coding-agent/src/app/{bootstrap,startup}.rs` |
| coding-agent 装配测试 | `crates/coding-agent/src/tools/mcp_meta_tools_tests.rs` |
| 依赖边 | `scripts/architecture/internal-dependencies.tsv`（+`extension-host → tool-runtime`） |
| 设计文档 | 本文件 |
| provenance 登记 | `docs/refactor/provenance/grok-build.md` |

## 验证

```text
cargo test -p extension-host --all-features
156 lib（wire 11 / state 5 含 transition 表 / lifecycle 13 / oauth 5 /
      credentials 6 / meta 5 / host 22）+ 9 hook_runner + 13 mcp_lifecycle 全绿
  - mcp_lifecycle：initialize 握手与工具发现 / tools/call 转发 / per-tool
    timeout / 取消 / liveness 超时重连与重新发现 / 进程崩溃重连恢复 /
    tools/list_changed 热更新 / 输出洪泛 / 非法 JSON 行跳过 / 初始失败
    不重试 / shutdown 在途调用取消 / 未启动与已关闭 host 不可用 /
    disabled server 不装配
cargo test -p coding-agent --all-features   225 + 3（mcp_meta_tools 装配）
cargo test -p tool-runtime --all-features   9 全绿
cargo clippy -p extension-host -p coding-agent --all-targets --all-features -- -D warnings 通过
cargo fmt --all -- --check                   通过
architecture-gate：21 依赖边（新增 extension-host → tool-runtime）、
  无新增 oversized debt（全部文件 ≤900 行）
```

## 与 Grok 参考实现的差异

1. **transport**：Grok 用 rmcp 框架（`TokioChildProcess` /
   `StreamableHttpClientTransport`）；Evo 手写 JSON lines stdio + reqwest
   HTTP，无 rpc 框架依赖。
2. **wire**：Grok 的 `wire.rs` 只有 ACP 常量；Evo 自建 JSON-RPC wire
   且严格解析（deny 未知字段，fail closed）。
3. **OAuth**：Grok 浏览器授权码流程（AuthorizationManager + DCR + 本地
   callback server + 跨进程 dedup）；Evo 用 **RFC 8628 device flow**
   （CLI 友好，无 callback server），dedup 简化为单进程。
4. **credential**：Grok 存 rmcp `StoredCredentials`（含跨进程 flock）；
   Evo 简化 `McpCredentials`（token/refresh 分离）+ 单进程 Mutex +
   原子写；跨进程 flock 登记债务。
5. **meta tools**：Grok 无 meta tool 概念（全部工具注册进 ToolBridge）；
   Evo 采用 `mcp_search` / `mcp_use`。
6. **liveness**：Grok 轮询 `is_transport_closed()`（rmcp 状态探测）；
   Evo 主动 `ping` 心跳（可配置间隔/超时）。
7. **状态机**：Grok `ClientStateKind` 四态无显式转换；Evo 有
   `apply_event` 纯决策层 + transition 表测试（含 reconnect attempt
   递增）。
8. **HTTP**：Grok 依赖 streamable HTTP + SSE（有重连退避 wrapper）；
   Evo 最小 `POST` 同步响应（SSE 登记债务）。
9. **沙箱**：Grok stdio 子进程无沙箱；Evo 强制 `SandboxProfile` +
   能力探测 fail-closed（与 ARC-610/ARC-710 一致）。

## 债务登记

| id | 清偿期限 | 说明 |
| --- | --- | --- |
| HTTP SSE / streaming | 出现真实需求时 | HTTP transport 只做 `POST` 同步响应；`notifications/tools/list_changed` 在 HTTP 下不可达（无 SSE 读流）。清偿需引入 SSE 读循环 + `last-event-id` 重连。 |
| resources 深度支持 | 出现真实需求时 | `resources/list` 未实现（最小集以 tools 为核心）；`tools/call` 返回的 resource content block 按 JSON 透传。 |
| ACP transport | 出现真实需求时 | master plan 明示本任务不做；wire/transport 的 session 结构已留扩展点（`TransportConfig` 可增 `Acp` 变体）。 |
| 跨进程 credential flock | 不迟于 Phase 10 | `FileCredentialStore` 单进程 Mutex 串行化；多进程并发写同文件可能互相覆盖（Grok 用 flock + 新鲜度守卫）。 |
| HTTP 401 触发 device flow 的交互路径 | Phase 9 | device flow 已实现且可测（mock 端点）；产品侧「401 后引导用户完成验证」的 UI 交互（展示 verification URI）由 CLI/Desktop 完成。 |
| tool 调用的请求级取消（stdio） | 不迟于 Phase 10 | JSON-RPC 无标准请求取消：超时/取消只放弃等待并丢弃迟到响应，服务器侧计算仍会跑完；不引入 `notifications/cancelled`（MCP 2025-06-18 未定义）。 |
| MCP server 输出（stderr 日志）呈现 | Phase 9 | stderr 后台 drain 丢弃；产品侧接入诊断展示由 CLI/Desktop 完成。 |

## 遗留问题

- 本 ARC 未引入 tracing 依赖：MCP 模块的诊断经 `McpHost` 的
  `DiagnosticSink`（`mcp_tool_rejected` / `mcp_reconnecting` /
  `mcp_connect_failed` / `mcp_refresh_failed` / `mcp_oauth_recovery_failed`
  / `mcp_tools_refresh_failed` / `mcp_state_transition`）落结构化记录；
  产品接入诊断通道时直接消费即可。
- 环境注入：MCP stdio server 的 `env` 白名单默认空（`AllowList` 空
  map）；用户配置 command 需要 PATH 时自行声明（与 hook 的白名单纪律
  一致，不继承宿主环境）。文档钉死：MCP server 环境是配置声明的
  白名单，非宿主环境继承。
- 重连风暴：退避无重试次数上限（与 Grok 的 reconnect-loop 防护一致），
  由 shutdown 打断；`mcp_reconnecting` 诊断可观测频度。若产品需要
  上限可在 `ReconnectConfig` 增加字段（向后兼容）。
