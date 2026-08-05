use std::io::{Read, Seek, SeekFrom};

use crate::mutex::MutexExt;
use crate::platform::io::output::format_size;
use crate::tools::FilesystemTarget;

pub(crate) async fn read_target_bytes(
    target: &FilesystemTarget,
    operation: &'static str,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let target = target.clone();
    tokio::task::spawn_blocking(move || {
        let handle = target.opened_file()?;
        let mut file = handle
            .lock_resource(operation)
            .map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| {
            format!(
                "{operation}: cannot stat {}: {error}",
                target.display_path().display()
            )
        })?;
        if metadata.len() > max_bytes as u64 {
            return Err(format!(
                "{operation}: refusing to read {} because it exceeds the {} safety limit",
                target.display_path().display(),
                format_size(max_bytes)
            ));
        }
        file.seek(SeekFrom::Start(0)).map_err(|error| {
            format!(
                "{operation}: cannot seek {}: {error}",
                target.display_path().display()
            )
        })?;
        let mut bytes = Vec::with_capacity(
            usize::try_from(metadata.len())
                .unwrap_or(max_bytes)
                .min(max_bytes),
        );
        file.by_ref()
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                format!(
                    "{operation}: cannot read {}: {error}",
                    target.display_path().display()
                )
            })?;
        if bytes.len() > max_bytes {
            return Err(format!(
                "{operation}: refusing to retain more than {} from {}",
                format_size(max_bytes),
                target.display_path().display()
            ));
        }
        Ok(bytes)
    })
    .await
    .map_err(|error| format!("{operation}: blocking read task failed: {error}"))?
}
