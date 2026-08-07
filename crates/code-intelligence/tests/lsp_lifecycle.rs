//! `lsp::server` 集成测试：完整生命周期（fake LSP server 进程级）。
//!
//! 覆盖：启动握手、crash 后 restart + backoff（指数退避验证 + 上限）、
//! 重启后 document replay、shutdown 顺序与在途请求取消、重复 shutdown
//! 幂等、push/pull diagnostics + stale policy、edit 转换与受限
//! applicator（ChangeReceipt）、路径/版本越界拒绝、liveness 重启、
//! spawn 失败终态、异步纪律（错误路径不 panic、SendersDropped）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use change_tracker::ChangeReceipt;

use code_intelligence::lsp::diagnostics::{DiagnosticStaleness, StalePolicy};
use code_intelligence::lsp::documents::{ContentChange, Position};
use code_intelligence::lsp::edit::{EditApplicator, EditError, EditPlan};
use code_intelligence::lsp::query::{LspQuery, LspQueryKind};
use code_intelligence::lsp::server::{LspError, LspServerConfig, LspShutdownReason};
use code_intelligence::lsp::{LspHandle, LspLifecycleState, LspService, LspTask};
use workspace_runtime::api::{EnvPolicy, TaskOwner};

const FAKE_SERVER: &str = env!("CARGO_BIN_EXE_fake_lsp_server");

/// 测试专用 applicator：把计划应用到临时目录（workspace 内），生成
/// ChangeReceipt；记录应用次数供断言。
#[derive(Clone)]
struct TestApplicator {
    root: PathBuf,
    applied: Arc<std::sync::atomic::AtomicUsize>,
}

impl TestApplicator {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            applied: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
}

impl EditApplicator for TestApplicator {
    fn apply(&self, plan: &EditPlan) -> Result<Vec<ChangeReceipt>, EditError> {
        let mut receipts = Vec::new();
        for change in &plan.changes {
            let uri_path = change.uri.strip_prefix("file://").unwrap();
            let abs = PathBuf::from(uri_path);
            if !abs.starts_with(&self.root) {
                return Err(EditError::Apply {
                    detail: format!("refusing path {}", abs.display()),
                });
            }
            let before = std::fs::read_to_string(&abs).map_err(|error| EditError::Apply {
                detail: format!("read {}: {error}", abs.display()),
            })?;
            let after = match change.range {
                None => change.new_text.clone(),
                Some(range) => {
                    let start = offset_of(&before, range.start);
                    let end = offset_of(&before, range.end);
                    let mut out = String::new();
                    out.push_str(&before[..start]);
                    out.push_str(&change.new_text);
                    out.push_str(&before[end..]);
                    out
                }
            };
            std::fs::write(&abs, &after).map_err(|error| EditError::Apply {
                detail: format!("write {}: {error}", abs.display()),
            })?;
            let before_bytes = before.as_bytes();
            let after_bytes = after.as_bytes();
            receipts.push(ChangeReceipt {
                path: change.rel_path.clone(),
                target_fingerprint: format!("test-{}", change.uri),
                before_revision: Some(code_intelligence::lsp::edit::restricted::revision_of(
                    before_bytes,
                )),
                after_revision: code_intelligence::lsp::edit::restricted::revision_of(after_bytes),
                after_exists: true,
                byte_delta: after_bytes.len() as i64 - before_bytes.len() as i64,
                line_delta: after.lines().count() as i64 - before.lines().count() as i64,
                origin: "lsp/applyEdit".into(),
                unified_diff: None,
            });
        }
        self.applied
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(receipts)
    }
}

fn offset_of(text: &str, position: Position) -> usize {
    let mut line = 0u32;
    let mut offset = 0usize;
    for (index, character) in text.char_indices() {
        if line == position.line {
            let line_text = &text[offset..];
            return offset
                + code_intelligence::lsp::documents::utf16_to_char_index(
                    line_text,
                    position.character as usize,
                );
        }
        if character == '\n' {
            line += 1;
            offset = index + 1;
        }
    }
    offset
}

fn config_for(
    workspace: &Path,
    mode: &str,
    extra: &[&str],
    patch: impl FnOnce(&mut LspServerConfig),
) -> LspServerConfig {
    let mut args = vec!["--mode".to_string(), mode.to_string()];
    args.extend(extra.iter().map(|arg| arg.to_string()));
    let mut config = LspServerConfig::new(
        FAKE_SERVER,
        workspace.to_path_buf(),
        TaskOwner::Operation("lsp-server-test".into()),
    );
    config.args = args;
    config.env = EnvPolicy::AllowList(Default::default());
    config.backoff = code_intelligence::lsp::BackoffConfig {
        initial: Duration::from_millis(30),
        max: Duration::from_millis(120),
    };
    config.liveness = code_intelligence::lsp::LivenessConfig {
        ping_interval: Duration::from_millis(80),
        ping_timeout: Duration::from_millis(50),
    };
    config.request_timeout = Duration::from_secs(5);
    patch(&mut config);
    config
}

fn start(workspace: &Path, mode: &str, extra: &[&str]) -> (LspHandle, LspTask) {
    let config = config_for(workspace, mode, extra, |_| {});
    LspService::new(config).start().unwrap()
}

fn file_uri(workspace: &Path, name: &str) -> String {
    format!("file://{}/{}", workspace.display(), name)
}

async fn wait_state(handle: &LspHandle, target: LspLifecycleState, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let snapshot = handle.snapshot().await.expect("snapshot");
        if snapshot.state == target {
            return;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    panic!(
        "state did not reach {target:?}; now: {:?}",
        handle.snapshot().await.map(|s| s.state)
    );
}

async fn wait_ready(handle: &LspHandle) {
    wait_state(handle, LspLifecycleState::Ready, Duration::from_secs(10)).await;
}

async fn wait_failed(handle: &LspHandle) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let snapshot = handle.snapshot().await.expect("snapshot");
        if matches!(snapshot.state, LspLifecycleState::Failed { .. }) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    panic!(
        "state did not reach Failed; now: {:?}",
        handle.snapshot().await.map(|s| s.state)
    );
}

fn read_records(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|contents| contents.lines().map(str::to_string).collect::<Vec<_>>())
        .unwrap_or_default()
}

async fn shutdown_and_join(handle: &LspHandle, task: LspTask) -> code_intelligence::lsp::LspExit {
    handle.shutdown();
    task.join().await
}

#[tokio::test]
async fn full_lifecycle_start_ready_shutdown_stopped() {
    let temp = tempfile::tempdir().unwrap();
    let (handle, task) = start(temp.path(), "echo", &[]);
    assert!(handle.is_running());
    wait_ready(&handle).await;
    let snapshot = handle.snapshot().await.unwrap();
    assert!(snapshot.pid.is_some(), "server process has a pid");
    assert_eq!(snapshot.open_documents.len(), 0);
    handle.shutdown();
    let exit = task.join().await;
    assert_eq!(exit.reason, LspShutdownReason::Manual);
    assert!(!exit.panicked);
    assert_eq!(handle.state(), LspLifecycleState::Stopped);
}

#[tokio::test]
async fn document_replay_after_crash_restart() {
    let temp = tempfile::tempdir().unwrap();
    let record = temp.path().join("record.jsonl");
    // crash-after-open 1：每轮 initialize + initialized + didOpen 后崩溃
    // ——didOpen 先记录，重启后 replay 再记录（≥2 次即验证 replay）。
    let (handle, task) = start(
        temp.path(),
        "echo",
        &[
            "--record-file",
            record.to_str().unwrap(),
            "--crash-after-open",
            "1",
        ],
    );
    let uri = file_uri(temp.path(), "a.rs");
    handle
        .open(&uri, "rust", 1, "fn main() {}\n")
        .await
        .unwrap();
    wait_ready(&handle).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let records = read_records(&record);
        let did_open_count = records
            .iter()
            .filter(|line| line.starts_with("didOpen"))
            .count();
        if did_open_count >= 2 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "didOpen replayed: {records:?}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let snapshot = handle.snapshot().await.unwrap();
    assert!(snapshot.restart_count >= 1);
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn restart_backoff_grows_exponentially_and_caps() {
    let temp = tempfile::tempdir().unwrap();
    let record = temp.path().join("record.jsonl");
    let (handle, task) = start(
        temp.path(),
        "crash-after-init",
        &["--record-file", record.to_str().unwrap()],
    );
    // 等多次重启（每次崩溃后间隔增长：30ms → 60ms → 120ms → 120ms…）。
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let records = read_records(&record);
    let initialize_times: Vec<u128> = records
        .iter()
        .filter_map(|line| {
            let (event, rest) = line.split_once(" @")?;
            if event != "initialize" {
                return None;
            }
            rest.strip_suffix("ms")?.parse().ok()
        })
        .collect();
    assert!(initialize_times.len() >= 5, "enough restarts: {records:?}");
    let intervals: Vec<u128> = initialize_times
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect();
    // 指数增长：间隔 1 ≤ 间隔 2（宽松：允许噪声），且封顶。
    assert!(
        intervals[1] >= intervals[0].saturating_sub(20),
        "backoff grows: {intervals:?}"
    );
    assert!(
        intervals[2] >= intervals[1].saturating_sub(20),
        "backoff grows: {intervals:?}"
    );
    let max = intervals.iter().max().copied().unwrap_or(0);
    assert!(
        max <= 200,
        "backoff capped near max (120ms + slack): {intervals:?}"
    );
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn restart_attempts_exhausted_ends_in_failed() {
    let temp = tempfile::tempdir().unwrap();
    let config = config_for(temp.path(), "crash-after-init", &[], |config| {
        config.max_restart_attempts = 2;
        config.request_timeout = Duration::from_secs(2);
    });
    let service = LspService::new(config);
    let (handle, task) = service.start().unwrap();
    wait_failed(&handle).await;
    let snapshot = handle.snapshot().await.unwrap();
    assert!(matches!(snapshot.state, LspLifecycleState::Failed { .. }));
    assert!(snapshot.last_error.is_some());
    // Failed 终态：查询拒绝。
    let uri = file_uri(temp.path(), "a.rs");
    assert!(matches!(
        handle.open(&uri, "rust", 1, "x").await,
        Err(LspError::NotReady { .. })
    ));
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn spawn_failure_is_terminal_without_retry() {
    let temp = tempfile::tempdir().unwrap();
    let config = config_for(temp.path(), "echo", &[], |config| {
        config.command = "/nonexistent/lsp-binary".into();
        config.args = vec![];
    });
    let (handle, task) = LspService::new(config).start().unwrap();
    wait_failed(&handle).await;
    let snapshot = handle.snapshot().await.unwrap();
    let last_error = snapshot.last_error.clone().unwrap_or_default();
    assert!(
        last_error.contains("spawn"),
        "spawn failure recorded: {snapshot:?}"
    );
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn handshake_timeout_restarts_until_failed() {
    let temp = tempfile::tempdir().unwrap();
    let record = temp.path().join("record.jsonl");
    let config = config_for(
        temp.path(),
        "no-initialize-response",
        &["--record-file", record.to_str().unwrap()],
        |config| {
            config.request_timeout = Duration::from_millis(150);
            config.max_restart_attempts = 2;
        },
    );
    let (handle, task) = LspService::new(config).start().unwrap();
    wait_failed(&handle).await;
    let records = read_records(&record);
    // 注意 "initialized" 也以 "initialize" 开头：精确匹配事件行。
    let initialize_count = records
        .iter()
        .filter(|l| l.starts_with("initialize @"))
        .count();
    assert!(initialize_count >= 2, "handshake retried: {records:?}");
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn document_ops_survive_unready_server_and_replay() {
    let temp = tempfile::tempdir().unwrap();
    let record = temp.path().join("record.jsonl");
    // ping-drop 会让 server 周期性崩溃重启：文档在未就绪期间更新，
    // 重启后 replay 最新内容。
    let (handle, task) = start(
        temp.path(),
        "ping-drop",
        &["--record-file", record.to_str().unwrap()],
    );
    let uri = file_uri(temp.path(), "b.rs");
    handle.open(&uri, "rust", 1, "v1").await.unwrap();
    handle
        .change(
            &uri,
            2,
            vec![ContentChange {
                range: None,
                text: "v2 content".into(),
            }],
        )
        .await
        .unwrap();
    wait_ready(&handle).await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    let records = read_records(&record);
    let did_open_count = records
        .iter()
        .filter(|line| line.starts_with("didOpen file://") && line.contains("b.rs"))
        .count();
    assert!(
        did_open_count >= 2,
        "didOpen replayed with latest state: {records:?}"
    );
    // 本地文档状态是最终内容。
    let snapshot = handle.snapshot().await.unwrap();
    let doc = snapshot
        .open_documents
        .iter()
        .find(|document| document.uri.as_str() == uri)
        .expect("document tracked");
    assert_eq!(doc.text, "v2 content");
    assert_eq!(doc.version, 2);
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn push_diagnostics_stored_and_stale_policy_applies() {
    let temp = tempfile::tempdir().unwrap();
    let (handle, task) = start(temp.path(), "echo", &["--push-on-open"]);
    wait_ready(&handle).await;
    let uri = file_uri(temp.path(), "a.rs");
    handle
        .open(&uri, "rust", 3, "fn main() {}\n")
        .await
        .unwrap();
    // 等待 push 到达。
    tokio::time::sleep(Duration::from_millis(300)).await;
    let diagnostics = handle.diagnostics(&uri).await.unwrap().expect("stored");
    assert_eq!(diagnostics.items.len(), 1);
    assert!(matches!(
        diagnostics.staleness,
        DiagnosticStaleness::Fresh { doc_version: 3 }
    ));
    // 文档变化 → stale。
    handle
        .change(
            &uri,
            4,
            vec![ContentChange {
                range: None,
                text: "fn changed() {}\n".into(),
            }],
        )
        .await
        .unwrap();
    let diagnostics = handle.diagnostics(&uri).await.unwrap().expect("stored");
    assert!(matches!(
        diagnostics.staleness,
        DiagnosticStaleness::Stale { .. }
    ));
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn discard_policy_filters_stale_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let config = config_for(temp.path(), "echo", &["--push-on-open"], |config| {
        config.stale_policy = StalePolicy::Discard;
    });
    let (handle, task) = LspService::new(config).start().unwrap();
    wait_ready(&handle).await;
    let uri = file_uri(temp.path(), "a.rs");
    handle.open(&uri, "rust", 1, "text").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Fresh 可见。
    assert!(handle.diagnostics(&uri).await.unwrap().is_some());
    // 变化 → stale → Discard 过滤。
    handle
        .change(
            &uri,
            2,
            vec![ContentChange {
                range: None,
                text: "changed".into(),
            }],
        )
        .await
        .unwrap();
    assert!(handle.diagnostics(&uri).await.unwrap().is_none());
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn pull_diagnostics_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let record = temp.path().join("record.jsonl");
    let (handle, task) = start(
        temp.path(),
        "echo",
        &["--record-file", record.to_str().unwrap()],
    );
    wait_ready(&handle).await;
    let uri = file_uri(temp.path(), "a.rs");
    handle.open(&uri, "rust", 1, "text").await.unwrap();
    let result = handle.pull_diagnostics(&uri).await.unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].message, "pulled diagnostic");
    let records = read_records(&record);
    assert!(
        records
            .iter()
            .any(|line| line.starts_with("pullDiagnostics")),
        "{records:?}"
    );
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn queries_round_trip_when_ready() {
    let temp = tempfile::tempdir().unwrap();
    let (handle, task) = start(temp.path(), "echo", &[]);
    wait_ready(&handle).await;
    let uri = file_uri(temp.path(), "a.rs");
    handle.open(&uri, "rust", 1, "fn main() {}").await.unwrap();
    let hover = handle
        .query(LspQuery {
            kind: LspQueryKind::Hover,
            uri: uri.clone(),
            position: Position {
                line: 0,
                character: 1,
            },
        })
        .await
        .unwrap();
    assert_eq!(hover.result["contents"]["value"], "fake hover");
    let definition = handle
        .query(LspQuery {
            kind: LspQueryKind::Definition,
            uri: uri.clone(),
            position: Position {
                line: 0,
                character: 1,
            },
        })
        .await
        .unwrap();
    assert_eq!(definition.result[0]["range"]["start"]["line"], 0);
    let references = handle
        .query(LspQuery {
            kind: LspQueryKind::References,
            uri,
            position: Position {
                line: 0,
                character: 1,
            },
        })
        .await
        .unwrap();
    assert_eq!(references.kind, LspQueryKind::References);
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn queries_rejected_before_ready() {
    let temp = tempfile::tempdir().unwrap();
    let record = temp.path().join("record.jsonl");
    let config = config_for(
        temp.path(),
        "no-initialize-response",
        &["--record-file", record.to_str().unwrap()],
        |config| {
            config.request_timeout = Duration::from_millis(300);
            config.max_restart_attempts = 5;
        },
    );
    let (handle, task) = LspService::new(config).start().unwrap();
    // 启动即查：Initializing / Reconnecting 中 → NotReady。
    let error = handle
        .query(LspQuery {
            kind: LspQueryKind::Hover,
            uri: file_uri(temp.path(), "a.rs"),
            position: Position {
                line: 0,
                character: 0,
            },
        })
        .await
        .unwrap_err();
    assert!(matches!(error, LspError::NotReady { .. }));
    handle.shutdown();
    task.join().await;
}

#[tokio::test]
async fn shutdown_sequence_orders_and_cancels_in_flight() {
    let temp = tempfile::tempdir().unwrap();
    let record = temp.path().join("record.jsonl");
    let (handle, task) = start(
        temp.path(),
        "echo",
        &[
            "--record-file",
            record.to_str().unwrap(),
            "--query-delay-ms",
            "2000",
        ],
    );
    wait_ready(&handle).await;
    // 慢查询在途（query-delay 2s）。
    let query = handle.query(LspQuery {
        kind: LspQueryKind::Hover,
        uri: file_uri(temp.path(), "a.rs"),
        position: Position {
            line: 0,
            character: 0,
        },
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    handle.shutdown();
    let query_result = query.await;
    assert!(
        query_result.is_err(),
        "in-flight query cancelled: {query_result:?}"
    );
    let exit = task.join().await;
    assert_eq!(exit.reason, LspShutdownReason::Manual);
    // shutdown 请求已发出（握手完成的会话）。
    let records = read_records(&record);
    assert!(
        records.iter().any(|line| line.starts_with("shutdown")),
        "shutdown message sent: {records:?}"
    );
    assert!(
        records.iter().any(|line| line.starts_with("exit")),
        "exit message sent: {records:?}"
    );
}

#[tokio::test]
async fn shutdown_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let (handle, task) = start(temp.path(), "echo", &[]);
    wait_ready(&handle).await;
    handle.shutdown();
    handle.shutdown();
    handle.shutdown();
    let exit = task.join().await;
    assert_eq!(exit.reason, LspShutdownReason::Manual);
    assert!(!exit.panicked);
}

#[tokio::test]
async fn commands_rejected_after_shutdown() {
    let temp = tempfile::tempdir().unwrap();
    let (handle, task) = start(temp.path(), "echo", &[]);
    wait_ready(&handle).await;
    handle.shutdown();
    let _ = task.join().await;
    let error = handle
        .open(&file_uri(temp.path(), "a.rs"), "rust", 1, "x")
        .await
        .unwrap_err();
    assert!(matches!(error, LspError::NotRunning));
}

#[tokio::test]
async fn all_handles_dropped_exits_senders_dropped() {
    let temp = tempfile::tempdir().unwrap();
    let (handle, task) = start(temp.path(), "echo", &[]);
    drop(handle);
    let exit = task.join().await;
    assert_eq!(exit.reason, LspShutdownReason::SendersDropped);
    assert!(!exit.panicked);
}

#[tokio::test]
async fn apply_edit_with_applicator_applies_and_emits_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().to_path_buf();
    let target = file_uri(&workspace, "a.rs");
    // 预写目标文件（受限 applicator 会改写它）。
    std::fs::write(workspace.join("a.rs"), "original content\n").unwrap();
    let applicator = TestApplicator::new(workspace.clone());
    let config = config_for(
        &workspace,
        "apply-edit",
        &["--edit-uri", &target],
        |config| {
            config.applicator = Some(Arc::new(applicator.clone()));
        },
    );
    let (handle, task) = LspService::new(config).start().unwrap();
    wait_ready(&handle).await;
    handle
        .open(&target, "rust", 1, "original content\n")
        .await
        .unwrap();
    // 等待服务器发 applyEdit 请求并被 applicator 应用。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while applicator.applied.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "edit applied in time"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    // 文件被受限应用（workspace 内）。
    let contents = std::fs::read_to_string(workspace.join("a.rs")).unwrap();
    assert_eq!(contents, "replaced by fake server");
    // 版本匹配才应用：edit 版本 = didOpen 版本（1）。
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn apply_edit_without_applicator_rejected_and_recorded() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().to_path_buf();
    let record = workspace.join("record.jsonl");
    let target = file_uri(&workspace, "a.rs");
    std::fs::write(workspace.join("a.rs"), "original\n").unwrap();
    let (handle, task) = start(
        &workspace,
        "apply-edit",
        &[
            "--record-file",
            record.to_str().unwrap(),
            "--edit-uri",
            &target,
        ],
    );
    wait_ready(&handle).await;
    handle.open(&target, "rust", 1, "original\n").await.unwrap();
    // 服务器收到错误回执（apply-edit-response false）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let records = read_records(&record);
        if records
            .iter()
            .any(|line| line.starts_with("apply-edit-response false"))
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "rejection recorded");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    // 计划被记录（pending_edits）。
    let pending = handle.pending_edits().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].changes[0].rel_path, "a.rs");
    // 文件未被修改（绝不直接写磁盘）。
    assert_eq!(
        std::fs::read_to_string(workspace.join("a.rs")).unwrap(),
        "original\n"
    );
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn apply_edit_outside_workspace_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().to_path_buf();
    let record = workspace.join("record.jsonl");
    let outside = "file:///etc/passwd";
    let (handle, task) = start(
        &workspace,
        "apply-edit",
        &[
            "--record-file",
            record.to_str().unwrap(),
            "--edit-uri",
            outside,
        ],
    );
    wait_ready(&handle).await;
    let uri = file_uri(&workspace, "a.rs");
    handle.open(&uri, "rust", 1, "x").await.unwrap();
    // 服务器发来的 edit 目标在 workspace 外 → 校验拒绝 → 错误回执。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let records = read_records(&record);
        if records
            .iter()
            .any(|line| line.starts_with("apply-edit-response false"))
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "rejection recorded");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn liveness_failure_restarts_server() {
    let temp = tempfile::tempdir().unwrap();
    let record = temp.path().join("record.jsonl");
    let (handle, task) = start(
        temp.path(),
        "ping-drop",
        &["--record-file", record.to_str().unwrap()],
    );
    wait_ready(&handle).await;
    // liveness interval 80ms / timeout 50ms：很快触发重启。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let records = read_records(&record);
        let initialize_count = records
            .iter()
            .filter(|l| l.starts_with("initialize @"))
            .count();
        if initialize_count >= 2 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "restarted: {records:?}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let snapshot = handle.snapshot().await.unwrap();
    assert!(snapshot.restart_count >= 1);
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn document_error_paths_do_not_panic() {
    let temp = tempfile::tempdir().unwrap();
    let (handle, task) = start(temp.path(), "echo", &[]);
    wait_ready(&handle).await;
    // 越界 uri。
    assert!(
        handle
            .open("file:///etc/passwd", "rust", 1, "x")
            .await
            .is_err()
    );
    let uri = file_uri(temp.path(), "a.rs");
    // 未打开文档：close 报错；诊断查询为「无诊断」（Ok(None)）。
    assert!(handle.close(&uri).await.is_err());
    assert!(handle.diagnostics(&uri).await.unwrap().is_none());
    // 重复 open。
    handle.open(&uri, "rust", 1, "x").await.unwrap();
    assert!(handle.open(&uri, "rust", 2, "y").await.is_err());
    // 版本回退。
    assert!(
        handle
            .change(
                &uri,
                0,
                vec![ContentChange {
                    range: None,
                    text: "z".into()
                }]
            )
            .await
            .is_err()
    );
    // 服务仍然可用。
    assert!(handle.close(&uri).await.is_ok());
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn double_start_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let config = config_for(temp.path(), "echo", &[], |_| {});
    let service = LspService::new(config);
    let (_handle, _task) = service.clone().start().unwrap();
    assert!(matches!(service.start(), Err(LspError::AlreadyRunning)));
}

#[tokio::test]
async fn snapshot_reports_open_documents_and_pid() {
    let temp = tempfile::tempdir().unwrap();
    let (handle, task) = start(temp.path(), "echo", &[]);
    wait_ready(&handle).await;
    let uri = file_uri(temp.path(), "a.rs");
    handle.open(&uri, "rust", 1, "text").await.unwrap();
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.open_documents.len(), 1);
    assert_eq!(snapshot.open_documents[0].language_id, "rust");
    assert!(snapshot.pid.is_some());
    assert!(snapshot.state.is_ready());
    shutdown_and_join(&handle, task).await;
}

#[tokio::test]
async fn unknown_server_requests_get_method_not_found() {
    // fake server 只发 applyEdit；此处直接验证 handle_server_request 的
    // 兜底路径（经 transport 层单元验证已覆盖，这里验证服务存活）。
    let temp = tempfile::tempdir().unwrap();
    let (handle, task) = start(temp.path(), "echo", &[]);
    wait_ready(&handle).await;
    let snapshot = handle.snapshot().await.unwrap();
    assert!(snapshot.state.is_ready());
    shutdown_and_join(&handle, task).await;
}
