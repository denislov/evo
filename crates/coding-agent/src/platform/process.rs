use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::platform::io::output::{TruncationLimit, truncate_tail};

const READ_CHUNK_BYTES: usize = 8 * 1024;
const DRAIN_GRACE: Duration = Duration::from_millis(500);

pub(crate) type ProcessUpdateCallback = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCapability {
    pub(crate) cwd: PathBuf,
    pub(crate) shell_path: Option<String>,
    pub(crate) command_prefix: Option<String>,
}

impl ShellCapability {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            shell_path: None,
            command_prefix: None,
        }
    }

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
pub(crate) enum ProgramKind {
    Shell { path: String, command_arg: String },
    Direct { program: String, args: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnvPolicy {
    // The shared contract deliberately represents inheritance even though all
    // current product call sites choose the safer allowlist policy.
    #[allow(dead_code)]
    Inherit,
    AllowList(HashMap<String, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutputBudget {
    pub(crate) max_bytes: usize,
    pub(crate) max_lines: usize,
    pub(crate) buffer_keep_bytes: usize,
    pub(crate) update_byte_threshold: usize,
    pub(crate) update_interval: Duration,
}

impl OutputBudget {
    pub(crate) fn new(max_bytes: usize, max_lines: usize) -> Self {
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
pub(crate) struct ProcessSpec {
    pub(crate) program: ProgramKind,
    pub(crate) command: String,
    pub(crate) cwd: PathBuf,
    pub(crate) env: EnvPolicy,
    pub(crate) timeout: Duration,
    pub(crate) output_budget: OutputBudget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessOutput {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) merged: String,
    pub(crate) stdout_bytes: usize,
    pub(crate) stderr_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProcessOutcome {
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

impl ProcessOutcome {
    pub(crate) fn output(&self) -> &ProcessOutput {
        match self {
            Self::Completed { output, .. }
            | Self::TimedOut { output }
            | Self::Cancelled { output }
            | Self::Failed { output, .. } => output,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Stdout,
    Stderr,
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
        let truncation = truncate_tail(
            &text,
            TruncationLimit {
                max_lines: self.budget.max_lines,
                max_bytes: self.budget.max_bytes,
            },
        );
        if !truncation.truncated && !self.overflowed {
            return text.into_owned();
        }
        let known_lines = self.total_lines();
        let byte_label = if self.budget.max_bytes.is_multiple_of(1024) {
            format!("{}KB", self.budget.max_bytes / 1024)
        } else {
            format!("{} bytes", self.budget.max_bytes)
        };
        format!(
            "{}\n\n[Output truncated: showing last {} of {known_lines} lines ({byte_label}/{}-line limit).]",
            truncation.content, truncation.output_lines, self.budget.max_lines
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

enum PendingTermination {
    Completed(Option<i32>),
    TimedOut,
    Cancelled,
    Failed(String),
}

pub(crate) async fn run(
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

    let mut command = command_from_spec(&spec);
    configure_process(&mut command, &spec);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ProcessOutcome::Failed {
                message: format!("failed to spawn: {error}"),
                output: output.finish(),
            };
        }
    };
    let mut process_tree = match ProcessTree::attach(&child) {
        Ok(process_tree) => process_tree,
        Err(error) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return ProcessOutcome::Failed {
                message: format!("failed to attach process tree containment: {error}"),
                output: output.finish(),
            };
        }
    };
    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_child_process_tree(&mut child, &process_tree).await;
            return ProcessOutcome::Failed {
                message: "failed to capture stdout".into(),
                output: output.finish(),
            };
        }
    };
    let mut stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_child_process_tree(&mut child, &process_tree).await;
            return ProcessOutcome::Failed {
                message: "failed to capture stderr".into(),
                output: output.finish(),
            };
        }
    };

    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut stdout_buffer = vec![0_u8; READ_CHUNK_BYTES];
    let mut stderr_buffer = vec![0_u8; READ_CHUNK_BYTES];
    let timeout = tokio::time::sleep(spec.timeout);
    tokio::pin!(timeout);
    let termination = loop {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => break PendingTermination::Cancelled,
            _ = &mut timeout => break PendingTermination::TimedOut,
            status = child.wait() => {
                break match status {
                    Ok(status) => PendingTermination::Completed(status.code()),
                    Err(error) => PendingTermination::Failed(format!("wait failed: {error}")),
                };
            }
            read = stdout.read(&mut stdout_buffer), if stdout_open => {
                match read {
                    Ok(0) => stdout_open = false,
                    Ok(read) => output.push(StreamKind::Stdout, &stdout_buffer[..read], on_update),
                    Err(error) => break PendingTermination::Failed(format!("stdout read failed: {error}")),
                }
            }
            read = stderr.read(&mut stderr_buffer), if stderr_open => {
                match read {
                    Ok(0) => stderr_open = false,
                    Ok(read) => output.push(StreamKind::Stderr, &stderr_buffer[..read], on_update),
                    Err(error) => break PendingTermination::Failed(format!("stderr read failed: {error}")),
                }
            }
        }
    };

    if !matches!(termination, PendingTermination::Completed(_)) {
        terminate_child_process_tree(&mut child, &process_tree).await;
    } else {
        process_tree.disarm();
    }
    drain_with_grace(stdout, stderr, &mut output, on_update).await;
    output.emit_final_update(on_update);
    let output = output.finish();
    match termination {
        PendingTermination::Completed(exit_code) => ProcessOutcome::Completed { exit_code, output },
        PendingTermination::TimedOut => ProcessOutcome::TimedOut { output },
        PendingTermination::Cancelled => ProcessOutcome::Cancelled { output },
        PendingTermination::Failed(message) => ProcessOutcome::Failed { message, output },
    }
}

fn command_from_spec(spec: &ProcessSpec) -> tokio::process::Command {
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

fn configure_process(command: &mut tokio::process::Command, spec: &ProcessSpec) {
    command
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match &spec.env {
        EnvPolicy::Inherit => {}
        EnvPolicy::AllowList(env) => {
            command.env_clear().envs(env);
        }
    }
    #[cfg(unix)]
    command.process_group(0);
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
    output: &mut OutputCollector,
    on_update: Option<&ProcessUpdateCallback>,
) {
    let _ = tokio::time::timeout(DRAIN_GRACE, drain_pipes(stdout, stderr, output, on_update)).await;
}

async fn drain_pipes(
    mut stdout: tokio::process::ChildStdout,
    mut stderr: tokio::process::ChildStderr,
    output: &mut OutputCollector,
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
                    Ok(read) => output.push(StreamKind::Stdout, &stdout_buffer[..read], on_update),
                }
            }
            read = stderr.read(&mut stderr_buffer), if stderr_open => {
                match read {
                    Ok(0) | Err(_) => stderr_open = false,
                    Ok(read) => output.push(StreamKind::Stderr, &stderr_buffer[..read], on_update),
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

async fn terminate_child_process_tree(child: &mut tokio::process::Child, tree: &ProcessTree) {
    tree.terminate(child).await;
}

#[cfg(unix)]
#[derive(Debug)]
struct ProcessTree;

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
struct ProcessTree;

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
struct ProcessTree {
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

pub(crate) async fn path_exists(path: &std::path::Path) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

pub(crate) async fn resolve_shell_path(custom_shell_path: Option<&str>) -> Result<String, String> {
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
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use tokio_util::sync::CancellationToken;

    use super::{
        EnvPolicy, OutputBudget, ProcessOutcome, ProcessSpec, ProcessUpdateCallback, ProgramKind,
        run,
    };
    use crate::test_support::ProcessFixture;

    fn shell_spec(command: String, timeout: Duration) -> ProcessSpec {
        ProcessSpec {
            program: ProgramKind::Shell {
                path: "/bin/sh".into(),
                command_arg: "-c".into(),
            },
            command,
            cwd: std::env::current_dir().expect("current directory"),
            env: EnvPolicy::AllowList(HashMap::from([(
                "PATH".into(),
                std::env::var("PATH").unwrap_or_default(),
            )])),
            timeout,
            output_budget: OutputBudget::new(50 * 1024, 2_000),
        }
    }

    #[cfg(unix)]
    async fn wait_for_pid(path: &std::path::Path) -> u32 {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(text) = tokio::fs::read_to_string(path).await
                    && let Ok(pid) = text.parse()
                {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("descendant pid should be written")
    }

    #[cfg(unix)]
    async fn assert_process_stopped(pid: u32) {
        tokio::time::timeout(Duration::from_secs(2), async move {
            loop {
                // A zombie has terminated and cannot execute work; on Linux it
                // may remain visible briefly until the container init reaps it.
                let zombie = tokio::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .await
                    .ok()
                    .and_then(|stat| {
                        stat.rsplit_once(") ")
                            .map(|(_, tail)| tail.starts_with('Z'))
                    })
                    .unwrap_or(false);
                // SAFETY: signal 0 only probes a process identifier captured
                // from the test fixture; it does not send a signal.
                let missing = unsafe { libc::kill(pid as i32, 0) } != 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                if zombie || missing {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("descendant process survived process-tree teardown");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_returns_only_after_sleep_is_terminated() {
        let fixture = ProcessFixture::new().expect("fixture");
        let cancellation = CancellationToken::new();
        let task_token = cancellation.clone();
        let task = tokio::spawn(async move {
            run(
                shell_spec(fixture.sleep_command(), Duration::from_secs(300)),
                &task_token,
                None,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let started = Instant::now();
        cancellation.cancel();
        let outcome = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("cancelled runner should return")
            .expect("runner task should join");
        assert!(matches!(outcome, ProcessOutcome::Cancelled { .. }));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_kills_descendants_before_returning() {
        let fixture = ProcessFixture::new().expect("fixture");
        let command = fixture.descendant_command();
        let pid_file = fixture.pid_file().to_path_buf();
        let cancellation = CancellationToken::new();
        let task_token = cancellation.clone();
        let task = tokio::spawn(async move {
            run(
                shell_spec(command, Duration::from_secs(300)),
                &task_token,
                None,
            )
            .await
        });
        let pid = wait_for_pid(&pid_file).await;
        cancellation.cancel();
        let outcome = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("cancelled runner should return")
            .expect("runner task should join");
        assert!(matches!(outcome, ProcessOutcome::Cancelled { .. }));
        assert_process_stopped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_uses_the_same_descendant_teardown() {
        let fixture = ProcessFixture::new().expect("fixture");
        let command = fixture.descendant_command();
        let pid_file = fixture.pid_file().to_path_buf();
        let task = tokio::spawn(async move {
            run(
                shell_spec(command, Duration::from_millis(150)),
                &CancellationToken::new(),
                None,
            )
            .await
        });
        let pid = wait_for_pid(&pid_file).await;
        let outcome = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("timed-out runner should return")
            .expect("runner task should join");
        assert!(matches!(outcome, ProcessOutcome::TimedOut { .. }));
        assert_process_stopped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn noisy_output_is_bounded_and_updates_are_throttled() {
        let fixture = ProcessFixture::new().expect("fixture");
        let updates = Arc::new(AtomicUsize::new(0));
        let callback_updates = updates.clone();
        let callback: ProcessUpdateCallback = Arc::new(move |_| {
            callback_updates.fetch_add(1, Ordering::Relaxed);
        });
        let outcome = run(
            shell_spec(fixture.noisy_command(), Duration::from_secs(10)),
            &CancellationToken::new(),
            Some(&callback),
        )
        .await;
        let ProcessOutcome::Completed {
            exit_code: Some(0),
            output,
        } = outcome
        else {
            panic!("noisy command should complete successfully: {outcome:?}");
        };
        assert!(output.stdout_bytes >= 16 * 1024 * 1024);
        assert!(output.stdout.len() <= 52 * 1024);
        assert!(output.merged.len() <= 52 * 1024);
        assert!(output.stdout.contains("Output truncated"));
        assert!(updates.load(Ordering::Relaxed) < 512);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn environment_is_replaced_by_the_explicit_allowlist() {
        let mut spec = shell_spec(
            "printf '%s:%s' \"$VISIBLE\" \"${HIDDEN-unset}\"".into(),
            Duration::from_secs(2),
        );
        spec.env = EnvPolicy::AllowList(HashMap::from([("VISIBLE".into(), "ok".into())]));
        let outcome = run(spec, &CancellationToken::new(), None).await;
        let ProcessOutcome::Completed {
            exit_code: Some(0),
            output,
        } = outcome
        else {
            panic!("environment probe should complete: {outcome:?}");
        };
        assert_eq!(output.stdout, "ok:unset");
        assert_eq!(output.stderr, "");
    }
}
