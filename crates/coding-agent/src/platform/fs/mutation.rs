use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::mutex::MutexExt;
use crate::platform::fs::capability::FilesystemTarget;

static FILE_MUTATION_QUEUES: LazyLock<Mutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Owns one path mutation fence. The guard is acquired before any read/derive
/// phase and must be moved into the blocking closure that performs the write.
/// Dropping the async caller after that transfer cannot release the fence: the
/// blocking closure remains its owner until the write and sync have finished.
pub struct MutationGuard {
    key: PathBuf,
    queue: Arc<AsyncMutex<()>>,
    lock: Option<OwnedMutexGuard<()>>,
}

impl std::fmt::Debug for MutationGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MutationGuard")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Drop for MutationGuard {
    fn drop(&mut self) {
        // Release the owned async mutex before checking the registry's strong
        // count, so an idle queue has exactly the registry + `self.queue` refs.
        self.lock.take();
        cleanup_queue(&self.key, &self.queue);
    }
}

pub struct FileMutation;

impl FileMutation {
    pub async fn begin(target: &FilesystemTarget) -> Result<MutationGuard, String> {
        Self::begin_path(target.display_path()).await
    }

    async fn begin_path(path: &Path) -> Result<MutationGuard, String> {
        let path = path.to_path_buf();
        let key = tokio::task::spawn_blocking(move || mutation_queue_key(&path))
            .await
            .map_err(|error| format!("file mutation queue: key task failed: {error}"))??;
        let queue = queue_for_key(&key)?;
        let lock = queue.clone().lock_owned().await;
        Ok(MutationGuard {
            key,
            queue,
            lock: Some(lock),
        })
    }
}

/// Resolve the deepest existing ancestor through the OS, then append the
/// missing suffix lexically. Existing and not-yet-created views of one target
/// therefore share a key, including when an existing parent is a symlink.
fn mutation_queue_key(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "file mutation queue: capability target is not absolute: {}",
            path.display()
        ));
    }
    let mut existing = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(&existing) {
            Ok(real_ancestor) => {
                let mut key = real_ancestor;
                for component in missing.iter().rev() {
                    key.push(component);
                }
                return Ok(key);
            }
            Err(error) if is_missing_path_error(&error) => {
                let leaf = existing.file_name().ok_or_else(|| {
                    format!(
                        "file mutation queue: no existing ancestor for {}",
                        path.display()
                    )
                })?;
                missing.push(leaf.to_os_string());
                existing.pop();
            }
            Err(error) => {
                return Err(format!(
                    "file mutation queue: failed to resolve {}: {error}",
                    existing.display()
                ));
            }
        }
    }
}

fn is_missing_path_error(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::NotFound)
}

fn queue_for_key(key: &Path) -> Result<Arc<AsyncMutex<()>>, String> {
    let mut queues = FILE_MUTATION_QUEUES
        .lock_resource("file mutation queue registry")
        .map_err(|error| error.to_string())?;
    Ok(queues
        .entry(key.to_path_buf())
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone())
}

fn cleanup_queue(key: &Path, queue: &Arc<AsyncMutex<()>>) {
    let mut queues = FILE_MUTATION_QUEUES.lock_or_recover("file mutation queue registry");
    if Arc::strong_count(queue) == 2
        && queues
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, queue))
    {
        queues.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use crate::mutex::MutexExt;

    use super::{FILE_MUTATION_QUEUES, FileMutation, mutation_queue_key};

    #[cfg(unix)]
    #[test]
    fn missing_and_existing_targets_below_a_symlink_parent_share_one_key() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let alias = temp.path().join("alias");
        symlink(&real, &alias).unwrap();
        let requested = alias.join("target.txt");
        let create_key = mutation_queue_key(&requested).unwrap();
        std::fs::write(real.join("target.txt"), "content").unwrap();
        let overwrite_key = mutation_queue_key(&requested).unwrap();
        assert_eq!(create_key, overwrite_key);
        assert_eq!(overwrite_key, real.join("target.txt"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocking_owner_retains_fence_after_join_handle_is_dropped() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.txt");
        let first = FileMutation::begin_path(&target).await.unwrap();
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let worker_started = started.clone();
        let worker_release = release.clone();
        let task = tokio::task::spawn_blocking(move || {
            let _fence = first;
            worker_started.store(true, Ordering::Release);
            while !worker_release.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        while !started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        drop(task);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), FileMutation::begin_path(&target))
                .await
                .is_err(),
            "a detached blocking write must retain its fence"
        );
        release.store(true, Ordering::Release);
        let second =
            tokio::time::timeout(Duration::from_secs(1), FileMutation::begin_path(&target))
                .await
                .expect("second mutation should acquire after the first write")
                .unwrap();
        drop(second);
        let key = mutation_queue_key(&target).unwrap();
        assert!(
            !FILE_MUTATION_QUEUES
                .lock_or_recover("test file mutation queue registry")
                .contains_key(&key)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocking_panic_releases_fence_and_cleans_registry() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("panic-target.txt");
        let guard = FileMutation::begin_path(&target).await.unwrap();
        let panic = tokio::task::spawn_blocking(move || {
            let _fence = guard;
            panic!("injected blocking mutation panic");
        })
        .await;
        assert!(panic.is_err());
        let second =
            tokio::time::timeout(Duration::from_secs(1), FileMutation::begin_path(&target))
                .await
                .expect("panic must release the mutation fence")
                .unwrap();
        drop(second);
        let key = mutation_queue_key(&target).unwrap();
        assert!(
            !FILE_MUTATION_QUEUES
                .lock_or_recover("test file mutation queue registry")
                .contains_key(&key)
        );
    }
}
