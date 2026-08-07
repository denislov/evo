//! Active prompt shutdown and session-close teardown with bounded deadlines.

use std::time::Duration;

use coding_agent::api::client::CodingAgentControlId;
use tokio::sync::mpsc;

use super::{ActivePrompt, RUNTIME_SHUTDOWN_DEADLINE, product_events::drain_product_events};
use crate::runtime::protocol::{
    DesktopBridgeError, DesktopRuntimeError, DesktopRuntimeUpdate, local_runtime_error,
};

pub(in crate::runtime) async fn close_active_prompt(
    mut active: ActivePrompt,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    data_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) -> Result<(), DesktopBridgeError> {
    let operation_id = active.operation_id.clone().or_else(|| {
        active
            .connection
            .state()
            .ok()
            .and_then(|snapshot| snapshot.submitted_operation)
            .map(|operation| operation.operation_id)
    });
    if let Some(operation_id) = operation_id.as_deref() {
        let control = active.connection.prompt_control(operation_id);
        let _ = control.abort(
            CodingAgentControlId("desktop-session-close".into()),
            "desktop session close",
        );
    }
    match tokio::time::timeout(RUNTIME_SHUTDOWN_DEADLINE, &mut active.task).await {
        Ok(Ok((mut session, _))) => {
            if !drain_product_events(&mut active, priority_updates, data_updates).await {
                let _ = active.connection.detach();
                let _ = session.shutdown().await;
                return Err(DesktopBridgeError::Session {
                    message: "desktop session close could not drain terminal ProductEvents".into(),
                });
            }
            let _ = active.connection.detach();
            session.shutdown().await?;
            Ok(())
        }
        Ok(Err(_)) => {
            let _ = active.connection.detach();
            Err(DesktopBridgeError::Session {
                message: "desktop session prompt task stopped unexpectedly".into(),
            })
        }
        Err(_) => {
            active.task.abort();
            let _ = active.task.await;
            let _ = active.connection.detach();
            Err(DesktopBridgeError::Session {
                message: format!(
                    "prompt operation {} did not stop within {} seconds",
                    operation_id.as_deref().unwrap_or("<starting>"),
                    RUNTIME_SHUTDOWN_DEADLINE.as_secs_f64()
                ),
            })
        }
    }
}

pub(in crate::runtime) async fn shutdown_active_prompt(
    active: Option<ActivePrompt>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
) {
    shutdown_active_prompt_with_deadline(active, priority_updates, RUNTIME_SHUTDOWN_DEADLINE).await;
}

pub(in crate::runtime) async fn shutdown_active_prompt_with_deadline(
    active: Option<ActivePrompt>,
    priority_updates: &mpsc::Sender<DesktopRuntimeUpdate>,
    shutdown_deadline: Duration,
) {
    let Some(mut active) = active else {
        return;
    };
    let operation_id = active.operation_id.clone().or_else(|| {
        active
            .connection
            .state()
            .ok()
            .and_then(|snapshot| snapshot.submitted_operation)
            .map(|operation| operation.operation_id)
    });
    if let Some(operation_id) = operation_id.as_deref() {
        let control = active.connection.prompt_control(operation_id);
        let _ = control.abort(
            CodingAgentControlId("desktop-runtime-shutdown".into()),
            "desktop runtime shutdown",
        );
    }
    match tokio::time::timeout(shutdown_deadline, &mut active.task).await {
        Ok(Ok((mut session, _))) => {
            let _ = session.shutdown().await;
        }
        Ok(Err(_)) => {
            let _ = priority_updates
                .send(DesktopRuntimeUpdate::RuntimeFailed {
                    error: local_runtime_error(
                        "runtime_task_panicked",
                        "A desktop runtime task stopped unexpectedly.",
                    ),
                })
                .await;
        }
        Err(_) => {
            active.task.abort();
            let _ = active.task.await;
            let _ = priority_updates
                .send(DesktopRuntimeUpdate::RuntimeFailed {
                    error: DesktopRuntimeError {
                        code: "shutdown_deadline_exceeded".into(),
                        message: format!(
                            "prompt operation {} did not stop within {} seconds",
                            operation_id.as_deref().unwrap_or("<starting>"),
                            shutdown_deadline.as_secs_f64()
                        ),
                    },
                })
                .await;
        }
    }
    let _ = active.connection.detach();
}
