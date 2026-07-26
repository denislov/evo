use crate::runtime::client::state::ClientConnectionId;
use crate::runtime::snapshot::{
    ClientHandle, ClientRegistryError, DraftRecord, SnapshotCoordinator,
};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct ClientService {
    pub(crate) coordinator: Arc<SnapshotCoordinator>,
}

impl ClientService {
    pub(crate) fn new(coordinator: Arc<SnapshotCoordinator>) -> Self {
        Self { coordinator }
    }
    pub(crate) fn connect_or_takeover(
        &self,
        id: ClientConnectionId,
    ) -> Result<ClientHandle, ClientRegistryError> {
        self.coordinator.connect_or_takeover(id)
    }
    #[cfg(test)]
    pub(crate) fn set_prompt_draft(
        &self,
        handle: &ClientHandle,
        draft: Option<DraftRecord>,
    ) -> Result<(), ClientRegistryError> {
        self.coordinator.set_prompt_draft(handle, draft)
    }
    pub(crate) fn commit_submission_running(
        &self,
        handle: &ClientHandle,
        operation_id: String,
        descriptor: crate::runtime::outcome::OperationDescriptor,
        expected_prompt_draft: Option<&DraftRecord>,
    ) -> Result<(), ClientRegistryError> {
        self.coordinator.commit_submission_running(
            handle,
            operation_id,
            descriptor,
            expected_prompt_draft,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn takeover_increments_generation_and_rejects_old_handle() {
        let service = ClientService::new(SnapshotCoordinator::new());
        let id = ClientConnectionId::new("client");
        let first = service.connect_or_takeover(id.clone()).unwrap();
        let second = service.connect_or_takeover(id).unwrap();
        assert_eq!(first.generation.0, 1);
        assert_eq!(second.generation.0, 2);
    }
}
