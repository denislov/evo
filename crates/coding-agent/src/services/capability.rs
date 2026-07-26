use crate::runtime::control::OperationActivity;
use crate::runtime::facade::CodingAgentCapabilities;

#[derive(Debug)]
pub(crate) struct CapabilityService;

impl CapabilityService {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn capabilities(
        &self,
        activity: &OperationActivity,
        persistent_session: bool,
    ) -> CodingAgentCapabilities {
        CodingAgentCapabilities::from_runtime_state(activity, persistent_session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::control::{OperationActivity, OperationKind};
    use crate::runtime::facade::CapabilityStatus;

    fn idle() -> OperationActivity {
        OperationActivity::for_tests(None, Vec::new(), None, 4)
    }

    #[test]
    fn capabilities_report_prompt_available_when_idle() {
        let capabilities = CapabilityService::new().capabilities(&idle(), true);

        assert_eq!(capabilities.prompt, CapabilityStatus::Available);
        assert_eq!(capabilities.branch_summary, CapabilityStatus::Available);
        assert_eq!(capabilities.agent_profiles, CapabilityStatus::Available);
        assert_eq!(capabilities.team_profiles, CapabilityStatus::Available);
        assert_eq!(capabilities.delegation, CapabilityStatus::Available);
        assert_eq!(capabilities.tools, CapabilityStatus::Available);
        assert_eq!(capabilities.shell, CapabilityStatus::Available);
    }

    #[test]
    fn capabilities_report_prompt_busy_for_active_operation() {
        let capabilities = CapabilityService::new().capabilities(
            &OperationActivity::for_tests(Some(OperationKind::Prompt), Vec::new(), None, 4),
            true,
        );

        assert_eq!(
            capabilities.prompt,
            CapabilityStatus::Busy {
                operation: "prompt".into(),
            }
        );
    }

    #[test]
    fn capabilities_disable_prompt_controls_when_no_prompt_is_running() {
        let capabilities = CapabilityService::new().capabilities(&idle(), true);

        assert_eq!(
            capabilities.abort,
            CapabilityStatus::Disabled {
                reason: "no cancellable operation is running".into(),
            }
        );
        for capability in [capabilities.steer, capabilities.follow_up] {
            assert_eq!(
                capability,
                CapabilityStatus::Disabled {
                    reason: "no prompt is running".into(),
                }
            );
        }
    }

    #[test]
    fn capabilities_enable_prompt_controls_only_for_running_prompt() {
        let prompt_capabilities = CapabilityService::new().capabilities(
            &OperationActivity::for_tests(Some(OperationKind::Prompt), Vec::new(), None, 4),
            true,
        );

        assert_eq!(prompt_capabilities.abort, CapabilityStatus::Available);
        assert_eq!(prompt_capabilities.steer, CapabilityStatus::Available);
        assert_eq!(prompt_capabilities.follow_up, CapabilityStatus::Available);
    }

    #[test]
    fn capabilities_follow_operation_class_concurrency_rules() {
        let capabilities = CapabilityService::new().capabilities(
            &OperationActivity::for_tests(Some(OperationKind::BranchSummary), Vec::new(), None, 4),
            true,
        );

        for capability in [
            capabilities.compact,
            capabilities.fork,
            capabilities.branch_summary,
            capabilities.self_healing_edit,
        ] {
            assert_eq!(
                capability,
                CapabilityStatus::Busy {
                    operation: "branch_summary".into(),
                }
            );
        }
        for capability in [
            capabilities.clone_session,
            capabilities.export,
            capabilities.agent_profiles,
            capabilities.team_profiles,
            capabilities.delegation,
        ] {
            assert_eq!(capability, CapabilityStatus::Available);
        }
    }

    #[test]
    fn capabilities_disable_persistent_session_operations_without_persistence() {
        let capabilities = CapabilityService::new().capabilities(&idle(), false);

        assert_eq!(
            capabilities.export,
            CapabilityStatus::Disabled {
                reason: "requires persistent Rust-native session".into(),
            }
        );
        assert_eq!(
            capabilities.compact,
            CapabilityStatus::Disabled {
                reason: "requires persistent Rust-native session".into(),
            }
        );
        assert_eq!(
            capabilities.clone_session,
            CapabilityStatus::Disabled {
                reason: "requires persistent Rust-native session".into(),
            }
        );
        assert_eq!(
            capabilities.branch_summary,
            CapabilityStatus::Disabled {
                reason: "requires persistent Rust-native session".into(),
            }
        );
        assert_eq!(
            capabilities.self_healing_edit,
            CapabilityStatus::Disabled {
                reason: "requires persistent Rust-native session".into(),
            }
        );
    }

    #[test]
    fn self_healing_edit_operation_kind_reports_stable_name() {
        assert_eq!(OperationKind::SelfHealingEdit.as_str(), "self_healing_edit");
    }
}
