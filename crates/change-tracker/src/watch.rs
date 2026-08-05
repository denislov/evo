//! Single-owner filesystem event service.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ignore::Match;
use ignore::gitignore::Gitignore;
use notify::event::{ModifyKind, RenameMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use workspace_runtime::api::WorkspaceHandle;

use crate::error::ChangeTrackerError;
use crate::event::{FsChangeKind, FsEvent, GitEvent, GitMetaEvent, SemanticEvent};
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
    Shutdown,
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
/// re-emitted as root-associated `GitEvent` values. Consumers receive only `FsEvent` values and
/// never depend on `notify` types.
pub struct FsEventService {
    command: CommandSender,
    sender: broadcast::Sender<FsEvent>,
    initial_receiver: Mutex<Option<broadcast::Receiver<FsEvent>>>,
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
        validate_options(&options)?;
        let root = std::fs::canonicalize(handle.root()).map_err(|error| {
            ChangeTrackerError::InvalidRoot {
                message: format!("cannot resolve {}: {error}", handle.root().display()),
            }
        })?;
        let (sender, initial_receiver) = broadcast::channel(options.event_queue);
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
            initial_receiver: Mutex::new(Some(initial_receiver)),
            handles: Mutex::new(vec![root]),
            shutdown,
            thread: Mutex::new(Some(worker.thread)),
        })
    }

    /// Subscribe to the normalized change stream.
    pub fn events(&self) -> broadcast::Receiver<FsEvent> {
        self.initial_receiver
            .lock()
            .expect("initial event receiver")
            .take()
            .unwrap_or_else(|| self.sender.subscribe())
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
        let result = self
            .command
            .lock()
            .expect("command channel")
            .try_send(Incoming::AddRoot {
                root: root.clone(),
                reply: reply_tx,
            });
        match result {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(ChangeTrackerError::WatchFailed {
                    message: "change-tracker command queue is saturated".into(),
                });
            }
            Err(TrySendError::Disconnected(_)) => return Err(ChangeTrackerError::Shutdown),
        }
        reply_rx
            .recv()
            .map_err(|_| ChangeTrackerError::Shutdown)?
            .map_err(|message| ChangeTrackerError::WatchFailed { message })?;
        let mut handles = self.handles.lock().expect("root list");
        if !handles.iter().any(|existing| existing == &root) {
            handles.push(root);
        }
        Ok(())
    }

    /// Cancel the worker and join it. Idempotent; `Drop` calls this too.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
        let result = self
            .command
            .lock()
            .expect("command channel")
            .send(Incoming::Shutdown);
        let _ = result;
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
        pending_directories: BTreeMap::new(),
        pending_renames: BTreeMap::new(),
        rename_from: HashMap::new(),
        rename_to: HashMap::new(),
        lost: Arc::new(AtomicU64::new(0)),
        sequence: 0,
        flush_deadline: None,
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
    pending_directories: BTreeMap<(PathBuf, PathBuf), bool>,
    pending_renames: BTreeMap<(PathBuf, PathBuf), PathBuf>,
    rename_from: HashMap<usize, (PathBuf, PathBuf)>,
    rename_to: HashMap<usize, (PathBuf, PathBuf)>,
    lost: Arc<AtomicU64>,
    sequence: u64,
    flush_deadline: Option<Instant>,
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
            move |result| forward_raw_result(&command, &lost, result),
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
        if self.shutdown.is_cancelled() {
            self.flush();
            return false;
        }
        if self
            .flush_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.flush();
        }
        let timeout = self
            .flush_deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(self.options.debounce);
        match self.command_rx.recv_timeout(timeout) {
            Ok(Incoming::Event(event)) => self.handle_event(event),
            Ok(Incoming::AddRoot { root, reply }) => {
                let result = self.add_root(root).map_err(|error| error.to_string());
                let _ = reply.send(result);
            }
            Ok(Incoming::Shutdown) => {
                self.flush();
                return false;
            }
            Err(RecvTimeoutError::Timeout) => self.flush(),
            Err(RecvTimeoutError::Disconnected) => return false,
        }
        if self.flush_deadline.is_none() && self.has_buffered_work() {
            self.flush_deadline = Some(Instant::now() + self.options.debounce);
        }
        true
    }

    fn has_buffered_work(&self) -> bool {
        !self.pending.is_empty()
            || !self.rename_from.is_empty()
            || !self.rename_to.is_empty()
            || self.lost.load(Ordering::Relaxed) > 0
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
        if let Err(error) = self.install_root(root.clone()) {
            if let Some(watcher) = self.watcher.as_mut() {
                let _ = watcher.unwatch(&root);
            }
            return Err(error);
        }
        Ok(())
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
        if matches!(
            event.kind,
            EventKind::Modify(ModifyKind::Name(
                RenameMode::Any | RenameMode::Both | RenameMode::Other
            ))
        ) {
            self.lost.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let Some(change) = classify_change(&event) else {
            return;
        };
        let paths = event.paths.clone();
        for path in &paths {
            if let Some((root, rel)) = self.git_path(path) {
                if let Some(kind) = git::classify(&rel, &event.kind) {
                    self.emit_git(&root, kind);
                }
                continue;
            }
            let is_directory = path_is_directory(&event.kind, path);
            let Some((root, rel)) = self.workspace_path(path, is_directory) else {
                continue;
            };
            match change {
                Change::Create(tracker) => {
                    self.handle_new_path(&root, rel, tracker, is_directory);
                    if is_directory == Some(true) {
                        self.record_created_tree(path);
                    }
                }
                Change::Remove(tracker) => self.handle_old_path(&root, rel, tracker, is_directory),
                Change::RenameTo(tracker) => {
                    self.handle_new_path(&root, rel, tracker, is_directory)
                }
                Change::RenameFrom(tracker) => {
                    self.handle_old_path(&root, rel, tracker, is_directory)
                }
                Change::Modify => {
                    self.record_pending(root, rel, FsChangeKind::Modified, is_directory);
                }
            }
        }
    }

    /// Recursive backends can report a new directory before they finish
    /// installing watches below it. Scan the just-created subtree once to
    /// close that race; later backend events merge into the same pending map.
    fn record_created_tree(&mut self, directory: &Path) {
        for entry in ignore::WalkBuilder::new(directory)
            .standard_filters(false)
            .follow_links(false)
            .build()
            .skip(1)
        {
            if self.shutdown.is_cancelled() {
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    self.lost.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };
            let is_directory = entry.file_type().is_some_and(|kind| kind.is_dir());
            if let Some((root, rel)) = self.workspace_path(entry.path(), Some(is_directory)) {
                self.handle_new_path(&root, rel, None, Some(is_directory));
            }
        }
    }

    /// A path appeared or received the destination side of a rename.
    fn handle_new_path(
        &mut self,
        root: &Path,
        rel: PathBuf,
        tracker: Option<usize>,
        is_directory: Option<bool>,
    ) {
        if let Some(tracker) = tracker
            && let Some((from_root, from_rel)) = self.rename_from.remove(&tracker)
        {
            self.record_renamed(&from_root, from_rel, root, rel, is_directory);
            return;
        }
        if let Some(tracker) = tracker {
            self.rename_to
                .insert(tracker, (root.to_path_buf(), rel.clone()));
        }
        self.record_pending(root.to_path_buf(), rel, FsChangeKind::Created, is_directory);
    }

    /// A path disappeared or received the source side of a rename.
    fn handle_old_path(
        &mut self,
        root: &Path,
        rel: PathBuf,
        tracker: Option<usize>,
        is_directory: Option<bool>,
    ) {
        if let Some(tracker) = tracker
            && let Some((to_root, to_rel)) = self.rename_to.remove(&tracker)
        {
            self.record_renamed(root, rel, &to_root, to_rel, is_directory);
            return;
        }
        if let Some(tracker) = tracker {
            self.rename_from
                .insert(tracker, (root.to_path_buf(), rel.clone()));
        }
        self.record_pending(root.to_path_buf(), rel, FsChangeKind::Removed, is_directory);
    }

    fn handle_both(&mut self, from: &Path, to: &Path) {
        use notify::event::{CreateKind, RemoveKind};

        let is_directory = to.is_dir();
        let from_git = self.git_path(from);
        let to_git = self.git_path(to);
        if let Some((root, rel)) = &from_git
            && let Some(kind) = git::classify(rel, &EventKind::Remove(RemoveKind::Any))
        {
            self.emit_git(root, kind);
        }
        if let Some((root, rel)) = &to_git
            && let Some(kind) = git::classify(rel, &EventKind::Create(CreateKind::Any))
        {
            self.emit_git(root, kind);
        }

        let from_workspace = from_git
            .is_none()
            .then(|| self.workspace_path(from, Some(is_directory)))
            .flatten();
        let to_workspace = to_git
            .is_none()
            .then(|| self.workspace_path(to, Some(is_directory)))
            .flatten();
        match (from_workspace, to_workspace) {
            (Some((from_root, from_rel)), Some((to_root, to_rel))) if from_root == to_root => {
                self.record_renamed(&from_root, from_rel, &to_root, to_rel, Some(is_directory));
            }
            (Some((from_root, from_rel)), Some((to_root, to_rel))) => {
                self.record_pending(
                    from_root,
                    from_rel,
                    FsChangeKind::Removed,
                    Some(is_directory),
                );
                self.record_pending(to_root, to_rel, FsChangeKind::Created, Some(is_directory));
            }
            (Some((root, rel)), None) => {
                self.record_pending(root, rel, FsChangeKind::Removed, Some(is_directory));
            }
            (None, Some((root, rel))) => {
                self.record_pending(root, rel, FsChangeKind::Created, Some(is_directory));
            }
            (None, None) => {}
        }
    }

    fn record_renamed(
        &mut self,
        from_root: &Path,
        from_rel: PathBuf,
        to_root: &Path,
        to_rel: PathBuf,
        is_directory: Option<bool>,
    ) {
        if from_root != to_root {
            self.record_pending(
                from_root.to_path_buf(),
                from_rel,
                FsChangeKind::Removed,
                is_directory,
            );
            self.record_pending(
                to_root.to_path_buf(),
                to_rel,
                FsChangeKind::Created,
                is_directory,
            );
            return;
        }
        let source_key = (from_root.to_path_buf(), from_rel.clone());
        let source_kind = self.pending.remove(&source_key);
        let source_directory = self.pending_directories.remove(&source_key);
        let original_from = if source_kind == Some(FsChangeKind::Renamed) {
            self.pending_renames.remove(&source_key).unwrap_or(from_rel)
        } else {
            from_rel
        };
        let destination_key = (to_root.to_path_buf(), to_rel.clone());
        let destination_directory = self.pending_directories.remove(&destination_key);
        let is_directory = is_directory
            .or(destination_directory)
            .or(source_directory)
            .unwrap_or(false);
        self.pending_renames.remove(&destination_key);
        if source_kind == Some(FsChangeKind::Created) {
            self.pending.insert(destination_key, FsChangeKind::Created);
            self.pending_directories
                .insert((to_root.to_path_buf(), to_rel), is_directory);
            return;
        }
        self.pending
            .insert(destination_key.clone(), FsChangeKind::Renamed);
        self.pending_directories
            .insert(destination_key.clone(), is_directory);
        self.pending_renames.insert(destination_key, original_from);
    }

    fn record_pending(
        &mut self,
        root: PathBuf,
        path: PathBuf,
        kind: FsChangeKind,
        is_directory: Option<bool>,
    ) {
        let key = (root, path);
        merge_pending(&mut self.pending, key.clone(), kind);
        if let Some(is_directory) = is_directory {
            self.pending_directories.insert(key, is_directory);
        }
    }

    fn flush(&mut self) {
        self.flush_deadline = None;
        if let Some(gap) = take_watch_gap(&self.lost) {
            let _ = self.sender.send(gap);
        }
        let pending = std::mem::take(&mut self.pending);
        for ((root, rel), kind) in pending {
            let is_directory = self
                .pending_directories
                .remove(&(root.clone(), rel.clone()))
                .unwrap_or(false);
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
                is_directory,
                from,
                kind,
            )));
        }
        self.rename_from.clear();
        self.rename_to.clear();
        self.pending_directories.clear();
        self.pending_renames.clear();
    }

    fn emit_git(&mut self, root: &Path, kind: GitMetaEvent) {
        self.sequence += 1;
        let _ = self
            .sender
            .send(FsEvent::Git(GitEvent::new(self.sequence, root, kind)));
    }

    fn git_path(&self, path: &Path) -> Option<(PathBuf, PathBuf)> {
        self.states
            .iter()
            .filter_map(|(root, state)| {
                let gitdir = state.gitdir.as_ref()?;
                let rel = path.strip_prefix(gitdir).ok()?;
                (!rel.as_os_str().is_empty())
                    .then(|| (gitdir.components().count(), root.clone(), rel.to_path_buf()))
            })
            .max_by_key(|(depth, _, _)| *depth)
            .map(|(_, root, rel)| (root, rel))
    }

    fn locate(&self, path: &Path) -> Option<(PathBuf, PathBuf)> {
        self.states
            .keys()
            .filter_map(|root| {
                let rel = path.strip_prefix(root).ok()?;
                (!rel.as_os_str().is_empty())
                    .then(|| (root.components().count(), root.clone(), rel.to_path_buf()))
            })
            .max_by_key(|(depth, _, _)| *depth)
            .map(|(_, root, rel)| (root, rel))
    }

    fn workspace_path(
        &self,
        path: &Path,
        is_directory: Option<bool>,
    ) -> Option<(PathBuf, PathBuf)> {
        let (root, rel) = self.locate(path)?;
        if rel.starts_with(".git") || self.ignored(&root, &rel, is_directory) {
            return None;
        }
        Some((root, rel))
    }

    fn ignored(&self, root: &Path, rel: &Path, is_directory: Option<bool>) -> bool {
        let Some(state) = self.states.get(root) else {
            return false;
        };
        let Some(ignore) = state.gitignore.as_ref() else {
            return false;
        };
        let matched = |is_dir| {
            matches!(
                ignore.matched_path_or_any_parents(rel, is_dir),
                Match::Ignore(_)
            )
        };
        is_directory.map_or_else(|| matched(false) || matched(true), matched)
    }
}

fn validate_options(options: &WatchOptions) -> Result<(), ChangeTrackerError> {
    if options.max_roots == 0 {
        return Err(ChangeTrackerError::InvalidOptions {
            message: "max_roots must be at least 1".into(),
        });
    }
    if options.event_queue == 0 {
        return Err(ChangeTrackerError::InvalidOptions {
            message: "event_queue must be at least 1".into(),
        });
    }
    if options.debounce.is_zero() {
        return Err(ChangeTrackerError::InvalidOptions {
            message: "debounce must be greater than zero".into(),
        });
    }
    if Instant::now().checked_add(options.debounce).is_none() {
        return Err(ChangeTrackerError::InvalidOptions {
            message: "debounce is too large for the platform clock".into(),
        });
    }
    Ok(())
}

fn forward_raw_result(
    command: &CommandSender,
    lost: &AtomicU64,
    result: notify::Result<notify::Event>,
) {
    match result {
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
    }
}

fn take_watch_gap(lost: &AtomicU64) -> Option<FsEvent> {
    let lost = lost.swap(0, Ordering::Relaxed);
    (lost > 0).then_some(FsEvent::WatchGap { lost })
}

fn path_is_directory(kind: &EventKind, path: &Path) -> Option<bool> {
    use notify::event::{CreateKind, RemoveKind};

    match kind {
        EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder) => Some(true),
        EventKind::Create(CreateKind::File) | EventKind::Remove(RemoveKind::File) => Some(false),
        EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(RenameMode::From)) => None,
        _ => Some(path.is_dir()),
    }
}

/// Merge a new change into the debounce window. Creation survives later
/// modification within the same window; removal followed by creation wins
/// with `Created`; a rename survives all later target-side changes because the
/// consumer can recover current content but cannot reconstruct the lost source
/// path.
fn merge_pending(
    pending: &mut BTreeMap<(PathBuf, PathBuf), FsChangeKind>,
    key: (PathBuf, PathBuf),
    incoming: FsChangeKind,
) {
    let kind = match (pending.get(&key).copied(), incoming) {
        (Some(FsChangeKind::Created), FsChangeKind::Modified) => FsChangeKind::Created,
        (Some(FsChangeKind::Renamed), _) => FsChangeKind::Renamed,
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
