use std::io::{self, Read};
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::WorktreeError;

const MAX_GIT_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_GIT_ERROR_BYTES: usize = 64 * 1024;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);

struct GitOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

pub(super) fn run_git(
    source: &Path,
    args: &[&str],
    dest: Option<&Path>,
    cancellation: &CancellationToken,
) -> Result<(), WorktreeError> {
    let output = execute_git(source, args, dest, cancellation, MAX_GIT_OUTPUT_BYTES)?;
    require_git_success(args, output).map(|_| ())
}

pub(super) fn git_capture(
    source: &Path,
    args: &[&str],
    cancellation: &CancellationToken,
    stdout_limit: usize,
) -> Result<Vec<u8>, WorktreeError> {
    let output = execute_git(source, args, None, cancellation, stdout_limit)?;
    require_git_success(args, output)
}

fn execute_git(
    source: &Path,
    args: &[&str],
    dest: Option<&Path>,
    cancellation: &CancellationToken,
    stdout_limit: usize,
) -> Result<GitOutput, WorktreeError> {
    check_cancelled(cancellation)?;
    let mut command = std::process::Command::new("git");
    command
        .args(args)
        .current_dir(source)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_git_process(&mut command);
    if let Some(dest) = dest {
        command.arg(dest);
    }
    let mut child = command.spawn().map_err(|error| WorktreeError::GitFailed {
        message: format!("cannot run git: {error}"),
    })?;
    let process_tree = BlockingProcessTree::attach(&child).map_err(|error| {
        terminate_child(&mut child);
        WorktreeError::GitFailed {
            message: format!("cannot contain git process tree: {error}"),
        }
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| WorktreeError::GitFailed {
            message: "cannot capture git stdout".into(),
        })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| WorktreeError::GitFailed {
            message: "cannot capture git stderr".into(),
        })?;
    let stdout_exceeded = Arc::new(AtomicBool::new(false));
    let stderr_exceeded = Arc::new(AtomicBool::new(false));
    let mut stdout_reader = Some(spawn_bounded_reader(
        stdout,
        stdout_limit,
        Arc::clone(&stdout_exceeded),
    ));
    let mut stderr_reader = Some(spawn_bounded_reader(
        stderr,
        MAX_GIT_ERROR_BYTES,
        Arc::clone(&stderr_exceeded),
    ));

    let status = loop {
        if cancellation.is_cancelled() {
            process_tree.terminate(&mut child);
            let _ = join_reader(stdout_reader.take().expect("stdout reader is present"));
            let _ = join_reader(stderr_reader.take().expect("stderr reader is present"));
            return Err(WorktreeError::Cancelled);
        }
        if stdout_exceeded.load(Ordering::Acquire) || stderr_exceeded.load(Ordering::Acquire) {
            process_tree.terminate(&mut child);
            let _ = join_reader(stdout_reader.take().expect("stdout reader is present"));
            let _ = join_reader(stderr_reader.take().expect("stderr reader is present"));
            return Err(WorktreeError::GitFailed {
                message: "git output exceeds the configured budget".into(),
            });
        }
        match child.try_wait().map_err(|error| WorktreeError::GitFailed {
            message: format!("cannot wait for git: {error}"),
        })? {
            Some(status) => break status,
            None => std::thread::sleep(PROCESS_POLL_INTERVAL),
        }
    };
    // Git commands must not leak hook/maintenance descendants that retain the
    // captured pipes after the direct child exits.
    process_tree.terminate(&mut child);
    let stdout = join_reader(stdout_reader.take().expect("stdout reader is present"))?;
    let stderr = join_reader(stderr_reader.take().expect("stderr reader is present"))?;
    if stdout_exceeded.load(Ordering::Acquire) || stderr_exceeded.load(Ordering::Acquire) {
        return Err(WorktreeError::GitFailed {
            message: "git output exceeds the configured budget".into(),
        });
    }
    check_cancelled(cancellation)?;
    Ok(GitOutput {
        status,
        stdout,
        stderr,
    })
}

fn check_cancelled(token: &CancellationToken) -> Result<(), WorktreeError> {
    if token.is_cancelled() {
        Err(WorktreeError::Cancelled)
    } else {
        Ok(())
    }
}

fn spawn_bounded_reader<R: Read + Send + 'static>(
    mut reader: R,
    limit: usize,
    exceeded: Arc<AtomicBool>,
) -> std::thread::JoinHandle<io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                return Ok(output);
            }
            if output.len().saturating_add(read) > limit {
                exceeded.store(true, Ordering::Release);
                continue;
            }
            output.extend_from_slice(&buffer[..read]);
        }
    })
}

fn join_reader(
    reader: std::thread::JoinHandle<io::Result<Vec<u8>>>,
) -> Result<Vec<u8>, WorktreeError> {
    reader
        .join()
        .map_err(|_| WorktreeError::GitFailed {
            message: "git output reader panicked".into(),
        })?
        .map_err(|error| WorktreeError::GitFailed {
            message: format!("cannot read git output: {error}"),
        })
}

fn terminate_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn configure_git_process(command: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

#[cfg(unix)]
struct BlockingProcessTree;

#[cfg(unix)]
impl BlockingProcessTree {
    fn attach(_child: &std::process::Child) -> io::Result<Self> {
        Ok(Self)
    }

    fn terminate(&self, child: &mut std::process::Child) {
        let group_killed = i32::try_from(child.id()).is_ok_and(|pid| {
            // SAFETY: the pid belongs to the live child placed in its own
            // process group by configure_git_process.
            let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
            result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        });
        if !group_killed {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

#[cfg(not(any(unix, windows)))]
struct BlockingProcessTree;

#[cfg(not(any(unix, windows)))]
impl BlockingProcessTree {
    fn attach(_child: &std::process::Child) -> io::Result<Self> {
        Ok(Self)
    }

    fn terminate(&self, child: &mut std::process::Child) {
        terminate_child(child);
    }
}

#[cfg(windows)]
struct BlockingProcessTree {
    job: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl BlockingProcessTree {
    fn attach(child: &std::process::Child) -> io::Result<Self> {
        use std::os::windows::io::AsRawHandle;
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
            return Err(io::Error::last_os_error());
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: limits has the exact structure and size required by the API.
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
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both handles are live for the duration of this call.
        if unsafe { AssignProcessToJobObject(job, child.as_raw_handle().cast()) } == 0 {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
            return Err(io::Error::last_os_error());
        }
        Ok(Self { job })
    }

    fn terminate(&self, child: &mut std::process::Child) {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        // SAFETY: the job handle is owned by this instance until Drop.
        if unsafe { TerminateJobObject(self.job, 1) } == 0 {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

#[cfg(windows)]
impl Drop for BlockingProcessTree {
    fn drop(&mut self) {
        // SAFETY: this instance uniquely owns the job handle.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.job) };
    }
}

fn require_git_success(args: &[&str], output: GitOutput) -> Result<Vec<u8>, WorktreeError> {
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(WorktreeError::GitFailed {
        message: format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim().lines().last().unwrap_or("unknown error")
        ),
    })
}
