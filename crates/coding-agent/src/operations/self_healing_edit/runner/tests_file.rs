use super::*;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;

    use super::{
        RealSelfHealingEditCheckRunner, SelfHealingEditCheckOutput, SelfHealingEditCheckRunner,
    };
    use crate::kernel::error::CodingSessionError;
    use crate::test_support::ProcessFixture;

    #[tokio::test]
    async fn real_check_runner_preserves_separate_bounded_streams() {
        let runner = RealSelfHealingEditCheckRunner {
            timeout: Duration::from_secs(2),
        };
        let output = runner
            .run_check(
                &std::env::current_dir().expect("current directory"),
                "printf stdout; printf stderr >&2; exit 9",
                &CancellationToken::new(),
            )
            .await
            .expect("non-zero check exit is a check result");
        assert_eq!(
            output,
            SelfHealingEditCheckOutput {
                command: "printf stdout; printf stderr >&2; exit 9".into(),
                stdout: "stdout".into(),
                stderr: "stderr".into(),
                exit_code: 9,
            }
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_check_runner_cancel_returns_without_waiting_for_shutdown_timeout() {
        let fixture = ProcessFixture::new().expect("fixture");
        let command = fixture.sleep_command();
        let cancellation = CancellationToken::new();
        let task_token = cancellation.clone();
        let task = tokio::spawn(async move {
            RealSelfHealingEditCheckRunner::default()
                .run_check(
                    &std::env::current_dir().expect("current directory"),
                    &command,
                    &task_token,
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancellation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("check cancellation should complete teardown")
            .expect("check task should join");
        assert_eq!(result, Err(CodingSessionError::Cancelled));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn real_check_runner_timeout_is_explicit_and_output_is_bounded() {
        let runner = RealSelfHealingEditCheckRunner {
            timeout: Duration::from_millis(50),
        };
        let error = runner
            .run_check(
                &std::env::current_dir().expect("current directory"),
                "printf started; sleep 300",
                &CancellationToken::new(),
            )
            .await
            .expect_err("check should time out");
        let CodingSessionError::Tool { message } = error else {
            panic!("timeout should be a tool error: {error:?}");
        };
        assert!(message.contains("self-healing edit check timed out after 0.05 seconds"));
        assert!(message.contains("started"));

        let fixture = ProcessFixture::new().expect("fixture");
        let noisy = RealSelfHealingEditCheckRunner {
            timeout: Duration::from_secs(10),
        }
        .run_check(
            &std::env::current_dir().expect("current directory"),
            &fixture.noisy_command(),
            &CancellationToken::new(),
        )
        .await
        .expect("noisy check should complete");
        assert_eq!(noisy.exit_code, 0);
        assert!(noisy.stdout.len() <= 52 * 1024);
        assert!(noisy.stdout.contains("Output truncated"));
    }
}
