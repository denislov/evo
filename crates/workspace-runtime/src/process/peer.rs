//! Long-lived duplex stdio child for interactive request/response protocols
//! (MCP stdio servers).

use crate::process::{
    ProcessSpec, ProcessTree, command_from_spec, configure_process, terminate_child_process_tree,
};

/// A long-lived interactive child (duplex stdio peer).
///
/// Unlike the one-shot [`run`] contract (which fixes `stdin = null` and
/// collects output to completion), [`spawn_peer`] keeps the child's stdin
/// piped so the caller can drive a request/response protocol (MCP stdio
/// servers). Sandbox enforcement and process-tree containment are identical
/// to [`run`]: the same [`SandboxProfile`] preparation runs at the spawn
/// boundary, unsupported platforms fail the spawn explicitly, and the child
/// is killed as a group on drop/terminate.
pub struct PeerProcess {
    child: tokio::process::Child,
    process_tree: ProcessTree,
    stdin: Option<tokio::process::ChildStdin>,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
}

impl std::fmt::Debug for PeerProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PeerProcess")
            .field("pid", &self.child.id())
            .finish()
    }
}

impl PeerProcess {
    /// Spawn a duplex stdio child under the spec's sandbox profile.
    ///
    /// Failures (sandbox unsupported, spawn error, pipe capture failure)
    /// return an explicit `Err`; a partially attached child is killed and
    /// reaped before the error is returned.
    pub async fn spawn(spec: ProcessSpec) -> Result<Self, String> {
        let mut command = command_from_spec(&spec);
        let prepared_sandbox = match spec.sandbox.as_ref().map(crate::sandbox::prepare_sandbox) {
            Some(Ok(prepared)) => prepared,
            Some(Err(error)) => {
                return Err(format!("sandbox setup failed: {error}"));
            }
            None => None,
        };
        configure_process(&mut command, &spec, prepared_sandbox, true);
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
        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                terminate_child_process_tree(&mut child, &process_tree).await;
                return Err("failed to capture stdin".into());
            }
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                terminate_child_process_tree(&mut child, &process_tree).await;
                return Err("failed to capture stdout".into());
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                terminate_child_process_tree(&mut child, &process_tree).await;
                return Err("failed to capture stderr".into());
            }
        };
        Ok(Self {
            child,
            process_tree,
            stdin: Some(stdin),
            stdout: Some(stdout),
            stderr: Some(stderr),
        })
    }

    pub fn stdin(&mut self) -> &mut tokio::process::ChildStdin {
        self.stdin.as_mut().expect("peer stdin already taken")
    }

    pub fn stdout(&mut self) -> &mut tokio::process::ChildStdout {
        self.stdout.as_mut().expect("peer stdout already taken")
    }

    pub fn stderr(&mut self) -> &mut tokio::process::ChildStderr {
        self.stderr.as_mut().expect("peer stderr already taken")
    }

    /// Take the child's stdin for long-lived exclusive ownership.
    pub fn take_stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.stdin.take()
    }

    /// Take the child's stdout for long-lived exclusive ownership.
    pub fn take_stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        self.stdout.take()
    }

    /// Take the child's stderr for long-lived exclusive ownership.
    pub fn take_stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.stderr.take()
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Wait for the child to exit; returns its exit code (`None` = signal).
    pub async fn wait(&mut self) -> Option<i32> {
        self.child
            .wait()
            .await
            .ok()
            .and_then(|status| status.code())
    }

    /// Kill the whole child process group and reap. Idempotent.
    pub async fn terminate(&mut self) {
        terminate_child_process_tree(&mut self.child, &self.process_tree).await;
    }

    /// Detach process-tree containment without killing the child.
    ///
    /// Call after the child has exited cleanly to disarm the group kill.
    pub fn disarm(&mut self) {
        self.process_tree.disarm();
    }
}
