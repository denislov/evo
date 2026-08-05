# Phase 4 / ARC-400：抽取 `change-tracker`

> 状态：完成（复验 2026-08-06）
> 前序：Phase 3 Gate（worktree 隔离、merge protocol、测试矩阵）
> 目标：把 review 从 tool event 投影升级为文件事实系统的第一层 —— 单 actor 所有权的
> 语义化 fs event service

## 决策

- **新 crate `change-tracker`，只依赖 `workspace-runtime`**：consumers 只看到
  `FsEvent`（`SemanticEvent` / `GitEvent` / `WatchGap`），绝不直接依赖
  `notify` 类型；`WorkspaceHandle` 是唯一进入点，crate 不持有任何产品
  session/UI 类型。
- **单 actor 线程模型**：`FsEventService::start` 立即返回前用 ready channel 同步
  等待 worker 完成 watcher 注册与 root 安装，并保留启动时创建的首个 receiver，
  消除 ready 到首次订阅之间的丢事件窗口；worker 独占 notify watcher、归一化、
  debounce 和 git 分类，命令（`add_root` / `shutdown`）经同一 actor 串行处理。
  `Drop`/`shutdown()` 幂等取消、显式唤醒并 join。
- **归一化规则**：
  - 路径相对化到 watch root；`rename` 按 backend tracker id 配对
    （`RenameFrom`/`RenameTo` 同 id，`Both` 直接成对），配对窗口内
    未配对片段保守降级为 `Removed`/`Created`；rename 两端分别经过 Git、
    root 与 ignore 判定，`Both` 和 fragment 两种上报形态在跨 root 或 ignore
    边界时都不伪造单 root rename。
  - bounded debounce 窗口内同路径合并，首个 buffered event 固定 flush deadline，
    持续事件流不能无限延后交付；合并优先级
    `Removed` 可被后续 `Created` 覆盖、`Created` 不被 `Modified` 降级、
    `Renamed` 不被目标路径的后续 `Modified` 抹除，保证消费者不会丢失
    无法从当前文件状态重建的 source path。
  - recursive backend 新增目录时立即补扫一次子树，关闭 backend 安装子目录 watch
    前的快速建树竞态；补扫仍复用同一 ignore/debounce 管线且不跟随 symlink。
  - `.git` 目录（含 worktree `gitdir:` 文件解析出的外部 gitdir）完全排除出
    workspace 事件流；其内部变化独立归一化为带 workspace root 与全局 sequence
    的 `GitEvent`
    （HEAD/refs → `HeadMoved`、index → `IndexChanged`、lock 与操作 marker →
    `OperationStarted/Completed`），消费者无需解析 `.git` 路径。
  - gitignore：root `.gitignore` 由 `ignore` crate 解析一次，
    `matched_path_or_any_parents` 过滤 `Ignore` 匹配路径；gitignore 热更新
    不在本 ARC 范围（hunk tracker 接 review 时再定）。
- **budget fail-closed**：`max_roots` 限制多 root 复用同一 watcher 的上限，
  超限 `WatchFailed` 报错且不注册；raw event 通道满时丢弃并累计
  `WatchGap { lost }`（slow consumer 的责任显式化），broadcast 满则丢事件
  并 `Lagged` 告知消费者；零 root/queue/debounce 以及平台时钟无法表示的
  debounce 配置在启动前结构化拒绝。
- **Grok `xai-fsnotify` 落点**：保留 semantic stream、debounce、watch budget、
  Git operation state 四要素，按 Evo 的 actor/typed contract 重建，不搬运
  notify backend 选择逻辑。

## 落点

| 变更 | 位置 |
| --- | --- |
| 新 crate 声明 | `crates/change-tracker/Cargo.toml`、根 workspace manifest |
| 事件类型 | `crates/change-tracker/src/event.rs` |
| 单 actor service | `crates/change-tracker/src/watch.rs` |
| git 元数据归一化 | `crates/change-tracker/src/git.rs` |
| 依赖 allowlist | `scripts/architecture/internal-dependencies.tsv` |

## 验证

```text
cargo test --locked -p change-tracker --all-features
24 passed（create/modify/remove、rename 配对、跨 root/ignore 边界降级、bounded debounce、
dynamic directory、gitignore 目录/rename 过滤、Git add/commit/ref/lock、普通 repo 与
linked worktree 的 root 归属、.git 不泄漏、多 root/嵌套 root、budget fail-closed、
WatchGap、启动订阅窗口、sequence 单调、非法配置、shutdown 幂等与即时唤醒）

cargo test --locked --workspace --all-features
全部通过

bash scripts/gate.sh
architecture gate 必须保持 execution_debts=0
```

## 后续

- ARC-410 HunkTracker actor 已完成，见 `docs/refactor/phase4-hunk-tracker.md`。
- ARC-420 Review domain：`FsEventService` 接入 `coding-agent` 后，review 不再
  由 tool event 投影推导。
- `notify` 平台差异（macOS FSEvents rename 形态、Windows 路径）沿用
  ARC-351 真实平台验证债务。
