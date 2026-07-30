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
