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
    Rooted { root: Arc<Dir>, path: PathBuf },
    OpenedFile(Arc<Mutex<File>>),
}

impl CapWalkEntry {
    pub(crate) fn read_bounded(&self, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
        match &self.source {
            CapWalkEntrySource::Rooted { root, path } => {
                let mut file = root.open(path)?;
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
        walk_directory(
            directory.clone(),
            directory,
            Path::new(""),
            0,
            &mut entries,
            &[],
        )?;
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
    root: Arc<Dir>,
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
    let discovered = {
        let mut discovered = Vec::new();
        let read_dir = directory
            .entries()
            .map_err(|error| format!("cannot read capability directory: {error}"))?;
        for result in read_dir {
            let entry = match result {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if entries.len().saturating_add(discovered.len()) >= MAX_WALK_ENTRIES {
                return Err(format!(
                    "filesystem walk exceeds the maximum entry count of {MAX_WALK_ENTRIES}"
                ));
            }
            let name = entry.file_name();
            if matches!(name.to_str(), Some(".git" | "node_modules")) {
                continue;
            }
            let name_path = PathBuf::from(&name);
            let relative = prefix.join(name_path);
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
            discovered.push((relative, kind));
        }
        discovered
    };
    drop(directory);

    for (relative, kind) in discovered {
        if entries.len() >= MAX_WALK_ENTRIES {
            return Err(format!(
                "filesystem walk exceeds the maximum entry count of {MAX_WALK_ENTRIES}"
            ));
        }
        entries.push(CapWalkEntry {
            source: CapWalkEntrySource::Rooted {
                root: root.clone(),
                path: relative.clone(),
            },
            relative: relative.clone(),
            kind,
        });
        if kind == CapWalkEntryKind::Directory
            && let Ok(child) = root.open_dir(&relative)
        {
            walk_directory(
                root.clone(),
                Arc::new(child),
                &relative,
                depth + 1,
                entries,
                &ignores,
            )?;
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::platform::fs::capability::FilesystemCapability;

    fn fixture_fd_count(root: &Path) -> usize {
        std::fs::read_dir("/proc/self/fd")
            .expect("read process fd directory")
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .filter(|target| target.starts_with(root))
            .count()
    }

    #[tokio::test]
    async fn directory_walk_descriptor_usage_is_constant() {
        let temp = tempfile::tempdir().expect("tempdir");
        for index in 0..128 {
            let directory = temp.path().join(format!("directory-{index}"));
            std::fs::create_dir(&directory).expect("create fixture directory");
            std::fs::write(directory.join("file.txt"), format!("value-{index}"))
                .expect("write fixture file");
        }
        let mut nested = temp.path().join("nested");
        for index in 0..48 {
            nested.push(format!("level-{index}"));
            std::fs::create_dir_all(&nested).expect("create nested fixture directory");
        }
        std::fs::write(nested.join("leaf.txt"), "nested-value").expect("write nested fixture file");

        let capability =
            FilesystemCapability::new(temp.path().to_path_buf()).expect("filesystem capability");
        capability
            .bind_tool_target("operation", "tool-call", "grep", ".")
            .await
            .expect("bind directory target");
        let target = capability
            .take_bound_tool_target("operation", "tool-call", "grep", ".")
            .expect("take directory target");
        let descriptors_before_walk = fixture_fd_count(temp.path());

        let CapWalkRoot::Directory(entries) = walk_target(&target).expect("walk fixture") else {
            panic!("fixture target must be a directory");
        };

        let descriptors_while_entries_live = fixture_fd_count(temp.path());
        assert!(
            descriptors_while_entries_live <= descriptors_before_walk + 1,
            "walk entries retained {} extra fixture descriptors",
            descriptors_while_entries_live.saturating_sub(descriptors_before_walk)
        );

        let nested_file = entries
            .iter()
            .find(|entry| entry.relative == Path::new("directory-127/file.txt"))
            .expect("nested fixture file");
        assert_eq!(
            nested_file.read_bounded(1024).expect("read nested file"),
            Some(b"value-127".to_vec())
        );
        let deep_file = entries
            .iter()
            .find(|entry| entry.relative.ends_with("level-47/leaf.txt"))
            .expect("deep fixture file");
        assert_eq!(
            deep_file.read_bounded(1024).expect("read deep file"),
            Some(b"nested-value".to_vec())
        );
    }
}
