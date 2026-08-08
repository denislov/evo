# Phase 10 / ARC-1030 最终清算

日期：2026-08-08。

## 清理结果

- 全仓 `TODO(ARC-*)` / execution debt 为零。
- Architecture Gate 唯一 oversized debt `agent-core/src/agent/turn/nodes.rs` 已按 compaction 与 continuation 状态职责拆分；债务表清空。
- `nodes.rs` 从 1199 行降至 811 行，新增子模块均小于 150 行。
- 发布实现新增的 `cli/desktop -> release-updater` 依赖边已纳入架构 allowlist。
- 未发现 dual-write 或 migration feature。
- 保留的 serde alias 仅剩 provider catalog 的 `allowFallbacks` 外部 wire 字段；web_fetch 的内部 `format` alias 已删除，schema 现在只接受 `output_format`。
- fixture 审计确认现有 DeepSeek SSE、TLS CA、cross-adapter ProductEvent、Phase 9 scenario 与 Desktop visual golden 均有活跃测试/审阅流程；没有可确认的旧 fixture，未做机械删除。

## 依赖审计

`cargo tree -d --workspace --all-targets` 已审阅：

- `notify 7` 仍由 theme hot reload 与 change tracker 使用；不是遗留 watcher。
- process 实现统一使用 std/tokio process 与 `workspace-runtime` primitive；没有第二个第三方 process runtime 可删除。
- `schemars 1.2` 仍是 ToolArgs/schema contract 的唯一生成库；没有并存的第一方 schema validator。
- license audit 发现直接依赖 `html2md` 为 GPL-3.0+；已替换为 Apache-2.0 的 `htmd 0.5.5`，避免把可清理的 copyleft 风险留给 notice。
- 大型重复主要来自 GPUI 的 Linux/macOS/Windows/Web 全目标图，以及上游图形/字体栈的版本分叉；第一方 manifests 没有可安全删除的重复大依赖。
- `sha2`、`syn`、`toml`、`windows-*` 等重复由不同上游版本约束产生，本任务不通过 patch/fork 强行统一。

## 文档与合规

- 重写 `docs/architecture.md`，加入 observability、release-updater、scenario-testing、发布和 migration 边界。
- 为全部 17 个 workspace crate 建立/更新 README，并将 manifest `readme` 指向对应文件。
- 新增 `docs/user-migration.md` 与 `docs/development-gates.md`。
- 两份 Grok 学习文档标记为历史输入，其行动建议不再作为 backlog。
- 新增根 `THIRD_PARTY_NOTICES.md`、GPUI provenance、Cargo license metadata 例外表和可执行 license audit。

## 最终验证

末次本地复验日期为 2026-08-08。以下命令均以 exit code 0 完成；完整 workspace 测试覆盖所有 workspace crates、all targets，并使用 `--test-threads=4`：

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets -- --test-threads=4
scripts/release-api-snapshots.sh
scripts/architecture-gate.sh --final
scripts/license-audit.sh
git diff --check
```

完整 workspace 测试最终结果：所有执行中的测试均通过，失败数为 0；仅保留仓库既有的环境依赖型 ignored 测试（包括付费 provider live 测试与 release performance baseline）。

本机跨平台复验记录：

- `cargo check --workspace --all-targets --target x86_64-pc-windows-msvc`：阻断于 Linux 主机缺少 MSVC `lib.exe`，且 tree-sitter/aws-lc-sys 需要原生 Windows C 工具链。
- `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`：阻断于缺少 `x86_64-w64-mingw32-gcc`，失败发生在 `aws-lc-sys` C 构建阶段。

上述两项均为验证环境限制，不构成 Rust 源码失败；Windows 原生构建、打包和运行时路径仍由 `.github/workflows/release.yml` 的 `windows-2025` job 负责。

跨平台 release job、PTY/VirtualTerminal scenario 与 Desktop replay 见 `docs/development-gates.md`。
