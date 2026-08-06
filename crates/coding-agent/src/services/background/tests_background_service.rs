#[cfg(test)]
mod tests {
    use crate::services::background::{BackgroundTaskService, CodingAgentBackgroundTaskState};
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
    use workspace_runtime::api::{TaskId, TaskOwner};

    use crate::application::snapshot::SnapshotCoordinator;
    use crate::services::event::EventService;

    use workspace_runtime::api::{OutputBudget, ProcessSpec, ProgramKind};

    fn shell_spec(command: String, max_bytes: usize) -> ProcessSpec {
        ProcessSpec {
            program: ProgramKind::Shell {
                path: "/bin/sh".into(),
                command_arg: "-c".into(),
            },
            command,
            cwd: std::env::current_dir().expect("current directory"),
            env: workspace_runtime::api::EnvPolicy::AllowList(HashMap::from([(
                "PATH".into(),
                std::env::var("PATH").unwrap_or_default(),
            )])),
            timeout: Duration::from_secs(300),
            output_budget: OutputBudget::new(max_bytes, 2_000),
            sandbox: None,
        }
    }

    fn service() -> BackgroundTaskService {
        let coordinator = SnapshotCoordinator::new();
        let events = EventService::with_snapshot_coordinator(coordinator);
        BackgroundTaskService::new(events)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_returns_a_queryable_task_that_finishes() {
        let service = service();
        let task_id = service
            .start(
                shell_spec("printf hello".into(), 64 * 1024),
                TaskOwner::Operation("op-service".into()),
                None,
            )
            .await
            .expect("spawn");
        let snapshots = service.list();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].task_id, task_id.to_string());
        assert_eq!(snapshots[0].owner, "operation:op-service");
        assert_eq!(snapshots[0].state, CodingAgentBackgroundTaskState::Running);
        let report = tokio::time::timeout(Duration::from_secs(5), service.wait(task_id))
            .await
            .expect("finish");
        assert_eq!(
            report.state,
            CodingAgentBackgroundTaskState::Completed { exit_code: Some(0) }
        );
        assert_eq!(report.output, "hello");
        assert_eq!(report.dropped_bytes, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_cursor_reads_increments() {
        let service = service();
        let task_id = service
            .start(
                shell_spec("printf abc; sleep 0.1; printf def".into(), 64 * 1024),
                TaskOwner::Operation("op-cursor".into()),
                None,
            )
            .await
            .expect("spawn");
        let mut cursor = 0;
        let mut seen = String::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while seen.len() < 6 && Instant::now() < deadline {
            let chunk = service.output(task_id, cursor).expect("task is registered");
            assert_eq!(chunk.dropped_bytes, None);
            seen.push_str(&chunk.text);
            cursor = chunk.next_cursor;
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        assert_eq!(seen, "abcdef");
        let tail = service.output(task_id, cursor).expect("registered");
        assert_eq!(tail.text, "");
        assert_eq!(tail.next_cursor, cursor);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn large_output_reports_dropped_bytes_in_snapshot_and_report() {
        let service = service();
        let task_id = service
            .start(
                shell_spec("yes 0123456789 | head -c 2097152".into(), 4096),
                TaskOwner::Session("sess-gap".into()),
                None,
            )
            .await
            .expect("spawn");
        let report = tokio::time::timeout(Duration::from_secs(5), service.wait(task_id))
            .await
            .expect("finish");
        assert!(report.total_bytes >= 2 * 1024 * 1024);
        let dropped = report
            .dropped_bytes
            .expect("truncated spool must report a gap");
        assert!(dropped > 0);
        assert!(report.output.len() <= 4096 + 64);
        let snapshot = service.snapshot(task_id).expect("registered");
        assert!(snapshot.dropped_bytes.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_terminates_the_task_and_emits_terminal_state() {
        let service = service();
        let task_id = service
            .start(
                shell_spec("sleep 300".into(), 64 * 1024),
                TaskOwner::Operation("op-cancel".into()),
                None,
            )
            .await
            .expect("spawn");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(service.cancel(task_id));
        let report = tokio::time::timeout(Duration::from_secs(2), service.wait(task_id))
            .await
            .expect("cancel resolves promptly");
        assert_eq!(report.state, CodingAgentBackgroundTaskState::Cancelled);
        assert!(!service.cancel(task_id));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminate_for_owner_kills_only_that_owners_running_tasks() {
        let service = service();
        let owned = service
            .start(
                shell_spec("sleep 300".into(), 64 * 1024),
                TaskOwner::Worktree("wt-1".into()),
                None,
            )
            .await
            .expect("spawn");
        let other = service
            .start(
                shell_spec("sleep 300".into(), 64 * 1024),
                TaskOwner::Worktree("wt-2".into()),
                None,
            )
            .await
            .expect("spawn");
        let terminated = service.terminate_for_owner(&TaskOwner::Worktree("wt-1".into()));
        assert_eq!(terminated, 1);
        let owned_report = tokio::time::timeout(Duration::from_secs(2), service.wait(owned))
            .await
            .expect("terminated task resolves");
        assert_eq!(
            owned_report.state,
            CodingAgentBackgroundTaskState::Cancelled
        );
        assert_eq!(
            service.snapshot(other).expect("registered").state,
            CodingAgentBackgroundTaskState::Running
        );
        service.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_terminates_running_tasks_and_rejects_new_spawns() {
        let service = service();
        let running = service
            .start(
                shell_spec("sleep 300".into(), 64 * 1024),
                TaskOwner::Session("sess-close".into()),
                None,
            )
            .await
            .expect("spawn");
        let terminated = service.shutdown().await;
        assert_eq!(terminated, 1);
        let report = tokio::time::timeout(Duration::from_secs(2), service.wait(running))
            .await
            .expect("shutdown terminates the task");
        assert_eq!(report.state, CodingAgentBackgroundTaskState::Cancelled);
        let error = service
            .start(
                shell_spec("echo nope".into(), 64 * 1024),
                TaskOwner::Session("sess-close".into()),
                None,
            )
            .await
            .expect_err("shut-down service rejects spawns");
        assert!(error.message.contains("shut down"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_any_and_wait_all_resolve_across_tasks() {
        let service = service();
        let short = service
            .start(
                shell_spec("sleep 0.05; echo quick".into(), 64 * 1024),
                TaskOwner::Session("sess-wait".into()),
                None,
            )
            .await
            .expect("spawn");
        let long = service
            .start(
                shell_spec("sleep 0.4; echo slow".into(), 64 * 1024),
                TaskOwner::Session("sess-wait".into()),
                None,
            )
            .await
            .expect("spawn");
        let (first_id, first_report) =
            tokio::time::timeout(Duration::from_secs(2), service.wait_any(&[long, short]))
                .await
                .expect("wait_any resolves")
                .expect("a task reports");
        assert_eq!(first_id, short);
        assert!(first_report.output.contains("quick"));
        let reports =
            tokio::time::timeout(Duration::from_secs(2), service.wait_all(&[short, long]))
                .await
                .expect("wait_all resolves");
        assert_eq!(reports.len(), 2);
        assert!(reports[0].1.output.contains("quick"));
        assert!(reports[1].1.output.contains("slow"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explicit_task_budget_times_out_long_running_commands() {
        let service = service();
        let task_id = service
            .start(
                shell_spec("sleep 300".into(), 64 * 1024),
                TaskOwner::Operation("op-budget".into()),
                Some(Duration::from_millis(100)),
            )
            .await
            .expect("spawn");
        let report = tokio::time::timeout(Duration::from_secs(2), service.wait(task_id))
            .await
            .expect("budget timeout terminates");
        assert_eq!(report.state, CodingAgentBackgroundTaskState::TimedOut);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_task_reports_failed_without_panicking() {
        let service = service();
        let report = service.wait(TaskId::from_u64(999)).await;
        assert_eq!(report.state, CodingAgentBackgroundTaskState::Failed);
        assert_eq!(service.snapshot(TaskId::from_u64(999)), None);
        assert_eq!(service.output(TaskId::from_u64(999), 0), None);
        assert!(!service.cancel(TaskId::from_u64(999)));
    }
}
