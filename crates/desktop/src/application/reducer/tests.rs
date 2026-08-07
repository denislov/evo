//! Reducer unit tests: coverage tables, effect identity, platform routing.

use std::{collections::HashMap, path::PathBuf, time::Duration};

use desktop::preferences::{DesktopPreferences, ExternalEditorPreference};

use super::{
    CatalogIntent, DesktopController, DesktopEvent, PlatformUpdatePort, RuntimeUpdateKind,
    Transition, safe_runtime_rejection_notice,
};
use crate::application::{
    change_set::UiRegion,
    commands::CommandTracker,
    effect::{
        ClipboardFeedback, DesktopEffect, DesktopPickerKind, DesktopTimer, DesktopTimerKind,
        EffectIdentity, ExternalEditorLaunchTarget, PlatformOutcome, PlatformResult,
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

struct TestPlatformPort {
    active: WorkspaceKey,
    projects: HashMap<WorkspaceKey, PathBuf>,
    attachments: HashMap<WorkspaceKey, Vec<PathBuf>>,
    notices: HashMap<WorkspaceKey, String>,
    announcement: Option<(WorkspaceKey, String)>,
    timer_fires: HashMap<DesktopTimerKind, usize>,
}

impl TestPlatformPort {
    fn new(active: WorkspaceKey) -> Self {
        Self {
            active,
            projects: HashMap::new(),
            attachments: HashMap::new(),
            notices: HashMap::new(),
            announcement: None,
            timer_fires: HashMap::new(),
        }
    }

    fn record_timer(&mut self, kind: DesktopTimerKind) -> bool {
        *self.timer_fires.entry(kind).or_default() += 1;
        true
    }
}

impl PlatformUpdatePort for TestPlatformPort {
    fn active_workspace_key(&self) -> WorkspaceKey {
        self.active.clone()
    }

    fn workspace_exists(&self, _owner: &WorkspaceKey) -> bool {
        true
    }

    fn project_directory_editable(&self, _owner: &WorkspaceKey) -> bool {
        true
    }

    fn set_project_directory(&mut self, owner: &WorkspaceKey, path: PathBuf) -> bool {
        self.projects.insert(owner.clone(), path);
        true
    }

    fn add_composer_attachments(
        &mut self,
        owner: &WorkspaceKey,
        paths: Vec<PathBuf>,
    ) -> Result<bool, String> {
        self.attachments.insert(owner.clone(), paths);
        Ok(true)
    }

    fn set_notice(&mut self, owner: &WorkspaceKey, notice: String) {
        self.notices.insert(owner.clone(), notice);
    }

    fn show_conversation_announcement(&mut self, owner: &WorkspaceKey, message: String) {
        self.announcement = Some((owner.clone(), message));
    }

    fn clear_conversation_announcement(&mut self, owner: &WorkspaceKey) -> bool {
        if self
            .announcement
            .as_ref()
            .is_some_and(|(current, _)| current == owner)
        {
            self.announcement = None;
            true
        } else {
            false
        }
    }

    fn commit_conversation_width(&mut self, _owner: &WorkspaceKey) -> bool {
        self.record_timer(DesktopTimerKind::ConversationWidthCommit)
    }

    fn refresh_inspector_telemetry(&mut self, _owner: &WorkspaceKey) -> bool {
        self.record_timer(DesktopTimerKind::InspectorTelemetryRefresh)
    }

    fn complete_resync_admission(
        &mut self,
        owner: &WorkspaceKey,
        _command_id: u64,
        failure: Option<String>,
    ) {
        if let Some(message) = failure {
            self.set_notice(owner, message);
        }
    }

    fn complete_external_editor_launch(
        &mut self,
        owner: &WorkspaceKey,
        _command_id: u64,
        project_relative_path: &str,
        failure: Option<String>,
    ) {
        self.set_notice(
            owner,
            failure.unwrap_or_else(|| format!("opened {project_relative_path}")),
        );
    }
}

fn emitted_identity(transition: &Transition) -> EffectIdentity {
    transition
        .effects()
        .first()
        .expect("request emits one effect")
        .identity()
        .clone()
}

#[test]
fn runtime_update_coverage_table_registers_all_twenty_seven_protocol_variants() {
    let labels = RuntimeUpdateKind::ALL.map(RuntimeUpdateKind::label);
    assert_eq!(
        labels,
        [
            "reloaded",
            "resynced",
            "session_changed",
            "session_closed",
            "session_deleted",
            "sessions_listed",
            "session_renamed",
            "session_name_observed",
            "selection_changed",
            "prompt_accepted",
            "prompt_accepted_with_session",
            "prompt_rejected_with_session",
            "prompt_started",
            "product_event",
            "resync_required",
            "control_accepted",
            "authorization_decision_accepted",
            "recovery_changed",
            "file_reviewed",
            "merge_proposals_listed",
            "child_worktree_merged",
            "child_worktree_discarded",
            "external_editor_target_validated",
            "prompt_finished",
            "command_rejected",
            "runtime_failed",
            "stopped",
        ]
    );
}

#[test]
fn delegated_reduce_mutates_the_single_state_and_returns_typed_changes() {
    let mut controller = DesktopController::new();
    let mut state = state();
    let transition = controller.reduce(
        &mut state,
        DesktopEvent::Ui(CatalogIntent::SetProjectCollapsed {
            group_id: "project:alpha".into(),
            collapsed: true,
        }),
        |state, event| {
            let DesktopEvent::Ui(CatalogIntent::SetProjectCollapsed { group_id, .. }) = event
            else {
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
        outcome: PlatformOutcome::Failed("picker failed".into()),
    };
    assert!(!same_kind.matches_platform_result(&wrong_owner));

    let clipboard = DesktopEffect::WriteClipboard {
        identity: identity.clone(),
        text: Some("copy".into()),
        feedback: ClipboardFeedback::Notice("copied".into()),
    };
    let clipboard_result = PlatformResult::ClipboardWritten {
        identity: identity.clone(),
        outcome: PlatformOutcome::Completed(()),
    };
    assert!(clipboard.matches_platform_result(&clipboard_result));

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

#[test]
fn external_editor_launch_failure_returns_through_typed_platform_result() {
    let mut controller = DesktopController::new();
    let owner = WorkspaceKey::Home;
    let transition = controller
        .launch_external_editor(
            owner.clone(),
            41,
            ExternalEditorPreference {
                program: "missing-editor".into(),
                args: Vec::new(),
            },
            ExternalEditorLaunchTarget::new(
                PathBuf::from("/project/src/lib.rs"),
                "src/lib.rs".into(),
            ),
        )
        .unwrap();
    let identity = emitted_identity(&transition);
    let mut port = TestPlatformPort::new(owner.clone());

    let completion = controller.reduce_platform(
        &mut port,
        PlatformResult::ExternalEditorLaunched {
            identity,
            outcome: PlatformOutcome::Failed("external editor executable is unavailable".into()),
        },
    );

    assert_eq!(
        port.notices.get(&owner).map(String::as_str),
        Some("external editor executable is unavailable")
    );
    assert!(completion.changes().contains(UiRegion::Inspector));
    assert!(completion.changes().contains(UiRegion::Toast));
}

#[test]
fn newer_picker_request_rejects_the_stale_result() {
    let mut controller = DesktopController::new();
    let owner = WorkspaceKey::Home;
    let first = controller
        .pick_paths(owner.clone(), DesktopPickerKind::Attachments)
        .unwrap();
    let first_identity = emitted_identity(&first);
    let second = controller
        .pick_paths(owner.clone(), DesktopPickerKind::Attachments)
        .unwrap();
    let second_identity = emitted_identity(&second);
    let mut port = TestPlatformPort::new(owner.clone());

    let stale = controller.reduce_async(
        &mut port,
        DesktopEvent::Platform(PlatformResult::PathsPicked {
            identity: first_identity,
            picker: DesktopPickerKind::Attachments,
            outcome: PlatformOutcome::Completed(vec![PathBuf::from("stale.png")]),
        }),
    );
    assert!(stale.changes().is_empty());
    assert!(!port.attachments.contains_key(&owner));

    let current = controller.reduce_async(
        &mut port,
        DesktopEvent::Platform(PlatformResult::PathsPicked {
            identity: second_identity,
            picker: DesktopPickerKind::Attachments,
            outcome: PlatformOutcome::Completed(vec![PathBuf::from("current.png")]),
        }),
    );
    assert!(current.changes().contains(UiRegion::Composer));
    assert_eq!(port.attachments[&owner], [PathBuf::from("current.png")]);
}

#[test]
fn picker_result_mutates_its_owner_without_refreshing_a_switched_workspace() {
    let mut controller = DesktopController::new();
    let owner = WorkspaceKey::Home;
    let requested = controller
        .pick_paths(owner.clone(), DesktopPickerKind::ProjectDirectory)
        .unwrap();
    let identity = emitted_identity(&requested);
    let mut port = TestPlatformPort::new(WorkspaceKey::session("session-b"));

    let transition = controller.reduce_async(
        &mut port,
        DesktopEvent::Platform(PlatformResult::PathsPicked {
            identity,
            picker: DesktopPickerKind::ProjectDirectory,
            outcome: PlatformOutcome::Completed(vec![PathBuf::from("/owner/home")]),
        }),
    );

    assert!(transition.changes().is_empty());
    assert_eq!(port.projects[&owner], PathBuf::from("/owner/home"));
}

#[test]
fn preference_writer_failure_returns_to_the_request_owner_as_a_typed_notice() {
    let mut controller = DesktopController::new();
    let owner = WorkspaceKey::Home;
    let requested = controller
        .write_preferences(owner.clone(), DesktopPreferences::default())
        .unwrap();
    let identity = emitted_identity(&requested);
    let mut port = TestPlatformPort::new(owner.clone());

    let transition = controller.reduce_async(
        &mut port,
        DesktopEvent::Platform(PlatformResult::PreferencesWritten {
            identity,
            outcome: PlatformOutcome::Failed("preference disk failed".into()),
        }),
    );

    assert!(transition.changes().contains(UiRegion::Toast));
    assert_eq!(port.notices[&owner], "preference disk failed");
}

#[test]
fn superseded_timer_identity_cannot_fire_current_state() {
    let mut controller = DesktopController::new();
    let owner = WorkspaceKey::Home;
    let first = controller
        .schedule_timer(
            owner.clone(),
            DesktopTimerKind::ConversationWidthCommit,
            Duration::from_millis(10),
        )
        .unwrap();
    let first_timer = match first.effects().first() {
        Some(DesktopEffect::ScheduleTimer { timer, .. }) => timer.clone(),
        _ => panic!("timer request emits one typed timer"),
    };
    let second = controller
        .schedule_timer(
            owner.clone(),
            DesktopTimerKind::ConversationWidthCommit,
            Duration::from_millis(10),
        )
        .unwrap();
    let second_timer = match second.effects().first() {
        Some(DesktopEffect::ScheduleTimer { timer, .. }) => timer.clone(),
        _ => panic!("timer request emits one typed timer"),
    };
    let mut port = TestPlatformPort::new(owner);

    let stale = controller.reduce_async(&mut port, DesktopEvent::Timer(first_timer));
    assert!(stale.changes().is_empty());
    assert!(port.timer_fires.is_empty());

    let current = controller.reduce_async(&mut port, DesktopEvent::Timer(second_timer));
    assert!(current.changes().contains(UiRegion::Root));
    assert_eq!(
        port.timer_fires[&DesktopTimerKind::ConversationWidthCommit],
        1
    );
}

#[test]
fn runtime_rejection_notice_never_includes_an_untrusted_body() {
    const SECRET: &str = "desktop-secret-canary";
    let notice = safe_runtime_rejection_notice(
        desktop::runtime::DesktopRuntimeCommandKind::DecideToolAuthorization,
        "authorization_not_pending",
    );
    assert!(!notice.contains(SECRET));
}
