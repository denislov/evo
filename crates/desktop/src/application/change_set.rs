use desktop::projection::{ContextDirtyFlags, DesktopProjectionDelta};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum UiRegion {
    Root,
    Conversation,
    ConversationHeader,
    Composer,
    Sessions,
    Inspector,
    InspectorTelemetry,
    Skills,
    Modal,
    Drawer,
    Toast,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UiChangeSet(u16);

impl UiChangeSet {
    pub(crate) const fn one(region: UiRegion) -> Self {
        Self(1 << region as u8)
    }

    pub(crate) fn from_regions(regions: &[UiRegion]) -> Self {
        let mut changes = Self::default();
        for &region in regions {
            changes.insert(region);
        }
        changes
    }

    pub(crate) const fn contains(self, region: UiRegion) -> bool {
        self.0 & (1 << region as u8) != 0
    }

    pub(crate) fn insert(&mut self, region: UiRegion) {
        self.0 |= 1 << region as u8;
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub(crate) fn for_projection(replaced: bool, delta: Option<&DesktopProjectionDelta>) -> Self {
        let conversation = delta.is_some_and(|delta| delta.conversation || delta.tools);
        let authorizations = delta.is_some_and(|delta| delta.authorizations);
        let inspector_immediate = delta.is_some_and(|delta| {
            delta.context.contains(ContextDirtyFlags::OPERATIONS)
                || delta.context.contains(ContextDirtyFlags::DELEGATIONS)
                || delta.context.contains(ContextDirtyFlags::CHANGES)
                || delta.diagnostics
                || delta.recoveries
                || delta.session
                || delta.profiles
                || delta.capabilities
                || delta.lifecycle
        });
        let mut changes = Self::default();
        if replaced || authorizations {
            changes.insert(UiRegion::Root);
            changes.insert(UiRegion::Composer);
        }
        if conversation {
            changes.insert(UiRegion::Conversation);
        }
        if replaced || inspector_immediate {
            changes.insert(UiRegion::Inspector);
        } else if delta.is_some_and(|delta| delta.context.contains(ContextDirtyFlags::USAGE)) {
            changes.insert(UiRegion::InspectorTelemetry);
        }
        if replaced
            || delta.is_some_and(|delta| {
                delta.context.contains(ContextDirtyFlags::OPERATIONS)
                    || delta.lifecycle
                    || delta.session
            })
        {
            changes.insert(UiRegion::ConversationHeader);
        }
        if replaced
            || delta.is_some_and(|delta| {
                delta.context.contains(ContextDirtyFlags::OPERATIONS)
                    || delta.lifecycle
                    || delta.session
                    || delta.authorizations
            })
        {
            changes.insert(UiRegion::Modal);
        }
        if replaced {
            changes.insert(UiRegion::Sessions);
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::{UiChangeSet, UiRegion};
    use desktop::projection::{ContextDirtyFlags, DesktopProjectionDelta};

    #[test]
    fn merge_is_idempotent_and_preserves_every_changed_region() {
        let mut changes = UiChangeSet::from_regions(&[UiRegion::Conversation, UiRegion::Composer]);
        changes.merge(UiChangeSet::one(UiRegion::Sessions));
        changes.merge(UiChangeSet::one(UiRegion::Conversation));

        assert!(changes.contains(UiRegion::Conversation));
        assert!(changes.contains(UiRegion::Composer));
        assert!(changes.contains(UiRegion::Sessions));
        assert!(!changes.contains(UiRegion::Inspector));
        assert!(!changes.is_empty());
    }

    #[test]
    fn projection_delta_maps_to_typed_regions_without_root_over_invalidation() {
        let streaming = DesktopProjectionDelta {
            conversation: true,
            ..DesktopProjectionDelta::default()
        };
        let streaming_changes = UiChangeSet::for_projection(false, Some(&streaming));
        assert!(streaming_changes.contains(UiRegion::Conversation));
        assert!(!streaming_changes.contains(UiRegion::Root));
        assert!(!streaming_changes.contains(UiRegion::Inspector));

        let authorization = DesktopProjectionDelta {
            authorizations: true,
            ..DesktopProjectionDelta::default()
        };
        let authorization_changes = UiChangeSet::for_projection(false, Some(&authorization));
        assert!(authorization_changes.contains(UiRegion::Root));
        assert!(authorization_changes.contains(UiRegion::Modal));

        let usage = DesktopProjectionDelta {
            context: ContextDirtyFlags::USAGE,
            ..DesktopProjectionDelta::default()
        };
        let usage_changes = UiChangeSet::for_projection(false, Some(&usage));
        assert!(usage_changes.contains(UiRegion::InspectorTelemetry));
        assert!(!usage_changes.contains(UiRegion::Inspector));
    }
}
