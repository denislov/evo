#![allow(
    dead_code,
    reason = "DSK-730 contract regions are consumed incrementally through DSK-743"
)]

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
    Modal,
    Toast,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UiChangeSet(u16);

impl UiChangeSet {
    pub(crate) const fn one(region: UiRegion) -> Self {
        Self(1 << region as u8)
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
}

#[cfg(test)]
mod tests {
    use super::{UiChangeSet, UiRegion};

    #[test]
    fn merge_is_idempotent_and_preserves_every_changed_region() {
        let mut changes = UiChangeSet::one(UiRegion::Conversation);
        changes.insert(UiRegion::Composer);
        changes.merge(UiChangeSet::one(UiRegion::Sessions));
        changes.merge(UiChangeSet::one(UiRegion::Conversation));

        assert!(changes.contains(UiRegion::Conversation));
        assert!(changes.contains(UiRegion::Composer));
        assert!(changes.contains(UiRegion::Sessions));
        assert!(!changes.contains(UiRegion::Inspector));
        assert!(!changes.is_empty());
    }
}
