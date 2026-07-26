use std::path::PathBuf;
use std::pin::Pin;

use futures::{Stream, future::BoxFuture};

use crate::execution::ExecutionError;

pub const MAX_SHELL_OUTPUT_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecutionOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOptions {
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionEvent {
    Stdout(String),
    Stderr(String),
    Exit(i32),
}

pub type ExecutionStream<'a> =
    Pin<Box<dyn Stream<Item = Result<ExecutionEvent, ExecutionError>> + Send + 'a>>;

pub trait Shell: Send + Sync {
    /// Stream one command's output in chunks no larger than
    /// [`MAX_SHELL_OUTPUT_CHUNK_BYTES`], followed by exactly one `Exit`.
    fn exec_stream<'a>(
        &'a self,
        command: &'a str,
        options: Option<ExecOptions>,
    ) -> ExecutionStream<'a>;
    fn cleanup_shell<'a>(&'a self) -> BoxFuture<'a, ()>;
}
