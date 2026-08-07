//! Effect executor: pending-effect identity tracking, platform-result
//! reduction, and foreground change routing.

use std::{collections::HashMap, path::PathBuf, time::Duration};

use desktop::preferences::{DesktopPreferences, ExternalEditorPreference};
use desktop::runtime::DesktopRuntimeUpdate;

use super::{DesktopEvent, EffectIdentityError, Transition, runtime::reduce_runtime_update};
use crate::application::{
    catalog::ProjectCatalogController,
    change_set::{UiChangeSet, UiRegion},
    effect::{
        ClipboardFeedback, DesktopEffect, DesktopPickerKind, DesktopTimer, DesktopTimerKind,
        EffectIdentity, EffectRequestId, ExternalEditorLaunchTarget, PlatformOutcome,
        PlatformResult,
    },
    runtime_state::RuntimeWorkspacePresentation,
    state::DesktopState,
    workspace::WorkspaceKey,
    workspace_state::{RuntimeWorkspaceDefaults, WorkspaceState},
};

pub(crate) trait PlatformUpdatePort {
    fn active_workspace_key(&self) -> WorkspaceKey;
    fn workspace_exists(&self, owner: &WorkspaceKey) -> bool;
    fn project_directory_editable(&self, owner: &WorkspaceKey) -> bool;
    fn set_project_directory(&mut self, owner: &WorkspaceKey, path: PathBuf) -> bool;
    fn add_composer_attachments(
        &mut self,
        owner: &WorkspaceKey,
        paths: Vec<PathBuf>,
    ) -> Result<bool, String>;
    fn set_notice(&mut self, owner: &WorkspaceKey, notice: String);
    fn show_conversation_announcement(&mut self, owner: &WorkspaceKey, message: String);
    fn clear_conversation_announcement(&mut self, owner: &WorkspaceKey) -> bool;
    fn commit_conversation_width(&mut self, owner: &WorkspaceKey) -> bool;
    fn refresh_inspector_telemetry(&mut self, owner: &WorkspaceKey) -> bool;
    fn complete_resync_admission(
        &mut self,
        owner: &WorkspaceKey,
        command_id: u64,
        failure: Option<String>,
    );
    fn complete_external_editor_launch(
        &mut self,
        owner: &WorkspaceKey,
        command_id: u64,
        project_relative_path: &str,
        failure: Option<String>,
    );
}

pub(crate) struct DesktopController {
    next_effect_request_id: u64,
    pending_effects: HashMap<EffectRequestId, DesktopEffect>,
}

impl DesktopController {
    pub(crate) fn new() -> Self {
        Self {
            next_effect_request_id: 0,
            pending_effects: HashMap::new(),
        }
    }

    pub(crate) fn reduce_runtime<Presentation: RuntimeWorkspacePresentation>(
        &mut self,
        state: &mut DesktopState<
            WorkspaceState<Presentation>,
            ProjectCatalogController,
            RuntimeWorkspaceDefaults,
        >,
        update: DesktopRuntimeUpdate,
    ) -> Transition {
        reduce_runtime_update(self, state, update)
    }

    /// Route an event through one mutable application-state authority while
    /// feature branches are migrated from the GPUI adapter in later tasks.
    pub(crate) fn reduce<Workspace, Catalog, WorkspaceDefaults>(
        &mut self,
        state: &mut DesktopState<Workspace, Catalog, WorkspaceDefaults>,
        event: DesktopEvent,
        delegate: impl FnOnce(
            &mut DesktopState<Workspace, Catalog, WorkspaceDefaults>,
            DesktopEvent,
        ) -> Transition,
    ) -> Transition {
        delegate(state, event)
    }

    pub(crate) fn reserve_effect_identity(
        &mut self,
        owner: WorkspaceKey,
    ) -> Result<EffectIdentity, EffectIdentityError> {
        let request_id = self.next_effect_request_id;
        self.next_effect_request_id = self
            .next_effect_request_id
            .checked_add(1)
            .ok_or(EffectIdentityError::Exhausted)?;
        Ok(EffectIdentity::new(EffectRequestId::new(request_id), owner))
    }

    pub(crate) fn pick_paths(
        &mut self,
        owner: WorkspaceKey,
        picker: DesktopPickerKind,
    ) -> Result<Transition, EffectIdentityError> {
        let identity = self.reserve_effect_identity(owner)?;
        Ok(self.register_effect(DesktopEffect::PickPaths { identity, picker }))
    }

    pub(crate) fn write_clipboard(
        &mut self,
        owner: WorkspaceKey,
        text: Option<String>,
        feedback: ClipboardFeedback,
    ) -> Result<Transition, EffectIdentityError> {
        let identity = self.reserve_effect_identity(owner)?;
        Ok(self.register_effect(DesktopEffect::WriteClipboard {
            identity,
            text,
            feedback,
        }))
    }

    pub(crate) fn write_preferences(
        &mut self,
        owner: WorkspaceKey,
        preferences: DesktopPreferences,
    ) -> Result<Transition, EffectIdentityError> {
        let identity = self.reserve_effect_identity(owner)?;
        Ok(self.register_effect(DesktopEffect::WritePreferences {
            identity,
            preferences,
        }))
    }

    pub(crate) fn request_resync(
        &mut self,
        owner: WorkspaceKey,
        command_id: u64,
    ) -> Result<Transition, EffectIdentityError> {
        let identity = self.reserve_effect_identity(owner)?;
        Ok(self.register_effect(DesktopEffect::RequestResync {
            identity,
            command_id,
        }))
    }

    pub(crate) fn launch_external_editor(
        &mut self,
        owner: WorkspaceKey,
        command_id: u64,
        preference: ExternalEditorPreference,
        target: ExternalEditorLaunchTarget,
    ) -> Result<Transition, EffectIdentityError> {
        let identity = self.reserve_effect_identity(owner)?;
        Ok(self.register_effect(DesktopEffect::LaunchExternalEditor {
            identity,
            command_id,
            preference,
            target,
        }))
    }

    pub(crate) fn schedule_timer(
        &mut self,
        owner: WorkspaceKey,
        kind: DesktopTimerKind,
        delay: Duration,
    ) -> Result<Transition, EffectIdentityError> {
        let identity = self.reserve_effect_identity(owner)?;
        Ok(self.register_effect(DesktopEffect::ScheduleTimer {
            timer: DesktopTimer::new(identity, kind),
            delay,
        }))
    }

    pub(crate) fn reduce_platform(
        &mut self,
        port: &mut impl PlatformUpdatePort,
        result: PlatformResult,
    ) -> Transition {
        let request_id = result.identity().request_id();
        let Some(effect) = self.pending_effects.get(&request_id) else {
            return Transition::default();
        };
        if !effect.matches_platform_result(&result) {
            return Transition::default();
        }
        let effect = self
            .pending_effects
            .remove(&request_id)
            .expect("a matching pending effect must still exist");
        reduce_platform_result(self, port, effect, result)
    }

    pub(crate) fn reduce_async(
        &mut self,
        port: &mut impl PlatformUpdatePort,
        event: DesktopEvent,
    ) -> Transition {
        match event {
            DesktopEvent::Platform(result) => self.reduce_platform(port, result),
            DesktopEvent::Timer(timer) => self.reduce_timer(port, timer),
            DesktopEvent::Ui(_) | DesktopEvent::Preferences(_) => {
                debug_assert!(false, "UI intents use their typed feature reducer");
                Transition::default()
            }
        }
    }

    pub(crate) fn reduce_timer(
        &mut self,
        port: &mut impl PlatformUpdatePort,
        timer: DesktopTimer,
    ) -> Transition {
        let request_id = timer.identity().request_id();
        let Some(DesktopEffect::ScheduleTimer {
            timer: expected, ..
        }) = self.pending_effects.get(&request_id)
        else {
            return Transition::default();
        };
        if expected != &timer {
            return Transition::default();
        }
        self.pending_effects.remove(&request_id);
        reduce_timer_result(port, timer)
    }

    fn register_effect(&mut self, effect: DesktopEffect) -> Transition {
        self.pending_effects
            .retain(|_, pending| !effect_supersedes(&effect, pending));
        self.pending_effects
            .insert(effect.identity().request_id(), effect.clone());
        Transition::default().with_effect(effect)
    }
}

impl Default for DesktopController {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn effect_supersedes(next: &DesktopEffect, pending: &DesktopEffect) -> bool {
    match (next, pending) {
        (
            DesktopEffect::PickPaths {
                identity: next_identity,
                picker: next_picker,
            },
            DesktopEffect::PickPaths {
                identity: pending_identity,
                picker: pending_picker,
            },
        ) => next_identity.owner() == pending_identity.owner() && next_picker == pending_picker,
        (DesktopEffect::WritePreferences { .. }, DesktopEffect::WritePreferences { .. }) => true,
        (
            DesktopEffect::RequestResync {
                identity: next_identity,
                ..
            },
            DesktopEffect::RequestResync {
                identity: pending_identity,
                ..
            },
        ) => next_identity.owner() == pending_identity.owner(),
        (
            DesktopEffect::LaunchExternalEditor {
                identity: next_identity,
                ..
            },
            DesktopEffect::LaunchExternalEditor {
                identity: pending_identity,
                ..
            },
        ) => next_identity.owner() == pending_identity.owner(),
        (
            DesktopEffect::ScheduleTimer {
                timer: next_timer, ..
            },
            DesktopEffect::ScheduleTimer {
                timer: pending_timer,
                ..
            },
        ) => next_timer.kind() == pending_timer.kind(),
        _ => false,
    }
}

pub(crate) fn reduce_platform_result(
    controller: &mut DesktopController,
    port: &mut impl PlatformUpdatePort,
    effect: DesktopEffect,
    result: PlatformResult,
) -> Transition {
    let owner = effect.identity().owner().clone();
    if !port.workspace_exists(&owner) {
        return Transition::default();
    }
    match (effect, result) {
        (DesktopEffect::PickPaths { picker, .. }, PlatformResult::PathsPicked { outcome, .. }) => {
            reduce_paths_picked(port, &owner, picker, outcome)
        }
        (
            DesktopEffect::WriteClipboard { feedback, .. },
            PlatformResult::ClipboardWritten { outcome, .. },
        ) => match outcome {
            PlatformOutcome::Completed(()) => match feedback {
                ClipboardFeedback::ConversationAnnouncement(message) => {
                    port.show_conversation_announcement(&owner, message);
                    let mut transition = foreground_transition(port, &owner, UiRegion::Root);
                    if let Ok(timer) = controller.schedule_timer(
                        owner,
                        DesktopTimerKind::ConversationAnnouncement,
                        Duration::from_secs(2),
                    ) {
                        transition.merge(timer);
                    }
                    transition
                }
                ClipboardFeedback::Notice(message) => {
                    port.set_notice(&owner, message);
                    foreground_notice_transition(port, &owner)
                }
            },
            PlatformOutcome::Cancelled => Transition::default(),
            PlatformOutcome::Failed(message) => {
                port.set_notice(&owner, message);
                foreground_notice_transition(port, &owner)
            }
        },
        (
            DesktopEffect::WritePreferences { .. },
            PlatformResult::PreferencesWritten { outcome, .. },
        ) => match outcome {
            PlatformOutcome::Completed(()) | PlatformOutcome::Cancelled => Transition::default(),
            PlatformOutcome::Failed(message) => {
                port.set_notice(&owner, message);
                foreground_notice_transition(port, &owner)
            }
        },
        (
            DesktopEffect::RequestResync { command_id, .. },
            PlatformResult::ResyncRequested { outcome, .. },
        ) => {
            let failure = match outcome {
                PlatformOutcome::Completed(()) => None,
                PlatformOutcome::Cancelled => Some("desktop resync request was cancelled".into()),
                PlatformOutcome::Failed(message) => Some(message),
            };
            let failed = failure.is_some();
            port.complete_resync_admission(&owner, command_id, failure);
            if failed {
                foreground_notice_transition(port, &owner)
            } else {
                Transition::default()
            }
        }
        (
            DesktopEffect::LaunchExternalEditor {
                command_id, target, ..
            },
            PlatformResult::ExternalEditorLaunched { outcome, .. },
        ) => {
            let project_relative_path = target.project_relative_path().to_owned();
            let failure = match outcome {
                PlatformOutcome::Completed(()) => None,
                PlatformOutcome::Cancelled => Some("external editor launch was cancelled".into()),
                PlatformOutcome::Failed(message) => Some(message),
            };
            port.complete_external_editor_launch(
                &owner,
                command_id,
                &project_relative_path,
                failure,
            );
            foreground_changes(
                port,
                &owner,
                &[UiRegion::Inspector, UiRegion::Root, UiRegion::Toast],
            )
        }
        _ => unreachable!("platform result was matched to its exact pending effect"),
    }
}

pub(crate) fn reduce_paths_picked(
    port: &mut impl PlatformUpdatePort,
    owner: &WorkspaceKey,
    picker: DesktopPickerKind,
    outcome: PlatformOutcome<Vec<PathBuf>>,
) -> Transition {
    let paths = match outcome {
        PlatformOutcome::Completed(paths) => paths,
        PlatformOutcome::Cancelled => return Transition::default(),
        PlatformOutcome::Failed(message) => {
            port.set_notice(owner, message);
            return foreground_notice_transition(port, owner);
        }
    };
    match picker {
        DesktopPickerKind::ProjectDirectory => {
            if !port.project_directory_editable(owner) {
                return Transition::default();
            }
            let mut paths = paths.into_iter();
            let Some(path) = paths.next() else {
                port.set_notice(
                    owner,
                    "The directory picker returned no project directory.".into(),
                );
                return foreground_notice_transition(port, owner);
            };
            if paths.next().is_some() {
                port.set_notice(
                    owner,
                    "The directory picker returned more than one project directory.".into(),
                );
                return foreground_notice_transition(port, owner);
            }
            if port.set_project_directory(owner, path) {
                foreground_changes(
                    port,
                    owner,
                    &[
                        UiRegion::Root,
                        UiRegion::ConversationHeader,
                        UiRegion::Composer,
                    ],
                )
            } else {
                Transition::default()
            }
        }
        DesktopPickerKind::Attachments => match port.add_composer_attachments(owner, paths) {
            Ok(true) => foreground_changes(port, owner, &[UiRegion::Root, UiRegion::Composer]),
            Ok(false) => Transition::default(),
            Err(message) => {
                port.set_notice(owner, message);
                foreground_notice_transition(port, owner)
            }
        },
    }
}

pub(crate) fn reduce_timer_result(
    port: &mut impl PlatformUpdatePort,
    timer: DesktopTimer,
) -> Transition {
    let owner = timer.identity().owner();
    let (changed, region) = match timer.kind() {
        DesktopTimerKind::ConversationAnnouncement => {
            (port.clear_conversation_announcement(owner), UiRegion::Root)
        }
        DesktopTimerKind::ConversationWidthCommit => {
            (port.commit_conversation_width(owner), UiRegion::Root)
        }
        DesktopTimerKind::InspectorTelemetryRefresh => {
            (port.refresh_inspector_telemetry(owner), UiRegion::Inspector)
        }
    };
    if changed {
        foreground_transition(port, owner, region)
    } else {
        Transition::default()
    }
}

pub(crate) fn foreground_notice_transition(
    port: &impl PlatformUpdatePort,
    owner: &WorkspaceKey,
) -> Transition {
    foreground_changes(port, owner, &[UiRegion::Root, UiRegion::Toast])
}

pub(crate) fn foreground_transition(
    port: &impl PlatformUpdatePort,
    owner: &WorkspaceKey,
    region: UiRegion,
) -> Transition {
    foreground_changes(port, owner, &[region])
}

pub(crate) fn foreground_changes(
    port: &impl PlatformUpdatePort,
    owner: &WorkspaceKey,
    regions: &[UiRegion],
) -> Transition {
    if &port.active_workspace_key() != owner {
        return Transition::default();
    }
    let mut changes = UiChangeSet::default();
    for region in regions {
        changes.insert(*region);
    }
    Transition::from_changes(changes)
}
