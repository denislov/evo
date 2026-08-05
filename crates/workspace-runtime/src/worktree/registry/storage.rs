use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{RegistryError, WorktreeRecord};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn deserialize_record(bytes: &[u8]) -> Result<WorktreeRecord, RegistryError> {
    serde_json::from_slice(bytes).map_err(|error| RegistryError::InvalidRecord {
        message: format!("cannot decode record: {error}"),
    })
}

pub(crate) fn write_record_atomic(
    path: &Path,
    record: &WorktreeRecord,
) -> Result<(), RegistryError> {
    let bytes = serde_json::to_vec(record).map_err(|error| RegistryError::Io {
        message: format!("cannot encode record: {error}"),
    })?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.tmp.{}.{}", std::process::id(), sequence));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp)
        .map_err(|error| RegistryError::Io {
            message: format!("cannot create record temp file: {error}"),
        })?;
    let result = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = result
        .and_then(|()| replace_record_file(&tmp, path))
        .and_then(|()| sync_parent_directory(path));
    result.map_err(|error| {
        let _ = fs::remove_file(&tmp);
        RegistryError::Io {
            message: format!("cannot commit record: {error}"),
        }
    })
}

#[cfg(not(windows))]
fn replace_record_file(tmp: &Path, path: &Path) -> io::Result<()> {
    fs::rename(tmp, path)
}

#[cfg(windows)]
fn replace_record_file(tmp: &Path, path: &Path) -> io::Result<()> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MoveFileExW};
    let from = tmp
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    let to = path
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    unsafe {
        if MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING) == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> io::Result<()> {
    File::open(path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "record has no parent directory",
        )
    })?)
    .and_then(|directory| directory.sync_all())
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub(super) fn read_directory(path: &Path) -> Result<Vec<fs::DirEntry>, RegistryError> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| RegistryError::Io {
            message: format!("cannot read directory {}: {error}", path.display()),
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| RegistryError::Io {
            message: format!("cannot read directory entry: {error}"),
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}
