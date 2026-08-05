use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cap_std::fs::File;

use crate::fs::mutation::MutationGuard;
use crate::resource::lock_resource;

const MAX_EDIT_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// Human-readable size label for budget messages, mirroring the shared
/// agent-core helper this crate is deliberately not allowed to depend on.
fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub struct OpenedEditFile {
    file: Arc<Mutex<File>>,
    display: PathBuf,
}

impl OpenedEditFile {
    pub fn new(file: Arc<Mutex<File>>, display: PathBuf) -> Self {
        Self { file, display }
    }

    pub async fn read_file(&self) -> Result<Vec<u8>, String> {
        let file = self.file.clone();
        let display = self.display.clone();
        tokio::task::spawn_blocking(move || {
            let mut file = lock_resource(&file, "edit opened file")
                .map_err(|error| format!("{error}: {}", display.display()))?;
            let metadata = file.metadata().map_err(|error| {
                format!(
                    "edit: cannot stat opened file {}: {error}",
                    display.display()
                )
            })?;
            if metadata.len() > MAX_EDIT_FILE_BYTES {
                return Err(format!(
                    "edit: refusing to read {} because it exceeds the {} safety limit",
                    display.display(),
                    format_size(MAX_EDIT_FILE_BYTES as usize)
                ));
            }
            file.seek(SeekFrom::Start(0)).map_err(|error| {
                format!(
                    "edit: cannot seek opened file {}: {error}",
                    display.display()
                )
            })?;
            let mut raw = Vec::with_capacity(
                usize::try_from(metadata.len())
                    .unwrap_or(MAX_EDIT_FILE_BYTES as usize)
                    .min(MAX_EDIT_FILE_BYTES as usize),
            );
            Read::by_ref(&mut *file)
                .take(MAX_EDIT_FILE_BYTES + 1)
                .read_to_end(&mut raw)
                .map_err(|error| {
                    format!(
                        "edit: cannot read opened file {}: {error}",
                        display.display()
                    )
                })?;
            if raw.len() > MAX_EDIT_FILE_BYTES as usize {
                return Err(format!(
                    "edit: refusing to retain more than {} from {}",
                    format_size(MAX_EDIT_FILE_BYTES as usize),
                    display.display()
                ));
            }
            Ok(raw)
        })
        .await
        .map_err(|error| format!("edit: blocking read task failed: {error}"))?
    }

    pub async fn write_file(&self, content: &[u8], mutation: MutationGuard) -> Result<(), String> {
        let file = self.file.clone();
        let display = self.display.clone();
        let content = content.to_vec();
        tokio::task::spawn_blocking(move || {
            let _mutation = mutation;
            let mut file = lock_resource(&file, "edit opened file")
                .map_err(|error| format!("{error}: {}", display.display()))?;
            file.seek(SeekFrom::Start(0)).map_err(|error| {
                format!(
                    "edit: cannot seek opened file {}: {error}",
                    display.display()
                )
            })?;
            file.set_len(0).map_err(|error| {
                format!(
                    "edit: cannot truncate opened file {}: {error}",
                    display.display()
                )
            })?;
            file.write_all(&content).map_err(|error| {
                format!(
                    "edit: failed to write opened file {}: {error}",
                    display.display()
                )
            })?;
            file.sync_all().map_err(|error| {
                format!(
                    "edit: failed to sync opened file {}: {error}",
                    display.display()
                )
            })
        })
        .await
        .map_err(|error| format!("edit: blocking write task failed: {error}"))?
    }
}
