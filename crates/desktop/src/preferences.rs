//! Bounded client-local preference persistence for the desktop adapter.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};

use crate::file_review::DesktopExternalEditorConfig;

const PREFERENCES_SCHEMA_VERSION: u16 = 1;
const MAX_PREFERENCES_BYTES: u64 = 64 * 1024;
const DESKTOP_DIRECTORY: &str = "desktop";
const PREFERENCES_FILE: &str = "preferences.json";
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

impl Default for WindowGeometry {
    fn default() -> Self {
        Self {
            x: 80,
            y: 60,
            width: 1_280,
            height: 840,
            maximized: false,
        }
    }
}

impl WindowGeometry {
    fn normalize(&mut self) {
        self.x = self.x.clamp(-32_768, 32_767);
        self.y = self.y.clamp(-32_768, 32_767);
        self.width = self.width.clamp(640, 7_680);
        self.height = self.height.clamp(480, 4_320);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopPreferences {
    pub schema_version: u16,
    pub window: WindowGeometry,
    pub sessions_panel_visible: bool,
    pub context_panel_visible: bool,
    pub reduced_motion: bool,
    pub ui_scale: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) external_editor: Option<DesktopExternalEditorConfig>,
}

impl Default for DesktopPreferences {
    fn default() -> Self {
        Self {
            schema_version: PREFERENCES_SCHEMA_VERSION,
            window: WindowGeometry::default(),
            sessions_panel_visible: true,
            context_panel_visible: true,
            reduced_motion: false,
            ui_scale: 1.0,
            external_editor: None,
        }
    }
}

impl DesktopPreferences {
    pub fn normalized(mut self) -> Self {
        self.schema_version = PREFERENCES_SCHEMA_VERSION;
        self.window.normalize();
        if !self.ui_scale.is_finite() {
            self.ui_scale = 1.0;
        }
        self.ui_scale = self.ui_scale.clamp(0.75, 2.0);
        if self
            .external_editor
            .as_ref()
            .is_some_and(|editor| editor.validate().is_err())
        {
            self.external_editor = None;
        }
        self
    }
}

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
    pending: Mutex<Option<DesktopPreferences>>,
    wake: Condvar,
    stopping: AtomicBool,
    latest_error: Mutex<Option<String>>,
}

impl PreferenceWriter {
    pub fn spawn(store: PreferenceStore) -> io::Result<Self> {
        let shared = Arc::new(PreferenceWriterShared {
            pending: Mutex::new(None),
            wake: Condvar::new(),
            stopping: AtomicBool::new(false),
            latest_error: Mutex::new(None),
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

    pub fn schedule(&self, preferences: DesktopPreferences) {
        *self
            .shared
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(preferences.normalized());
        self.shared.wake.notify_one();
    }

    pub fn take_error(&self) -> Option<String> {
        self.shared
            .latest_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
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
            preferences: preferences.normalized(),
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

        let normalized = preferences.clone().normalized();
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

        if let Some(preferences) = next {
            if let Err(error) = store.save(&preferences) {
                *shared
                    .latest_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.to_string());
            }
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
            reduced_motion: true,
            ui_scale: 99.0,
            external_editor: None,
        };

        let saved = store.save(&preferences).unwrap();
        assert_eq!(saved.schema_version, PREFERENCES_SCHEMA_VERSION);
        assert_eq!(saved.window.x, -32_768);
        assert_eq!(saved.window.y, 32_767);
        assert_eq!(saved.window.width, 640);
        assert_eq!(saved.window.height, 4_320);
        assert_eq!(saved.ui_scale, 2.0);

        let loaded = store.load().unwrap();
        assert_eq!(loaded.preferences, saved);
        assert_eq!(loaded.recovery, None);
    }

    #[test]
    fn external_editor_preferences_round_trip_as_program_and_literal_argv() {
        let temp = tempfile::tempdir().unwrap();
        let store = PreferenceStore::new(temp.path());
        let mut preferences = DesktopPreferences {
            external_editor: Some(DesktopExternalEditorConfig {
                program: "code".into(),
                args: vec!["--reuse-window".into(), "literal;argument".into()],
            }),
            ..DesktopPreferences::default()
        };

        let saved = store.save(&preferences).unwrap();
        assert_eq!(saved.external_editor, preferences.external_editor);
        assert_eq!(store.load().unwrap().preferences, saved);

        preferences.external_editor = Some(DesktopExternalEditorConfig {
            program: "/bin/sh".into(),
            args: vec!["-c".into()],
        });
        assert!(preferences.normalized().external_editor.is_none());
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
        for width in 640..700 {
            let mut preferences = DesktopPreferences::default();
            preferences.window.width = width;
            writer.schedule(preferences);
        }

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
        assert_eq!(writer.take_error(), None);
    }
}
