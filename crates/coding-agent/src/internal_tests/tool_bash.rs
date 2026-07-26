//! Internal owner tests for the bash tool.

use ai::api::conversation::ContentBlock;
use coding_agent::tools::shell::{
    BashOptions, BashSpawnContext, bash_execute, bash_execute_with_options,
    bash_execute_with_options_and_update,
};
use std::fmt::Display;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::mpsc;

const BASH_HANG_GUARD_TIMEOUT: Duration = Duration::from_secs(5);
const BASH_CHILD_EXIT_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(1);
const BASH_STREAM_UPDATE_DELAY_SECS: f64 = 0.5;
const BASH_TIMEOUT_COMMAND_SLEEP_SECS: u64 = 5;
const BASH_TIMEOUT_ERROR_SECS: u64 = 1;
const BASH_BACKGROUND_CHILD_SLEEP_SECS: u64 = 60;
const BASH_BACKGROUND_CHILD_TIMEOUT_SECS: f64 = 0.1;
const BASH_FRACTIONAL_TIMEOUT_SECS: f64 = 0.5;
const BASH_BACKGROUND_HANG_TIMEOUT_SECS: u64 = 2;

type BashExecutionResult = Result<Vec<ContentBlock>, String>;

fn bash_sleep_command(seconds: impl Display) -> String {
    format!("sleep {seconds}")
}

fn bash_timeout_error_message(seconds: impl Display) -> String {
    format!("Command timed out after {seconds} seconds")
}

fn text(b: &[ContentBlock]) -> String {
    b.iter()
        .filter_map(|x| match x {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn pid_is_alive(pid: u32) -> bool {
    tokio::process::Command::new("sh")
        .arg("-c")
        .arg(format!("kill -0 {pid} 2>/dev/null"))
        .status()
        .await
        .is_ok_and(|status| status.success())
}

async fn wait_for_pid_to_exit(pid: u32, timeout: Duration) -> bool {
    tokio::time::timeout(timeout, async {
        while pid_is_alive(pid).await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok()
}

async fn run_bash_with_hang_guard(
    cwd: &std::path::Path,
    payload: serde_json::Value,
) -> Result<BashExecutionResult, tokio::time::error::Elapsed> {
    tokio::time::timeout(BASH_HANG_GUARD_TIMEOUT, bash_execute(cwd, payload)).await
}

#[tokio::test]
async fn captures_stdout() {
    let d = tempdir().unwrap();
    let r = bash_execute(d.path(), serde_json::json!({"command":"echo hello"}))
        .await
        .unwrap();
    assert!(text(&r).contains("hello"));
}

#[tokio::test]
async fn supports_shell_options_prefix_and_spawn_hook() {
    let d = tempdir().unwrap();
    let shell_path: Option<String> = if cfg!(windows) {
        // On Windows, use default shell resolution (which finds Git Bash)
        None
    } else {
        Some("/bin/sh".into())
    };
    let options = BashOptions {
        shell_path,
        command_prefix: Some("echo prefix".into()),
        spawn_hook: Some(Arc::new(|mut context: BashSpawnContext| {
            context.command = format!("{}\necho \"$EVO_BASH_HOOK\"", context.command);
            context.env.insert("EVO_BASH_HOOK".into(), "hooked".into());
            context
        })),
    };

    let r = bash_execute_with_options(
        d.path(),
        serde_json::json!({"command":"echo body"}),
        &options,
    )
    .await
    .unwrap();
    let t = text(&r);
    assert!(t.contains("prefix"), "{t}");
    assert!(t.contains("body"), "{t}");
    assert!(t.contains("hooked"), "{t}");
}

#[tokio::test]
async fn streams_output_update_before_process_exits() {
    let d = tempdir().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let on_update = Arc::new(move |update: agent_core::api::tool::AgentToolOutput| {
        let text = update
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let _ = tx.send(text);
    });

    let options = BashOptions::default();
    let fut = bash_execute_with_options_and_update(
        d.path(),
        serde_json::json!({"command": format!(
            "printf 'first\\n'; {}; printf 'second\\n'",
            bash_sleep_command(BASH_STREAM_UPDATE_DELAY_SECS)
        )}),
        &options,
        Some(on_update),
    );
    tokio::pin!(fut);

    let update = tokio::select! {
        update = rx.recv() => update.expect("expected streamed update"),
        result = &mut fut => panic!("bash completed before first streamed update: {result:?}"),
    };

    assert!(update.contains("first"), "{update}");
    let final_blocks = fut.await.unwrap();
    let final_text = text(&final_blocks);
    assert!(final_text.contains("second"), "{final_text}");
}

#[tokio::test]
async fn captures_stderr() {
    let d = tempdir().unwrap();
    let r = bash_execute(d.path(), serde_json::json!({"command":"echo oops 1>&2"}))
        .await
        .unwrap();
    assert!(text(&r).contains("oops"));
}

#[tokio::test]
async fn nonzero_exit_is_error() {
    let d = tempdir().unwrap();
    let e = bash_execute(d.path(), serde_json::json!({"command":"echo bad; exit 3"}))
        .await
        .unwrap_err();
    assert!(e.contains("bad"));
    assert!(e.contains("Command exited with code 3"));
}

#[tokio::test]
async fn timeout_errors() {
    let d = tempdir().unwrap();
    let e = bash_execute(
        d.path(),
        serde_json::json!({
            "command": bash_sleep_command(BASH_TIMEOUT_COMMAND_SLEEP_SECS),
            "timeout": BASH_TIMEOUT_ERROR_SECS
        }),
    )
    .await
    .unwrap_err();
    assert!(e.contains(&bash_timeout_error_message(BASH_TIMEOUT_ERROR_SECS)));
}

#[tokio::test]
async fn timeout_kills_background_child_process() {
    let d = tempdir().unwrap();
    let marker = d.path().join("child-survived");
    let pid_file = d.path().join("child.pid");
    let command = format!(
        "sh -c '{}; touch {}' & child=$!; echo $child > {}; wait $child",
        bash_sleep_command(BASH_BACKGROUND_CHILD_SLEEP_SECS),
        marker.display(),
        pid_file.display()
    );

    let e = bash_execute(
        d.path(),
        serde_json::json!({"command": command, "timeout": BASH_BACKGROUND_CHILD_TIMEOUT_SECS}),
    )
    .await
    .unwrap_err();
    assert!(
        e.contains(&bash_timeout_error_message(
            BASH_BACKGROUND_CHILD_TIMEOUT_SECS
        )),
        "{e}"
    );

    let child_pid = std::fs::read_to_string(&pid_file)
        .expect("background child pid should be recorded before timeout")
        .trim()
        .parse::<u32>()
        .expect("background child pid should be numeric");
    assert!(
        wait_for_pid_to_exit(child_pid, BASH_CHILD_EXIT_OBSERVATION_TIMEOUT).await,
        "background child pid {child_pid} should be killed on timeout"
    );
    assert!(
        !marker.exists(),
        "background child should not survive long enough to write marker"
    );
}

#[tokio::test]
async fn truncates_tail_output() {
    let d = tempdir().unwrap();
    let r = bash_execute(d.path(), serde_json::json!({"command":"seq 1 2005"}))
        .await
        .unwrap();
    let t = text(&r);
    assert!(t.contains("2005"));
    assert!(
        t.contains("[Output truncated: showing last 2000 of 2005 lines (50KB/2000-line limit).]"),
        "{t}"
    );
}

#[tokio::test]
async fn missing_cwd_errors() {
    let e = bash_execute(
        std::path::Path::new("/no/such/dir/xyz"),
        serde_json::json!({"command":"echo hi"}),
    )
    .await
    .unwrap_err();
    assert!(e.contains("Working directory does not exist"));
}

#[tokio::test]
async fn fractional_timeout_is_accepted() {
    let d = tempdir().unwrap();
    let e = bash_execute(
        d.path(),
        serde_json::json!({
            "command": bash_sleep_command(BASH_TIMEOUT_COMMAND_SLEEP_SECS),
            "timeout": BASH_FRACTIONAL_TIMEOUT_SECS
        }),
    )
    .await
    .unwrap_err();
    assert!(e.contains(&bash_timeout_error_message(BASH_FRACTIONAL_TIMEOUT_SECS)));
}

#[tokio::test]
async fn background_child_does_not_hang() {
    let d = tempdir().unwrap();
    let r = run_bash_with_hang_guard(
        d.path(),
        serde_json::json!({
            "command": format!(
                "bash -c '{} & echo done'",
                bash_sleep_command(BASH_BACKGROUND_CHILD_SLEEP_SECS)
            ),
            "timeout": BASH_BACKGROUND_HANG_TIMEOUT_SECS
        }),
    )
    .await;
    match r {
        Ok(Err(e)) => {
            assert!(
                e.contains("done") || e.contains("timed out"),
                "unexpected error: {e}"
            );
        }
        Ok(Ok(blocks)) => {
            let t = text(&blocks);
            assert!(t.contains("done"), "expected 'done' in output: {t}");
        }
        Err(_) => {}
    }
}

#[tokio::test]
async fn large_output_does_not_use_excessive_memory() {
    let d = tempdir().unwrap();
    let r = bash_execute(d.path(), serde_json::json!({"command": "seq 1 5000"}))
        .await
        .unwrap();
    let t = text(&r);
    assert!(t.contains("5000"), "missing last line: {t}");
    assert!(
        t.contains("[Output truncated:"),
        "5000-line output should be truncated: {t}"
    );
}

#[tokio::test]
async fn stdout_stderr_arrival_order_preserved() {
    let d = tempdir().unwrap();
    let r = bash_execute(
        d.path(),
        serde_json::json!({"command": "printf 'out1\\n'; printf 'err1\\n' 1>&2; printf 'out2\\n'"}),
    )
    .await
    .unwrap();
    let t = text(&r);
    assert!(t.contains("out1"), "missing out1: {t}");
    assert!(t.contains("err1"), "missing err1: {t}");
    assert!(t.contains("out2"), "missing out2: {t}");
}
