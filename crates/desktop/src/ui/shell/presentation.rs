use crate::projection::{DesktopProjection, DesktopProjectionLifecycle, DesktopRecoveryStatus};
use crate::runtime::DesktopRecoveryAction;
use crate::ui::shell::SemanticStatus;

pub(crate) fn semantic_status(projection: Option<&DesktopProjection>) -> SemanticStatus {
    let Some(projection) = projection else {
        return SemanticStatus::Idle;
    };
    match projection.lifecycle() {
        DesktopProjectionLifecycle::Failed | DesktopProjectionLifecycle::NeedsResync => {
            SemanticStatus::Error
        }
        DesktopProjectionLifecycle::Stopped => SemanticStatus::Warning,
        DesktopProjectionLifecycle::Running
            if !projection.snapshot().pending_authorizations.is_empty() =>
        {
            SemanticStatus::Authorization
        }
        DesktopProjectionLifecycle::Running if projection.snapshot().active_operation.is_some() => {
            SemanticStatus::Running
        }
        DesktopProjectionLifecycle::Running => SemanticStatus::Idle,
    }
}

pub(crate) const fn runtime_state_label(
    lifecycle: DesktopProjectionLifecycle,
    operation_active: bool,
) -> &'static str {
    match (lifecycle, operation_active) {
        (DesktopProjectionLifecycle::Running, true) => "connected · active",
        (DesktopProjectionLifecycle::Running, false) => "connected · idle",
        (DesktopProjectionLifecycle::NeedsResync, _) => "resync required",
        (DesktopProjectionLifecycle::Failed, _) => "failed",
        (DesktopProjectionLifecycle::Stopped, _) => "stopped",
    }
}

pub(crate) const fn recovery_status_label(status: DesktopRecoveryStatus) -> &'static str {
    match status {
        DesktopRecoveryStatus::Pending => "pending",
        DesktopRecoveryStatus::Resolved => "resolved",
        DesktopRecoveryStatus::Recovered => "recovered",
    }
}

pub(crate) const fn recovery_action_label(action: DesktopRecoveryAction) -> &'static str {
    match action {
        DesktopRecoveryAction::Retry => "retry",
        DesktopRecoveryAction::MarkFailed => "mark-failed",
        DesktopRecoveryAction::Abort => "abort",
    }
}

pub(crate) fn usage_cost_label(cost: Option<f64>) -> String {
    cost.filter(|cost| cost.is_finite() && *cost >= 0.0)
        .map(|cost| format!("${cost:.4}"))
        .unwrap_or_else(|| "—".into())
}
