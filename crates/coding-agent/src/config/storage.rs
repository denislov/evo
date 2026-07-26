use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) const MAX_CONFIG_FILE_BYTES: usize = 1024 * 1024;

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

pub(super) fn read_bounded_text(path: &Path) -> io::Result<Option<String>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }

    let file = match options.open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "configuration path is not a regular file",
        ));
    }
    if metadata.len() > MAX_CONFIG_FILE_BYTES as u64 {
        return Err(file_too_large());
    }

    let initial_capacity = usize::try_from(metadata.len())
        .unwrap_or(MAX_CONFIG_FILE_BYTES)
        .min(MAX_CONFIG_FILE_BYTES);
    let mut bytes = Vec::with_capacity(initial_capacity);
    file.take((MAX_CONFIG_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CONFIG_FILE_BYTES {
        return Err(file_too_large());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

pub(super) fn atomic_write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    if contents.len() > MAX_CONFIG_FILE_BYTES {
        return Err(file_too_large());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to replace a symbolic-link configuration file",
        ));
    }

    let mut temporary =
        create_private_temp(parent, path.file_name().unwrap_or(OsStr::new("config")))?;
    (|| {
        temporary.file.write_all(contents)?;
        temporary.file.sync_all()?;
        fs::rename(&temporary.path, path)?;
        temporary.committed = true;
        sync_directory(parent)
    })()
}

fn create_private_temp(parent: &Path, file_name: &OsStr) -> io::Result<TemporaryFile> {
    for _ in 0..32 {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let mut temporary_name = std::ffi::OsString::from(".");
        temporary_name.push(file_name);
        temporary_name.push(format!(".{}.{}.tmp", std::process::id(), sequence));
        let path = parent.join(temporary_name);

        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => {
                return Ok(TemporaryFile {
                    file,
                    path,
                    committed: false,
                });
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique configuration temporary file",
    ))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    // Rust's standard library does not expose portable directory fsync.
    Ok(())
}

fn file_too_large() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "configuration file exceeds the {} byte limit",
            MAX_CONFIG_FILE_BYTES
        ),
    )
}

struct TemporaryFile {
    file: File,
    path: PathBuf,
    committed: bool,
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reader_rejects_oversized_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let file = File::create(&path).unwrap();
        file.set_len(MAX_CONFIG_FILE_BYTES as u64 + 1).unwrap();

        let error = read_bounded_text(&path).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("exceeds"));
    }

    #[cfg(unix)]
    #[test]
    fn reader_and_writer_refuse_symbolic_link_targets() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.toml");
        let link = dir.path().join("settings.toml");
        fs::write(&target, "theme = \"safe\"\n").unwrap();
        symlink(&target, &link).unwrap();

        assert!(read_bounded_text(&link).is_err());
        assert!(atomic_write_private(&link, b"theme = \"changed\"\n").is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "theme = \"safe\"\n");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_writer_uses_private_mode_from_creation() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.toml");

        atomic_write_private(&path, b"[openai]\nkey = \"secret\"\n").unwrap();

        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
