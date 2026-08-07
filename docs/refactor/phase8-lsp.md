# Phase 8 / ARC-820：LSP lifecycle

> 状态：完成（2026-08-07）
> 前序：ARC-800（`code-intelligence` 骨架）、ARC-810（codebase graph）
> 目标：语言服务器生命周期治理 —— server start/restart/backoff、
> workspace config、document open/change/close replay、push/pull
> diagnostics + stale policy、进程治理（SandboxProfile + background
> task ownership）、LSP edit → workspace edit/ChangeReceipt 转换层
> （绝不直接写磁盘）。

## 决策

### 协议层：手写 Content-Length 帧 wire（不引入 async-lsp）

评估了 `async-lsp = 0.2.3`（Grok 用法）后选择手写，理由：

1. **角色错配**：async-lsp 0.2.x 的核心是 **server** 侧（
   `LanguageServer` trait + router）；client 侧（`ClientState`）只有
   请求封装，liveness、重启、document replay、服务器→客户端请求
   （`workspace/applyEdit`）都要在框架外自搭。
2. **进程治理缺位**：Evo 要求的 SandboxProfile 强制（fail-closed）与
   background task ownership 在 async-lsp 的 transport 层不存在，
   必须自写 spawn 边界。
3. **项目先例**：MCP 的 JSON-RPC wire/transport 为手写（Phase 7），
   LSP 的帧协议与严格解析风格一致；引入 async-lsp 还会带来
   lsp-types 依赖树（大版本锁旧）。
4. LSP wire 本身极小：`Content-Length: N\r\n\r\n` 头 + N 字节 JSON 体，
   读写各一个函数。

模块划分（`crates/code-intelligence/src/lsp/`）：

| 文件 | 职责 |
| --- | --- |
| `wire.rs` | JSON-RPC 2.0 消息类型 + Content-Length 帧读写（同步/async）+ 严格解析（fail closed）+ 错误分类 |
| `state.rs` | 生命周期状态机（纯决策层 + transition 表测试）+ 指数退避 |
| `transport.rs` | stdio 帧会话：spawn（SandboxProfile 强制）+ 读循环 + id 分发 + 通知/服务器请求 fan-out + 取消/超时 |
| `documents.rs` | document open/change/close、版本跟踪、change 合并、replay 列表、UTF-16 偏移换算 |
| `diagnostics.rs` | push/pull 诊断存储 + stale policy 状态机 |
| `edit.rs` | `workspace/applyEdit` → 校验 → `EditPlan` → 注入的受限 `EditApplicator`（ChangeReceipt） |
| `query.rs` | hover/definition/references 查询面（统一入口） |
| `server/mod.rs` | `LspService`/`LspHandle`/`LspTask` 公共面（config/错误/snapshot） |
| `server/actor.rs` | actor 生命周期驱动（start/restart/backoff/replay/shutdown）与命令处理 |
| `bin/fake_lsp_server.rs` | 测试辅助 fake server（行为模式注入） |

### 生命周期状态机（`state.rs` + `server/actor.rs`）

```text
Idle ─► Starting ─► Initializing ─► Ready ──(崩溃/传输死/liveness 失败)──┐
         │            │              │                                    │
         │(spawn 失败)│(握手失败)     └────────► Reconnecting ─► Starting ─┘
         ▼            ▼                          │ (指数退避 + 文档 replay)
       Failed        Reconnecting(attempt+1)     ▼ (次数用尽)
                                               Failed
任意状态 ─► ShuttingDown ─► Stopped
```

- **spawn 失败不重试**（进程未创建，二进制不存在等）→ `Failed` 终态；
  spawn 成功后的任何失败（握手失败 / 崩溃 / liveness 超时）都进入
  `Reconnecting` 指数退避（`initial * 2^(attempt-1)`，封顶 `max`）。
  **与 MCP 差异**：MCP 握手失败直接 `Failed`；语言服务器启动期不稳定
  是常态，LSP 重试。
- **attempt 全局累计**（`enter_reconnecting`）：Ready 崩溃后状态机
  给出 `Reconnecting{1}`，actor 用累计 attempt 重写（每次失败 +1），
  超过 `max_restart_attempts` 即 `GiveUp` → `Failed` 终态（需显式
  shutdown 退出）。
- **shutdown 顺序**（确定性，幂等）：状态 `ShuttingDown`（新命令被拒）
  → 取消令牌（在途网络请求立即失败）→ `shutdown` 请求（2s 超时，用
  独立令牌——全局令牌已取消）→ `exit` 通知 → 100ms 优雅窗口 → 终止
  子进程并回收读循环 → `Stopped`。shutdown 消息只发给握手完成的会话
  （`handshaken` 在 transition 前捕获）。

### document replay（`documents.rs` + actor）

- **本地保持完整文本**：任何 change 先在本地应用（同一版本多批 change
  合并），然后向服务器发**全量 didChange**（`range: null` + 最新文本）。
  LSP spec 明确支持全量同步，本地文本与服务器严格一致（不依赖增量
  计算的正确性）。
- **版本单调**：`version < 当前` 拒绝；`== 当前` 视为同批合并；
  `> 当前` 接受并更新。
- **uri 校验**：`file://` scheme + workspace 内（词法包含检查，
  `..` 逃逸拒绝，无 IO）。
- **replay**：重启握手完成后按 uri 排序重发 `didOpen`（最新文本 +
  版本）；server 未就绪期间的文档操作仍更新本地状态，恢复后自动收敛。
- **UTF-16 偏移**：LSP `position.character` 是 UTF-16 code unit，
  本模块提供 UTF-16 ↔ char ↔ 字节换算（`utf16_to_char_index` +
  `position_to_char_index`），range 应用与 edit 校验共用。

### diagnostics：push + pull + stale policy（`diagnostics.rs`）

状态机（转换表测试钉死）：

```text
                         publish(version == doc)            doc change
   (uri, doc_version) ─────────────────────► Fresh(doc_version) ──► Stale
         │  publish(version != doc)                                  │
         ├──────────────────────────────► Stale{reason}              │
         │  publish(no version)                                      │
         └──────────────────────────────► Unknown ───────────────────► Stale
```

- `Fresh`：推送版本 == 文档版本；`Stale`：版本落后/超前或文档已变化；
  `Unknown`：不携带版本，文档变化后自动转 `Stale`。
- `StalePolicy::Mark`（默认）返回全部 + 标记；`Discard` 只返回 Fresh。
- **只存储已打开文档的诊断**（未打开 → 忽略，fail closed 简化）。
- pull（`textDocument/pullDiagnostics`）经网络请求，响应入库（版本
  未知 → `Unknown`）；push 与 pull 共用同一 store。
- 诊断版本对应关系在**文档关闭时清除**（内容已不存在）。

### LSP 查询面（`query.rs`）——不并入 `QueryBackend`

hover / definition / references 定义为独立查询面（`LspHandle::query`），
**不实现 `QueryBackend` trait**：

1. `QueryBackend::query` 是同步签名（actor 在独立 task 中执行），而
   LSP 查询是 async 网络往返（等待语言服务器响应），无法塞入。
2. 生命周期差异：LSP 是子进程服务（start/restart/liveness），与索引
   服务的内存模型无关；并入会让 `CodeIntelligenceService` 的确定性
   shutdown 依赖 LSP 的 async 状态。
3. `QueryKind::Diagnostics` 保持 ARC-800 的 `Unimplemented` 占位；
   ARC-830 的 tool adapter 直接消费两个 handle（LSP 走
   `LspHandle::query` / `diagnostics`），如需统一聚合再加轻量门面。
4. 查询在独立 task 中执行（不阻塞 actor 的命令顺序处理），经共享
   session 快照转发；shutdown 时共享取消令牌立即失败。

### edit 转换层（`edit.rs`）——绝不直接写磁盘

`workspace/applyEdit` 是**服务器 → 客户端**请求（LSP 特有，MCP 无）。
处理流程：

1. 校验（fail closed，任一失败整个 edit 拒绝）：
   - uri 必须 `file://` + workspace 内（词法逃逸拒绝，同 document 层）；
   - 目标文档必须已打开（`changes` 形态不带版本，版本校验只能针对
     打开文档）；
   - `documentChanges` 携带的版本必须等于文档当前版本；
   - range 越界、逆序 edits 拒绝。
2. 生成 `EditPlan`（每文件 PlannedChange：uri / rel_path / range /
   new_text）。
3. 注入的 `EditApplicator` trait 执行并返回 `ChangeReceipt` 列表
   （ARC-830 在此接线 coding-agent 的完整 authorization / review
   流程；本任务测试用临时目录的受限应用）。
4. **无 applicator 时拒绝**（返回错误响应，绝不静默吞掉 edit），计划
   记录到 `pending_edits` 查询面供调用方查看。

多文件事务：计划是列表，原子性由 applicator 决定（本任务受限应用
逐文件执行、失败即中止，见债务登记）。

### 进程治理（`transport.rs`）

- **SandboxProfile 强制**：`PeerProcess::spawn`（workspace-runtime，
  MCP 先例）在 spawn 边界应用 profile；`config.sandbox = None` 时用
  `SandboxProfile::product_default(workspace_root)`；能力不足平台
  spawn 显式失败（fail-closed，无静默降级）。
- **background task ownership**：`LspServerConfig::task_owner`
  （`TaskOwner`）必填；进程生命周期由服务直接治理（terminate 进程组
  + 回收读循环），与 MCP stdio 先例一致（TaskRegistry 的 spawn 模型
  是 spool + 无 stdin 交互，不适用于交互式帧会话——owner 语义以
  config 携带 + 服务自身 shutdown 收敛，见债务登记）。
- **输出预算**：单帧上限（`max_frame_bytes`，默认 16 MiB）防超大帧 /
  输出洪泛；stderr drain 防止子进程写满管道阻塞。
- **环境白名单**：`EnvPolicy::AllowList`（`Inherit` 仅显式配置）。
- **坏帧 fail closed**：帧协议坏帧 = 流不同步（无法恢复帧边界），
  读循环终止并上报死亡（重启）；与 MCP 坏行跳过（行协议可恢复）不同。
- **liveness**：Ready 下按 `ping_interval` 发 `ping`，`ping_timeout`
  内无响应判定死亡 → 重启。interval 周期来自配置（`reset()` 的
  next_tick = 构造时刻 + period，构造时刻早已过去 → 进入 Ready 立即
  ping 一次，之后按周期）。

## 落点

| 变更 | 位置 |
| --- | --- |
| 协议层 | `crates/code-intelligence/src/lsp/wire.rs` |
| 生命周期状态机 | `crates/code-intelligence/src/lsp/state.rs` |
| stdio 会话 | `crates/code-intelligence/src/lsp/transport.rs` |
| document 状态 | `crates/code-intelligence/src/lsp/documents.rs` |
| diagnostics store + stale policy | `crates/code-intelligence/src/lsp/diagnostics.rs` |
| edit 转换层 | `crates/code-intelligence/src/lsp/edit.rs` |
| 查询面 | `crates/code-intelligence/src/lsp/query.rs` |
| 服务公共面 / actor | `crates/code-intelligence/src/lsp/server/{mod,actor}.rs` |
| 模块组织 + re-export | `crates/code-intelligence/src/lsp/mod.rs` |
| 公开 API | `crates/code-intelligence/src/api.rs`（LSP 位置类型别名 LspPosition/LspRange 避 graph 冲突） |
| fake server 辅助二进制 | `crates/code-intelligence/src/bin/fake_lsp_server.rs` |
| 进程级集成测试 | `crates/code-intelligence/tests/{lsp_lifecycle,lsp_transport}.rs` |
| 单元测试 | `src/lsp/{wire_tests,diagnostics_tests,edit_tests}.rs` + `documents.rs`/`state.rs` 内嵌 |
| 依赖 | 根 `Cargo.toml`：`sha2 = "0.10"`（receipt revision 哈希，与 change-tracker 一致）；code-intelligence 追加 `tokio` features（io-util/process/time）与 `tokio-util`（CancellationToken） |
| 设计文档 | `docs/refactor/phase8-lsp.md`（本文件） |
| provenance 登记 | `docs/refactor/provenance/grok-build.md`（ARC-820 段） |

## 验证

```text
cargo test -p code-intelligence --all-features
246 passed（lib 207 + lsp_lifecycle 24 + lsp_transport 15；
ARC-800/810 既有 150 项不回归 + ARC-820 新增 96 项）
- wire 14：消息形状 round-trip/非法 JSON/非对象/缺 jsonrpc/未知字段/
  版本拒绝/歧义形状/帧 round-trip（sync+async）/额外头行/大小写不敏感/
  缺 Content-Length/非数字/重复头/超大帧/截断帧（头中/载荷中/空）/
  超大头部/取消错误检测/错误响应序列化
- state 4：transition 表完整（15 条合法转换 + Failed 产物变体）/
  非法转换拒绝/重复 shutdown 吸收/backoff 指数增长与封顶
- documents 10：open/change/close 生命周期/重复 open 拒绝/版本回退
  拒绝/同版本合并批/增量 range 应用/UTF-16 代理对（surrogate pair
  偏移与插入点）/越界 range/uri 校验（非 file/相对/逃逸/workspace 外）/
  replay 排序与最新内容/未打开错误
- diagnostics 11：版本匹配 Fresh/转换表（匹配/落后/超前/无版本）/
  文档变化 Fresh→Stale、Unknown→Stale、Stale 保持/Mark 全量返回/
  Discard 过滤/关闭清理/push params 解析/坏 params 拒绝/pull
  params+result round-trip/refresh_all
- edit 12：合法 plan/版本必须匹配/路径越界拒绝/未打开拒绝/越界 range
  拒绝/逆序 edits 拒绝/applyEdit params 解析（changes+documentChanges+
  空拒绝+坏 edits）/受限 applicator ChangeReceipt 语义（before/after
  revision、byte/line delta）/applicator 拒绝越界路径/相对路径正斜杠/
  通知 params 组装/多文件计划
- lsp_transport 15（fake server 进程级）：initialize 握手/通知转发/
  服务器请求转发与回执/坏帧 fail closed/截断帧 fail closed/输出洪泛
  fail closed/启动垃圾帧/请求超时与取消/迟到响应丢弃/服务器错误上抛/
  spawn 失败结构化错误/close 终止进程且幂等/record 文件事件/
  sandbox 默认 profile spawn/applyEdit 请求形状解析
- lsp_lifecycle 24（完整生命周期）：start→ready→shutdown→stopped/
  crash 后 document replay/restart backoff 指数与封顶/重启次数用尽
  Failed/spawn 失败不重试/握手超时重试至 Failed/未就绪期间文档操作
  与 replay/push diagnostics + stale 标记/Discard 过滤/pull 往返/
  查询往返（hover/definition/references）/未就绪查询拒绝/shutdown 顺序
  与在途取消（shutdown+exit 消息验证）/幂等 shutdown/关闭后命令拒绝/
  SendersDropped/applyEdit 注入 applicator（文件被受限应用）/
  无 applicator 拒绝并记录（文件未动）/workspace 外 edit 拒绝/
  liveness 失败重启/document 错误路径不 panic/重复 start 拒绝/
  snapshot 报告/未知服务器请求兜底

cargo clippy -p code-intelligence --all-targets --all-features -- -D warnings  通过
cargo fmt --all -- --check  通过
scripts/architecture-gate.sh  通过（dependency_edges=23，无新内部边；
sha2/tokio-util 为第三方依赖，code-intelligence → workspace-runtime /
change-tracker 边已登记）
```

## 与 Grok 参考实现的差异清单

1. **无 async-lsp**：Grok 用 async-lsp 0.2.3 做 client（
   `implementations/lsp/`）；Evo 手写 wire/会话（决策见上）。
2. **生命周期**：Grok 无重启/backoff/document replay（async-lsp 的
   ClientState 无进程治理）；Evo 的 `Reconnecting` 指数退避 +
   `max_restart_attempts` GiveUp + 重启后 didOpen replay 为自研。
3. **document 状态**：Grok 不管理 document state（server 端才有）；
   Evo 的 DocumentStore（版本单调/同版本合并/全量同步）为自研。
4. **diagnostics**：Grok 的 publishDiagnostics 直接转发 UI，无版本
   状态机；Evo 的 Fresh/Stale/Unknown + Mark/Discard policy 为自研。
5. **edit 授权**：Grok 的 applyEdit 直接转发编辑基础设施；Evo 的
   「校验 → 计划 → 注入 applicator → ChangeReceipt」转换层为自研
   （授权边界以 ARC-830 收口）。
6. **进程治理**：Grok 无 sandbox / task ownership 概念；Evo 强制
   SandboxProfile（fail-closed）+ owner 语义。
7. **坏帧策略**：MCP 坏行跳过（行协议可恢复）；LSP 坏帧 fail closed
   （帧流无法恢复边界）。
8. **测试**：无直接复制；进程级 fake server + 状态机 transition 表
   按 Evo 语义重写。

## 债务登记

（以下为文档登记的后续增强项；无 `TODO(ARC-*)` 源码标记，execution-debt.tsv
保持空——与 ARC-800/810 一致，收敛项在计划完整时处理）

- **礼貌性 `$/cancelRequest`**：客户端取消在途请求时只在本地丢弃
  pending（迟到响应按 id 丢弃），不向服务器发 `$/cancelRequest`
  通知；服务器侧计算不停止。
- **`workspaceEdit.operations` 形态**：`WorkspaceEdit` 只支持
  `changes` 与 `documentChanges`（TextDocumentEdit）；resource
  operations（create/rename/delete 文件）未实现，需要时加。
- **多文件事务原子性**：`EditPlan` 是计划列表，原子性由 applicator
  决定；受限应用逐文件执行、失败即中止并返回错误（已应用文件不回滚）。
  ARC-830 的授权/review 接线时确定事务语义。
- **pull diagnostics 高级特性**：`resultId` 增量拉取未实现（每次
  全量拉取，`resultId` 传入 `null`）；inter-file diagnostics 与
  workspace diagnostics 未实现。
- **TaskRegistry 登记**：LSP 进程经 `PeerProcess` 直接治理（owner 以
  config 携带），未登记进 `TaskRegistry`（其 spawn 模型为 spool + 无
  stdin，不适用交互式帧会话）；ARC-830 若需 owner 组统一终止，扩展
  registry 支持外部句柄登记。
- **查询结果语义映射**：hover 的 markdown 提取、definition 位置解析
  等原始 JSON 语义映射留给 ARC-830 消费层。
- **liveness 立即 ping**：进入 Ready 时立即 ping 一次（interval
  reset 语义），对慢启动服务器略激进；无实际影响（ping 轻量）。

## 后续

- ARC-830：tool adapter 消费 `api.rs` 的 LSP 面（`LspHandle` /
  `EditApplicator` 注入点 / `pending_edits` / `change_receipts`），
  独立 ToolCapabilities 与 output budget；接线 coding-agent 的
  authorization / review 流程。
