//! Bounded atomic preference persistence and coalescing writer thread.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use futures::channel::oneshot;

use crate::platform::external_editor::validate_external_editor_preference;
use crate::preferences::{DesktopPreferences, PREFERENCES_SCHEMA_VERSION};

const MAX_PREFERENCES_BYTES: u64 = 64 * 1024;
const DESKTOP_DIRECTORY: &str = "desktop";
const PREFERENCES_FILE: &str = "preferences.json";
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreferenceRecovery {
    CorruptJson,
    UnsupportedSchema { found: u16 },
    Oversized { bytes: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreferenceLoad {
    pub preferences: DesktopPreferences,
    pub recovery: Option<PreferenceRecovery>,
}

impl PreferenceLoad {
    fn defaults(recovery: Option<PreferenceRecovery>) -> Self {
        Self {
            preferences: DesktopPreferences::default(),
            recovery,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PreferenceStoreError {
    #[error("desktop preference path is a symbolic link: {path}")]
    SymbolicLink { path: PathBuf },
    #[error("desktop preference path is not a directory: {path}")]
    NotDirectory { path: PathBuf },
    #[error("desktop preference path is not a regular file: {path}")]
    NotFile { path: PathBuf },
    #[error("desktop preferences I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("desktop preferences serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct PreferenceStore {
    directory: PathBuf,
    file: PathBuf,
}

/// Coalescing background writer for GPUI-owned preference changes.
///
/// Scheduling never waits for filesystem I/O. If several resize events arrive
/// while a write is active, only the newest complete preference snapshot is
/// retained.
pub struct PreferenceWriter {
    shared: Arc<PreferenceWriterShared>,
    thread: Option<JoinHandle<()>>,
}

struct PreferenceWriterShared {
    pending: Mutex<Option<PreferenceWriteRequest>>,
    wake: Condvar,
    stopping: AtomicBool,
}

struct PreferenceWriteRequest {
    preferences: DesktopPreferences,
    completion: oneshot::Sender<PreferenceWriteResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreferenceWriteResult {
    Written,
    Superseded,
    Failed(String),
}

impl PreferenceWriter {
    pub fn spawn(store: PreferenceStore) -> io::Result<Self> {
        let shared = Arc::new(PreferenceWriterShared {
            pending: Mutex::new(None),
            wake: Condvar::new(),
            stopping: AtomicBool::new(false),
        });
        let worker_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("evo-desktop-preferences".into())
            .spawn(move || preference_writer_loop(store, &worker_shared))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    pub fn schedule(
        &self,
        preferences: DesktopPreferences,
    ) -> oneshot::Receiver<PreferenceWriteResult> {
        let (completion, receiver) = oneshot::channel();
        let replaced = self
            .shared
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(PreferenceWriteRequest {
                preferences: preferences.normalized(),
                completion,
            });
        if let Some(replaced) = replaced {
            let _ = replaced.completion.send(PreferenceWriteResult::Superseded);
        }
        self.shared.wake.notify_one();
        receiver
    }
}

impl Drop for PreferenceWriter {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        self.shared.wake.notify_one();
        let Some(writer_thread) = self.thread.take() else {
            return;
        };
        let _ = thread::Builder::new()
            .name("evo-desktop-preferences-reaper".into())
            .spawn(move || {
                let _ = writer_thread.join();
            });
    }
}

impl PreferenceStore {
    /// Place adapter-only state below the product-resolved configuration root.
    pub fn new(global_config_dir: impl AsRef<Path>) -> Self {
        let directory = global_config_dir.as_ref().join(DESKTOP_DIRECTORY);
        let file = directory.join(PREFERENCES_FILE);
        Self { directory, file }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.file
    }

    pub fn load(&self) -> Result<PreferenceLoad, PreferenceStoreError> {
        match fs::symlink_metadata(&self.directory) {
            Ok(metadata) => {
                reject_symlink(&self.directory, &metadata)?;
                if !metadata.is_dir() {
                    return Err(PreferenceStoreError::NotDirectory {
                        path: self.directory.clone(),
                    });
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(PreferenceLoad::defaults(None));
            }
            Err(source) => return Err(io_error(&self.directory, source)),
        }
        let metadata = match fs::symlink_metadata(&self.file) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(PreferenceLoad::defaults(None));
            }
            Err(source) => return Err(io_error(&self.file, source)),
        };
        reject_symlink(&self.file, &metadata)?;
        if !metadata.is_file() {
            return Err(PreferenceStoreError::NotFile {
                path: self.file.clone(),
            });
        }
        if metadata.len() > MAX_PREFERENCES_BYTES {
            return Ok(PreferenceLoad::defaults(Some(
                PreferenceRecovery::Oversized {
                    bytes: metadata.len(),
                },
            )));
        }

        let file =
            open_preference_file(&self.file).map_err(|source| io_error(&self.file, source))?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_PREFERENCES_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| io_error(&self.file, source))?;
        if bytes.len() as u64 > MAX_PREFERENCES_BYTES {
            return Ok(PreferenceLoad::defaults(Some(
                PreferenceRecovery::Oversized {
                    bytes: bytes.len() as u64,
                },
            )));
        }

        let preferences = match serde_json::from_slice::<DesktopPreferences>(&bytes) {
            Ok(preferences) => preferences,
            Err(_) => {
                return Ok(PreferenceLoad::defaults(Some(
                    PreferenceRecovery::CorruptJson,
                )));
            }
        };
        if preferences.schema_version != PREFERENCES_SCHEMA_VERSION {
            return Ok(PreferenceLoad::defaults(Some(
                PreferenceRecovery::UnsupportedSchema {
                    found: preferences.schema_version,
                },
            )));
        }
        Ok(PreferenceLoad {
            preferences: normalize_platform_preferences(preferences),
            recovery: None,
        })
    }

    pub fn save(
        &self,
        preferences: &DesktopPreferences,
    ) -> Result<DesktopPreferences, PreferenceStoreError> {
        self.ensure_directory()?;
        if let Ok(metadata) = fs::symlink_metadata(&self.file) {
            reject_symlink(&self.file, &metadata)?;
            if !metadata.is_file() {
                return Err(PreferenceStoreError::NotFile {
                    path: self.file.clone(),
                });
            }
        }

        let normalized = normalize_platform_preferences(preferences.clone());
        let bytes = serde_json::to_vec_pretty(&normalized)?;
        debug_assert!((bytes.len() as u64) <= MAX_PREFERENCES_BYTES);

        let temp_path = self.directory.join(format!(
            ".preferences.{}.{}.tmp",
            std::process::id(),
            TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = self.write_atomic(&temp_path, &bytes);
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result?;
        Ok(normalized)
    }

    fn ensure_directory(&self) -> Result<(), PreferenceStoreError> {
        match fs::symlink_metadata(&self.directory) {
            Ok(metadata) => {
                reject_symlink(&self.directory, &metadata)?;
                if !metadata.is_dir() {
                    return Err(PreferenceStoreError::NotDirectory {
                        path: self.directory.clone(),
                    });
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let parent = self
                    .directory
                    .parent()
                    .expect("desktop directory has parent");
                fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
                fs::create_dir(&self.directory)
                    .map_err(|source| io_error(&self.directory, source))?;
            }
            Err(source) => return Err(io_error(&self.directory, source)),
        }
        Ok(())
    }

    fn write_atomic(&self, temp_path: &Path, bytes: &[u8]) -> Result<(), PreferenceStoreError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(temp_path)
            .map_err(|source| io_error(temp_path, source))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| io_error(temp_path, source))?;
        fs::rename(temp_path, &self.file).map_err(|source| io_error(&self.file, source))?;
        sync_directory(&self.directory)?;
        Ok(())
    }
}

fn normalize_platform_preferences(preferences: DesktopPreferences) -> DesktopPreferences {
    let mut preferences = preferences.normalized();
    if preferences
        .external_editor
        .as_ref()
        .is_some_and(|editor| validate_external_editor_preference(editor).is_err())
    {
        preferences.external_editor = None;
    }
    preferences
}

fn reject_symlink(path: &Path, metadata: &fs::Metadata) -> Result<(), PreferenceStoreError> {
    if metadata.file_type().is_symlink() {
        Err(PreferenceStoreError::SymbolicLink {
            path: path.to_path_buf(),
        })
    } else {
        Ok(())
    }
}

fn io_error(path: &Path, source: io::Error) -> PreferenceStoreError {
    PreferenceStoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(unix)]
fn open_preference_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn open_preference_file(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_preference_file(path: &Path) -> io::Result<File> {
    File::open(path)
}

fn preference_writer_loop(store: PreferenceStore, shared: &PreferenceWriterShared) {
    loop {
        let mut pending = shared
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while pending.is_none() && !shared.stopping.load(Ordering::Acquire) {
            pending = shared
                .wake
                .wait(pending)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let next = pending.take();
        let stopping = shared.stopping.load(Ordering::Acquire);
        drop(pending);

        if let Some(request) = next {
            let result = match store.save(&request.preferences) {
                Ok(_) => PreferenceWriteResult::Written,
                Err(error) => PreferenceWriteResult::Failed(error.to_string()),
            };
            let _ = request.completion.send(result);
            continue;
        }
        if stopping {
            break;
        }
    }
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<(), PreferenceStoreError> {
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|source| io_error(directory, source))
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> Result<(), PreferenceStoreError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::preferences::ExternalEditorPreference;
    use crate::preferences::{
        DesktopThinkingLevel, MAX_PERSISTED_SESSION_ID_BYTES,
        MAX_PERSISTED_SESSION_THINKING_LEVELS, WindowGeometry,
    };
    use crate::ui::shell::{CONTEXT_PANEL_MAX_WIDTH, SESSION_PANEL_MIN_WIDTH};

    #[test]
    fn missing_preferences_return_bounded_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let loaded = PreferenceStore::new(temp.path()).load().unwrap();
        assert_eq!(loaded.preferences, DesktopPreferences::default());
        assert_eq!(loaded.recovery, None);
    }

    #[test]
    fn preferences_round_trip_and_normalize_untrusted_geometry() {
        let temp = tempfile::tempdir().unwrap();
        let store = PreferenceStore::new(temp.path());
        let preferences = DesktopPreferences {
            schema_version: 900,
            window: WindowGeometry {
                x: i32::MIN,
                y: i32::MAX,
                width: 1,
                height: u32::MAX,
                maximized: true,
            },
            sessions_panel_visible: false,
            context_panel_visible: true,
            sessions_panel_width: 1,
            context_panel_width: u32::MAX,
            reduced_motion: true,
            ui_scale: 99.0,
            external_editor: None,
            scratch_workspace_id: Some("workspace-stable".into()),
            session_thinking_levels: BTreeMap::new(),
        };

        let saved = store.save(&preferences).unwrap();
        assert_eq!(saved.schema_version, PREFERENCES_SCHEMA_VERSION);
        assert_eq!(saved.window.x, -32_768);
        assert_eq!(saved.window.y, 32_767);
        assert_eq!(saved.window.width, 640);
        assert_eq!(saved.window.height, 4_320);
        assert_eq!(saved.sessions_panel_width, SESSION_PANEL_MIN_WIDTH);
        assert_eq!(saved.context_panel_width, CONTEXT_PANEL_MAX_WIDTH);
        assert_eq!(saved.ui_scale, 2.0);
        assert_eq!(
            saved.scratch_workspace_id.as_deref(),
            Some("workspace-stable")
        );

        let loaded = store.load().unwrap();
        assert_eq!(loaded.preferences, saved);
        assert_eq!(loaded.recovery, None);
    }

    #[test]
    fn session_thinking_preferences_are_sparse_bounded_and_forward_tolerant() {
        let mut preferences = DesktopPreferences::default();
        assert_eq!(
            preferences.thinking_level_for_session("session-a"),
            DesktopThinkingLevel::Default
        );
        assert!(
            preferences.set_thinking_level_for_session("session-a", DesktopThinkingLevel::High)
        );
        assert!(
            !preferences.set_thinking_level_for_session("session-a", DesktopThinkingLevel::High)
        );
        assert_eq!(
            preferences.thinking_level_for_session("session-a"),
            DesktopThinkingLevel::High
        );
        assert!(
            preferences.set_thinking_level_for_session("session-a", DesktopThinkingLevel::Default)
        );
        assert!(
            !preferences
                .session_thinking_levels
                .contains_key("session-a")
        );

        for index in 0..=MAX_PERSISTED_SESSION_THINKING_LEVELS {
            assert!(preferences.set_thinking_level_for_session(
                &format!("session-{index:03}"),
                DesktopThinkingLevel::Low
            ));
        }
        assert_eq!(
            preferences.session_thinking_levels.len(),
            MAX_PERSISTED_SESSION_THINKING_LEVELS
        );
        assert!(preferences.session_thinking_levels.contains_key(&format!(
            "session-{MAX_PERSISTED_SESSION_THINKING_LEVELS:03}"
        )));
        assert!(!preferences.set_thinking_level_for_session("", DesktopThinkingLevel::XHigh));
        assert!(!preferences.set_thinking_level_for_session(
            &"x".repeat(MAX_PERSISTED_SESSION_ID_BYTES + 1),
            DesktopThinkingLevel::XHigh
        ));
        let temp = tempfile::tempdir().unwrap();
        let store = PreferenceStore::new(temp.path());
        let saved = store.save(&preferences).unwrap();
        assert_eq!(store.load().unwrap().preferences, saved);

        let future = serde_json::json!({
            "schema_version": PREFERENCES_SCHEMA_VERSION,
            "window": {
                "x": 0, "y": 0, "width": 1200, "height": 800, "maximized": false
            },
            "sessions_panel_visible": true,
            "context_panel_visible": true,
            "reduced_motion": false,
            "ui_scale": 1.0,
            "session_thinking_levels": {"future-session": "future-level"}
        });
        let normalized = serde_json::from_value::<DesktopPreferences>(future)
            .unwrap()
            .normalized();
        assert!(normalized.session_thinking_levels.is_empty());
    }

    #[test]
    fn external_editor_preferences_round_trip_as_program_and_literal_argv() {
        let temp = tempfile::tempdir().unwrap();
        let store = PreferenceStore::new(temp.path());
        let mut preferences = DesktopPreferences {
            external_editor: Some(ExternalEditorPreference {
                program: "code".into(),
                args: vec!["--reuse-window".into(), "literal;argument".into()],
            }),
            ..DesktopPreferences::default()
        };

        let saved = store.save(&preferences).unwrap();
        assert_eq!(saved.external_editor, preferences.external_editor);
        assert_eq!(store.load().unwrap().preferences, saved);

        preferences.external_editor = Some(ExternalEditorPreference {
            program: "/bin/sh".into(),
            args: vec!["-c".into()],
        });
        assert!(store.save(&preferences).unwrap().external_editor.is_none());
    }

    #[test]
    fn corrupt_unknown_and_oversized_files_recover_without_rewriting() {
        let temp = tempfile::tempdir().unwrap();
        let store = PreferenceStore::new(temp.path());
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();

        fs::write(store.path(), b"{no").unwrap();
        assert_eq!(
            store.load().unwrap().recovery,
            Some(PreferenceRecovery::CorruptJson)
        );
        assert_eq!(fs::read(store.path()).unwrap(), b"{no");

        let unsupported = serde_json::json!({
            "schema_version": 2,
            "window": {
                "x": 0, "y": 0, "width": 1200, "height": 800, "maximized": false
            },
            "sessions_panel_visible": true,
            "context_panel_visible": true,
            "reduced_motion": false,
            "ui_scale": 1.0
        });
        fs::write(store.path(), serde_json::to_vec(&unsupported).unwrap()).unwrap();
        assert_eq!(
            store.load().unwrap().recovery,
            Some(PreferenceRecovery::UnsupportedSchema { found: 2 })
        );

        fs::write(store.path(), vec![b'x'; MAX_PREFERENCES_BYTES as usize + 1]).unwrap();
        assert_eq!(
            store.load().unwrap().recovery,
            Some(PreferenceRecovery::Oversized {
                bytes: MAX_PREFERENCES_BYTES + 1
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_file_and_directory_are_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let external = temp.path().join("external.json");
        fs::write(&external, b"do not touch").unwrap();

        let file_root = temp.path().join("file-root");
        fs::create_dir_all(file_root.join(DESKTOP_DIRECTORY)).unwrap();
        symlink(
            &external,
            file_root.join(DESKTOP_DIRECTORY).join(PREFERENCES_FILE),
        )
        .unwrap();
        let store = PreferenceStore::new(&file_root);
        assert!(matches!(
            store.load(),
            Err(PreferenceStoreError::SymbolicLink { .. })
        ));
        assert!(matches!(
            store.save(&DesktopPreferences::default()),
            Err(PreferenceStoreError::SymbolicLink { .. })
        ));
        assert_eq!(fs::read(&external).unwrap(), b"do not touch");

        let directory_root = temp.path().join("directory-root");
        fs::create_dir_all(&directory_root).unwrap();
        symlink(
            file_root.join(DESKTOP_DIRECTORY),
            directory_root.join(DESKTOP_DIRECTORY),
        )
        .unwrap();
        let store = PreferenceStore::new(&directory_root);
        assert!(matches!(
            store.load(),
            Err(PreferenceStoreError::SymbolicLink { .. })
        ));
        assert!(matches!(
            store.save(&DesktopPreferences::default()),
            Err(PreferenceStoreError::SymbolicLink { .. })
        ));
    }

    #[test]
    fn background_writer_coalesces_without_blocking_the_caller() {
        let temp = tempfile::tempdir().unwrap();
        let store = PreferenceStore::new(temp.path());
        let writer = PreferenceWriter::spawn(store.clone()).unwrap();
        let mut latest = None;
        for width in 640..700 {
            let mut preferences = DesktopPreferences::default();
            preferences.window.width = width;
            latest = Some(writer.schedule(preferences));
        }

        assert_eq!(
            futures::executor::block_on(latest.expect("the final write is scheduled")).unwrap(),
            PreferenceWriteResult::Written
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if store
                .load()
                .is_ok_and(|loaded| loaded.preferences.window.width == 699)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "coalesced preferences were not persisted"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn background_writer_reports_failure_on_the_scheduled_completion() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(DESKTOP_DIRECTORY), b"not a directory").unwrap();
        let writer = PreferenceWriter::spawn(PreferenceStore::new(temp.path())).unwrap();

        let result = futures::executor::block_on(writer.schedule(DesktopPreferences::default()))
            .expect("the writer thread returns one typed completion");

        assert!(
            matches!(result, PreferenceWriteResult::Failed(message) if message.contains("not a directory"))
        );
    }
}
