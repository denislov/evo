use super::*;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use super::{TaskOwner, TaskRegistry, TaskState};
    use crate::process::{EnvPolicy, OutputBudget, ProcessSpec, ProgramKind};

    fn shell_spec(command: String, max_bytes: usize) -> ProcessSpec {
        ProcessSpec {
            program: ProgramKind::Shell {
                path: "/bin/sh".into(),
                command_arg: "-c".into(),
            },
            command,
            cwd: std::env::current_dir().expect("current directory"),
            env: EnvPolicy::AllowList(HashMap::from([(
                "PATH".into(),
                std::env::var("PATH").unwrap_or_default(),
            )])),
            timeout: Duration::from_secs(300),
            output_budget: OutputBudget::new(max_bytes, 2_000),
        }
    }

    fn registry() -> TaskRegistry {
        TaskRegistry::new()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn background_task_runs_to_completion_with_full_output() {
        let registry = registry();
        let handle = registry
            .spawn(
                shell_spec("printf hello; printf ' world' >&2".into(), 64 * 1024),
                TaskOwner::Operation("op-1".into()),
                None,
            )
            .await
            .expect("spawn");
        let report = tokio::time::timeout(Duration::from_secs(5), handle.wait())
            .await
            .expect("task should finish");
        assert_eq!(report.state, TaskState::Completed { exit_code: Some(0) });
        // The merged spool interleaves streams as they arrive; byte totals and
        // per-stream accounting must be exact even when ordering varies.
        assert_eq!(report.output.len(), "hello world".len());
        assert!(report.output.contains("hello"));
        assert!(report.output.contains("world"));
        assert_eq!(report.stdout_bytes, 5);
        assert_eq!(report.stderr_bytes, 6);
        assert_eq!(report.gap, None);
        assert_eq!(registry.list().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_cursor_reads_increments_and_never_replays() {
        let registry = registry();
        let handle = registry
            .spawn(
                shell_spec(
                    "printf abc; sleep 0.15; printf def; sleep 0.15; printf ghi".into(),
                    64 * 1024,
                ),
                TaskOwner::Operation("op-cursor".into()),
                None,
            )
            .await
            .expect("spawn");
        let mut cursor = 0;
        let mut seen = String::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while seen.len() < 9 && Instant::now() < deadline {
            let chunk = handle.output(cursor);
            assert_eq!(chunk.gap, None, "bounded spool must not drop small output");
            seen.push_str(&chunk.text);
            cursor = chunk.next_cursor;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(seen, "abcdefghi");
        let tail = handle.output(cursor);
        assert_eq!(tail.text, "");
        assert_eq!(tail.next_cursor, cursor);
        let report = tokio::time::timeout(Duration::from_secs(2), handle.wait())
            .await
            .expect("finish");
        assert_eq!(report.output, "abcdefghi");
        assert_eq!(report.gap, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn large_output_drops_old_bytes_and_reports_gap() {
        let registry = registry();
        let handle = registry
            .spawn(
                shell_spec("yes 0123456789 | head -c 2097152".into(), 4096),
                TaskOwner::Session("sess-gap".into()),
                None,
            )
            .await
            .expect("spawn");
        let report = tokio::time::timeout(Duration::from_secs(5), handle.wait())
            .await
            .expect("finish");
        assert_eq!(report.state, TaskState::Completed { exit_code: Some(0) });
        assert!(report.total_bytes >= 2 * 1024 * 1024);
        assert!(report.output.len() <= 4096 + 64);
        let gap = report.gap.expect("dropped output must be reported");
        assert!(gap.dropped_bytes > 0);
        assert!(report.total_bytes - report.output.len() as u64 <= gap.dropped_bytes);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reading_from_a_stale_cursor_reports_an_explicit_gap() {
        let registry = registry();
        let handle = registry
            .spawn(
                shell_spec("yes 0123456789 | head -c 2097152".into(), 4096),
                TaskOwner::Session("sess-stale".into()),
                None,
            )
            .await
            .expect("spawn");
        tokio::time::timeout(Duration::from_secs(5), handle.wait())
            .await
            .expect("finish");
        let chunk = handle.output(0);
        assert!(chunk.gap.is_some(), "stale cursor must report a gap");
        assert!(chunk.text.len() <= 4096 + 64);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_terminates_the_process_tree_and_reports_cancelled() {
        let registry = registry();
        let handle = registry
            .spawn(
                shell_spec("sleep 300".into(), 64 * 1024),
                TaskOwner::Operation("op-cancel".into()),
                None,
            )
            .await
            .expect("spawn");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(handle.cancel());
        let started = Instant::now();
        let report = tokio::time::timeout(Duration::from_secs(2), handle.wait())
            .await
            .expect("cancelled task should finish promptly");
        assert_eq!(report.state, TaskState::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!registry.cancel(handle.task_id()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_all_resolves_every_task_and_wait_any_resolves_first() {
        let registry = registry();
        let short = registry
            .spawn(
                shell_spec("sleep 0.05; echo quick".into(), 64 * 1024),
                TaskOwner::Session("sess-wait".into()),
                None,
            )
            .await
            .expect("spawn");
        let long = registry
            .spawn(
                shell_spec("sleep 0.5; echo slow".into(), 64 * 1024),
                TaskOwner::Session("sess-wait".into()),
                None,
            )
            .await
            .expect("spawn");
        let (first_id, first_report) = tokio::time::timeout(
            Duration::from_secs(2),
            registry.wait_any(&[long.task_id(), short.task_id()]),
        )
        .await
        .expect("wait_any should resolve")
        .expect("wait_any should report");
        assert_eq!(first_id, short.task_id());
        assert!(first_report.output.contains("quick"));

        let reports = tokio::time::timeout(
            Duration::from_secs(2),
            registry.wait_all(&[short.task_id(), long.task_id()]),
        )
        .await
        .expect("wait_all should resolve");
        assert_eq!(reports.len(), 2);
        assert!(reports[0].1.output.contains("quick"));
        assert!(reports[1].1.output.contains("slow"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminate_all_for_owner_kills_only_that_owners_tasks() {
        let registry = registry();
        let owned = registry
            .spawn(
                shell_spec("sleep 300".into(), 64 * 1024),
                TaskOwner::Worktree("wt-1".into()),
                None,
            )
            .await
            .expect("spawn");
        let other = registry
            .spawn(
                shell_spec("sleep 300".into(), 64 * 1024),
                TaskOwner::Worktree("wt-2".into()),
                None,
            )
            .await
            .expect("spawn");
        let terminated = registry.terminate_all_for_owner(&TaskOwner::Worktree("wt-1".into()));
        assert_eq!(terminated, 1);
        let owned_report = tokio::time::timeout(Duration::from_secs(2), owned.wait())
            .await
            .expect("terminated task finishes");
        assert_eq!(owned_report.state, TaskState::Cancelled);
        assert!(other.status().is_running());
        assert_eq!(
            registry
                .list_for_owner(&TaskOwner::Worktree("wt-1".into()))
                .len(),
            1
        );
        registry.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_terminates_running_tasks_and_rejects_new_spawns() {
        let registry = registry();
        let running = registry
            .spawn(
                shell_spec("sleep 300".into(), 64 * 1024),
                TaskOwner::Session("sess-shutdown".into()),
                None,
            )
            .await
            .expect("spawn");
        let finished = registry
            .spawn(
                shell_spec("echo done".into(), 64 * 1024),
                TaskOwner::Session("sess-shutdown".into()),
                None,
            )
            .await
            .expect("spawn");
        tokio::time::timeout(Duration::from_secs(2), finished.wait())
            .await
            .expect("finished task resolves");
        let terminated = registry.shutdown().await;
        assert_eq!(terminated, 1);
        let report = tokio::time::timeout(Duration::from_secs(2), running.wait())
            .await
            .expect("shutdown terminates the running task");
        assert_eq!(report.state, TaskState::Cancelled);
        let error = registry
            .spawn(
                shell_spec("echo nope".into(), 64 * 1024),
                TaskOwner::Session("sess-shutdown".into()),
                None,
            )
            .await
            .expect_err("shut-down registry rejects spawns");
        assert!(error.message.contains("shut down"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn crash_exit_reports_the_nonzero_exit_code() {
        let registry = registry();
        let handle = registry
            .spawn(
                shell_spec("printf boom; exit 7".into(), 64 * 1024),
                TaskOwner::Operation("op-crash".into()),
                None,
            )
            .await
            .expect("spawn");
        let report = tokio::time::timeout(Duration::from_secs(5), handle.wait())
            .await
            .expect("finish");
        assert_eq!(report.state, TaskState::Completed { exit_code: Some(7) });
        assert_eq!(report.output, "boom");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_failure_does_not_register_a_task() {
        let registry = registry();
        let spec = ProcessSpec {
            program: ProgramKind::Direct {
                program: "definitely-not-a-real-program-arc600".into(),
                args: Vec::new(),
            },
            command: String::new(),
            cwd: std::env::current_dir().expect("current directory"),
            env: EnvPolicy::AllowList(HashMap::new()),
            timeout: Duration::from_secs(5),
            output_budget: OutputBudget::new(64 * 1024, 2_000),
        };
        let error = registry
            .spawn(spec, TaskOwner::Operation("op-spawn-fail".into()), None)
            .await
            .expect_err("missing program must fail the spawn");
        assert!(error.message.contains("failed to spawn"));
        assert!(registry.list().is_empty());
    }

    #[test]
    fn task_timeout_budget_terminates_the_task() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let registry = registry();
            let handle = registry
                .spawn(
                    shell_spec("sleep 300".into(), 64 * 1024),
                    TaskOwner::Session("sess-timeout".into()),
                    Some(Duration::from_millis(100)),
                )
                .await
                .expect("spawn");
            let report = tokio::time::timeout(Duration::from_secs(2), handle.wait())
                .await
                .expect("budget timeout terminates");
            assert_eq!(report.state, TaskState::TimedOut);
            registry.shutdown().await;
        });
    }

    #[test]
    fn owner_display_round_trips_kind_and_id() {
        let owner = TaskOwner::Worktree("wt-9".into());
        assert_eq!(owner.kind(), "worktree");
        assert_eq!(owner.id(), "wt-9");
        assert_eq!(owner.to_string(), "worktree:wt-9");
        assert_eq!(
            TaskOwner::Operation("op-1".into()),
            TaskOwner::Operation("op-1".into())
        );
        assert_ne!(owner, TaskOwner::Operation("wt-9".into()));
    }
}
