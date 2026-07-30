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
    pub(crate) fn commit_submission_running(
        &self,
        handle: &ClientHandle,
        operation_id: String,
        descriptor: crate::runtime::operation::contract::OperationDescriptor,
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
