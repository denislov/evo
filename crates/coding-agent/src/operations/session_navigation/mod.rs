use crate::runtime::facade::{
    CodingAgentSessionHydration, CodingAgentSessionOptions, CodingAgentSessionTree,
    CodingSessionError,
};
use crate::session::service::SessionService;

pub(crate) fn hydrate(
    options: CodingAgentSessionOptions,
) -> Result<CodingAgentSessionHydration, CodingSessionError> {
    SessionService::hydrate(&options)
}

pub(crate) fn tree_view(
    options: CodingAgentSessionOptions,
) -> Result<CodingAgentSessionTree, CodingSessionError> {
    SessionService::tree_view(&options)
}

pub(crate) fn clone_session(
    options: CodingAgentSessionOptions,
) -> Result<CodingAgentSessionHydration, CodingSessionError> {
    SessionService::open(&options)?
        .clone_current()?
        .hydrated_view()
}

pub(crate) fn fork_session(
    options: CodingAgentSessionOptions,
    target_leaf_id: Option<&str>,
) -> Result<CodingAgentSessionHydration, CodingSessionError> {
    SessionService::open(&options)?
        .fork_current(target_leaf_id)?
        .hydrated_view()
}
