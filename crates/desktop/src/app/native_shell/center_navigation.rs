#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CenterNavigationTarget {
    NewConversation,
    Skills,
    Session(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum CenterSurface {
    #[default]
    Primary,
    Skills,
}
