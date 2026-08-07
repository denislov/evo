//! 增量 reindex：消费 `change-tracker` 的 `FsEvent` 流更新共享索引。
//!
//! 设计（与 Grok `IndexManager` 的差异）：
//!
//! - Grok 的 channel actor 由自身接收文件事件；Evo 复用 `change-tracker`
//!   的 `FsEventService`（debounce / rename 配对 / gitignore 过滤已由其
//!   完成），本模块只消费语义事件；
//! - 事件策略：
//!   - `Created` / `Modified`（文件）→ `reindex_file` 重解析替换；
//!   - `Removed` → `remove_file`（符号消失）；
//!   - `Renamed` → 旧路径 `rename_file` + 目标路径重解析（内容可能已变）；
//!   - `WatchGap` / broadcast `Lagged` → 全量 `reconcile`（重新扫描、
//!     对比 meta、修正漂移）；
//!   - `Git` 事件忽略（revision 语义由调用方决定，见债务登记）；
//!   - 目录事件忽略（文件级事件会单独到达；Grok 同款）。
//! - shutdown 顺序：停止消费（watch 信号）→ 同步等待在途事件处理完
//!   （std mpsc 完成信号，允许 `QueryBackend::shutdown` 保持同步签名）
//!   → 由调用方（`GraphQueryBackend::shutdown`）持久化 → 关闭。

// Evo 原创模块（消费 change-tracker 的 FsEvent；Grok 的 IndexManager 事件
// 面为自建 channel，语义近似但不复用）。
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use change_tracker::{FsChangeKind, FsEvent};
use tokio::sync::{broadcast, watch};

use crate::languages::LanguageRegistry;

use super::build::{IndexSkip, reconcile, reindex_file};
use super::index::CodebaseIndex;

/// 停止时等待在途事件处理完的时长上限（防御性；正常情况远小于此）。
const STOP_JOIN_TIMEOUT: Duration = Duration::from_secs(30);

/// 事件消费 actor 的句柄。
#[derive(Debug)]
pub struct IncrementalIndexer {
    shutdown_tx: watch::Sender<bool>,
    done: Option<Receiver<()>>,
    _join: tokio::task::JoinHandle<()>,
}

impl IncrementalIndexer {
    /// 停止消费并同步等待在途事件处理完。随后索引保持最终一致；
    /// 持久化由调用方（`GraphQueryBackend::shutdown`）负责。
    /// 幂等：重复调用直接返回。
    pub fn stop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(done) = self.done.take() {
            let _ = done.recv_timeout(STOP_JOIN_TIMEOUT);
        }
    }
}

impl Drop for IncrementalIndexer {
    fn drop(&mut self) {
        // 未显式 stop：发信号并尽力等待（避免在途事件半途被取消）。
        self.stop();
    }
}

/// 启动增量 reindex actor。`skipped` 收集器由调用方传入（可跨多次
/// reconcile 累计）。
pub fn spawn_incremental_indexer(
    index: Arc<RwLock<CodebaseIndex>>,
    root: PathBuf,
    registry: LanguageRegistry,
    events: broadcast::Receiver<FsEvent>,
    skipped: Arc<std::sync::Mutex<Vec<IndexSkip>>>,
) -> IncrementalIndexer {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (done_tx, done_rx) = channel();
    let join = tokio::spawn(async move {
        consume_loop(index, root, registry, events, shutdown_rx, skipped).await;
        let _ = done_tx.send(());
    });
    IncrementalIndexer {
        shutdown_tx,
        done: Some(done_rx),
        _join: join,
    }
}

/// 增量消费主循环。
async fn consume_loop(
    index: Arc<RwLock<CodebaseIndex>>,
    root: PathBuf,
    registry: LanguageRegistry,
    mut events: broadcast::Receiver<FsEvent>,
    mut shutdown_rx: watch::Receiver<bool>,
    skipped: Arc<std::sync::Mutex<Vec<IndexSkip>>>,
) {
    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                let _ = changed;
                break;
            }
            event = events.recv() => {
                match event {
                    Ok(FsEvent::Workspace(semantic)) => {
                        if semantic.is_directory {
                            continue;
                        }
                        handle_semantic(
                            &index, &root, &registry, &semantic.path, semantic.from.as_deref(),
                            semantic.kind, &skipped,
                        );
                    }
                    Ok(FsEvent::WatchGap { .. }) => {
                        handle_reconcile(&index, &root, &registry, &skipped);
                    }
                    Ok(FsEvent::Git(_)) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // 消费落后：事件丢失，全量 reconcile 收敛。
                        handle_reconcile(&index, &root, &registry, &skipped);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

/// 处理一个语义事件。
fn handle_semantic(
    index: &Arc<RwLock<CodebaseIndex>>,
    root: &Path,
    registry: &LanguageRegistry,
    path: &Path,
    from: Option<&Path>,
    kind: FsChangeKind,
    skipped: &Arc<std::sync::Mutex<Vec<IndexSkip>>>,
) {
    let rel = match normalize(path) {
        Some(rel) => rel,
        None => return,
    };
    let from_rel = from.and_then(normalize);
    let mut guard = match index.write() {
        Ok(guard) => guard,
        Err(_) => return, // 索引锁中毒：静默退出（下一事件 / reconcile 收敛）。
    };
    let mut skip_buf = Vec::new();
    match kind {
        FsChangeKind::Created | FsChangeKind::Modified => {
            reindex_file(&mut guard, root, &rel, registry, &mut skip_buf);
        }
        FsChangeKind::Removed => {
            guard.remove_file(&rel);
        }
        FsChangeKind::Renamed => {
            if let Some(from_rel) = from_rel {
                guard.rename_file(&from_rel, &rel);
            }
            // 目标路径重解析：内容可能已变（rename 与编辑可能同窗口）。
            reindex_file(&mut guard, root, &rel, registry, &mut skip_buf);
        }
    }
    if !skip_buf.is_empty()
        && let Ok(mut collected) = skipped.lock()
    {
        collected.extend(skip_buf);
    }
}

/// WatchGap / Lagged：全量 reconcile。
fn handle_reconcile(
    index: &Arc<RwLock<CodebaseIndex>>,
    root: &Path,
    registry: &LanguageRegistry,
    skipped: &Arc<std::sync::Mutex<Vec<IndexSkip>>>,
) {
    let Ok(mut guard) = index.write() else {
        return;
    };
    let mut skip_buf = Vec::new();
    reconcile(&mut guard, root, registry, &mut skip_buf);
    if !skip_buf.is_empty()
        && let Ok(mut collected) = skipped.lock()
    {
        collected.extend(skip_buf);
    }
}

/// workspace-relative 路径归一化。
fn normalize(path: &Path) -> Option<String> {
    let rel = path.to_str()?;
    Some(rel.replace('\\', "/"))
}
