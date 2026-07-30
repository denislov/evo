#![allow(
    dead_code,
    reason = "DSK-730 root event branches are consumed incrementally by DSK-731 and DSK-733"
)]

use desktop::runtime::DesktopRuntimeUpdate;
use thiserror::Error;

use super::{
    change_set::{UiChangeSet, UiRegion},
    effect::{DesktopEffect, DesktopTimer, EffectIdentity, EffectRequestId, PlatformResult},
    state::DesktopState,
    workspace::WorkspaceKey,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UiIntent {
    SetProjectCollapsed { group_id: String, collapsed: bool },
}

#[derive(Debug, Clone)]
pub(crate) enum DesktopEvent {
    Ui(UiIntent),
    Runtime(Box<DesktopRuntimeUpdate>),
    Platform(PlatformResult),
    Timer(DesktopTimer),
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Transition {
    changes: UiChangeSet,
    effects: Vec<DesktopEffect>,
}

impl Transition {
    pub(crate) const fn changed(region: UiRegion) -> Self {
        Self {
            changes: UiChangeSet::one(region),
            effects: Vec::new(),
        }
    }

    pub(crate) fn with_effect(mut self, effect: DesktopEffect) -> Self {
        self.effects.push(effect);
        self
    }

    pub(crate) const fn changes(&self) -> UiChangeSet {
        self.changes
    }

    pub(crate) fn effects(&self) -> &[DesktopEffect] {
        &self.effects
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.changes.merge(other.changes);
        self.effects.extend(other.effects);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum EffectIdentityError {
    #[error("desktop effect request id space is exhausted")]
    Exhausted,
}

pub(crate) struct DesktopController {
    next_effect_request_id: u64,
}

impl DesktopController {
    pub(crate) const fn new() -> Self {
        Self {
            next_effect_request_id: 0,
        }
    }

    /// Route an event through one mutable application-state authority while
    /// feature branches are migrated from the GPUI adapter in later tasks.
    pub(crate) fn reduce<Workspace, Catalog>(
        &mut self,
        state: &mut DesktopState<Workspace, Catalog>,
        event: DesktopEvent,
        delegate: impl FnOnce(&mut DesktopState<Workspace, Catalog>, DesktopEvent) -> Transition,
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
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use desktop::preferences::DesktopPreferences;

    use super::{DesktopController, DesktopEvent, Transition, UiIntent};
    use crate::application::{
        change_set::UiRegion,
        commands::CommandTracker,
        effect::{
            DesktopEffect, DesktopPickerKind, DesktopTimer, DesktopTimerKind, EffectIdentity,
            PlatformOutcome, PlatformResult,
        },
        state::DesktopState,
        workspace::{WorkspaceKey, WorkspaceStore},
    };

    fn state() -> DesktopState<&'static str, Vec<String>> {
        DesktopState::new(
            WorkspaceStore::new("home"),
            CommandTracker::default(),
            Vec::new(),
            DesktopPreferences::default(),
        )
    }

    #[test]
    fn delegated_reduce_mutates_the_single_state_and_returns_typed_changes() {
        let mut controller = DesktopController::new();
        let mut state = state();
        let transition = controller.reduce(
            &mut state,
            DesktopEvent::Ui(UiIntent::SetProjectCollapsed {
                group_id: "project:alpha".into(),
                collapsed: true,
            }),
            |state, event| {
                let DesktopEvent::Ui(UiIntent::SetProjectCollapsed { group_id, .. }) = event else {
                    panic!("test event must remain typed");
                };
                state.catalog.push(group_id);
                Transition::changed(UiRegion::Sessions)
            },
        );

        assert_eq!(state.catalog, ["project:alpha"]);
        assert!(transition.changes().contains(UiRegion::Sessions));
    }

    #[test]
    fn platform_results_require_kind_request_id_and_owner_identity() {
        let mut controller = DesktopController::new();
        let owner = WorkspaceKey::Home;
        let identity = controller.reserve_effect_identity(owner.clone()).unwrap();
        let same_kind = DesktopEffect::PickPaths {
            identity: identity.clone(),
            picker: DesktopPickerKind::Attachments,
        };
        let matching = PlatformResult::PathsPicked {
            identity: identity.clone(),
            picker: DesktopPickerKind::Attachments,
            outcome: PlatformOutcome::Completed(vec![PathBuf::from("image.png")]),
        };
        assert!(same_kind.matches_platform_result(&matching));

        let wrong_kind = DesktopEffect::PickPaths {
            identity: identity.clone(),
            picker: DesktopPickerKind::ProjectDirectory,
        };
        assert!(!wrong_kind.matches_platform_result(&matching));

        let wrong_request = PlatformResult::PathsPicked {
            identity: controller.reserve_effect_identity(owner).unwrap(),
            picker: DesktopPickerKind::Attachments,
            outcome: PlatformOutcome::Cancelled,
        };
        assert!(!same_kind.matches_platform_result(&wrong_request));

        let different_owner =
            EffectIdentity::new(identity.request_id(), WorkspaceKey::session("session-b"));
        let wrong_owner = PlatformResult::PathsPicked {
            identity: different_owner,
            picker: DesktopPickerKind::Attachments,
            outcome: PlatformOutcome::Failed,
        };
        assert!(!same_kind.matches_platform_result(&wrong_owner));

        let editor = DesktopEffect::OpenExternalEditor {
            identity: identity.clone(),
        };
        let editor_result = PlatformResult::ExternalEditorOpened {
            identity: identity.clone(),
            outcome: PlatformOutcome::Completed(()),
        };
        assert!(editor.matches_platform_result(&editor_result));

        let timer = DesktopTimer::new(
            identity.clone(),
            DesktopTimerKind::InspectorTelemetryRefresh,
        );
        let timer_effect = DesktopEffect::ScheduleTimer {
            timer,
            delay: std::time::Duration::from_millis(250),
        };
        assert_eq!(timer_effect.identity(), &identity);
        assert_eq!(timer_effect.identity().owner(), &WorkspaceKey::Home);
    }
}
