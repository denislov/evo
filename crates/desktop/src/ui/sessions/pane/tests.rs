use std::path::PathBuf;

use coding_agent::api::view::{CodingAgentWorkspaceKind, CodingAgentWorkspaceOverview};
use desktop::runtime::DesktopSessionCatalogEntry;

use crate::application::catalog::ProjectCatalogGroup;
use gpui::KeyDownEvent;

use super::*;

fn project_group(
    group_id: &str,
    kind: CodingAgentWorkspaceKind,
    display_name: &str,
    session_ids: &[&str],
    collapsed: bool,
) -> ProjectCatalogGroup {
    let workspace = CodingAgentWorkspaceOverview {
        group_id: group_id.into(),
        kind,
        display_name: display_name.into(),
        display_path: (kind == CodingAgentWorkspaceKind::Project)
            .then(|| PathBuf::from(format!("/work/{display_name}"))),
    };
    ProjectCatalogGroup {
        sessions: session_ids
            .iter()
            .map(|session_id| DesktopSessionCatalogEntry {
                session_id: (*session_id).into(),
                name: Some(format!("{display_name} session")),
                workspace: workspace.clone(),
                ..Default::default()
            })
            .collect(),
        workspace,
        collapsed,
    }
}

#[test]
fn relative_session_time_is_stable_and_bounded() {
    let now = OffsetDateTime::parse("2026-07-27T12:00:00Z", &Rfc3339).unwrap();
    assert_eq!(relative_session_time("2026-07-27T11:59:45Z", now), "now");
    assert_eq!(
        relative_session_time("2026-07-27T11:35:00Z", now),
        "25m ago"
    );
    assert_eq!(relative_session_time("2026-07-27T06:00:00Z", now), "6h ago");
    assert_eq!(relative_session_time("2026-07-24T12:00:00Z", now), "3d ago");
    assert_eq!(
        relative_session_time("2026-06-01T00:00:00Z", now),
        "2026-06-01"
    );
    assert_eq!(relative_session_time("malformed", now), "malformed");
}

#[test]
fn project_tree_exposes_four_concurrent_runtime_presentations() {
    let groups = [
        project_group(
            "project:current",
            CodingAgentWorkspaceKind::Project,
            "Current",
            &["current-session"],
            false,
        ),
        project_group(
            "project:running",
            CodingAgentWorkspaceKind::Project,
            "Running",
            &["running-session"],
            false,
        ),
        project_group(
            "project:error",
            CodingAgentWorkspaceKind::Project,
            "Error",
            &["error-session"],
            false,
        ),
        project_group(
            "project:available",
            CodingAgentWorkspaceKind::Project,
            "Available",
            &["available-session"],
            false,
        ),
    ];
    let runtime_states: Arc<[SessionRuntimeState]> = Arc::from([
        SessionRuntimeState {
            session_id: Arc::from("running-session"),
            status: SemanticStatus::Running,
        },
        SessionRuntimeState {
            session_id: Arc::from("error-session"),
            status: SemanticStatus::Error,
        },
    ]);

    let labels = groups
        .iter()
        .map(|group| {
            let (status, contains_active) = project_runtime_summary(
                group,
                "current-session",
                SemanticStatus::Idle,
                &runtime_states,
            );
            runtime_status_label(status, contains_active)
        })
        .collect::<Vec<_>>();

    assert_eq!(labels, ["current", "running", "error", "available"]);
    assert_eq!(
        session_runtime_status(
            "current-session",
            "current-session",
            SemanticStatus::Idle,
            &runtime_states,
        ),
        Some(SemanticStatus::Idle)
    );
    assert_eq!(
        session_runtime_status(
            "available-session",
            "current-session",
            SemanticStatus::Idle,
            &runtime_states,
        ),
        None
    );
}

#[test]
fn project_status_uses_highest_attention_descendant() {
    let group = project_group(
        "project:mixed",
        CodingAgentWorkspaceKind::Project,
        "Mixed",
        &["idle", "running", "error"],
        false,
    );
    let runtime_states: Arc<[SessionRuntimeState]> = Arc::from([
        SessionRuntimeState {
            session_id: Arc::from("idle"),
            status: SemanticStatus::Idle,
        },
        SessionRuntimeState {
            session_id: Arc::from("running"),
            status: SemanticStatus::Running,
        },
        SessionRuntimeState {
            session_id: Arc::from("error"),
            status: SemanticStatus::Error,
        },
    ]);

    assert_eq!(
        project_runtime_summary(&group, "elsewhere", SemanticStatus::Idle, &runtime_states),
        (Some(SemanticStatus::Error), false)
    );
}

#[test]
fn projectless_and_legacy_groups_have_explicit_titles() {
    let projectless = project_group(
        "projectless:global",
        CodingAgentWorkspaceKind::Projectless,
        "Managed scratch path",
        &["projectless-session"],
        false,
    );
    let legacy = project_group(
        "legacy:unscoped",
        CodingAgentWorkspaceKind::Legacy,
        "",
        &["legacy-session"],
        false,
    );

    assert_eq!(project_title(&projectless), "无项目");
    assert_eq!(project_title(&legacy), "Legacy sessions");
}

#[test]
fn project_tree_keyboard_activation_is_limited_to_enter_and_space() {
    let mut enter = KeyDownEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
        is_held: false,
        prefer_character_input: false,
    };
    assert!(is_keyboard_activation(&enter));
    enter.keystroke = gpui::Keystroke::parse("space").unwrap();
    assert!(is_keyboard_activation(&enter));
    enter.keystroke = gpui::Keystroke::parse("down").unwrap();
    assert!(!is_keyboard_activation(&enter));
}
