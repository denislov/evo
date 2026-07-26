//! Theme hot reload — ported from `startThemeWatcher` / `onThemeChange` in
//! `theme.ts`.
//!
//! A [`ThemeWatcher`] watches the custom-themes directory for changes to the
//! currently active theme file (`<name>.json`), coalesces rapid edits via a
//! debounce window, reparses the file on a worker thread, and emits a
//! [`ThemeReloadSignal`] through a `tokio` channel. Built-in `dark`/`light`
//! themes are never watched, matching TS `startThemeWatcher`.
//!
//! Stop is cooperative: a shared `AtomicBool` flag and condition variable let
//! the debounce worker wake promptly when the watcher is dropped. We can't rely
//! on the channel closing because the worker itself holds a sender clone.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use super::ThemeJson;
use crate::{bounded_io, limits};

/// A reloaded theme ready to apply: the parsed [`ThemeJson`] and its name.
#[derive(Debug, Clone)]
pub struct ThemeReloadSignal {
    pub name: String,
    pub theme: ThemeJson,
}

/// Decide whether a theme name should be watched. Returns the `<name>.json`
/// watch target for custom themes, or `None` for built-in `dark`/`light`
/// (mirrors the early return in TS `startThemeWatcher`).
pub fn should_watch_target(name: &str) -> Option<PathBuf> {
    match name {
        "dark" | "light" => None,
        other => Some(PathBuf::from(format!("{other}.json"))),
    }
}

/// A filesystem theme watcher. Dropping it stops watching and joins the
/// debounce worker thread.
pub struct ThemeWatcher {
    watcher: Option<RecommendedWatcher>,
    debounce_handle: Option<std::thread::JoinHandle<()>>,
    pending: Arc<(Mutex<DebounceState>, Condvar)>,
    stop_flag: Arc<AtomicBool>,
}

#[derive(Default)]
struct DebounceState {
    deadline: Option<Instant>,
}

impl DebounceState {
    fn schedule(&mut self, now: Instant, debounce: Duration) {
        self.deadline = Some(now + debounce);
    }

    fn clear(&mut self) {
        self.deadline = None;
    }
}

impl ThemeWatcher {
    /// Start watching `themes_dir` for changes to `<name>.json`.
    ///
    /// `debounce` is the coalescing window (TS uses 100ms). Reload signals are
    /// delivered on the returned receiver. For a built-in theme no watcher is
    /// started and the receiver stays empty, so callers can treat the result
    /// uniformly.
    pub fn start(
        themes_dir: PathBuf,
        name: String,
        debounce: Duration,
    ) -> std::io::Result<(Self, mpsc::Receiver<ThemeReloadSignal>)> {
        let (tx, rx) = mpsc::channel(1);

        let stop_flag = Arc::new(AtomicBool::new(false));
        let pending: Arc<(Mutex<DebounceState>, Condvar)> =
            Arc::new((Mutex::new(DebounceState::default()), Condvar::new()));

        // Built-in themes are not watched.
        let target = match should_watch_target(&name) {
            Some(target) => target,
            None => {
                return Ok((
                    Self {
                        watcher: None,
                        debounce_handle: None,
                        pending,
                        stop_flag,
                    },
                    rx,
                ));
            }
        };

        let stop_flag_for_debouncer = stop_flag.clone();
        let pending_for_debouncer = pending.clone();

        // Debounce worker: waits until the scheduled deadline, reloads, and
        // exits once `stop_flag` is set.
        let name_for_debouncer = name.clone();
        let dir_for_debouncer = themes_dir.clone();
        let target_for_debouncer = target.clone();
        let debounce_handle = std::thread::spawn(move || {
            debounce_loop(
                stop_flag_for_debouncer,
                pending_for_debouncer,
                dir_for_debouncer,
                target_for_debouncer,
                name_for_debouncer,
                tx,
            );
        });

        // Filesystem watcher: on a relevant event, schedule a reload at
        // now + debounce. Runs on notify's internal callback thread.
        let pending_for_watcher = pending.clone();
        let target_for_watcher = target.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else {
                return;
            };
            if !is_relevant_event(&event, &target_for_watcher) {
                return;
            }
            let (pending_lock, pending_changed) = &*pending_for_watcher;
            if let Ok(mut p) = pending_lock.lock() {
                p.schedule(Instant::now(), debounce);
                pending_changed.notify_one();
            }
        })
        .map_err(std::io::Error::other)?;

        let mut watcher = watcher;
        // Watch the directory (inotify watches files via the dir on Linux).
        if themes_dir.exists() {
            let _ = watcher.watch(&themes_dir, RecursiveMode::NonRecursive);
        }

        Ok((
            Self {
                watcher: Some(watcher),
                debounce_handle: Some(debounce_handle),
                pending,
                stop_flag,
            },
            rx,
        ))
    }

    /// Cancel any pending reload, drop the fs watcher, and join the debounce
    /// worker. The condition variable wakes the worker so this returns promptly.
    fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        let (pending_lock, pending_changed) = &*self.pending;
        if let Ok(mut p) = pending_lock.lock() {
            p.clear();
        }
        pending_changed.notify_all();
        // Drop the fs watcher first so no new events are scheduled.
        self.watcher = None;
        if let Some(handle) = self.debounce_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ThemeWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Whether an fs event should trigger a reload: it must touch the watched
/// theme file and be a content change (create/modify/remove), not a
/// metadata-only event.
fn is_relevant_event(event: &notify::Event, target: &Path) -> bool {
    let relevant_kind = matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    );
    if !relevant_kind {
        return false;
    }
    event
        .paths
        .iter()
        .any(|p| p.file_name().is_some_and(|n| n == target))
}

/// Debounce loop body. Waits for the shared deadline; when due, reparses the
/// theme file and sends a [`ThemeReloadSignal`]. Exits when `stop_flag` is set
/// or the receiver is dropped.
#[allow(clippy::too_many_arguments)]
fn debounce_loop(
    stop_flag: Arc<AtomicBool>,
    pending: Arc<(Mutex<DebounceState>, Condvar)>,
    themes_dir: PathBuf,
    target: PathBuf,
    name: String,
    tx: mpsc::Sender<ThemeReloadSignal>,
) {
    let (pending_lock, pending_changed) = &*pending;
    loop {
        let mut pending_guard = match pending_lock.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        loop {
            if stop_flag.load(Ordering::Relaxed) {
                return;
            }
            let now = Instant::now();
            match pending_guard.deadline {
                Some(deadline) if now >= deadline => {
                    pending_guard.deadline = None;
                    break;
                }
                Some(deadline) => {
                    let wait = deadline.saturating_duration_since(now);
                    let Ok((guard, _)) = pending_changed.wait_timeout(pending_guard, wait) else {
                        return;
                    };
                    pending_guard = guard;
                }
                None => {
                    let Ok(guard) = pending_changed.wait(pending_guard) else {
                        return;
                    };
                    pending_guard = guard;
                }
            }
        }
        drop(pending_guard);

        // Reload from disk; ignore transient missing/invalid files
        // (TS keeps the last good theme and ignores parse errors).
        let theme_file = themes_dir.join(&target);
        if let Ok(content) = bounded_io::read_text(&theme_file, limits::MAX_THEME_FILE_BYTES)
            && let Ok(theme) = serde_json::from_str::<ThemeJson>(&content)
        {
            let display_name = if theme.name.is_empty() {
                name.clone()
            } else {
                theme.name.clone()
            };
            if tx
                .blocking_send(ThemeReloadSignal {
                    name: display_name,
                    theme,
                })
                .is_err()
            {
                return; // receiver dropped -> stop
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEME_RELOAD_TEST_DEBOUNCE: Duration = Duration::from_millis(80);
    const THEME_RELOAD_TEST_RAPID_OFFSET: Duration = Duration::from_millis(20);
    const THEME_RELOAD_TEST_EXTENDED_DEADLINE: Duration = Duration::from_millis(100);

    fn theme_reload_test_clock_anchor() -> Instant {
        Instant::now()
    }

    #[test]
    fn debounce_state_extends_deadline_for_rapid_events() {
        let start = theme_reload_test_clock_anchor();
        let mut state = DebounceState::default();

        state.schedule(start, THEME_RELOAD_TEST_DEBOUNCE);
        state.schedule(
            start + THEME_RELOAD_TEST_RAPID_OFFSET,
            THEME_RELOAD_TEST_DEBOUNCE,
        );

        assert_eq!(
            state.deadline,
            Some(start + THEME_RELOAD_TEST_EXTENDED_DEADLINE)
        );
        state.clear();
        assert_eq!(state.deadline, None);
    }
}
