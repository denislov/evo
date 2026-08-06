# Phase 6 / ARC-620：安全 web_fetch

> 状态：完成
> 前序：Phase 6 / ARC-630（provider resilience）
> 目标：coding-agent 内置 `web_fetch` 工具，经 ai crate 的 SSRF 防护 fetch
> 管线访问外部页面 —— 本地 harness 执行、模型以 Function 工具声明使用；
> 网络读取具备 fail-closed 的安全边界（SSRF、预算、内容类型）。

## 决策

- **SSRF 防护在 fetch 管线内逐跳重验证**（`crates/ai/src/transport/fetch/`）：
  每跳重新解析 DNS 并对**全部**解析结果逐一校验，任何一条命中阻止列表即
  fail-closed（不因部分记录是公网地址而放行）。阻止列表覆盖 loopback、
  RFC1918、link-local、cloud metadata（169.254.169.254 单独显式守卫）、
  IPv4-mapped IPv6、unspecified、multicast、broadcast、ULA。IP 字面量在
  `resolve_host` 入口直接校验（不经过 DNS），IPv6 bracketed host 先剥括号
  再解析，否则字面量会漏进 DNS 通道绕过校验——这是本 ARC 修掉的既存缺陷。
- **连接策略：hyper 客户端 + 自有 `SafeConnector` + tokio-rustls 固定 SNI**：
  不走 hyper 默认连接池（其重定向/DNS 行为不受控），connector 拿到的是
  SSRF 校验后的 IP 集合（DNS pinning），TLS 层以原始 host 固定 SNI 并
  校验证书链，`Content-Encoding` 只接受 identity，防止解压炸弹绕过字节预算。
- **预算双闸门**：声明长度超预算在读取前拒绝（`ContentLengthOverBudget`，
  不读 body）；无 Content-Length 的流式 body 在收集时按预算截断并置
  `truncated` 标记（`truncated` 结果不进缓存、输出文本显式标注）。逐跳
  超时独立预算：resolve 5s、connect 10s、total 30s（含 body）、转换 10s。
- **缓存**：进程内存缓存（TTL 60s、64 条、总 16 MiB），key 为规范化 URL +
  format；只缓存成功且未截断的结果，失败/截断永不缓存。
- **内容投影**：HTML→Markdown 用 `html2md`（`format: markdown`，默认）；
  `format: text` 走内部 HTML 剥除器。无 Content-Type 或非 text/html、
  text/plain 一律拒绝（fail-closed，`MediaKind::Other`），PDF/image/video
  明确留待 consumer 扩展。
- **非 2xx 结构化错误**：非重定向非成功状态返回 `HttpStatus` 错误，携带
  `status`/`url` details，供模型区分「页面不存在」与「网络故障」。
- **工具注册在 `services/runtime.rs` 的 typed runtime match**：与 bash 同
  一注册点（`services/runtime.rs:227`），经 `runtime.typed_tool_ids()`
  （来自 `tools/mod.rs::builtin_runtime_tool_ids`）+ capability set
  （`product_tool_ids`）双闸门过滤；profile 显式枚举工具列表时需列出
  `web_fetch`（与 bash 一致，不做隐式授权）。
- **FetchClient 生命周期：每次 runtime 构建共享一个 `Arc<FetchClient>`**：
  `FetchClient` 为 `Send + Sync`（内部 `Arc<dyn DnsResolver>` +
  `Arc<rustls::ClientConfig>` + `Arc<FetchCache>`），一次 operation 的
  runtime 内所有调用共享 TLS 配置与内存缓存；工具构造函数
  `web_fetch_runtime_tool_with(client)` 接受注入，测试用 `for_testing`
  （test-support 下放行 loopback）起本地 server 做真实 socket 集成测试。
- **错误映射（FetchErrorKind → ToolErrorKind）**：URL/方案/userinfo →
  `InvalidArguments`；SSRF 拦截 → `Unauthorized`（策略拒绝，不重试）；
  DNS/传输 → `Unavailable`；各类超时 → `Timeout`；HTTP 状态/重定向超限/
  长度超预算/编码/解码 → `Execution`。保留管线 message 与 details，
  message 前缀 `web_fetch:`。
- **authorization_risk 选 `SideEffect`**：tool-contract 现无「外部网络只读」
  变体；`WorkspaceLocalReadOnly` 语义是工作区内（授权层对其直接放行，
  见 `services/authorization/evaluation.rs:197`），声明它会绕过网络访问
  确认，不安全；`SideEffect` 使 Ask 模式提示用户、Plan 模式 fail-closed
  拒绝，符合「网络读取需确认」的产品语义。
- **参数 schema 约束**：`format` 是 JSON Schema 元数据键，tool-contract
  schema 白名单将其作为生成元数据删除，参数以 `output_format` 声明并
  `serde(alias = "format")` 兼容；`Option<枚举>` 会被 schemars 生成
  `enum: [..., null]` 混入 null，通过 tool-contract 校验失败，故 format
  用非 Option + `#[serde(default)]`。
- **预算常量**：上限硬约束 `MAX_WEB_FETCH_BYTES = 16 MiB`、
  `MAX_WEB_FETCH_URL_BYTES = 8 KiB` 放 `kernel/limits.rs`；软默认 2 MiB
  及超时等产品配置显式列在工具内 `fetch_client_config()`（对齐管线默认，
  防上游漂移）。

## 落点

| 变更 | 位置 |
| --- | --- |
| SSRF 防护 fetch 管线（前置 ARC，本 ARC 消费） | `crates/ai/src/transport/fetch/`（fetch/connector/convert/cache/errors/resolve/ssrf） |
| 管线公开 facade | `crates/ai/src/api.rs`（`api::fetch` 模块） |
| web_fetch 工具（args/definition/execute/错误映射） | `crates/coding-agent/src/tools/web_fetch.rs`（新增） |
| 模块注册 + 工具列表（product/builtin/filter） | `crates/coding-agent/src/tools/mod.rs` |
| 工具注册分支 | `crates/coding-agent/src/services/runtime.rs` |
| 预算常量 | `crates/coding-agent/src/kernel/limits.rs` |
| 工具清单测试更新 | `crates/coding-agent/src/tools/server_tools_tests.rs` |
| 设计文档 | `docs/refactor/phase6-web-fetch.md`（本文） |

## 验证

```text
cargo test --locked -p coding-agent --all-features
通过：lib 197 + 集成 2 + 2 + doctest 7 = 208 passed, 0 failed
其中 web_fetch 新增 12 项：
- definition/授权契约：read_only + Parallel + cancel/timeout、无 streaming、
  provider_executed=false、authorization_risk=SideEffect、schema 拒绝未知字段与缺失 url
- 参数校验（离线）：空 url、非法 format、max_bytes 0 / 超 16 MiB、未知字段
- 错误映射全表：15 个 FetchErrorKind → ToolErrorKind 逐一断言
- SSRF（严格 client，离线）：127.0.0.1 / [::1] / 169.254.169.254 /
  10.0.0.1 / 192.168.1.1 → Unauthorized
- 真实 socket 集成（for_testing + 本地 loopback server，std TcpListener）：
  HTML→Markdown + details 元数据、format=text 剥 HTML、流式截断标注 +
  details.truncated、二次请求命中缓存（server 仅收 1 次）、404 → Execution
  且 details.status=404

cargo check --workspace --all-features
通过（ai / agent-core / coding-agent / desktop / cli 全编译）

cargo clippy -p coding-agent -p ai --all-targets --all-features -- -D warnings
通过（0 warnings）

cargo fmt --all -- --check
通过
```

既有偶发：`test_support::tests::temp_session_env_repairs_a_partial_commit_on_reopen`
一次运行中因 `.writer.lock` 跨进程竞争失败，与 web_fetch 无关（复跑通过）。

## 后续

- **provider-side web search 与本地 fetch 是两个 ToolKind**：`web_search`
  是 provider-executed（`ToolKind::WebSearch`，声明发给模型、结果随消息
  返回），`web_fetch` 是 harness 执行（`ToolKind::Function`）；后续 Phase
  做 search 时保持该区分，二者互不替代。
- **PDF/image/video 待 consumer 显式接入**：管线已按 `MediaKind` 分类，
  拒绝逻辑集中在 convert 层，consumer 明确 opt-in 时扩展即可。
- `web_fetch` 的 cancel 依赖 fetch 内部的预算超时兜底（fetch 无中途
  cancel 注入点），执行层以 `tokio::select!` 响应取消信号。
