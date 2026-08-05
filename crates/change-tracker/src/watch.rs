//! Single-owner filesystem event service.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ignore::Match;
use ignore::gitignore::Gitignore;
use notify::event::{ModifyKind, RenameMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use workspace_runtime::api::WorkspaceHandle;

use crate::error::ChangeTrackerError;
use crate::event::{FsChangeKind, FsEvent, GitMetaEvent, SemanticEvent};
use crate::git;

/// Tunables for one event service. Defaults suit interactive workspaces; the
/// debounce window trades latency against burst merging.
#[derive(Debug, Clone)]
pub struct WatchOptions {
    /// How long the worker waits for a quiet window before flushing merged
    /// events. Bursts within one window collapse to their final state.
    pub debounce: Duration,
    /// Maximum number of watch roots per service.
    pub max_roots: usize,
    /// Broadcast capacity. Slow consumers miss events beyond this window;
    /// the service itself never blocks on a consumer.
    pub event_queue: usize,
    /// Normalize git metadata changes (`HEAD`, `index`, refs, locks) into
    /// `FsEvent::Git`.
    pub git_meta: bool,
    /// Drop paths matched by the root's `.gitignore`.
    pub ignore_gitignored: bool,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(40),
            max_roots: 1,
            event_queue: 1024,
            git_meta: true,
            ignore_gitignored: true,
        }
    }
}

struct RootState {
    gitignore: Option<Gitignore>,
    gitdir: Option<PathBuf>,
}

type CommandSender = Arc<Mutex<SyncSender<Incoming>>>;

/// Everything the service needs to talk to and stop its worker.
struct WorkerHandle {
    command: CommandSender,
    thread: JoinHandle<()>,
    ready: Receiver<Result<(), String>>,
}

enum Incoming {
    Event(notify::Event),
    AddRoot {
        root: PathBuf,
        reply: SyncSender<Result<(), String>>,
    },
}

/// One raw change classified into a semantic role. Rename fragments carry the
/// backend tracker id so fragments of the same move can be paired.
enum Change {
    Create(Option<usize>),
    Remove(Option<usize>),
    Modify,
    RenameFrom(Option<usize>),
    RenameTo(Option<usize>),
}

/// Single-owner filesystem event service over one or more workspace roots.
///
/// Raw `notify` events are normalized on a dedicated worker thread: paths
/// become workspace-relative, rename fragments pair into one `Renamed` event,
/// bursts are debounced, gitignored paths are dropped, and `.git` changes are
/// re-emitted as `GitMetaEvent`. Consumers receive only `FsEvent` values and
/// never depend on `notify` types.
pub struct FsEventService {
    command: CommandSender,
    sender: broadcast::Sender<FsEvent>,
    handles: Mutex<Vec<PathBuf>>,
    shutdown: CancellationToken,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl FsEventService {
    /// Start watching `handle.root()` on a background worker.
    pub fn start(
        handle: &WorkspaceHandle,
        options: WatchOptions,
    ) -> Result<Self, ChangeTrackerError> {
        let root = std::fs::canonicalize(handle.root()).map_err(|error| {
            ChangeTrackerError::InvalidRoot {
                message: format!("cannot resolve {}: {error}", handle.root().display()),
            }
        })?;
        let (sender, _) = broadcast::channel(options.event_queue);
        let shutdown = CancellationToken::new();
        let worker = spawn_worker(root.clone(), &options, sender.clone(), shutdown.clone())?;
        worker
            .ready
            .recv()
            .map_err(|_| ChangeTrackerError::Shutdown)?
            .map_err(|message| ChangeTrackerError::WatchFailed { message })?;
        Ok(Self {
            command: worker.command,
            sender,
            handles: Mutex::new(vec![root]),
            shutdown,
            thread: Mutex::new(Some(worker.thread)),
        })
    }

    /// Subscribe to the normalized change stream.
    pub fn events(&self) -> broadcast::Receiver<FsEvent> {
        self.sender.subscribe()
    }

    /// The first watched workspace root.
    pub fn root(&self) -> PathBuf {
        self.handles
            .lock()
            .expect("root list")
            .first()
            .cloned()
            .expect("service always watches at least one root")
    }

    /// Extend the service to an additional workspace root, reusing the same
    /// watcher. Fails closed when the root budget is exhausted.
    pub fn add_root(&self, handle: &WorkspaceHandle) -> Result<(), ChangeTrackerError> {
        let root = std::fs::canonicalize(handle.root()).map_err(|error| {
            ChangeTrackerError::InvalidRoot {
                message: format!("cannot resolve {}: {error}", handle.root().display()),
            }
        })?;
        let (reply_tx, reply_rx) = sync_channel::<Result<(), String>>(1);
        self.command
            .lock()
            .expect("command channel")
            .try_send(Incoming::AddRoot {
                root: root.clone(),
                reply: reply_tx,
            })
            .map_err(|_| ChangeTrackerError::Shutdown)?;
        reply_rx
            .recv()
            .map_err(|_| ChangeTrackerError::Shutdown)?
            .map_err(|message| ChangeTrackerError::WatchFailed { message })?;
        self.handles.lock().expect("root list").push(root);
        Ok(())
    }

    /// Cancel the worker and join it. Idempotent; `Drop` calls this too.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
        if let Some(thread) = self.thread.lock().expect("worker thread").take() {
            let _ = thread.join();
        }
    }
}

impl Drop for FsEventService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn spawn_worker(
    first_root: PathBuf,
    options: &WatchOptions,
    sender: broadcast::Sender<FsEvent>,
    shutdown: CancellationToken,
) -> Result<WorkerHandle, ChangeTrackerError> {
    let (command_tx, command_rx) = sync_channel::<Incoming>(options.event_queue);
    let (ready_tx, ready_rx) = sync_channel::<Result<(), String>>(1);
    let command: CommandSender = Arc::new(Mutex::new(command_tx));
    let mut worker = Worker {
        command: Arc::clone(&command),
        command_rx,
        sender,
        shutdown,
        options: options.clone(),
        states: HashMap::new(),
        pending: BTreeMap::new(),
        pending_renames: BTreeMap::new(),
        rename_from: HashMap::new(),
        rename_to: HashMap::new(),
        lost: Arc::new(AtomicU64::new(0)),
        sequence: 0,
        watcher: None,
    };
    let handle = std::thread::Builder::new()
        .name("change-tracker".into())
        .spawn(move || worker.start(first_root, ready_tx))
        .map_err(|error| ChangeTrackerError::Io {
            message: format!("cannot spawn change-tracker worker: {error}"),
        })?;
    Ok(WorkerHandle {
        command,
        thread: handle,
        ready: ready_rx,
    })
}

struct Worker {
    command: CommandSender,
    command_rx: Receiver<Incoming>,
    sender: broadcast::Sender<FsEvent>,
    shutdown: CancellationToken,
    options: WatchOptions,
    states: HashMap<PathBuf, RootState>,
    pending: BTreeMap<(PathBuf, PathBuf), FsChangeKind>,
    pending_renames: BTreeMap<(PathBuf, PathBuf), PathBuf>,
    rename_from: HashMap<usize, (PathBuf, PathBuf)>,
    rename_to: HashMap<usize, (PathBuf, PathBuf)>,
    lost: Arc<AtomicU64>,
    sequence: u64,
    watcher: Option<RecommendedWatcher>,
}

impl Worker {
    fn start(&mut self, first_root: PathBuf, ready: SyncSender<Result<(), String>>) {
        let result = self.install_watcher(first_root);
        let _ = ready.send(result.as_ref().map_err(Clone::clone).copied());
        if result.is_err() {
            return;
        }
        while self.step() {}
        self.flush();
    }

    fn install_watcher(&mut self, first_root: PathBuf) -> Result<(), String> {
        let lost = Arc::clone(&self.lost);
        let command = Arc::clone(&self.command);
        let mut watcher = RecommendedWatcher::new(
            move |result: notify::Result<notify::Event>| match result {
                Ok(event) => {
                    if command
                        .lock()
                        .expect("command channel")
                        .try_send(Incoming::Event(event))
                        .is_err()
                    {
                        lost.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(_) => {
                    lost.fetch_add(1, Ordering::Relaxed);
                }
            },
            notify::Config::default(),
        )
        .map_err(|error| format!("cannot create watcher: {error}"))?;
        watcher
            .watch(&first_root, RecursiveMode::Recursive)
            .map_err(|error| format!("cannot watch {}: {error}", first_root.display()))?;
        self.watcher = Some(watcher);
        self.install_root(first_root)?;
        Ok(())
    }

    fn step(&mut self) -> bool {
        if self.shutdown.is_cancelled() && self.pending.is_empty() {
            return false;
        }
        match self.command_rx.recv_timeout(self.options.debounce) {
            Ok(Incoming::Event(event)) => self.handle_event(event),
            Ok(Incoming::AddRoot { root, reply }) => {
                let result = self.add_root(root).map_err(|error| error.to_string());
                let _ = reply.send(result);
            }
            Err(RecvTimeoutError::Timeout) => self.flush(),
            Err(RecvTimeoutError::Disconnected) => return false,
        }
        true
    }

    fn install_root(&mut self, root: PathBuf) -> Result<(), String> {
        let gitignore = if self.options.ignore_gitignored {
            let path = root.join(".gitignore");
            if path.is_file() {
                let (ignore, error) = Gitignore::new(&path);
                if let Some(error) = error {
                    return Err(format!("cannot parse {}: {error}", path.display()));
                }
                Some(ignore)
            } else {
                None
            }
        } else {
            None
        };
        let gitdir = if self.options.git_meta {
            resolve_gitdir(&root)
        } else {
            None
        };
        if let Some(gitdir) = &gitdir
            && !gitdir.starts_with(&root)
        {
            self.watch(gitdir)?;
        }
        self.states
            .insert(root.clone(), RootState { gitignore, gitdir });
        Ok(())
    }

    fn add_root(&mut self, root: PathBuf) -> Result<(), String> {
        if self.states.contains_key(&root) {
            return Ok(());
        }
        if self.states.len() >= self.options.max_roots {
            return Err(format!(
                "watch root budget exceeded (limit {})",
                self.options.max_roots
            ));
        }
        self.watch(&root)?;
        self.install_root(root)
    }

    fn watch(&mut self, path: &Path) -> Result<(), String> {
        self.watcher
            .as_mut()
            .expect("worker owns its watcher")
            .watch(path, RecursiveMode::Recursive)
            .map_err(|error| format!("cannot watch {}: {error}", path.display()))
    }

    fn handle_event(&mut self, event: notify::Event) {
        if let EventKind::Modify(ModifyKind::Name(RenameMode::Both)) = event.kind
            && let [from, to] = event.paths.as_slice()
        {
            self.handle_both(from, to);
            return;
        }
        let Some(change) = classify_change(&event) else {
            return;
        };
        let paths = event.paths.clone();
        for path in &paths {
            if self.inside_gitdir(path) {
                if let Some(meta) = self.classify_git_path(path, &event.kind) {
                    let _ = self.sender.send(FsEvent::Git(meta));
                }
                continue;
            }
            let Some((root, rel)) = self.locate(path) else {
                continue;
            };
            if self.ignored(&root, &rel) {
                continue;
            }
            match change {
                Change::Create(tracker) => self.handle_new_path(&root, rel, tracker),
                Change::Remove(tracker) => self.handle_old_path(&root, rel, tracker),
                Change::RenameTo(tracker) => self.handle_new_path(&root, rel, tracker),
                Change::RenameFrom(tracker) => self.handle_old_path(&root, rel, tracker),
                Change::Modify => {
                    merge_pending(&mut self.pending, (root, rel), FsChangeKind::Modified);
                }
            }
        }
    }

    /// A path appeared or received the destination side of a rename.
    fn handle_new_path(&mut self, root: &Path, rel: PathBuf, tracker: Option<usize>) {
        if let Some(tracker) = tracker
            && let Some((from_root, from_rel)) = self.rename_from.remove(&tracker)
        {
            self.record_renamed(&from_root, from_rel, root, rel);
            return;
        }
        if let Some(tracker) = tracker {
            self.rename_to
                .insert(tracker, (root.to_path_buf(), rel.clone()));
        }
        merge_pending(
            &mut self.pending,
            (root.to_path_buf(), rel),
            FsChangeKind::Created,
        );
    }

    /// A path disappeared or received the source side of a rename.
    fn handle_old_path(&mut self, root: &Path, rel: PathBuf, tracker: Option<usize>) {
        if let Some(tracker) = tracker
            && let Some((to_root, to_rel)) = self.rename_to.remove(&tracker)
        {
            self.record_renamed(root, rel, &to_root, to_rel);
            return;
        }
        if let Some(tracker) = tracker {
            self.rename_from
                .insert(tracker, (root.to_path_buf(), rel.clone()));
        }
        merge_pending(
            &mut self.pending,
            (root.to_path_buf(), rel),
            FsChangeKind::Removed,
        );
    }

    fn handle_both(&mut self, from: &Path, to: &Path) {
        let Some((from_root, from_rel)) = self.locate(from) else {
            return;
        };
        let Some((to_root, to_rel)) = self.locate(to) else {
            return;
        };
        self.record_renamed(&from_root, from_rel, &to_root, to_rel);
    }

    fn record_renamed(
        &mut self,
        from_root: &Path,
        from_rel: PathBuf,
        to_root: &Path,
        to_rel: PathBuf,
    ) {
        self.pending
            .remove(&(from_root.to_path_buf(), from_rel.clone()));
        self.pending.insert(
            (to_root.to_path_buf(), to_rel.clone()),
            FsChangeKind::Renamed,
        );
        self.pending_renames
            .insert((to_root.to_path_buf(), to_rel.clone()), from_rel);
    }

    fn flush(&mut self) {
        let lost = self.lost.swap(0, Ordering::Relaxed);
        if lost > 0 {
            let _ = self.sender.send(FsEvent::WatchGap { lost });
        }
        let pending = std::mem::take(&mut self.pending);
        for ((root, rel), kind) in pending {
            let from = if kind == FsChangeKind::Renamed {
                self.pending_renames.remove(&(root.clone(), rel.clone()))
            } else {
                None
            };
            self.sequence += 1;
            let _ = self.sender.send(FsEvent::Workspace(SemanticEvent::new(
                self.sequence,
                &root,
                rel,
                from,
                kind,
            )));
        }
        self.rename_from.clear();
        self.rename_to.clear();
        self.pending_renames.clear();
    }

    fn inside_gitdir(&self, path: &Path) -> bool {
        self.states.values().any(|state| {
            state
                .gitdir
                .as_ref()
                .is_some_and(|gitdir| path.starts_with(gitdir))
        })
    }

    fn classify_git_path(&self, path: &Path, kind: &notify::EventKind) -> Option<GitMetaEvent> {
        for state in self.states.values() {
            let gitdir = state.gitdir.as_ref()?;
            if let Ok(rel) = path.strip_prefix(gitdir)
                && !rel.as_os_str().is_empty()
            {
                return git::classify(rel, kind);
            }
        }
        None
    }

    fn locate(&self, path: &Path) -> Option<(PathBuf, PathBuf)> {
        for root in self.states.keys() {
            if let Ok(rel) = path.strip_prefix(root)
                && !rel.as_os_str().is_empty()
            {
                return Some((root.clone(), rel.to_path_buf()));
            }
        }
        None
    }

    fn ignored(&self, root: &Path, rel: &Path) -> bool {
        let Some(state) = self.states.get(root) else {
            return false;
        };
        let Some(ignore) = state.gitignore.as_ref() else {
            return false;
        };
        matches!(
            ignore.matched_path_or_any_parents(rel, false),
            Match::Ignore(_)
        )
    }
}

/// Merge a new change into the debounce window. Creation survives later
/// modification within the same window; removal followed by creation wins
/// with `Created`; everything else collapses to the latest change.
fn merge_pending(
    pending: &mut BTreeMap<(PathBuf, PathBuf), FsChangeKind>,
    key: (PathBuf, PathBuf),
    incoming: FsChangeKind,
) {
    let kind = match (pending.get(&key).copied(), incoming) {
        (Some(FsChangeKind::Created), FsChangeKind::Modified) => FsChangeKind::Created,
        (Some(FsChangeKind::Removed), FsChangeKind::Created) => FsChangeKind::Created,
        (_, incoming) => incoming,
    };
    pending.insert(key, kind);
}

fn classify_change(event: &notify::Event) -> Option<Change> {
    use notify::event::{AccessKind, CreateKind, RemoveKind};
    match event.kind {
        EventKind::Create(CreateKind::Any | CreateKind::File | CreateKind::Folder) => {
            Some(Change::Create(event.attrs.tracker()))
        }
        EventKind::Remove(RemoveKind::Any | RemoveKind::File | RemoveKind::Folder) => {
            Some(Change::Remove(event.attrs.tracker()))
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
            Some(Change::RenameFrom(event.attrs.tracker()))
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
            Some(Change::RenameTo(event.attrs.tracker()))
        }
        EventKind::Modify(
            ModifyKind::Any | ModifyKind::Data(_) | ModifyKind::Metadata(_) | ModifyKind::Name(_),
        ) => Some(Change::Modify),
        EventKind::Access(AccessKind::Any | AccessKind::Close(_) | AccessKind::Open(_)) => None,
        _ => None,
    }
}

fn resolve_gitdir(root: &Path) -> Option<PathBuf> {
    let git = root.join(".git");
    if git.is_dir() {
        return Some(git);
    }
    if !git.is_file() {
        return None;
    }
    let contents = std::fs::read_to_string(&git).ok()?;
    let line = contents.lines().next()?.strip_prefix("gitdir:")?.trim();
    let path = Path::new(line);
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        Some(root.join(path))
    }
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod watch_tests;
