//! Hook 修改自动归因 `HookEdit`（ARC-730 hunk review 闭环）。
//!
//! [`HookEditAttribution`] 实现 extension-host 的 [`HookLifecycle`] 观察点
//! （注入到 `ExtensionHostOptions.hook_lifecycle`），在真实 hook 进程执行
//! 前后对比工作区文件状态：before 记录每个被 track 文件的磁盘基线
//! （revision + 内容），after 对 revision 变化的文件生成 `ChangeReceipt`
//! 并以 [`ChangeSource::HookEdit`] 提交给 review tracker。
//!
//! 与 change-tracker 因果窗口（ARC-410）的语义：
//!
//! - hook 写文件后，fs event 会在 `causal_window` 内到达 tracker。after
//!   回调在窗口内 `record_receipt(HookEdit)` 时，receipt 与 fs event 按
//!   `(path, after_exists, after_revision)` 双向匹配消费：receipt 先到则
//!   之后到达的 event 被消费（归因 HookEdit）；event 先到（未过期）则
//!   receipt 匹配消费它（同样归因 HookEdit）。
//! - 若 fs event 已过期被归因为 `ExternalEdit`（hook 运行超过窗口、
//!   receipt 的 before_revision 校验失败 = "stale"），**不重新归因**：
//!   既有外部修改事实保持不变，本次归因落诊断失败记录（先到先得，
//!   tracker 不支持覆盖既有事实）。
//! - tracker 从未 track 过的新文件（hook 新建）：before 基线不可见，
//!   窗口过期后按既有语义归因 `ExternalEdit`；本次自动归因只覆盖
//!   tracker 已知文件（含 accept 后快照为空的文件，经 facts 历史追踪）。
//!
//! 观察失败不阻断 hook 执行（全部错误吞掉并落 `hook_attribution_failed`
//! 诊断）。

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use change_tracker::{ChangeSource, HunkTrackerHandle, HunkTrackerSnapshot, TrackingContext};
use extension_host::api::{
    DiagnosticLevel, DiagnosticRecord, DiagnosticSink, ExtensionEvent, HookLifecycle,
    HookRunOutcome, HookSpec,
};
use workspace_runtime::api::{FilesystemReviewTargetError, OpenedEditFile, WorkspaceAccessHandle};

use crate::mutex::MutexExt;
use crate::tools::filesystem::diff::generate_unified_patch;
use crate::tools::filesystem::mutation_receipt::{bounded_diff, content_revision, receipt};

/// 归因内容读取上限（对齐 tracker 的 `max_content_bytes`）。
const MAX_ATTRIBUTION_BYTES: usize = 1024 * 1024;

/// before 时记录的单个文件基线。
#[derive(Debug, Clone, Default)]
struct BaselineEntry {
    exists: bool,
    /// 磁盘内容 hash（`exists` 时必有）。
    before_revision: Option<String>,
    /// 磁盘内容（用于生成 unified diff；超限文件为 `None`）。
    content: Option<Vec<u8>>,
}

/// Hook 修改自动归因：tracker handle 经 [`HookEditAttribution::bind`] 注入
/// （session 装配完成 review service 后调用）。
#[derive(Debug)]
pub(crate) struct HookEditAttribution {
    tracker: Arc<Mutex<Option<HunkTrackerHandle>>>,
    workspace: Option<WorkspaceAccessHandle>,
    baseline: Arc<Mutex<Option<BTreeMap<PathBuf, BaselineEntry>>>>,
    diagnostics: Option<Arc<dyn DiagnosticSink>>,
}

impl Clone for HookEditAttribution {
    fn clone(&self) -> Self {
        Self {
            tracker: Arc::clone(&self.tracker),
            workspace: self.workspace.clone(),
            baseline: Arc::clone(&self.baseline),
            diagnostics: self.diagnostics.clone(),
        }
    }
}

impl HookEditAttribution {
    pub(crate) fn new(
        tracker: Arc<Mutex<Option<HunkTrackerHandle>>>,
        workspace_root: impl Into<PathBuf>,
        diagnostics: Option<Arc<dyn DiagnosticSink>>,
    ) -> Self {
        let workspace_root = workspace_root.into();
        let workspace = WorkspaceAccessHandle::open_source(workspace_root).ok();
        Self {
            tracker,
            workspace,
            baseline: Arc::new(Mutex::new(None)),
            diagnostics,
        }
    }

    fn record_failure(&self, message: impl std::fmt::Display) {
        if let Some(sink) = &self.diagnostics {
            sink.emit(DiagnosticRecord {
                level: DiagnosticLevel::Warning,
                code: "hook_attribution_failed".into(),
                message: message.to_string(),
                extension_id: None,
                context: Default::default(),
            });
        }
    }

    fn handle(&self) -> Option<HunkTrackerHandle> {
        self.tracker
            .lock_resource("hook attribution tracker")
            .ok()?
            .as_ref()
            .cloned()
    }

    /// 通过 workspace capability 读取内容（上限截断；文件缺失返回 `None`）。
    async fn read_workspace_file(&self, relative: &Path) -> Option<Vec<u8>> {
        let relative = relative.to_string_lossy().replace('\\', "/");
        let Some(workspace) = &self.workspace else {
            self.record_failure("hook edit attribution has no workspace capability");
            return None;
        };
        let target = match workspace.prepare_workspace_review_target(&relative).await {
            Ok(target) => target,
            Err(FilesystemReviewTargetError::NotFound) => return None,
            Err(error) => {
                self.record_failure(format!(
                    "hook edit attribution could not open {relative}: {error:?}"
                ));
                return None;
            }
        };
        let opened = match target.opened_file() {
            Ok(opened) => opened,
            Err(error) => {
                self.record_failure(format!(
                    "hook edit attribution could not bind {relative}: {error}"
                ));
                return None;
            }
        };
        let bytes = match OpenedEditFile::new(opened, target.display_path().to_path_buf())
            .read_file()
            .await
        {
            Ok(bytes) => bytes,
            Err(error) => {
                self.record_failure(format!(
                    "hook edit attribution could not read {relative}: {error}"
                ));
                return None;
            }
        };
        (bytes.len() <= MAX_ATTRIBUTION_BYTES).then_some(bytes)
    }

    /// before 基线：tracker 已知文件（files 快照 ∪ facts 历史）的磁盘状态。
    async fn capture_baseline(&self, handle: &HunkTrackerHandle) {
        let Ok(snapshot) = handle.snapshot().await else {
            return;
        };
        let mut baseline = BTreeMap::new();
        for path in tracked_paths(&snapshot) {
            let content = self.read_workspace_file(&path).await;
            let before_revision = content.as_deref().map(content_revision);
            baseline.insert(
                path,
                BaselineEntry {
                    exists: content.is_some(),
                    before_revision,
                    content,
                },
            );
        }
        let mut baseline_map = self
            .baseline
            .lock_resource("hook attribution baseline")
            .expect("hook attribution baseline lock is not poisoned");
        *baseline_map = Some(baseline);
    }

    /// after 归因：对比磁盘与基线，变化的文件生成 `HookEdit` receipt。
    async fn attribute_changes(&self, handle: &HunkTrackerHandle, event: &ExtensionEvent) {
        let baseline = self
            .baseline
            .lock_resource("hook attribution baseline")
            .ok()
            .and_then(|mut baseline| baseline.take());
        let Some(baseline) = baseline else {
            return;
        };
        let Ok(snapshot) = handle.snapshot().await else {
            return;
        };
        for path in tracked_paths(&snapshot) {
            let before = baseline.get(&path).cloned().unwrap_or_default();
            let after = self.read_workspace_file(&path).await;
            let unchanged = before.exists == after.is_some()
                && before.before_revision == after.as_deref().map(content_revision);
            if unchanged {
                continue;
            }
            self.attribute_one(handle, event, &path, &before, after)
                .await;
        }
    }

    async fn attribute_one(
        &self,
        handle: &HunkTrackerHandle,
        event: &ExtensionEvent,
        path: &Path,
        before: &BaselineEntry,
        after: Option<Vec<u8>>,
    ) {
        let relative = path.to_string_lossy().replace('\\', "/");
        let Some(filesystem) = &self.workspace else {
            self.record_failure(format!(
                "hook edit attribution skipped for {relative}: workspace capability unavailable"
            ));
            return;
        };
        // fingerprint 与 review 动作（accept/reject）同源：存在用
        // review 目标、被删除用 vacant write 目标。
        let fingerprint = match after.is_some() {
            true => match filesystem.prepare_workspace_review_target(&relative).await {
                Ok(target) => target.target_fingerprint().to_owned(),
                Err(error) => {
                    self.record_failure(format!(
                        "hook edit attribution skipped for {relative}: {error:?}"
                    ));
                    return;
                }
            },
            false => match filesystem.prepare_target_for_tool("write", &relative).await {
                Ok(target) => target.target_fingerprint().to_owned(),
                Err(error) => {
                    self.record_failure(format!(
                        "hook edit attribution skipped for {relative}: {error:?}"
                    ));
                    return;
                }
            },
        };
        let diff = before
            .content
            .as_deref()
            .zip(after.as_deref())
            .and_then(|(before_bytes, after_bytes)| {
                std::str::from_utf8(before_bytes)
                    .ok()
                    .zip(std::str::from_utf8(after_bytes).ok())
            })
            .map(|(before, after)| generate_unified_patch(&relative, before, after))
            .and_then(bounded_diff);
        let change = receipt(
            relative.clone(),
            fingerprint,
            before.content.as_deref(),
            after.as_deref(),
            "hook_edit",
            diff,
        );
        let context = TrackingContext {
            session_id: event.session_id.clone(),
            turn_id: "hook".into(),
            operation_id: "hook_edit".into(),
            tool_call_id: None,
        };
        if let Err(error) = handle
            .record_receipt(change, ChangeSource::HookEdit, context)
            .await
        {
            // 因果窗口已过期 / 磁盘状态被外部修改覆盖：既有事实优先，
            // 不重新归因（先到先得）。
            self.record_failure(format!(
                "hook edit attribution failed for {relative}: {error}"
            ));
        }
    }
}

impl HookLifecycle for HookEditAttribution {
    fn before<'a>(
        &'a self,
        _event: &'a ExtensionEvent,
        _spec: &'a HookSpec,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let attribution = self.clone();
        Box::pin(async move {
            let Some(handle) = attribution.handle() else {
                return;
            };
            attribution.capture_baseline(&handle).await;
        })
    }

    fn after<'a>(
        &'a self,
        event: &'a ExtensionEvent,
        _spec: &'a HookSpec,
        _outcome: &'a HookRunOutcome,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let attribution = self.clone();
        Box::pin(async move {
            let Some(handle) = attribution.handle() else {
                return;
            };
            attribution.attribute_changes(&handle, event).await;
        })
    }
}

/// tracker 已知文件：files 快照（最新）优先，facts 历史兜底（accept 后
/// 快照为空的文件仍可归因后续 hook 修改）。
fn tracked_paths(snapshot: &HunkTrackerSnapshot) -> Vec<PathBuf> {
    let mut paths: BTreeMap<PathBuf, ()> = BTreeMap::new();
    for fact in &snapshot.facts {
        paths.insert(fact.path.clone(), ());
    }
    for file in &snapshot.files {
        paths.insert(file.path.clone(), ());
    }
    paths.into_keys().collect()
}

#[cfg(test)]
#[path = "hook_attribution_tests.rs"]
mod tests;
