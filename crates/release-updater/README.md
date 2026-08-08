# `release-updater`

Evo 的 GitHub Releases updater 核心。它固定资产命名与平台矩阵，执行有界 release 查询、staged download、SHA-256 校验、解包和失败保留旧版本的安装切换。

该 crate 不主动更新：CLI/Desktop 决定何时检查、提示和取得用户确认。发布源默认是 `denislov/evo`。

第一方依赖：无。

验证：

```bash
cargo test -p release-updater --all-targets
cargo clippy -p release-updater --all-targets -- -D warnings
```

