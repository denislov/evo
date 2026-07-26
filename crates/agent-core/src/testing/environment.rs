use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::future::{BoxFuture, FutureExt};

use crate::execution::{ExecOptions, ExecutionOutput, FileInfo, FileKind, FileSystem, Shell};
use crate::execution::{ExecutionError, FileError};

#[derive(Debug, Clone)]
pub struct InMemoryExecutionEnv {
    cwd: PathBuf,
    inner: Arc<Mutex<InMemoryEnvState>>,
}

#[derive(Debug, Default)]
struct InMemoryEnvState {
    files: BTreeMap<PathBuf, Vec<u8>>,
    dirs: BTreeSet<PathBuf>,
    commands: BTreeMap<String, ExecutionOutput>,
    temp_counter: u64,
}

impl InMemoryExecutionEnv {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        let cwd = cwd.into();
        let mut dirs = BTreeSet::new();
        dirs.insert(cwd.clone());
        Self {
            cwd,
            inner: Arc::new(Mutex::new(InMemoryEnvState {
                dirs,
                ..Default::default()
            })),
        }
    }

    pub fn set_command(&self, command: impl Into<String>, output: ExecutionOutput) {
        self.inner
            .lock()
            .unwrap()
            .commands
            .insert(command.into(), output);
    }

    fn abs(&self, path: &str) -> PathBuf {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            normalize_path(&path)
        } else {
            normalize_path(&self.cwd.join(path))
        }
    }

    fn ensure_parent_dir(state: &mut InMemoryEnvState, path: &Path) {
        let mut current = PathBuf::new();
        if let Some(parent) = path.parent() {
            for component in parent.components() {
                current.push(component);
                state.dirs.insert(current.clone());
            }
        }
    }

    fn missing(path: &Path) -> FileError {
        FileError::NotFound {
            message: format!("file not found: {}", path.display()),
            path: Some(path.to_path_buf()),
        }
    }
}

impl FileSystem for InMemoryExecutionEnv {
    fn cwd(&self) -> PathBuf {
        self.cwd.clone()
    }

    fn absolute_path<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<PathBuf, FileError>> {
        async move { Ok(self.abs(path)) }.boxed()
    }

    fn join_path<'a>(&'a self, parts: &'a [&'a str]) -> BoxFuture<'a, Result<PathBuf, FileError>> {
        async move {
            let mut joined = PathBuf::new();
            for part in parts {
                joined.push(part);
            }
            Ok(self.abs(&joined.display().to_string()))
        }
        .boxed()
    }

    fn read_text_file<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<String, FileError>> {
        async move {
            let bytes = self.read_binary_file(path).await?;
            String::from_utf8(bytes).map_err(|error| FileError::Io {
                message: error.to_string(),
                path: Some(self.abs(path)),
            })
        }
        .boxed()
    }

    fn read_text_lines<'a>(
        &'a self,
        path: &'a str,
        max_lines: Option<usize>,
    ) -> BoxFuture<'a, Result<Vec<String>, FileError>> {
        async move {
            let text = self.read_text_file(path).await?;
            Ok(text
                .lines()
                .take(max_lines.unwrap_or(usize::MAX))
                .map(str::to_string)
                .collect())
        }
        .boxed()
    }

    fn read_binary_file<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<u8>, FileError>> {
        async move {
            let path = self.abs(path);
            self.inner
                .lock()
                .unwrap()
                .files
                .get(&path)
                .cloned()
                .ok_or_else(|| Self::missing(&path))
        }
        .boxed()
    }

    fn write_file<'a>(
        &'a self,
        path: &'a str,
        content: &'a [u8],
    ) -> BoxFuture<'a, Result<(), FileError>> {
        async move {
            let path = self.abs(path);
            let mut state = self.inner.lock().unwrap();
            Self::ensure_parent_dir(&mut state, &path);
            state.files.insert(path, content.to_vec());
            Ok(())
        }
        .boxed()
    }

    fn append_file<'a>(
        &'a self,
        path: &'a str,
        content: &'a [u8],
    ) -> BoxFuture<'a, Result<(), FileError>> {
        async move {
            let path = self.abs(path);
            let mut state = self.inner.lock().unwrap();
            Self::ensure_parent_dir(&mut state, &path);
            state
                .files
                .entry(path)
                .or_default()
                .extend_from_slice(content);
            Ok(())
        }
        .boxed()
    }

    fn file_info<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<FileInfo, FileError>> {
        async move {
            let path = self.abs(path);
            let state = self.inner.lock().unwrap();
            if let Some(bytes) = state.files.get(&path) {
                return Ok(file_info(path, FileKind::File, Some(bytes.len() as u64)));
            }
            if state.dirs.contains(&path) {
                return Ok(file_info(path, FileKind::Directory, None));
            }
            Err(Self::missing(&path))
        }
        .boxed()
    }

    fn list_dir<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Vec<FileInfo>, FileError>> {
        async move {
            let path = self.abs(path);
            let state = self.inner.lock().unwrap();
            if !state.dirs.contains(&path) {
                return Err(Self::missing(&path));
            }

            let mut entries = Vec::new();
            for dir in &state.dirs {
                if dir.parent() == Some(path.as_path()) && dir != &path {
                    entries.push(file_info(dir.clone(), FileKind::Directory, None));
                }
            }
            for (file, bytes) in &state.files {
                if file.parent() == Some(path.as_path()) {
                    entries.push(file_info(
                        file.clone(),
                        FileKind::File,
                        Some(bytes.len() as u64),
                    ));
                }
            }
            entries.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(entries)
        }
        .boxed()
    }

    fn canonical_path<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<PathBuf, FileError>> {
        async move {
            let path = self.abs(path);
            if self.exists(path.to_string_lossy().as_ref()).await? {
                Ok(path)
            } else {
                Err(Self::missing(&path))
            }
        }
        .boxed()
    }

    fn exists<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<bool, FileError>> {
        async move {
            let path = self.abs(path);
            let state = self.inner.lock().unwrap();
            Ok(state.files.contains_key(&path) || state.dirs.contains(&path))
        }
        .boxed()
    }

    fn create_dir<'a>(
        &'a self,
        path: &'a str,
        recursive: bool,
    ) -> BoxFuture<'a, Result<(), FileError>> {
        async move {
            let path = self.abs(path);
            let mut state = self.inner.lock().unwrap();
            if recursive {
                let mut current = PathBuf::new();
                for component in path.components() {
                    current.push(component);
                    state.dirs.insert(current.clone());
                }
            } else {
                state.dirs.insert(path);
            }
            Ok(())
        }
        .boxed()
    }

    fn remove<'a>(
        &'a self,
        path: &'a str,
        recursive: bool,
        force: bool,
    ) -> BoxFuture<'a, Result<(), FileError>> {
        async move {
            let path = self.abs(path);
            let mut state = self.inner.lock().unwrap();
            let existed = state.files.remove(&path).is_some() || state.dirs.remove(&path);
            if recursive {
                state.files.retain(|p, _| !p.starts_with(&path));
                state.dirs.retain(|p| !p.starts_with(&path));
            }
            if !existed && !force {
                return Err(Self::missing(&path));
            }
            Ok(())
        }
        .boxed()
    }

    fn create_temp_dir<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, Result<PathBuf, FileError>> {
        async move {
            let path = {
                let mut state = self.inner.lock().unwrap();
                state.temp_counter += 1;
                self.cwd.join(format!("{}{}", prefix, state.temp_counter))
            };
            self.create_dir(path.to_string_lossy().as_ref(), true)
                .await?;
            Ok(path)
        }
        .boxed()
    }

    fn create_temp_file<'a>(
        &'a self,
        prefix: &'a str,
        suffix: &'a str,
    ) -> BoxFuture<'a, Result<PathBuf, FileError>> {
        async move {
            let path = {
                let mut state = self.inner.lock().unwrap();
                state.temp_counter += 1;
                self.cwd
                    .join(format!("{}{}{}", prefix, state.temp_counter, suffix))
            };
            self.write_file(path.to_string_lossy().as_ref(), b"")
                .await?;
            Ok(path)
        }
        .boxed()
    }

    fn cleanup_files<'a>(&'a self) -> BoxFuture<'a, ()> {
        async move {
            let mut state = self.inner.lock().unwrap();
            state.files.clear();
            state.dirs.clear();
            state.dirs.insert(self.cwd.clone());
        }
        .boxed()
    }
}

impl Shell for InMemoryExecutionEnv {
    fn exec_stream<'a>(
        &'a self,
        command: &'a str,
        _options: Option<ExecOptions>,
    ) -> crate::execution::ExecutionStream<'a> {
        let output = self.inner.lock().unwrap().commands.get(command).cloned();
        let Some(output) = output else {
            return Box::pin(futures::stream::once(async move {
                Err(ExecutionError::ShellUnavailable {
                    message: format!("no faux command registered: {command}"),
                })
            }));
        };

        let mut events = chunk_events(output.stdout, crate::execution::ExecutionEvent::Stdout);
        events.extend(chunk_events(
            output.stderr,
            crate::execution::ExecutionEvent::Stderr,
        ));
        events.push(crate::execution::ExecutionEvent::Exit(output.exit_code));
        Box::pin(futures::stream::iter(events.into_iter().map(Ok)))
    }

    fn cleanup_shell<'a>(&'a self) -> BoxFuture<'a, ()> {
        async move {
            self.inner.lock().unwrap().commands.clear();
        }
        .boxed()
    }
}

fn chunk_events(
    text: String,
    event: fn(String) -> crate::execution::ExecutionEvent,
) -> Vec<crate::execution::ExecutionEvent> {
    let mut events = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + crate::execution::MAX_SHELL_OUTPUT_CHUNK_BYTES).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        events.push(event(text[start..end].to_string()));
        start = end;
    }
    events
}

fn file_info(path: PathBuf, kind: FileKind, len: Option<u64>) -> FileInfo {
    FileInfo {
        name: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string(),
        path,
        kind,
        len,
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    normalized
}
