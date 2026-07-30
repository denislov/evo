#![allow(
    dead_code,
    reason = "DSK-730 platform effect contract is consumed by the DSK-733 executor migration"
)]

use std::{path::PathBuf, time::Duration};

use super::workspace::WorkspaceKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct EffectRequestId(u64);

impl EffectRequestId {
    pub(super) const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EffectIdentity {
    request_id: EffectRequestId,
    owner: WorkspaceKey,
}

impl EffectIdentity {
    pub(super) const fn new(request_id: EffectRequestId, owner: WorkspaceKey) -> Self {
        Self { request_id, owner }
    }

    pub(crate) const fn request_id(&self) -> EffectRequestId {
        self.request_id
    }

    pub(crate) const fn owner(&self) -> &WorkspaceKey {
        &self.owner
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DesktopPickerKind {
    Attachments,
    ProjectDirectory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DesktopTimerKind {
    ConversationAnnouncement,
    ConversationHeightRefresh,
    ConversationWidthCommit,
    InspectorTelemetryRefresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopTimer {
    identity: EffectIdentity,
    kind: DesktopTimerKind,
}

impl DesktopTimer {
    pub(crate) const fn new(identity: EffectIdentity, kind: DesktopTimerKind) -> Self {
        Self { identity, kind }
    }

    pub(crate) const fn identity(&self) -> &EffectIdentity {
        &self.identity
    }

    pub(crate) const fn kind(&self) -> DesktopTimerKind {
        self.kind
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlatformOutcome<T> {
    Completed(T),
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlatformResult {
    PathsPicked {
        identity: EffectIdentity,
        picker: DesktopPickerKind,
        outcome: PlatformOutcome<Vec<PathBuf>>,
    },
    ExternalEditorOpened {
        identity: EffectIdentity,
        outcome: PlatformOutcome<()>,
    },
    PreferencesWritten {
        identity: EffectIdentity,
        outcome: PlatformOutcome<()>,
    },
}

impl PlatformResult {
    pub(crate) const fn identity(&self) -> &EffectIdentity {
        match self {
            Self::PathsPicked { identity, .. }
            | Self::ExternalEditorOpened { identity, .. }
            | Self::PreferencesWritten { identity, .. } => identity,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DesktopEffect {
    PickPaths {
        identity: EffectIdentity,
        picker: DesktopPickerKind,
    },
    OpenExternalEditor {
        identity: EffectIdentity,
    },
    WritePreferences {
        identity: EffectIdentity,
    },
    ScheduleTimer {
        timer: DesktopTimer,
        delay: Duration,
    },
}

impl DesktopEffect {
    pub(crate) const fn identity(&self) -> &EffectIdentity {
        match self {
            Self::PickPaths { identity, .. }
            | Self::OpenExternalEditor { identity }
            | Self::WritePreferences { identity } => identity,
            Self::ScheduleTimer { timer, .. } => timer.identity(),
        }
    }

    pub(crate) fn matches_platform_result(&self, result: &PlatformResult) -> bool {
        if self.identity() != result.identity() {
            return false;
        }
        matches!(
            (self, result),
            (
                Self::PickPaths { picker: expected, .. },
                PlatformResult::PathsPicked { picker: actual, .. }
            ) if expected == actual
        ) || matches!(
            (self, result),
            (
                Self::OpenExternalEditor { .. },
                PlatformResult::ExternalEditorOpened { .. }
            ) | (
                Self::WritePreferences { .. },
                PlatformResult::PreferencesWritten { .. }
            )
        )
    }
}
