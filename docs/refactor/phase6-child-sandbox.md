# Phase 6 / ARC-610：Child-process sandbox

> 状态：完成
> 前序：ARC-600（background task registry 共享同一 spawn 边界）
> 目标：获准执行的外部进程（首版：bash 工具，foreground + background）在
> spawn 边界受 OS 级策略约束 —— 跨平台 `SandboxProfile` + 能力探测 +
> fail-closed，Linux 用 Landlock 真实生效，macOS/Windows 报告不支持并
> 显式拒绝（不静默 unrestricted）。
> Phase 6 Gate 判定项：**获准 shell 仍受 OS policy 限制**（本 ARC 覆盖；
> provider resilience、web fetch SSRF 由其他 ARC 覆盖）。

## 决策

### profile 语义（每维度）

- **read roots / write roots：显式白名单 = 全量拒绝之外的例外**。非空列表
  时，列表之外的绝对路径被 OS 策略拒绝（fail-closed per path）；**空列表
  = 该方向无约束（继承宿主默认）**，避免把空集合误读为"拒绝一切"。
  write roots 同时授予读权限（Landlock 权限不隐含：写文件需要
  `WRITE_FILE`，但只授予写不授予读会让进程无法校验自己写的内容，因此
  write root 规则 = 全量 fs access）。read roots 只授 `READ_FILE |
  READ_DIR | EXECUTE`（Landlock 的 execute 属于 read 集合），**不授
  REFER**：从只读目录移出/链接文件会被拒绝，语义与"只读"一致。
  ABI v3+ 的 `TRUNCATE` 授予 write roots（`> file` 重定向需要）。
- **exec policy：首版仅 `Unrestricted`**。Landlock 不限制 `execve` 本身
  （执行受文件读权限门控），严格白名单需要 seccomp user-notification 或
  ptrace 类机制，超出首版范围；`ExecPolicy::AllowList` 枚举预留，任何
  平台请求它都返回 `SandboxUnsupported`（fail-closed），能力报告标注
  `exec: unrestricted-only (fs-gated)`。
- **network policy：首版 `none | all`**。`None` 在 Linux 上通过 Landlock
  ABI v4（内核 6.7+）的 `handled_access_net = BindTcp | ConnectTcp` 且
  不添加任何 port rule 实现：Landlock 对无规则匹配的 TCP connect/bind
  **全部拒绝**。`Loopback` 枚举预留，任何平台请求都 fail-closed
  （ABI v4 的 port rule 只能按端口号过滤，无法表达"仅 loopback 地址"，
  需按 `ip_local_port_range` + 系统端口组合建模，留待后续）。
- **env policy：复用 `EnvPolicy` 作为进程级约束**。关系明确：
  `ProcessSpec.env` 是内容（spawner 想传什么），`SandboxProfile.env` 是
  约束（进程最多能看到哪些 key）。组合规则：最终 env =
  spec 内容 ∩ profile 允许的 key（spec `Inherit` + profile
  `AllowList` 时对继承环境过滤）。实现完全在 spawn 边界（父进程
  `env_clear().envs(...)`），所有平台一致支持，能力报告
  `env: spawn-time environment filter`。shell 工具用
  `AllowList(safe_process_env())`，因此实际等于双重过滤。

### Linux Landlock 实现

- **依赖**：`landlock` crate 0.4.7（landlock-lsm 官方仓库
  landlock-lsm/rust-landlock，MIT/Apache-2.0）。用它做 ABI 探测、
  `handle_access` 兼容性校验、`PathFd` 打开（O_PATH+CLOEXEC）、
  `PathBeneath` 规则构建，避免手写 `landlock_*` 系统调用的 uapi 结构。
  理由：官方维护、覆盖内核 ABI 演进（V1-V9）、`CompatLevel` 语义完备。
- **固定 ABI 而非动态 ABI**：`REQUIRED_FS_ABI = V3`（REFER v2 +
  TRUNCATE v3）、`REQUIRED_NET_ABI = V4`（TCP）。探测 syscall
  （`landlock_create_ruleset(null, 0, VERSION)`）返回内核 ABI，低于要求
  时返回 `SandboxUnsupported`（fail-closed）。刻意不按探测值动态降级
  handled access：crate 文档明确警告动态 ABI 导致行为不可复现，固定
  版本使"内核支持则行为一致"成为确定性保证。
- **fork/exec 之间应用，async-signal-safe**：父进程构建 ruleset
  （canonicalize + add rule，可分配可报错），取出 `OwnedFd` 并设
  CLOEXEC；`tokio::Command::pre_exec` 闭包里只做两个裸 syscall：
  `prctl(PR_SET_NO_NEW_PRIVS, 1)`（阻断 setuid 提权）+
  `landlock_restrict_self(fd, 0)`。闭包内不分配、不 panic、无锁，
  失败通过返回 `io::Error` 使 spawn 失败。`landlock` crate 自带的
  `restrict_self()` 会做状态更新与错误包装，**不**在 pre_exec 中使用。
- **不存在的 root 路径跳过**：`PathFd::open` 返回 `ENOENT` 时跳过该
  root —— Landlock deny-by-default 使其不可访问，跳过绝不比失败更弱；
  其他打开错误（权限等）fail-closed。
- **seccomp：登记为后续，不做**。最小 BPF（阻止 ptrace /
  `process_vm_*`）可降低 Landlock 之外的内核攻击面，但首版复杂度不可控
  （过滤规则需随工具链 syscall 面维护、`SECCOMP_MODE_FILTER` 与
  `no_new_privs` 顺序、调试成本），且不贡献 Gate 判定项。文档记录。

### 平台分级与 fail-closed

- **macOS**：Seatbelt / `sandbox-exec` 在新 macOS 上受限且被弃用，App
  Sandbox 容器没有挂到子进程。首版 `SandboxCapability.fs/network =
  unsupported("...not implemented")`；请求 fs 约束或 network≠all 的
  profile 在 prepare 阶段返回 `SandboxUnsupported::Platform`。**显式
  失败，绝不静默 unrestricted**。
- **Windows**：Job Object 已有进程树约束（ARC-300），受限 token /
  AppContainer 子进程隔离未实现。同样能力报告 + fail-closed。
- **capability 报告**：`SandboxCapability::current()` 每维度返回
  `CapabilityDimension { supported, detail }`（detail 携带原因/探测值，
  如 `landlock abi=5 (requires v3)`）。测试用它做 skip，产品用它做
  诊断。探测是纯 syscall，cheap。

### spawn 边界接入与产品策略

- `ProcessSpec` 新增 `sandbox: Option<SandboxProfile>`（默认 `None`，
  既有行为零变化）；`SpawnedProcess::spawn` 在 spawn 前 prepare
  （平台不支持 → `ProcessOutcome::Failed { message: "sandbox setup
  failed: ..." }`，显式失败），`configure_process` 安装 env 过滤与
  pre_exec。**sandbox 只应用在 child spawn 边界，Desktop 主进程不受
  影响**。background driver 与前台共享 `SpawnedProcess::spawn`，
  ARC-600 的"沙箱只落一处"成立，background 自动同策略。
- **bash 工具默认 profile**：`SandboxProfile::product_default(cwd)`：
  read = workspace + 系统目录（/bin /sbin /usr /lib /lib64 /etc /opt
  /var /proc /dev /run /tmp）+ `$HOME`；write = workspace + /tmp +
  /dev（`/dev/null` 类 sink）；exec unrestricted；network all；env
  Inherit（spec 层已是 allowlist）。无用户配置概念，会话内 bash 一律
  携带。
- **fail-closed 是默认策略**：平台能力不足（macOS/Windows/旧内核）时
  shell 返回明确错误（"sandbox setup failed: ..."），**不做显式降级
  授权路径**（无配置项，避免 speculative config）；文档记录这是安全
  优先选择，后续可通过配置放宽为非沙箱 shell。这是有意的产品行为
  变化：本机 macOS/Windows 上 bash 工具将不可用，直到对应平台实现。
- 内部校验命令（self-healing edit 的 `sh -c` 检查）不属于"获准
  shell"，`sandbox: None`，不受影响。

## 落点

| 变更 | 位置 |
| --- | --- |
| SandboxProfile / NetworkPolicy / ExecPolicy / SandboxCapability / SandboxUnsupported / PreparedSandbox | `crates/workspace-runtime/src/sandbox/mod.rs`（新增） |
| Linux Landlock（ABI 探测、ruleset 构建、pre_exec restrict） | `crates/workspace-runtime/src/sandbox/linux.rs`（新增） |
| macOS / Windows / 其他平台能力分级 | `crates/workspace-runtime/src/sandbox/{macos,windows,other}.rs`（新增） |
| 单元测试（profile 语义 / env 组合 / fail-closed / 能力报告） | `crates/workspace-runtime/src/sandbox/tests_sandbox.rs`（新增） |
| Linux 集成测试（真实 Landlock 拒绝越界读写、网络；能力探测 skip） | `crates/workspace-runtime/src/sandbox/linux/tests_linux.rs`（新增） |
| ProcessSpec.sandbox 字段 + spawn 边界接入（prepare / env 过滤 / pre_exec） | `crates/workspace-runtime/src/process/mod.rs` |
| 公开 facade | `crates/workspace-runtime/src/api.rs` |
| bash 工具默认 sandbox profile + 端到端测试 | `crates/coding-agent/src/tools/shell.rs` |
| 设计文档 | `docs/refactor/phase6-child-sandbox.md`（本文件） |

## 验证

```text
cargo test --locked -p workspace-runtime --all-features
127 passed（111 既有 + 16 新增）
- tests_sandbox 8 项：product_default 覆盖、空 roots 不约束 fs、
  env 组合 4 向、exec allow-list fail-closed、loopback fail-closed、
  capability dimension 携带原因、Linux 能力报告与内核探测一致
- linux/tests_linux 8 项集成（真实 Landlock，能力探测 skip）：
  写 write roots 之外被拒（EACCES，文件未创建）、读 roots 之外被拒、
  写 write root 内成功、读 read root 内成功、read-only root 拒绝写、
  无 sandbox 保持 legacy 行为、network=none 拒绝 TCP connect、
  prepare 路径可执行
- 本机内核 6.12（landlock abi=5）：全部真实执行，无 skip

cargo test --locked -p coding-agent --all-features
225 passed（224 既有 + 1 新增 bash_runs_inside_the_product_default_sandbox：
workspace 内读写成功；$HOME 写被 OS 拒绝 exit=1 且文件未创建）

cargo check --workspace --all-features
通过

cargo clippy -p workspace-runtime -p coding-agent --all-targets --all-features -- -D warnings
通过（0 warnings）

cargo fmt --all -- --check
通过

scripts/architecture-gate.sh
architecture_gate rust_files=670 dependency_edges=17 oversized_debts=35 execution_debts=0
```

## 后续（登记债务，均为分级允许项，非缺陷）

- **seccomp 最小 BPF**（linux.rs 能力报告网络/exec 之外的纵深防御）：
  阻止 ptrace / process_vm_* / 内核攻击面收窄。需要按工具链维护 syscall
  白名单，复杂度首版不可控。
- **exec allow-list**：需 seccomp user-notification 或 ptrace 拦截
  execve，当前所有平台 fail-closed。
- **network loopback**：ABI v4 port rule 无法按地址过滤，需
  `ip_local_port_range` + 系统端口建模；当前 fail-closed。
- **macOS Seatbelt / Windows AppContainer 或受限 token**：平台实现，
  实现前该平台 bash 工具 fail-closed 不可用（已文档化的产品行为）。
- **hook / MCP stdio server / LSP**：仓库当前无这些 spawn 路径；接入
  时复用 `ProcessSpec.sandbox` 与 `prepare_sandbox`，无需新机制。
- **显式降级授权配置**：允许用户配置"非沙箱 shell"的逃生通道，当前
  无配置项，shell 在能力不足平台一律报错。
- **read roots 的 REFER**：只读目录不支持移出/链接（语义见上），如
  产品需要可后续为特定 root 放开。
