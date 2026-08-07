use super::*;

use change_tracker::{HunkTrackerOptions, HunkTrackingService, WatchOptions};
use extension_host::api::DiagnosticRecord;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};
use tool_contract::api::definition::ToolId;
use workspace_runtime::api::{WorkspaceHandle, WorkspaceKind};

fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn baseline_receipt(
    path: &str,
    fingerprint: &str,
    before: Option<&[u8]>,
    after: Option<&[u8]>,
    origin: &str,
) -> change_tracker::ChangeReceipt {
    let diff = before
        .zip(after)
        .and_then(|(before, after)| {
            std::str::from_utf8(before)
                .ok()
                .zip(std::str::from_utf8(after).ok())
        })
        .map(|(before, after)| {
            crate::tools::filesystem::diff::generate_unified_patch(path, before, after)
        })
        .and_then(crate::tools::filesystem::mutation_receipt::bounded_diff);
    crate::tools::filesystem::mutation_receipt::receipt(
        path.into(),
        fingerprint.into(),
        before,
        after,
        origin,
        diff,
    )
}

/// 真实 review 目标指纹（与 review 动作 accept/reject 同源）。
async fn target_fingerprint(workspace: &std::path::Path, path: &str) -> String {
    let filesystem = workspace_runtime::api::WorkspaceAccessHandle::open_source(workspace).unwrap();
    filesystem
        .prepare_workspace_review_target(path)
        .await
        .unwrap()
        .target_fingerprint()
        .to_owned()
}

/// 真实跟踪服务：FsEventService watch + HunkTracker（与产品装配一致）。
fn start_service(workspace: &std::path::Path) -> HunkTrackingService {
    let identity = WorkspaceHandle::new(WorkspaceKind::Source, workspace).unwrap();
    HunkTrackingService::start(
        &identity,
        WatchOptions::default(),
        HunkTrackerOptions::default(),
    )
    .unwrap()
}

async fn wait_tracked(
    handle: &change_tracker::HunkTrackerHandle,
    path: &str,
) -> change_tracker::TrackedFileSnapshot {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let snapshot = handle.snapshot().await.unwrap();
        if let Some(file) = snapshot
            .files
            .iter()
            .find(|f| f.path.to_string_lossy() == path)
        {
            return file.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "file {path} is never tracked"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn post_tool_event(workspace: &std::path::Path) -> ExtensionEvent {
    ExtensionEvent::new(
        extension_host::api::ExtensionEventKind::PostToolUse,
        "s1",
        workspace.to_string_lossy(),
        "t",
        extension_host::api::ExtensionEventPayload::PostToolUse {
            tool_name: ToolId::new("bash").unwrap(),
            tool_input: json!({}),
            tool_result: json!({"ok": true}),
            tool_input_truncated: false,
            tool_result_truncated: false,
            duration_ms: None,
            path: None,
        },
    )
}

fn hook_spec(name: &str) -> HookSpec {
    HookSpec {
        name: name.into(),
        event: extension_host::api::ExtensionEventKind::PostToolUse,
        match_tool: None,
        match_path: None,
        match_profile: None,
        priority: 0,
        command: "true".into(),
        source_dir: std::path::PathBuf::from("/ext"),
        timeout_secs: None,
        enabled: true,
        matcher: extension_host::api::HookMatcher::match_all(),
    }
}

fn succeeded() -> HookRunOutcome {
    HookRunOutcome::Success
}

/// 收集归因诊断的 sink。
#[derive(Debug, Default, Clone)]
struct CollectingSink {
    failures: Arc<AtomicUsize>,
    records: Arc<Mutex<Vec<String>>>,
}

impl DiagnosticSink for CollectingSink {
    fn emit(&self, record: DiagnosticRecord) {
        self.records.lock().unwrap().push(record.code.clone());
        if record.code == "hook_attribution_failed" {
            self.failures.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// 未绑定 tracker 时观察点 no-op（不 panic、不产生副作用）。
#[tokio::test]
async fn unbound_observer_is_a_noop() {
    let attribution = HookEditAttribution::new(
        Arc::new(Mutex::new(None)),
        tempfile::tempdir().unwrap().path(),
        None,
    );
    let event = post_tool_event(tempfile::tempdir().unwrap().path());
    let spec = hook_spec("h");
    attribution.before(&event, &spec).await;
    attribution.after(&event, &spec, &succeeded()).await;
}

/// hook 修改已 track 文件 → after 自动归因 `HookEdit`（review 快照可见、
/// 有 hunks）；未修改不产生新事实。
#[tokio::test]
async fn hook_edit_is_attributed_automatically() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let notes = workspace.join("notes.txt");
    std::fs::write(&notes, b"before\n").unwrap();
    let service = start_service(&workspace);
    let handle = service.handle();
    // 模拟 agent 先编辑过该文件（AgentEdit 归因建立 track）：磁盘
    // "initial" → agent 写入 "before" → 提交 receipt（before=initial）。
    let fingerprint = target_fingerprint(&workspace, "notes.txt").await;
    std::fs::write(&notes, b"initial\n").unwrap();
    std::fs::write(&notes, b"before\n").unwrap();
    handle
        .record_receipt(
            baseline_receipt(
                "notes.txt",
                &fingerprint,
                Some(b"initial\n"),
                Some(b"before\n"),
                "edit",
            ),
            ChangeSource::AgentEdit,
            TrackingContext {
                session_id: "s1".into(),
                turn_id: "t1".into(),
                operation_id: "op1".into(),
                tool_call_id: Some("tool-1".into()),
            },
        )
        .await
        .unwrap();
    wait_tracked(&handle, "notes.txt").await;
    let slot = Arc::new(Mutex::new(Some(handle.clone())));
    let attribution = HookEditAttribution::new(slot, &workspace, None);
    let event = post_tool_event(&workspace);
    let spec = hook_spec("formatter");

    // hook 生命周期：before（基线）→ hook 进程写文件 → after（归因）。
    attribution.before(&event, &spec).await;
    std::fs::write(&notes, b"after\n").unwrap();
    attribution.after(&event, &spec, &succeeded()).await;

    let snapshot = handle.snapshot().await.unwrap();
    let file = snapshot
        .files
        .iter()
        .find(|file| file.path.to_string_lossy() == "notes.txt")
        .expect("notes.txt is tracked");
    assert_eq!(file.source, ChangeSource::HookEdit);
    // review 基线（相对首份 receipt 的初始状态），非上次修改。
    assert_eq!(
        file.before_revision.as_deref(),
        Some(revision(b"initial\n").as_str())
    );
    assert_eq!(file.after_revision, revision(b"after\n"));
    assert!(
        !file.hunks.is_empty(),
        "hook edit must produce reviewable hunks"
    );
    let hook_facts = snapshot
        .facts
        .iter()
        .filter(|fact| fact.source == ChangeSource::HookEdit)
        .count();
    assert_eq!(hook_facts, 1, "one HookEdit fact is recorded");

    // 同一 hook 生命周期不修改文件 → 无新事实。
    let facts_before = snapshot.facts.len();
    attribution.before(&event, &spec).await;
    attribution.after(&event, &spec, &succeeded()).await;
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.facts.len(),
        facts_before,
        "unchanged workspace produces no new attribution"
    );
}

/// accept 后（快照清空）再次 hook 修改 → 仍经 facts 历史归因 `HookEdit`。
#[tokio::test]
async fn hook_edit_after_accept_is_attributed_again() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let notes = workspace.join("notes.txt");
    std::fs::write(&notes, b"before\n").unwrap();
    let service = start_service(&workspace);
    let handle = service.handle();
    let fingerprint = target_fingerprint(&workspace, "notes.txt").await;
    std::fs::write(&notes, b"initial\n").unwrap();
    std::fs::write(&notes, b"before\n").unwrap();
    handle
        .record_receipt(
            baseline_receipt(
                "notes.txt",
                &fingerprint,
                Some(b"initial\n"),
                Some(b"before\n"),
                "edit",
            ),
            ChangeSource::AgentEdit,
            TrackingContext {
                session_id: "s1".into(),
                turn_id: "t1".into(),
                operation_id: "op1".into(),
                tool_call_id: Some("tool-1".into()),
            },
        )
        .await
        .unwrap();
    wait_tracked(&handle, "notes.txt").await;
    let slot = Arc::new(Mutex::new(Some(handle.clone())));
    let attribution = HookEditAttribution::new(slot, &workspace, None);
    let event = post_tool_event(&workspace);
    let spec = hook_spec("formatter");

    // 第一轮 hook 修改 → HookEdit。
    attribution.before(&event, &spec).await;
    std::fs::write(&notes, b"after\n").unwrap();
    attribution.after(&event, &spec, &succeeded()).await;

    // accept_file（baseline 前进、快照清空）。
    let file = wait_tracked(&handle, "notes.txt").await;
    handle
        .accept_file(
            "notes.txt",
            file.recorded_sequence,
            &file.after_revision,
            &fingerprint,
        )
        .await
        .unwrap();

    // 第二轮 hook 修改 → 再次 HookEdit（facts 历史兜底）。
    attribution.before(&event, &spec).await;
    std::fs::write(&notes, b"after2\n").unwrap();
    attribution.after(&event, &spec, &succeeded()).await;

    let snapshot = handle.snapshot().await.unwrap();
    let file = snapshot
        .files
        .iter()
        .find(|file| file.path.to_string_lossy() == "notes.txt")
        .unwrap();
    assert_eq!(file.source, ChangeSource::HookEdit);
    assert_eq!(
        file.before_revision.as_deref(),
        Some(revision(b"after\n").as_str())
    );
    assert_eq!(file.after_revision, revision(b"after2\n"));
}

/// 因果窗口已过（fs event 已归因外部修改）→ receipt 校验失败（stale），
/// 不重新归因：文件保持既有外部事实，诊断记录失败。
#[tokio::test]
async fn expired_causal_window_keeps_external_attribution() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let notes = workspace.join("notes.txt");
    std::fs::write(&notes, b"before\n").unwrap();
    let service = start_service(&workspace);
    let handle = service.handle();
    let fingerprint = target_fingerprint(&workspace, "notes.txt").await;
    std::fs::write(&notes, b"initial\n").unwrap();
    std::fs::write(&notes, b"before\n").unwrap();
    handle
        .record_receipt(
            baseline_receipt(
                "notes.txt",
                &fingerprint,
                Some(b"initial\n"),
                Some(b"before\n"),
                "edit",
            ),
            ChangeSource::AgentEdit,
            TrackingContext {
                session_id: "s1".into(),
                turn_id: "t1".into(),
                operation_id: "op1".into(),
                tool_call_id: Some("tool-1".into()),
            },
        )
        .await
        .unwrap();
    wait_tracked(&handle, "notes.txt").await;
    let sink = Arc::new(CollectingSink::default());
    let slot = Arc::new(Mutex::new(Some(handle.clone())));
    let attribution = HookEditAttribution::new(slot, &workspace, Some(sink.clone()));
    let event = post_tool_event(&workspace);
    let spec = hook_spec("formatter");

    // 外部进程（非 hook）在窗口外修改：fs event 过期 → ExternalEdit。
    std::fs::write(&notes, b"external\n").unwrap();
    tokio::time::sleep(
        HunkTrackerOptions::default().causal_window + std::time::Duration::from_millis(200),
    )
    .await;
    let _ = handle.snapshot().await.unwrap(); // flush 过期事件。

    // hook 生命周期尝试归因：before 基线是 external 状态；hook 未修改
    // 文件 → 无变化 → 不产生 receipt。
    attribution.before(&event, &spec).await;
    attribution.after(&event, &spec, &succeeded()).await;
    let snapshot = handle.snapshot().await.unwrap();
    let file = snapshot
        .files
        .iter()
        .find(|file| file.path.to_string_lossy() == "notes.txt")
        .unwrap();
    assert_eq!(
        file.source,
        ChangeSource::ExternalEditOnAgentFile,
        "agent-touched file keeps the external attribution once the window expired"
    );
    assert_eq!(sink.failures.load(Ordering::SeqCst), 0);
}

/// 窗口内 receipt 优先：hook 修改后归因在 fs event 过期前完成。
/// （receipt 与 fs event 的因果窗口关联：receipt 先到，后续 event 被消费。）
#[tokio::test]
async fn receipt_within_causal_window_consumes_fs_event() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let notes = workspace.join("notes.txt");
    std::fs::write(&notes, b"before\n").unwrap();
    let service = start_service(&workspace);
    let handle = service.handle();
    let fingerprint = target_fingerprint(&workspace, "notes.txt").await;
    std::fs::write(&notes, b"initial\n").unwrap();
    std::fs::write(&notes, b"before\n").unwrap();
    handle
        .record_receipt(
            baseline_receipt(
                "notes.txt",
                &fingerprint,
                Some(b"initial\n"),
                Some(b"before\n"),
                "edit",
            ),
            ChangeSource::AgentEdit,
            TrackingContext {
                session_id: "s1".into(),
                turn_id: "t1".into(),
                operation_id: "op1".into(),
                tool_call_id: Some("tool-1".into()),
            },
        )
        .await
        .unwrap();
    wait_tracked(&handle, "notes.txt").await;
    let slot = Arc::new(Mutex::new(Some(handle.clone())));
    let attribution = HookEditAttribution::new(slot, &workspace, None);
    let event = post_tool_event(&workspace);
    let spec = hook_spec("formatter");

    attribution.before(&event, &spec).await;
    std::fs::write(&notes, b"after\n").unwrap();
    // fs event 已在窗口内到达（pending）→ receipt 消费它。
    attribution.after(&event, &spec, &succeeded()).await;
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot.pending_events, 0,
        "fs event is consumed by the receipt"
    );
    let file = snapshot
        .files
        .iter()
        .find(|file| file.path.to_string_lossy() == "notes.txt")
        .unwrap();
    assert_eq!(file.source, ChangeSource::HookEdit);
}
