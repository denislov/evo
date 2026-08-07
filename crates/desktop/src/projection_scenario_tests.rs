use std::path::PathBuf;

use coding_agent::api::embedding::{
    CodingAgentEmbeddingSnapshot, CodingAgentResourceSummary, CodingAgentSettingsSummary,
};
use coding_agent::api::view::{CodingAgentTranscriptSnapshot, ProfileId};
use scenario_testing::{initial_snapshot, load_scenarios, semantic_state};

use crate::projection::{DesktopProjection, DesktopProjectionApply, ProjectionEvent};
use crate::runtime::DesktopRuntimeHydratedSnapshot;

fn phase9_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scenario-testing/fixtures/phase9.yaml")
}

fn projection() -> DesktopProjection {
    let session = initial_snapshot();
    let session_id = session.session.session_id.clone();
    DesktopProjection::new(DesktopRuntimeHydratedSnapshot {
        project: CodingAgentEmbeddingSnapshot {
            cwd: PathBuf::from("/phase9-scenario"),
            workspace: None,
            global_config_dir: PathBuf::from("/phase9-scenario/config"),
            selected_model_id: "scenario-model".into(),
            default_agent_profile_id: ProfileId::from("default"),
            models: Vec::new(),
            profiles: Vec::new(),
            resources: CodingAgentResourceSummary {
                skill_names: Vec::new(),
                prompt_template_names: Vec::new(),
                commands: Vec::new(),
                context_files: Vec::new(),
            },
            settings: CodingAgentSettingsSummary {
                default_provider: None,
                default_model: None,
                default_thinking_level: None,
                session_dir: None,
                no_context_files: true,
            },
            diagnostics: Vec::new(),
        },
        session,
        transcript: CodingAgentTranscriptSnapshot::new(session_id, None, Vec::new()),
        pending_recoveries: Vec::new(),
    })
    .expect("desktop scenario projection")
}

#[test]
fn desktop_replays_the_same_phase9_product_scenario_to_the_same_terminal_state() {
    let document = load_scenarios(phase9_fixture()).expect("phase 9 scenarios");
    for scenario in &document.scenarios {
        let mut projection = projection();
        for event in &scenario.events {
            assert!(matches!(
                projection.apply(ProjectionEvent::Product(event.clone())),
                DesktopProjectionApply::Applied(_)
            ));
        }
        let terminal = semantic_state(projection.product_for_tests());
        assert_eq!(terminal, scenario.expected, "{}", scenario.scenario.name);
        if scenario.scenario.reconnect.replay_last_event {
            assert!(matches!(
                projection.apply(ProjectionEvent::Product(
                    scenario.events.last().expect("reconnect event").clone()
                )),
                DesktopProjectionApply::IgnoredDuplicate
            ));
            assert_eq!(semantic_state(projection.product_for_tests()), terminal);
        }
    }
}
