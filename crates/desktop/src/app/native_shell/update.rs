use desktop::projection::{ContextDirtyFlags, DesktopProjectionDelta};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ProjectionDirtyRouting {
    pub(super) root: bool,
    pub(super) conversation: bool,
    pub(super) composer: bool,
    pub(super) inspector_immediate: bool,
    pub(super) inspector_telemetry: bool,
    pub(super) status: bool,
    pub(super) conversation_header: bool,
    pub(super) overlay: bool,
    pub(super) sessions: bool,
    pub(super) file_changes: bool,
}

impl ProjectionDirtyRouting {
    pub(super) fn for_projection(replaced: bool, delta: Option<&DesktopProjectionDelta>) -> Self {
        let conversation = delta.is_some_and(|delta| delta.conversation || delta.tools);
        let authorizations = delta.is_some_and(|delta| delta.authorizations);
        let inspector_immediate = delta.is_some_and(inspector_projection_immediate_dirty);
        let inspector_telemetry = !inspector_immediate
            && delta.is_some_and(|delta| delta.context.contains(ContextDirtyFlags::USAGE));
        Self {
            root: replaced || authorizations,
            conversation,
            composer: replaced || authorizations,
            inspector_immediate: replaced || inspector_immediate,
            inspector_telemetry,
            status: replaced || delta.is_some_and(status_projection_dirty),
            conversation_header: replaced
                || delta.is_some_and(conversation_header_projection_dirty),
            overlay: replaced || delta.is_some_and(overlay_host_projection_dirty),
            sessions: replaced,
            file_changes: delta
                .is_some_and(|delta| delta.context.contains(ContextDirtyFlags::CHANGES)),
        }
    }
}

#[cfg(test)]
pub(super) fn inspector_projection_dirty(delta: &DesktopProjectionDelta) -> bool {
    inspector_projection_immediate_dirty(delta) || delta.context.contains(ContextDirtyFlags::USAGE)
}

#[cfg(test)]
pub(super) fn root_projection_dirty(
    replaced: bool,
    delta: Option<&DesktopProjectionDelta>,
) -> bool {
    ProjectionDirtyRouting::for_projection(replaced, delta).root
}

pub(super) fn inspector_projection_immediate_dirty(delta: &DesktopProjectionDelta) -> bool {
    delta.context.contains(ContextDirtyFlags::OPERATIONS)
        || delta.context.contains(ContextDirtyFlags::DELEGATIONS)
        || delta.context.contains(ContextDirtyFlags::CHANGES)
        || delta.diagnostics
        || delta.recoveries
        || delta.session
        || delta.profiles
        || delta.capabilities
        || delta.lifecycle
}

pub(super) fn status_projection_dirty(delta: &DesktopProjectionDelta) -> bool {
    delta.context.contains(ContextDirtyFlags::OPERATIONS)
        || delta.authorizations
        || delta.terminal
        || delta.recoveries
        || delta.session
        || delta.profiles
        || delta.capabilities
        || delta.lifecycle
}

pub(super) fn conversation_header_projection_dirty(delta: &DesktopProjectionDelta) -> bool {
    delta.context.contains(ContextDirtyFlags::OPERATIONS) || delta.lifecycle || delta.session
}

pub(super) fn overlay_host_projection_dirty(delta: &DesktopProjectionDelta) -> bool {
    conversation_header_projection_dirty(delta) || delta.authorizations
}
