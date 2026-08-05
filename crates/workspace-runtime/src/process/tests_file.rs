use super::*;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use tokio_util::sync::CancellationToken;

    use super::{
        EnvPolicy, OutputBudget, ProcessOutcome, ProcessSpec, ProcessUpdateCallback, ProgramKind,
        run,
    };

    /// Test fixture for shell command probes, mirroring the product test
    /// support helper this crate is deliberately not allowed to depend on.
    struct ProcessFixture {
        _temp_dir: tempfile::TempDir,
        pid_file: std::path::PathBuf,
    }

    impl ProcessFixture {
        fn new() -> Result<Self, String> {
            let temp_dir = tempfile::tempdir().map_err(|error| error.to_string())?;
            let pid_file = temp_dir.path().join("descendant.pid");
            Ok(Self {
                _temp_dir: temp_dir,
                pid_file,
            })
        }

        fn pid_file(&self) -> &std::path::Path {
            &self.pid_file
        }

        #[cfg(unix)]
        fn sleep_command(&self) -> String {
            "sleep 300".into()
        }

        #[cfg(windows)]
        fn sleep_command(&self) -> String {
            "Start-Sleep -Seconds 300".into()
        }

        #[cfg(unix)]
        fn noisy_command(&self) -> String {
            "yes 0123456789 | head -c 16777216".into()
        }

        #[cfg(windows)]
        fn noisy_command(&self) -> String {
            "1..1048576 | ForEach-Object { '0123456789' }".into()
        }

        #[cfg(unix)]
        fn descendant_command(&self) -> String {
            format!(
                "sleep 300 & child=$!; printf '%s' \"$child\" > {}; wait \"$child\"",
                shell_quote(&self.pid_file)
            )
        }

        #[cfg(windows)]
        fn descendant_command(&self) -> String {
            format!(
                "$child = Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 300' -PassThru; Set-Content -NoNewline -Path '{}' -Value $child.Id; Wait-Process -Id $child.Id",
                self.pid_file.display()
            )
        }
    }

    #[cfg(unix)]
    fn shell_quote(path: &std::path::Path) -> String {
        format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
    }

    fn shell_spec(command: String, timeout: Duration) -> ProcessSpec {
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
            timeout,
            output_budget: OutputBudget::new(50 * 1024, 2_000),
        }
    }

    #[cfg(unix)]
    async fn wait_for_pid(path: &std::path::Path) -> u32 {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(text) = tokio::fs::read_to_string(path).await
                    && let Ok(pid) = text.parse()
                {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("descendant pid should be written")
    }

    #[cfg(unix)]
    async fn assert_process_stopped(pid: u32) {
        tokio::time::timeout(Duration::from_secs(2), async move {
            loop {
                // A zombie has terminated and cannot execute work; on Linux it
                // may remain visible briefly until the container init reaps it.
                let zombie = tokio::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .await
                    .ok()
                    .and_then(|stat| {
                        stat.rsplit_once(") ")
                            .map(|(_, tail)| tail.starts_with('Z'))
                    })
                    .unwrap_or(false);
                // SAFETY: signal 0 only probes a process identifier captured
                // from the test fixture; it does not send a signal.
                let missing = unsafe { libc::kill(pid as i32, 0) } != 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                if zombie || missing {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("descendant process survived process-tree teardown");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_returns_only_after_sleep_is_terminated() {
        let fixture = ProcessFixture::new().expect("fixture");
        let cancellation = CancellationToken::new();
        let task_token = cancellation.clone();
        let task = tokio::spawn(async move {
            run(
                shell_spec(fixture.sleep_command(), Duration::from_secs(300)),
                &task_token,
                None,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let started = Instant::now();
        cancellation.cancel();
        let outcome = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("cancelled runner should return")
            .expect("runner task should join");
        assert!(matches!(outcome, ProcessOutcome::Cancelled { .. }));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_kills_descendants_before_returning() {
        let fixture = ProcessFixture::new().expect("fixture");
        let command = fixture.descendant_command();
        let pid_file = fixture.pid_file().to_path_buf();
        let cancellation = CancellationToken::new();
        let task_token = cancellation.clone();
        let task = tokio::spawn(async move {
            run(
                shell_spec(command, Duration::from_secs(300)),
                &task_token,
                None,
            )
            .await
        });
        let pid = wait_for_pid(&pid_file).await;
        cancellation.cancel();
        let outcome = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("cancelled runner should return")
            .expect("runner task should join");
        assert!(matches!(outcome, ProcessOutcome::Cancelled { .. }));
        assert_process_stopped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_uses_the_same_descendant_teardown() {
        let fixture = ProcessFixture::new().expect("fixture");
        let command = fixture.descendant_command();
        let pid_file = fixture.pid_file().to_path_buf();
        let task = tokio::spawn(async move {
            run(
                shell_spec(command, Duration::from_millis(150)),
                &CancellationToken::new(),
                None,
            )
            .await
        });
        let pid = wait_for_pid(&pid_file).await;
        let outcome = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("timed-out runner should return")
            .expect("runner task should join");
        assert!(matches!(outcome, ProcessOutcome::TimedOut { .. }));
        assert_process_stopped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn noisy_output_is_bounded_and_updates_are_throttled() {
        let fixture = ProcessFixture::new().expect("fixture");
        let updates = Arc::new(AtomicUsize::new(0));
        let callback_updates = updates.clone();
        let callback: ProcessUpdateCallback = Arc::new(move |_| {
            callback_updates.fetch_add(1, Ordering::Relaxed);
        });
        let outcome = run(
            shell_spec(fixture.noisy_command(), Duration::from_secs(10)),
            &CancellationToken::new(),
            Some(&callback),
        )
        .await;
        let ProcessOutcome::Completed {
            exit_code: Some(0),
            output,
        } = outcome
        else {
            panic!("noisy command should complete successfully: {outcome:?}");
        };
        assert!(output.stdout_bytes >= 16 * 1024 * 1024);
        assert!(output.stdout.len() <= 52 * 1024);
        assert!(output.merged.len() <= 52 * 1024);
        assert!(output.stdout.contains("Output truncated"));
        assert!(updates.load(Ordering::Relaxed) < 512);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn environment_is_replaced_by_the_explicit_allowlist() {
        let mut spec = shell_spec(
            "printf '%s:%s' \"$VISIBLE\" \"${HIDDEN-unset}\"".into(),
            Duration::from_secs(2),
        );
        spec.env = EnvPolicy::AllowList(HashMap::from([("VISIBLE".into(), "ok".into())]));
        let outcome = run(spec, &CancellationToken::new(), None).await;
        let ProcessOutcome::Completed {
            exit_code: Some(0),
            output,
        } = outcome
        else {
            panic!("environment probe should complete: {outcome:?}");
        };
        assert_eq!(output.stdout, "ok:unset");
        assert_eq!(output.stderr, "");
    }

    #[test]
    fn tail_line_limit_ignores_the_terminal_newline_element() {
        let (tail, lines) = super::tail_text("line1\nline2\nline3\n", 2, 1024);
        assert_eq!(tail, "line2\nline3");
        assert_eq!(lines, 2);
    }

    #[test]
    fn tail_byte_limit_reports_the_lines_actually_retained() {
        let (tail, lines) = super::tail_text("a\nb\ncdefghijkl", 2, 6);
        assert_eq!(tail, "ghijkl");
        assert_eq!(lines, 1);
    }
}
