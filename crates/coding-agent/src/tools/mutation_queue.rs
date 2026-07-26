use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

static FILE_MUTATION_QUEUES: LazyLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

async fn mutation_queue_key(file_path: &Path) -> Result<PathBuf, String> {
    let resolved = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("file mutation queue: failed to resolve cwd: {e}"))?
            .join(file_path)
    };

    match tokio::fs::canonicalize(&resolved).await {
        Ok(real_path) => Ok(real_path),
        Err(error) if is_missing_path_error(&error) => Ok(resolved),
        Err(error) => Err(format!(
            "file mutation queue: failed to resolve {}: {error}",
            resolved.display()
        )),
    }
}

fn is_missing_path_error(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::NotFound)
}

fn queue_for_key(key: &Path) -> Result<Arc<tokio::sync::Mutex<()>>, String> {
    let mut queues = FILE_MUTATION_QUEUES
        .lock()
        .map_err(|_| "file mutation queue: registry lock poisoned".to_string())?;
    Ok(queues
        .entry(key.to_path_buf())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone())
}

fn cleanup_queue(key: &Path, queue: &Arc<tokio::sync::Mutex<()>>) {
    let Ok(mut queues) = FILE_MUTATION_QUEUES.lock() else {
        return;
    };
    if Arc::strong_count(queue) == 2
        && queues
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, queue))
    {
        queues.remove(key);
    }
}

pub async fn with_file_mutation_queue<T, F, Fut>(
    file_path: &Path,
    operation: F,
) -> Result<T, String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let key = mutation_queue_key(file_path).await?;
    let queue = queue_for_key(&key)?;
    let guard = queue.lock().await;
    let result = operation().await;
    drop(guard);
    cleanup_queue(&key, &queue);
    result
}
