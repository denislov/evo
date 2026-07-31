use std::{path::PathBuf, time::Duration};

use desktop::preferences::{DesktopPreferences, ExternalEditorPreference};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DesktopTimerKind {
    ConversationAnnouncement,
    ConversationHeightRefresh,
    ConversationWidthCommit,
    InspectorTelemetryRefresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClipboardFeedback {
    ConversationAnnouncement(String),
    Notice(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalEditorLaunchTarget {
    path: PathBuf,
    project_relative_path: String,
}

impl ExternalEditorLaunchTarget {
    pub(crate) fn new(path: PathBuf, project_relative_path: String) -> Self {
        Self {
            path,
            project_relative_path,
        }
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub(crate) fn project_relative_path(&self) -> &str {
        &self.project_relative_path
    }
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
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlatformResult {
    PathsPicked {
        identity: EffectIdentity,
        picker: DesktopPickerKind,
        outcome: PlatformOutcome<Vec<PathBuf>>,
    },
    ClipboardWritten {
        identity: EffectIdentity,
        outcome: PlatformOutcome<()>,
    },
    PreferencesWritten {
        identity: EffectIdentity,
        outcome: PlatformOutcome<()>,
    },
    ResyncRequested {
        identity: EffectIdentity,
        outcome: PlatformOutcome<()>,
    },
    ExternalEditorLaunched {
        identity: EffectIdentity,
        outcome: PlatformOutcome<()>,
    },
}

impl PlatformResult {
    pub(crate) const fn identity(&self) -> &EffectIdentity {
        match self {
            Self::PathsPicked { identity, .. }
            | Self::ClipboardWritten { identity, .. }
            | Self::PreferencesWritten { identity, .. }
            | Self::ResyncRequested { identity, .. }
            | Self::ExternalEditorLaunched { identity, .. } => identity,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DesktopEffect {
    PickPaths {
        identity: EffectIdentity,
        picker: DesktopPickerKind,
    },
    WriteClipboard {
        identity: EffectIdentity,
        text: Option<String>,
        feedback: ClipboardFeedback,
    },
    WritePreferences {
        identity: EffectIdentity,
        preferences: DesktopPreferences,
    },
    RequestResync {
        identity: EffectIdentity,
        command_id: u64,
    },
    LaunchExternalEditor {
        identity: EffectIdentity,
        command_id: u64,
        preference: ExternalEditorPreference,
        target: ExternalEditorLaunchTarget,
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
            | Self::WriteClipboard { identity, .. }
            | Self::WritePreferences { identity, .. }
            | Self::RequestResync { identity, .. }
            | Self::LaunchExternalEditor { identity, .. } => identity,
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
                Self::WriteClipboard { .. },
                PlatformResult::ClipboardWritten { .. }
            ) | (
                Self::WritePreferences { .. },
                PlatformResult::PreferencesWritten { .. }
            ) | (
                Self::RequestResync { .. },
                PlatformResult::ResyncRequested { .. }
            ) | (
                Self::LaunchExternalEditor { .. },
                PlatformResult::ExternalEditorLaunched { .. }
            )
        )
    }
}
