use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

mod background;
mod peer;

pub use background::{
    OutputGap, TaskHandle, TaskId, TaskOutputChunk, TaskOwner, TaskRegistry, TaskReport,
    TaskSnapshot, TaskSpawnError, TaskState,
};
pub use peer::PeerProcess;

const READ_CHUNK_BYTES: usize = 8 * 1024;
const DRAIN_GRACE: Duration = Duration::from_millis(500);

pub type ProcessUpdateCallback = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellCapability {
    pub(crate) cwd: PathBuf,
    pub(crate) shell_path: Option<String>,
    pub(crate) command_prefix: Option<String>,
}

impl ShellCapability {
    pub(crate) fn with_configuration(
        cwd: PathBuf,
        shell_path: Option<String>,
        command_prefix: Option<String>,
    ) -> Self {
        Self {
            cwd,
            shell_path,
            command_prefix,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramKind {
    Shell { path: String, command_arg: String },
    Direct { program: String, args: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvPolicy {
    // The shared contract deliberately represents inheritance even though all
    // current product call sites choose the safer allowlist policy.
    #[allow(dead_code)]
    Inherit,
    AllowList(HashMap<String, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputBudget {
    pub max_bytes: usize,
    pub max_lines: usize,
    pub buffer_keep_bytes: usize,
    pub update_byte_threshold: usize,
    pub update_interval: Duration,
}

impl OutputBudget {
    pub fn new(max_bytes: usize, max_lines: usize) -> Self {
        Self {
            max_bytes,
            max_lines,
            buffer_keep_bytes: max_bytes.saturating_add(16 * 1024).max(max_bytes),
            update_byte_threshold: 64 * 1024,
            update_interval: Duration::from_millis(100),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    pub program: ProgramKind,
    pub command: String,
    pub cwd: PathBuf,
    pub env: EnvPolicy,
    pub timeout: Duration,
    pub output_budget: OutputBudget,
    /// Optional sandbox applied at the spawn boundary. `None` keeps the
    /// legacy unrestricted behavior. When set, unsupported platforms fail the
    /// spawn explicitly instead of silently running unrestricted.
    pub sandbox: Option<crate::sandbox::SandboxProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    pub stdout: String,
    pub stderr: String,
    pub merged: String,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessOutcome {
    Completed {
        exit_code: Option<i32>,
        output: ProcessOutput,
    },
    TimedOut {
        output: ProcessOutput,
    },
    Cancelled {
        output: ProcessOutput,
    },
    Failed {
        message: String,
        output: ProcessOutput,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Stdout,
    Stderr,
}

/// Destination for process output bytes shared by the one-shot `run()` and the
/// background task spool. Both the bounded tail renderer and the cursor-based
/// spool consume the same spawn/collect core without altering its semantics.
pub(crate) trait OutputSink {
    fn push_stdout(&mut self, data: &[u8], on_update: Option<&ProcessUpdateCallback>);
    fn push_stderr(&mut self, data: &[u8], on_update: Option<&ProcessUpdateCallback>);
}

#[derive(Debug)]
struct OutputTail {
    buffer: Vec<u8>,
    newline_count: usize,
    total_bytes: usize,
    ends_with_newline: bool,
    overflowed: bool,
    budget: OutputBudget,
}

impl OutputTail {
    fn new(budget: OutputBudget) -> Self {
        Self {
            buffer: Vec::new(),
            newline_count: 0,
            total_bytes: 0,
            ends_with_newline: false,
            overflowed: false,
            budget,
        }
    }

    fn push(&mut self, data: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(data.len());
        self.newline_count = self
            .newline_count
            .saturating_add(data.iter().filter(|byte| **byte == b'\n').count());
        if let Some(last) = data.last() {
            self.ends_with_newline = *last == b'\n';
        }
        self.buffer.extend_from_slice(data);
        let drain_threshold = self.budget.buffer_keep_bytes.saturating_mul(2);
        if self.buffer.len() > drain_threshold {
            drain_utf8_prefix(&mut self.buffer, self.budget.buffer_keep_bytes);
            self.overflowed = true;
        }
    }

    fn render(&self) -> String {
        let text = String::from_utf8_lossy(&self.buffer);
        if !self.overflowed
            && self.total_lines() <= self.budget.max_lines
            && self.total_bytes <= self.budget.max_bytes
        {
            return text.into_owned();
        }
        let (kept, kept_lines) = tail_text(&text, self.budget.max_lines, self.budget.max_bytes);
        let known_lines = self.total_lines();
        let byte_label = if self.budget.max_bytes.is_multiple_of(1024) {
            format!("{}KB", self.budget.max_bytes / 1024)
        } else {
            format!("{} bytes", self.budget.max_bytes)
        };
        format!(
            "{kept}\n\n[Output truncated: showing last {kept_lines} of {known_lines} lines ({byte_label}/{}-line limit).]",
            self.budget.max_lines
        )
    }

    fn total_lines(&self) -> usize {
        self.newline_count
            .saturating_add(usize::from(self.total_bytes > 0 && !self.ends_with_newline))
    }
}

#[derive(Debug)]
struct OutputCollector {
    stdout: OutputTail,
    stderr: OutputTail,
    merged: OutputTail,
    bytes_since_update: usize,
    last_update: Instant,
    emitted_update: bool,
}

impl OutputCollector {
    fn new(budget: OutputBudget) -> Self {
        Self {
            stdout: OutputTail::new(budget),
            stderr: OutputTail::new(budget),
            merged: OutputTail::new(budget),
            bytes_since_update: 0,
            last_update: Instant::now(),
            emitted_update: false,
        }
    }

    fn push(&mut self, stream: StreamKind, data: &[u8], on_update: Option<&ProcessUpdateCallback>) {
        match stream {
            StreamKind::Stdout => self.stdout.push(data),
            StreamKind::Stderr => self.stderr.push(data),
        }
        self.merged.push(data);
        self.bytes_since_update = self.bytes_since_update.saturating_add(data.len());
        let should_emit = !self.emitted_update
            || self.bytes_since_update >= self.merged.budget.update_byte_threshold
            || self.last_update.elapsed() >= self.merged.budget.update_interval;
        if should_emit {
            self.emit_update(on_update);
        }
    }

    fn emit_final_update(&mut self, on_update: Option<&ProcessUpdateCallback>) {
        if self.bytes_since_update > 0 {
            self.emit_update(on_update);
        }
    }

    fn emit_update(&mut self, on_update: Option<&ProcessUpdateCallback>) {
        if let Some(on_update) = on_update {
            on_update(self.merged.render());
        }
        self.bytes_since_update = 0;
        self.last_update = Instant::now();
        self.emitted_update = true;
    }

    fn finish(self) -> ProcessOutput {
        ProcessOutput {
            stdout: self.stdout.render(),
            stderr: self.stderr.render(),
            merged: self.merged.render(),
            stdout_bytes: self.stdout.total_bytes,
            stderr_bytes: self.stderr.total_bytes,
        }
    }
}

impl OutputSink for OutputCollector {
    fn push_stdout(&mut self, data: &[u8], on_update: Option<&ProcessUpdateCallback>) {
        self.push(StreamKind::Stdout, data, on_update);
    }

    fn push_stderr(&mut self, data: &[u8], on_update: Option<&ProcessUpdateCallback>) {
        self.push(StreamKind::Stderr, data, on_update);
    }
}

pub(crate) enum PendingTermination {
    Completed(Option<i32>),
    TimedOut,
    Cancelled,
    Failed(String),
}

pub async fn run(
    spec: ProcessSpec,
    cancellation: &CancellationToken,
    on_update: Option<&ProcessUpdateCallback>,
) -> ProcessOutcome {
    let mut output = OutputCollector::new(spec.output_budget);
    if cancellation.is_cancelled() {
        return ProcessOutcome::Cancelled {
            output: output.finish(),
        };
    }
    let mut process = match SpawnedProcess::spawn(&spec).await {
        Ok(process) => process,
        Err(message) => {
            return ProcessOutcome::Failed {
                message,
                output: output.finish(),
            };
        }
    };
    let timeout = Box::pin(tokio::time::sleep(spec.timeout)) as BoxTimeout;
    let termination = process
        .run_until_terminated(&mut output, cancellation, Some(timeout), on_update)
        .await;
    if !matches!(termination, PendingTermination::Completed(_)) {
        process.terminate_tree().await;
    } else {
        process.disarm();
    }
    process.drain_remaining(&mut output, on_update).await;
    output.emit_final_update(on_update);
    let output = output.finish();
    match termination {
        PendingTermination::Completed(exit_code) => ProcessOutcome::Completed { exit_code, output },
        PendingTermination::TimedOut => ProcessOutcome::TimedOut { output },
        PendingTermination::Cancelled => ProcessOutcome::Cancelled { output },
        PendingTermination::Failed(message) => ProcessOutcome::Failed { message, output },
    }
}

/// A spawned child with process-tree containment and captured pipes, shared by
/// the one-shot `run()` and the background task driver. Owns the spawn-time
/// failure cleanup (kill and reap a partially attached child) so both callers
/// cannot leak descendants on early failure.
pub(crate) struct SpawnedProcess {
    child: tokio::process::Child,
    process_tree: ProcessTree,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
}

async fn spawn_process(spec: &ProcessSpec, stdin_piped: bool) -> Result<SpawnedProcess, String> {
    let mut command = command_from_spec(spec);
    let prepared_sandbox = match spec.sandbox.as_ref().map(crate::sandbox::prepare_sandbox) {
        Some(Ok(prepared)) => prepared,
        Some(Err(error)) => {
            return Err(format!("sandbox setup failed: {error}"));
        }
        None => None,
    };
    configure_process(&mut command, spec, prepared_sandbox, stdin_piped);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Err(format!("failed to spawn: {error}"));
        }
    };
    let process_tree = match ProcessTree::attach(&child) {
        Ok(process_tree) => process_tree,
        Err(error) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(format!(
                "failed to attach process tree containment: {error}"
            ));
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => Some(stdout),
        None => {
            terminate_child_process_tree(&mut child, &process_tree).await;
            return Err("failed to capture stdout".into());
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => Some(stderr),
        None => {
            terminate_child_process_tree(&mut child, &process_tree).await;
            return Err("failed to capture stderr".into());
        }
    };
    Ok(SpawnedProcess {
        child,
        process_tree,
        stdout,
        stderr,
    })
}

impl SpawnedProcess {
    async fn spawn(spec: &ProcessSpec) -> Result<Self, String> {
        spawn_process(spec, false).await
    }

    /// The shared read/await loop: cancellation, optional timeout, child
    /// exit, and pipe reads are all observed in one place so foreground and
    /// background execution terminate descendants identically.
    async fn run_until_terminated(
        &mut self,
        sink: &mut (dyn OutputSink + Send),
        cancellation: &CancellationToken,
        timeout: Option<BoxTimeout>,
        on_update: Option<&ProcessUpdateCallback>,
    ) -> PendingTermination {
        let mut stdout_open = true;
        let mut stderr_open = true;
        let mut stdout_buffer = vec![0_u8; READ_CHUNK_BYTES];
        let mut stderr_buffer = vec![0_u8; READ_CHUNK_BYTES];
        let mut timeout = timeout;
        loop {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => break PendingTermination::Cancelled,
                _ = async { timeout.as_mut().expect("timeout guarded").await }, if timeout.is_some() => break PendingTermination::TimedOut,
                status = self.child.wait() => {
                    break match status {
                        Ok(status) => PendingTermination::Completed(status.code()),
                        Err(error) => PendingTermination::Failed(format!("wait failed: {error}")),
                    };
                }
                read = self.stdout.as_mut().expect("stdout pipe owned").read(&mut stdout_buffer), if stdout_open => {
                    match read {
                        Ok(0) => stdout_open = false,
                        Ok(read) => sink.push_stdout(&stdout_buffer[..read], on_update),
                        Err(error) => break PendingTermination::Failed(format!("stdout read failed: {error}")),
                    }
                }
                read = self.stderr.as_mut().expect("stderr pipe owned").read(&mut stderr_buffer), if stderr_open => {
                    match read {
                        Ok(0) => stderr_open = false,
                        Ok(read) => sink.push_stderr(&stderr_buffer[..read], on_update),
                        Err(error) => break PendingTermination::Failed(format!("stderr read failed: {error}")),
                    }
                }
            }
        }
    }

    async fn terminate_tree(&mut self) {
        terminate_child_process_tree(&mut self.child, &self.process_tree).await;
    }

    fn disarm(&mut self) {
        self.process_tree.disarm();
    }

    async fn drain_remaining(
        &mut self,
        sink: &mut (dyn OutputSink + Send),
        on_update: Option<&ProcessUpdateCallback>,
    ) {
        let stdout = self.stdout.take().expect("stdout pipe still owned");
        let stderr = self.stderr.take().expect("stderr pipe still owned");
        drain_with_grace(stdout, stderr, sink, on_update).await;
    }
}

pub(crate) type BoxTimeout = Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
pub(super) fn command_from_spec(spec: &ProcessSpec) -> tokio::process::Command {
    match &spec.program {
        ProgramKind::Shell { path, command_arg } => {
            let mut command = tokio::process::Command::new(path);
            command.arg(command_arg).arg(&spec.command);
            command
        }
        ProgramKind::Direct { program, args } => {
            let mut command = tokio::process::Command::new(program);
            command.args(args).arg(&spec.command);
            command
        }
    }
}

pub(super) fn configure_process(
    command: &mut tokio::process::Command,
    spec: &ProcessSpec,
    prepared_sandbox: Option<crate::sandbox::PreparedSandbox>,
    stdin_piped: bool,
) {
    command
        .current_dir(&spec.cwd)
        .stdin(if stdin_piped {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let profile_env = spec.sandbox.as_ref().map(|profile| &profile.env);
    if let Some(env) = crate::sandbox::resolve_env(&spec.env, profile_env) {
        command.env_clear().envs(env);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        if let Some(prepared_sandbox) = prepared_sandbox {
            // SAFETY: the closure only performs async-signal-safe raw syscalls
            // (prctl + landlock_restrict_self) on an owned descriptor and
            // never allocates or panics; the child either execs under the
            // sandbox or fails the spawn, both explicit outcomes.
            unsafe {
                command
                    .as_std_mut()
                    .pre_exec(move || prepared_sandbox.restrict_self());
            }
        }
    }
    #[cfg(not(unix))]
    let _ = prepared_sandbox;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.as_std_mut().creation_flags(CREATE_NO_WINDOW);
    }
}

async fn drain_with_grace(
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    sink: &mut (dyn OutputSink + Send),
    on_update: Option<&ProcessUpdateCallback>,
) {
    let _ = tokio::time::timeout(DRAIN_GRACE, drain_pipes(stdout, stderr, sink, on_update)).await;
}

async fn drain_pipes(
    mut stdout: tokio::process::ChildStdout,
    mut stderr: tokio::process::ChildStderr,
    sink: &mut (dyn OutputSink + Send),
    on_update: Option<&ProcessUpdateCallback>,
) {
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut stdout_buffer = vec![0_u8; READ_CHUNK_BYTES];
    let mut stderr_buffer = vec![0_u8; READ_CHUNK_BYTES];
    while stdout_open || stderr_open {
        tokio::select! {
            read = stdout.read(&mut stdout_buffer), if stdout_open => {
                match read {
                    Ok(0) | Err(_) => stdout_open = false,
                    Ok(read) => sink.push_stdout(&stdout_buffer[..read], on_update),
                }
            }
            read = stderr.read(&mut stderr_buffer), if stderr_open => {
                match read {
                    Ok(0) | Err(_) => stderr_open = false,
                    Ok(read) => sink.push_stderr(&stderr_buffer[..read], on_update),
                }
            }
        }
    }
}

fn drain_utf8_prefix(buffer: &mut Vec<u8>, keep: usize) {
    let drop = buffer.len().saturating_sub(keep);
    if drop == 0 {
        return;
    }
    let mut split = drop;
    while split < buffer.len() && (buffer[split] & 0xC0) == 0x80 {
        split += 1;
    }
    buffer.drain(..split);
}

/// Keep the last `max_lines` lines and at most `max_bytes` bytes of `content`,
/// cutting at UTF-8 character boundaries. Returns the kept text and how many
/// lines of the original it represents. Mirrors the tail semantics of the
/// shared agent-core truncation helper this crate must not depend on.
fn tail_text(content: &str, max_lines: usize, max_bytes: usize) -> (String, usize) {
    let mut lines = content.split('\n').collect::<Vec<_>>();
    if lines.len() > 1 && lines.last() == Some(&"") {
        lines.pop();
    }
    let mut output = Vec::new();
    let mut output_bytes = 0usize;
    for line in lines.iter().rev() {
        if output.len() >= max_lines {
            break;
        }
        let line_bytes = line.len() + usize::from(!output.is_empty());
        if output_bytes.saturating_add(line_bytes) > max_bytes {
            if output.is_empty() {
                output.push(truncate_str_from_end(line, max_bytes));
            }
            break;
        }
        output.push((*line).to_owned());
        output_bytes += line_bytes;
    }
    output.reverse();
    let kept_lines = output.len();
    (output.join("\n"), kept_lines)
}

fn truncate_str_from_end(text: &str, max_bytes: usize) -> String {
    let mut bytes = 0usize;
    let mut chars = Vec::new();
    for ch in text.chars().rev() {
        let len = ch.len_utf8();
        if bytes + len > max_bytes {
            break;
        }
        bytes += len;
        chars.push(ch);
    }
    chars.into_iter().rev().collect()
}

pub(super) async fn terminate_child_process_tree(
    child: &mut tokio::process::Child,
    tree: &ProcessTree,
) {
    tree.terminate(child).await;
}

#[cfg(unix)]
#[derive(Debug)]
pub(super) struct ProcessTree;

#[cfg(unix)]
impl ProcessTree {
    fn attach(_child: &tokio::process::Child) -> std::io::Result<Self> {
        Ok(Self)
    }

    fn disarm(&mut self) {}

    async fn terminate(&self, child: &mut tokio::process::Child) {
        let group_killed = child.id().is_some_and(|pid| {
            let Ok(pid) = i32::try_from(pid) else {
                return false;
            };
            // SAFETY: `pid` came from the live child and negating it targets
            // the process group created with `process_group(0)` above.
            let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
            result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        });
        if !group_killed {
            let _ = child.kill().await;
        }
        let _ = child.wait().await;
    }
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug)]
pub(super) struct ProcessTree;

#[cfg(not(any(unix, windows)))]
impl ProcessTree {
    fn attach(_child: &tokio::process::Child) -> std::io::Result<Self> {
        Ok(Self)
    }

    fn disarm(&mut self) {}

    async fn terminate(&self, child: &mut tokio::process::Child) {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

#[cfg(windows)]
#[derive(Debug)]
pub(super) struct ProcessTree {
    job: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl ProcessTree {
    fn attach(child: &tokio::process::Child) -> std::io::Result<Self> {
        use std::ptr::{null, null_mut};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        // SAFETY: null security/name pointers request an unnamed job with
        // default security. Every non-null handle is closed in Drop.
        let job = unsafe { CreateJobObjectW(null(), null()) };
        if job == null_mut() {
            return Err(std::io::Error::last_os_error());
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` has the exact structure and byte length required by
        // JobObjectExtendedLimitInformation and lives through the call.
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if configured == 0 {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
            return Err(std::io::Error::last_os_error());
        }
        let process = child
            .raw_handle()
            .ok_or_else(|| std::io::Error::other("spawned child has no process handle"))?;
        // SAFETY: both handles are valid and remain owned by their wrappers.
        if unsafe { AssignProcessToJobObject(job, process.cast()) } == 0 {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { job })
    }

    fn disarm(&mut self) {
        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: same structure contract as in attach. Failure deliberately
        // leaves kill-on-close armed so descendants cannot escape containment.
        let _ = unsafe {
            SetInformationJobObject(
                self.job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
    }

    async fn terminate(&self, child: &mut tokio::process::Child) {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        // SAFETY: the job handle is valid until Drop and contains the child.
        let terminated = unsafe { TerminateJobObject(self.job, 1) } != 0;
        if !terminated {
            let _ = child.kill().await;
        }
        let _ = child.wait().await;
    }
}

#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        // SAFETY: this instance uniquely owns the job handle.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.job) };
    }
}

pub async fn path_exists(path: &std::path::Path) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

pub async fn resolve_shell_path(custom_shell_path: Option<&str>) -> Result<String, String> {
    if let Some(shell_path) = custom_shell_path {
        if path_exists(std::path::Path::new(shell_path)).await {
            return Ok(shell_path.to_owned());
        }
        return Err(format!("Custom shell path not found: {shell_path}"));
    }

    #[cfg(windows)]
    {
        let candidates: &[&str] = &[
            "C:\\Program Files\\Git\\bin\\bash.exe",
            "C:\\Program Files (x86)\\Git\\bin\\bash.exe",
        ];
        for path in candidates {
            if path_exists(std::path::Path::new(path)).await {
                return Ok((*path).to_owned());
            }
        }
        if let Ok(output) = tokio::process::Command::new("where")
            .arg("bash.exe")
            .output()
            .await
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(path) = stdout.lines().map(str::trim).find(|path| !path.is_empty())
                && path_exists(std::path::Path::new(path)).await
            {
                return Ok(path.to_owned());
            }
        }
        return Err(
            "No bash shell found. Options:\n  \
                1. Install Git for Windows (https://git-scm.com)\n  \
                2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  \
                Searched Git Bash in: C:\\Program Files\\Git\\bin\\bash.exe, C:\\Program Files (x86)\\Git\\bin\\bash.exe"
                .into(),
        );
    }

    #[cfg(not(windows))]
    if path_exists(std::path::Path::new("/bin/bash")).await {
        Ok("/bin/bash".into())
    } else {
        Ok("bash".into())
    }
}

#[cfg(test)]
mod tests_file;
