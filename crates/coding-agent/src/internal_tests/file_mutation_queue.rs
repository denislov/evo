//! Internal owner tests for the file-mutation queue.

use coding_agent::tools::mutation_queue::with_file_mutation_queue;
use std::time::Duration;
use tokio::sync::oneshot;

const FILE_MUTATION_SIGNAL_TIMEOUT: Duration = Duration::from_millis(500);

async fn recv_file_mutation_signal<T>(rx: oneshot::Receiver<T>, context: &str) -> T {
    tokio::time::timeout(FILE_MUTATION_SIGNAL_TIMEOUT, rx)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {context}"))
        .unwrap_or_else(|_| panic!("file mutation signal channel closed before {context}"))
}

#[tokio::test]
async fn serializes_mutations_for_the_same_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    std::fs::write(&path, "").unwrap();

    let (first_entered_tx, first_entered_rx) = oneshot::channel();
    let (release_first_tx, release_first_rx) = oneshot::channel();
    let (second_attempted_tx, second_attempted_rx) = oneshot::channel();
    let (second_entered_tx, mut second_entered_rx) = oneshot::channel();

    let first = {
        let path = path.clone();
        tokio::spawn(async move {
            with_file_mutation_queue(&path, || async {
                let _ = first_entered_tx.send(());
                release_first_rx.await.unwrap();
                Ok::<_, String>(())
            })
            .await
            .unwrap();
        })
    };

    first_entered_rx.await.unwrap();

    let second = {
        let path = path.clone();
        tokio::spawn(async move {
            let _ = second_attempted_tx.send(());
            with_file_mutation_queue(&path, || async {
                let _ = second_entered_tx.send(());
                Ok::<_, String>(())
            })
            .await
            .unwrap();
        })
    };

    second_attempted_rx.await.unwrap();
    assert!(matches!(
        second_entered_rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));

    release_first_tx.send(()).unwrap();
    second_entered_rx.await.unwrap();
    first.await.unwrap();
    second.await.unwrap();
}

#[tokio::test]
async fn allows_mutations_for_different_files_to_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.txt");
    let right = dir.path().join("right.txt");
    std::fs::write(&left, "").unwrap();
    std::fs::write(&right, "").unwrap();

    let (left_entered_tx, left_entered_rx) = oneshot::channel();
    let (release_left_tx, release_left_rx) = oneshot::channel();
    let (right_attempted_tx, right_attempted_rx) = oneshot::channel();
    let (right_entered_tx, right_entered_rx) = oneshot::channel();

    let left_task = {
        let left = left.clone();
        tokio::spawn(async move {
            with_file_mutation_queue(&left, || async {
                let _ = left_entered_tx.send(());
                release_left_rx.await.unwrap();
                Ok::<_, String>(())
            })
            .await
            .unwrap();
        })
    };

    left_entered_rx.await.unwrap();

    let right_task = {
        let right = right.clone();
        tokio::spawn(async move {
            let _ = right_attempted_tx.send(());
            with_file_mutation_queue(&right, || async {
                let _ = right_entered_tx.send(());
                Ok::<_, String>(())
            })
            .await
            .unwrap();
        })
    };

    right_attempted_rx.await.unwrap();
    recv_file_mutation_signal(
        right_entered_rx,
        "different-file mutation to enter while left file is still locked",
    )
    .await;

    release_left_tx.send(()).unwrap();
    right_task.await.unwrap();
    left_task.await.unwrap();
}
