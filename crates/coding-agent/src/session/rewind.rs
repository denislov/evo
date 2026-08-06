use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use change_tracker::HunkTrackerCheckpoint;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use workspace_runtime::api::WorkspaceSnapshot;

use crate::kernel::error::CodingSessionError;
use crate::session::repository::SessionHandle;

const CHECKPOINT_VERSION: u32 = 1;
const MAX_CHECKPOINT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CHECKPOINT_ID_BYTES: usize = 128;
const CHECKPOINT_DIR: &str = "rewind";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RewindCheckpoint {
    pub(crate) version: u32,
    pub(crate) checkpoint_id: String,
    pub(crate) session_id: String,
    pub(crate) branch_id: String,
    pub(crate) leaf_id: String,
    pub(crate) session_sequence: u64,
    pub(crate) tracker: HunkTrackerCheckpoint,
    pub(crate) workspace: WorkspaceSnapshot,
    pub(crate) digest: String,
}

impl RewindCheckpoint {
    pub(crate) fn create(
        checkpoint_id: String,
        session_id: String,
        branch_id: String,
        leaf_id: String,
        session_sequence: u64,
        tracker: HunkTrackerCheckpoint,
        workspace: WorkspaceSnapshot,
    ) -> Result<Self, CodingSessionError> {
        validate_checkpoint_id(&checkpoint_id)?;
        let mut checkpoint = Self {
            version: CHECKPOINT_VERSION,
            checkpoint_id,
            session_id,
            branch_id,
            leaf_id,
            session_sequence,
            tracker,
            workspace,
            digest: String::new(),
        };
        checkpoint.digest = checkpoint.compute_digest()?;
        Ok(checkpoint)
    }

    pub(crate) fn validate(&self, expected_session_id: &str) -> Result<(), CodingSessionError> {
        validate_checkpoint_id(&self.checkpoint_id)?;
        self.workspace
            .validate()
            .map_err(|error| CodingSessionError::Session {
                message: format!(
                    "rewind checkpoint {} has an invalid workspace snapshot: {error}",
                    self.checkpoint_id
                ),
            })?;
        if self.version != CHECKPOINT_VERSION
            || self.session_id != expected_session_id
            || self.branch_id.trim().is_empty()
            || self.leaf_id.trim().is_empty()
            || self.digest != self.compute_digest()?
        {
            return Err(CodingSessionError::Session {
                message: format!(
                    "rewind checkpoint {} failed ownership or digest validation",
                    self.checkpoint_id
                ),
            });
        }
        Ok(())
    }

    fn compute_digest(&self) -> Result<String, CodingSessionError> {
        let mut unsigned = self.clone();
        unsigned.digest.clear();
        let bytes = serde_json::to_vec(&unsigned).map_err(|error| CodingSessionError::Session {
            message: format!("cannot encode rewind checkpoint digest: {error}"),
        })?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

pub(crate) fn save(
    handle: &SessionHandle,
    checkpoint: &RewindCheckpoint,
) -> Result<(), CodingSessionError> {
    checkpoint.validate(&handle.manifest().session_id)?;
    let directory = checkpoint_dir(handle);
    fs::create_dir_all(&directory).map_err(io_error)?;
    let destination = checkpoint_path(handle, &checkpoint.checkpoint_id)?;
    let temporary = directory.join(format!(
        ".{}.tmp-{}",
        checkpoint.checkpoint_id,
        std::process::id()
    ));
    let bytes = serde_json::to_vec(checkpoint).map_err(|error| CodingSessionError::Session {
        message: format!("cannot encode rewind checkpoint: {error}"),
    })?;
    if bytes.len() as u64 > MAX_CHECKPOINT_BYTES {
        return Err(CodingSessionError::Session {
            message: format!("rewind checkpoint exceeds {MAX_CHECKPOINT_BYTES} bytes"),
        });
    }
    let result = (|| -> Result<(), CodingSessionError> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(io_error)?;
        file.write_all(&bytes).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        fs::rename(&temporary, &destination).map_err(io_error)?;
        sync_directory(&directory)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn load(
    handle: &SessionHandle,
    checkpoint_id: &str,
) -> Result<RewindCheckpoint, CodingSessionError> {
    let path = checkpoint_path(handle, checkpoint_id)?;
    let metadata = fs::metadata(&path).map_err(io_error)?;
    if metadata.len() > MAX_CHECKPOINT_BYTES {
        return Err(CodingSessionError::Session {
            message: format!("rewind checkpoint exceeds {MAX_CHECKPOINT_BYTES} bytes"),
        });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(&path)
        .map_err(io_error)?
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    let checkpoint: RewindCheckpoint =
        serde_json::from_slice(&bytes).map_err(|error| CodingSessionError::Session {
            message: format!(
                "cannot decode rewind checkpoint {}: {error}",
                path.display()
            ),
        })?;
    checkpoint.validate(&handle.manifest().session_id)?;
    Ok(checkpoint)
}

pub(crate) fn remove(
    handle: &SessionHandle,
    checkpoint_id: &str,
) -> Result<(), CodingSessionError> {
    let path = checkpoint_path(handle, checkpoint_id)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

pub(crate) fn cleanup_orphans(handle: &SessionHandle) -> Result<(), CodingSessionError> {
    let directory = checkpoint_dir(handle);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    for entry in entries {
        let entry = entry.map_err(io_error)?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(".tmp-"))
        {
            fs::remove_file(path).map_err(io_error)?;
        }
    }
    Ok(())
}

fn validate_checkpoint_id(value: &str) -> Result<(), CodingSessionError> {
    if value.is_empty()
        || value.len() > MAX_CHECKPOINT_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(CodingSessionError::Input {
            message: "rewind checkpoint id is invalid".into(),
        });
    }
    Ok(())
}

fn checkpoint_dir(handle: &SessionHandle) -> PathBuf {
    handle.session_dir().join(CHECKPOINT_DIR)
}

fn checkpoint_path(
    handle: &SessionHandle,
    checkpoint_id: &str,
) -> Result<PathBuf, CodingSessionError> {
    validate_checkpoint_id(checkpoint_id)?;
    Ok(checkpoint_dir(handle).join(format!("{checkpoint_id}.json")))
}

fn sync_directory(path: &Path) -> Result<(), CodingSessionError> {
    File::open(path)
        .map_err(io_error)?
        .sync_all()
        .map_err(io_error)
}

fn io_error(error: std::io::Error) -> CodingSessionError {
    CodingSessionError::Resource {
        message: format!("rewind checkpoint storage failed: {error}"),
    }
}
