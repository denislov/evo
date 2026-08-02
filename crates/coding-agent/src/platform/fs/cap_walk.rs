use crate::mutex::MutexExt;
use crate::platform::fs::capability::FilesystemTarget;
use cap_std::fs::{Dir, File};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub(crate) const MAX_WALK_DEPTH: usize = 64;
pub(crate) const MAX_WALK_ENTRIES: usize = 100_000;
const MAX_GITIGNORE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapWalkEntryKind {
    File,
    Directory,
    Other,
}

#[derive(Clone)]
pub(crate) struct CapWalkEntry {
    source: CapWalkEntrySource,
    pub(crate) relative: PathBuf,
    pub(crate) kind: CapWalkEntryKind,
}

#[derive(Clone)]
enum CapWalkEntrySource {
    Child { parent: Arc<Dir>, name: PathBuf },
    OpenedFile(Arc<Mutex<File>>),
}

impl CapWalkEntry {
    pub(crate) fn read_bounded(&self, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
        match &self.source {
            CapWalkEntrySource::Child { parent, name } => {
                let mut file = parent.open(name)?;
                read_file_bounded(&mut file, max_bytes)
            }
            CapWalkEntrySource::OpenedFile(file) => {
                let mut file = file
                    .lock_resource("filesystem walk opened file")
                    .map_err(io::Error::other)?;
                file.seek(SeekFrom::Start(0))?;
                read_file_bounded(&mut file, max_bytes)
            }
        }
    }
}

fn read_file_bounded(file: &mut File, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
    let metadata = file.metadata()?;
    if metadata.len() > max_bytes {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(max_bytes as usize)
            .min(max_bytes as usize),
    );
    file.by_ref().take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes as usize {
        return Ok(None);
    }
    Ok(Some(bytes))
}

pub(crate) enum CapWalkRoot {
    File(CapWalkEntry),
    Directory(Vec<CapWalkEntry>),
}

pub(crate) fn walk_target(target: &FilesystemTarget) -> Result<CapWalkRoot, String> {
    if let Ok(directory) = target.opened_directory() {
        let mut entries = Vec::new();
        walk_directory(directory, Path::new(""), 0, &mut entries, &[])?;
        return Ok(CapWalkRoot::Directory(entries));
    }

    let file = target.opened_file()?;
    Ok(CapWalkRoot::File(CapWalkEntry {
        source: CapWalkEntrySource::OpenedFile(file),
        relative: target
            .relative_path()
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".")),
        kind: CapWalkEntryKind::File,
    }))
}

fn walk_directory(
    directory: Arc<Dir>,
    prefix: &Path,
    depth: usize,
    entries: &mut Vec<CapWalkEntry>,
    inherited_ignores: &[Arc<Gitignore>],
) -> Result<(), String> {
    if depth > MAX_WALK_DEPTH {
        return Err(format!(
            "filesystem walk exceeds the maximum depth of {MAX_WALK_DEPTH}"
        ));
    }
    let mut ignores = inherited_ignores.to_vec();
    if let Some(ignore) = load_gitignore(&directory, prefix) {
        ignores.push(Arc::new(ignore));
    }
    let read_dir = directory
        .entries()
        .map_err(|error| format!("cannot read capability directory: {error}"))?;
    for result in read_dir {
        let entry = match result {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if entries.len() >= MAX_WALK_ENTRIES {
            return Err(format!(
                "filesystem walk exceeds the maximum entry count of {MAX_WALK_ENTRIES}"
            ));
        }
        let name = entry.file_name();
        if matches!(name.to_str(), Some(".git" | "node_modules")) {
            continue;
        }
        let name_path = PathBuf::from(&name);
        let relative = prefix.join(&name_path);
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        let kind = if file_type.is_dir() {
            CapWalkEntryKind::Directory
        } else if file_type.is_file() {
            CapWalkEntryKind::File
        } else {
            CapWalkEntryKind::Other
        };
        if is_ignored(&ignores, &relative, kind == CapWalkEntryKind::Directory) {
            continue;
        }
        entries.push(CapWalkEntry {
            source: CapWalkEntrySource::Child {
                parent: directory.clone(),
                name: name_path,
            },
            relative: relative.clone(),
            kind,
        });
        if kind == CapWalkEntryKind::Directory
            && let Ok(child) = entry.open_dir()
        {
            walk_directory(Arc::new(child), &relative, depth + 1, entries, &ignores)?;
        }
    }
    Ok(())
}

fn load_gitignore(directory: &Dir, prefix: &Path) -> Option<Gitignore> {
    let mut file = directory.open(".gitignore").ok()?;
    let metadata = file.metadata().ok()?;
    if metadata.len() > MAX_GITIGNORE_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(MAX_GITIGNORE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_GITIGNORE_BYTES as usize {
        return None;
    }
    let content = String::from_utf8_lossy(&bytes);
    let root = if prefix.as_os_str().is_empty() {
        Path::new(".")
    } else {
        prefix
    };
    let mut builder = GitignoreBuilder::new(root);
    let source = root.join(".gitignore");
    for line in content.lines() {
        let _ = builder.add_line(Some(source.clone()), line);
    }
    builder.build().ok()
}

fn is_ignored(matchers: &[Arc<Gitignore>], relative: &Path, is_dir: bool) -> bool {
    let mut ignored = false;
    for matcher in matchers {
        let matched = matcher.matched_path_or_any_parents(relative, is_dir);
        if matched.is_ignore() {
            ignored = true;
        } else if matched.is_whitelist() {
            ignored = false;
        }
    }
    ignored
}
