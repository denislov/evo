//! Deterministic support for coding-agent unit tests.
//!
//! Its persistence harness deliberately uses the real
//! session repository and transaction writer so crash-consistency tests cover
//! production framing, fsync, tail repair, and writer ownership behavior.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ai::api::client::AiClient;
use ai::api::provider::ApiProvider;
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::mutex::MutexExt;

use crate::platform::time::{Clock, IdGenerator};
use crate::profiles::ProfileId;
use crate::session::event::{OperationKind, SessionEventData, SessionEventEnvelope};
use crate::session::manifest::PersistedWorkspaceScope;
use crate::session::repository::{
    CreateSessionOptions, SessionHandle, SessionIoFault, SessionIoFaultPlan, SessionLogStore,
};
use crate::session::transaction::SessionTransactionWriter;

/// Keeps a test-scoped provider registry alive for the duration of a test.
pub struct ProviderGuard {
    ai_client: AiClient,
}

impl ProviderGuard {
    pub fn register(api: impl Into<String>, provider: Arc<dyn ApiProvider>) -> Self {
        Self::register_many(vec![(api.into(), provider)])
    }

    pub fn register_many(providers: Vec<(String, Arc<dyn ApiProvider>)>) -> Self {
        let ai_client = AiClient::new();
        for (api, provider) in providers {
            ai_client.register_provider(api, provider);
        }
        Self { ai_client }
    }

    pub fn ai_client(&self) -> AiClient {
        self.ai_client.clone()
    }
}

/// A deterministic clock whose queued timestamps are consumed in order.
#[derive(Debug, Clone)]
pub struct FakeClock {
    state: Arc<Mutex<FakeClockState>>,
}

#[derive(Debug)]
struct FakeClockState {
    current: String,
    queued: VecDeque<String>,
}

impl FakeClock {
    pub fn new(timestamp: impl Into<String>) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeClockState {
                current: timestamp.into(),
                queued: VecDeque::new(),
            })),
        }
    }

    pub fn push(&self, timestamp: impl Into<String>) {
        self.state
            .lock_or_recover("test fake clock state")
            .queued
            .push_back(timestamp.into());
    }

    pub fn current(&self) -> String {
        self.state
            .lock_or_recover("test fake clock state")
            .current
            .clone()
    }

    pub fn now(&self) -> String {
        self.now_rfc3339()
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new("2026-01-01T00:00:00Z")
    }
}

impl Clock for FakeClock {
    fn now_rfc3339(&self) -> String {
        let mut state = self.state.lock_or_recover("test fake clock state");
        if let Some(next) = state.queued.pop_front() {
            state.current = next;
        }
        state.current.clone()
    }
}

/// Generates stable, human-readable IDs such as `evt_0007`.
#[derive(Debug, Clone)]
pub struct SeqIdGenerator {
    next: u64,
}

impl SeqIdGenerator {
    pub const fn new(first: u64) -> Self {
        Self { next: first }
    }

    pub fn next_id(&mut self, prefix: &str) -> String {
        let value = self.next;
        self.next = self
            .next
            .checked_add(1)
            .expect("test ID sequence overflowed");
        format!("{prefix}_{value:04}")
    }
}

impl Default for SeqIdGenerator {
    fn default() -> Self {
        Self::new(1)
    }
}

impl IdGenerator for SeqIdGenerator {
    fn next_session_id(&mut self) -> String {
        self.next_id("sess")
    }

    fn next_event_id(&mut self) -> String {
        self.next_id("evt")
    }

    fn next_root_operation_id(&mut self) -> String {
        self.next_id("op")
    }

    fn next_child_operation_id(&mut self) -> String {
        self.next_id("op")
    }

    fn next_session_copy_id(&mut self) -> String {
        self.next_id("copy")
    }

    fn next_turn_id(&mut self) -> String {
        self.next_id("turn")
    }

    fn next_message_id(&mut self) -> String {
        self.next_id("msg")
    }

    fn next_tool_call_id(&mut self) -> String {
        self.next_id("tool")
    }

    fn next_leaf_id(&mut self) -> String {
        self.next_id("leaf")
    }

    fn next_branch_id(&mut self) -> String {
        self.next_id("branch")
    }
}

/// Real session persistence rooted in a temporary directory, with one-shot
/// durable-I/O failure injection.
#[derive(Debug)]
pub struct TempSessionEnv {
    _temp_dir: TempDir,
    session_id: String,
    store: SessionLogStore,
    handle: SessionHandle,
    writer: Option<SessionTransactionWriter>,
    io_faults: SessionIoFaultPlan,
    ids: SeqIdGenerator,
    clock: FakeClock,
}

impl TempSessionEnv {
    pub fn new() -> Result<Self, String> {
        let temp_dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp_dir.path().join("sessions");
        let session_id = "test-session".to_owned();
        let io_faults = SessionIoFaultPlan::default();
        let store = SessionLogStore::with_io_faults(&root, io_faults.clone());
        let clock = FakeClock::default();
        let mut ids = SeqIdGenerator::default();
        let workspace_scope = PersistedWorkspaceScope::Projectless {
            workspace_id: "test-workspace".into(),
        };
        let handle = store
            .create_session(
                CreateSessionOptions::new(session_id.clone(), clock.now_rfc3339())
                    .default_agent_profile_id(ProfileId::from("default"))
                    .workspace_scope(workspace_scope.clone()),
            )
            .map_err(|error| error.to_string())?;
        let writer = SessionTransactionWriter::new(store.clone(), handle.clone())
            .map_err(|error| error.to_string())?;
        let created = SessionEventEnvelope::new(
            session_id.clone(),
            ids.next_event_id(),
            clock.now_rfc3339(),
            SessionEventData::SessionCreated {
                cwd: None,
                workspace_scope: Some(workspace_scope),
            },
        );
        writer
            .initialize_session_with_receipt_blocking(created)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            _temp_dir: temp_dir,
            session_id,
            store,
            handle,
            writer: Some(writer),
            io_faults,
            ids,
            clock,
        })
    }

    pub fn committed_sequence(&self) -> Result<u64, String> {
        self.writer()
            .map(SessionTransactionWriter::committed_session_sequence)
    }

    pub fn event_count(&self) -> Result<usize, String> {
        self.store
            .read_events(&self.handle)
            .map(|events| events.len())
            .map_err(|error| error.to_string())
    }

    pub fn append_diagnostic(&mut self, message: impl Into<String>) -> Result<(), String> {
        let event = SessionEventEnvelope::new(
            self.session_id.clone(),
            self.ids.next_event_id(),
            self.clock.now_rfc3339(),
            SessionEventData::DiagnosticEmitted {
                level: crate::session::event::DiagnosticLevel::Info,
                message: message.into(),
            },
        );
        self.writer()?
            .append_checkpoint_events_blocking(vec![event])
            .map_err(|error| error.to_string())
    }

    pub fn append_committed_operation(&mut self, name: impl Into<String>) -> Result<(), String> {
        let operation_id = self.ids.next_root_operation_id();
        let timestamp = self.clock.now_rfc3339();
        let events = vec![
            SessionEventEnvelope::new(
                self.session_id.clone(),
                self.ids.next_event_id(),
                timestamp.clone(),
                SessionEventData::OperationStarted {
                    operation: OperationKind::Other { name: name.into() },
                    runtime_generation: Default::default(),
                },
            )
            .with_operation_id(operation_id.clone()),
            SessionEventEnvelope::new(
                self.session_id.clone(),
                self.ids.next_event_id(),
                timestamp,
                SessionEventData::OperationCommitted { new_leaf_id: None },
            )
            .with_operation_id(operation_id),
        ];
        self.writer()?
            .append_checkpoint_events_blocking(events)
            .map_err(|error| error.to_string())
    }

    /// Make the next durable append persist at most `bytes` bytes, then return
    /// an ENOSPC-style error. Reopening the writer exercises torn-tail repair.
    pub fn fail_next_write_after(&self, bytes: usize) {
        self.io_faults.push(SessionIoFault::WriteAfterBytes(bytes));
    }

    /// Make the next durable append write and flush its bytes, then fail at the
    /// fsync boundary. This intentionally produces an ambiguous commit result.
    pub fn fail_next_fsync(&self) {
        self.io_faults.push(SessionIoFault::Sync);
    }

    /// Close and reopen the real transaction writer, returning storage-repair
    /// diagnostics produced while acquiring the new writer lease.
    pub fn reopen(&mut self) -> Result<Vec<String>, String> {
        self.shutdown_writer()?;
        self.handle = self
            .store
            .open_session_id(&self.session_id)
            .map_err(|error| error.to_string())?;
        let writer = SessionTransactionWriter::new(self.store.clone(), self.handle.clone())
            .map_err(|error| error.to_string())?;
        let recoveries = writer.startup_storage_recoveries().to_vec();
        self.writer = Some(writer);
        Ok(recoveries)
    }

    pub fn shutdown_writer(&mut self) -> Result<(), String> {
        let Some(writer) = self.writer.take() else {
            return Ok(());
        };
        writer.shutdown().map_err(|error| error.to_string())
    }

    fn writer(&self) -> Result<&SessionTransactionWriter, String> {
        self.writer
            .as_ref()
            .ok_or_else(|| "temporary session writer is closed".to_owned())
    }
}

impl Drop for TempSessionEnv {
    fn drop(&mut self) {
        let _ = self.shutdown_writer();
    }
}

/// Deterministically pauses a future at one named cancellation checkpoint.
#[derive(Debug, Clone)]
pub struct CancellationHarness {
    label: Arc<str>,
    token: CancellationToken,
    reached: Arc<AtomicBool>,
    reached_notify: Arc<Notify>,
    release_notify: Arc<Notify>,
}

impl CancellationHarness {
    pub fn new(label: impl Into<Arc<str>>) -> Self {
        Self {
            label: label.into(),
            token: CancellationToken::new(),
            reached: Arc::new(AtomicBool::new(false)),
            reached_notify: Arc::new(Notify::new()),
            release_notify: Arc::new(Notify::new()),
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub async fn checkpoint(&self) {
        self.reached.store(true, Ordering::Release);
        self.reached_notify.notify_waiters();
        tokio::select! {
            _ = self.token.cancelled() => {}
            _ = self.release_notify.notified() => {}
        }
    }

    pub async fn wait_until_reached(&self) {
        while !self.reached.load(Ordering::Acquire) {
            self.reached_notify.notified().await;
        }
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }
}

/// Commands used to exercise timeout, output-budget, and process-tree paths.
#[derive(Debug)]
pub struct ProcessFixture {
    _temp_dir: TempDir,
    pid_file: PathBuf,
}

impl ProcessFixture {
    pub fn new() -> Result<Self, String> {
        let temp_dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let pid_file = temp_dir.path().join("descendant.pid");
        Ok(Self {
            _temp_dir: temp_dir,
            pid_file,
        })
    }

    pub fn pid_file(&self) -> &Path {
        &self.pid_file
    }

    #[cfg(unix)]
    pub fn sleep_command(&self) -> String {
        "sleep 300".into()
    }

    #[cfg(windows)]
    pub fn sleep_command(&self) -> String {
        "Start-Sleep -Seconds 300".into()
    }

    #[cfg(unix)]
    pub fn noisy_command(&self) -> String {
        "yes 0123456789 | head -c 16777216".into()
    }

    #[cfg(windows)]
    pub fn noisy_command(&self) -> String {
        "1..1048576 | ForEach-Object { '0123456789' }".into()
    }

    #[cfg(unix)]
    pub fn descendant_command(&self) -> String {
        format!(
            "sleep 300 & child=$!; printf '%s' \"$child\" > {}; wait \"$child\"",
            shell_quote(&self.pid_file)
        )
    }

    #[cfg(windows)]
    pub fn descendant_command(&self) -> String {
        format!(
            "$child = Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 300' -PassThru; Set-Content -NoNewline -Path '{}' -Value $child.Id; Wait-Process -Id $child.Id",
            self.pid_file.display()
        )
    }
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_ids_and_clock_advance_in_order() {
        let mut ids = SeqIdGenerator::new(7);
        assert_eq!(ids.next_id("evt"), "evt_0007");
        assert_eq!(ids.next_id("evt"), "evt_0008");

        let clock = FakeClock::new("2026-08-01T00:00:00Z");
        clock.push("2026-08-01T00:00:01Z");
        assert_eq!(clock.current(), "2026-08-01T00:00:00Z");
        assert_eq!(clock.now(), "2026-08-01T00:00:01Z");
    }

    #[tokio::test]
    async fn cancellation_harness_pauses_at_the_named_checkpoint() {
        let harness = CancellationHarness::new("before-side-effect");
        let checkpoint = harness.clone();
        let task = tokio::spawn(async move {
            checkpoint.checkpoint().await;
        });
        harness.wait_until_reached().await;
        assert_eq!(harness.label(), "before-side-effect");
        harness.cancel();
        task.await.expect("checkpoint task joins");
    }

    #[test]
    fn temp_session_env_repairs_a_partial_commit_on_reopen() {
        let mut env = TempSessionEnv::new().expect("temporary session environment");
        env.append_committed_operation("baseline")
            .expect("baseline commit");
        let committed_before = env.committed_sequence().expect("committed sequence");
        let events_before = env.event_count().expect("event count");

        env.fail_next_write_after(31);
        let error = env
            .append_diagnostic("must be torn")
            .expect_err("the injected ENOSPC must fail the append");
        assert!(error.contains("No space") || error.contains("ENOSPC"));

        let recoveries = env.reopen().expect("writer reopens and repairs tail");
        assert!(
            recoveries
                .iter()
                .any(|message| message.contains("unterminated") || message.contains("tail")),
            "expected explicit torn-tail recovery evidence, got {recoveries:?}"
        );
        assert_eq!(
            env.committed_sequence().expect("reopened sequence"),
            committed_before
        );
        assert_eq!(env.event_count().expect("reopened events"), events_before);
    }

    #[test]
    fn temp_session_env_can_fail_the_fsync_boundary() {
        let mut env = TempSessionEnv::new().expect("temporary session environment");
        env.fail_next_fsync();
        let error = env
            .append_diagnostic("ambiguous durability")
            .expect_err("the injected fsync must fail the append");
        assert!(error.contains("injected fsync failure"));
    }
}
