# Evo 开发者 Gate

本文是当前开发验证命令的权威入口。历史 phase 文档中的测试数量和旧失败说明仅作为当时证据，不覆盖本文。

## 本地最小 Gate

所有改动至少运行：

```bash
cargo fmt --all -- --check
git diff --check
scripts/architecture-gate.sh
```

代码改动还应运行受影响 crate 的 test 与 clippy：

```bash
cargo test -p <crate> --all-targets
cargo clippy -p <crate> --all-targets -- -D warnings
```

## 最终架构 Gate

Phase/Release 收敛使用：

```bash
scripts/architecture-gate.sh --final
```

它强制：

- production Rust 文件不超过 900 行；test 文件不超过 1200 行；
- oversized debt registry 为空；
- execution debt registry 与 `TODO(ARC-DNNN, Phase N)` marker 都为空；
- first-party Cargo 依赖图与 allowlist 完全一致且无环；
- CLI/Desktop 只通过 `coding_agent::api` 访问产品 facade。

不允许通过提高 baseline 或新增 Phase 10 以后债务来绕过 final mode。

## Workspace Gate

发布前运行：

```bash
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
scripts/release-api-snapshots.sh
scripts/architecture-gate.sh --final
scripts/license-audit.sh
scripts/core-perf-gate.sh
```

Desktop 依赖本地 patched `gpui-component`。干净 checkout 先运行：

```bash
scripts/vendor-gpui-component.sh
```

## Adapter scenarios

```bash
cargo test -p scenario-testing --all-targets
cargo test -p cli --all-targets
cargo test -p tui --all-targets --features test-support
cargo test -p desktop --all-targets
```

这些测试覆盖 shared ProductEvent semantic oracle、mock inference SSE、VirtualTerminal/PTTY 等价场景与 Desktop deterministic replay。视觉 golden 更新必须通过 `scripts/desktop-visual-golden.sh` 的 review 流程，不允许直接覆盖图片而不审阅。

## 跨平台矩阵

Release 支持平台必须至少完成：

| 平台 | 必需检查 |
| --- | --- |
| Linux x86_64 GNU | workspace check/test/clippy、sandbox integration、CLI/TUI scenarios、Desktop replay、release build/package/install smoke |
| Windows x86_64 MSVC | workspace check/test/clippy、process tree/worktree/registry tests、CLI scenario、Desktop replay、PowerShell install/update smoke |

macOS/ARM 当前不是 updater 发布目标，但涉及通用 `cfg`、path 或 platform code 的改动仍应执行相应 `cargo check --target ...`，或明确报告工具链/SDK 不可用。

## License 与 provenance

`scripts/license-audit.sh` 验证 Cargo metadata license 完整性、缺失 metadata 例外、根 notice、provenance 和本地 patch 声明。它是机械一致性检查，不替代法律意见。新增 git/vendored dependency、复制/翻译代码或 license 变化必须先更新：

- `THIRD_PARTY_NOTICES.md`
- `docs/refactor/provenance/`
- `scripts/licenses/metadata-exceptions.tsv`（仅当上游 manifest 确实缺失 license 字段）

## Release Gate

Tag 发布前还必须验证：

- `.github/workflows/release.yml` 的四个资产名符合 contract；
- `checksums.txt` 覆盖所有资产；
- `scripts/install.sh` 与 `scripts/install.ps1` 校验失败时不安装；
- CLI `coding-agent update` 与 Desktop 确认流使用 staged install；
- 下载中断、hash mismatch、解包失败和替换失败均保留旧版本。

