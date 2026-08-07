use std::path::PathBuf;

use scenario_testing::{initial_snapshot, load_scenarios, replay_terminal, semantic_state};

use super::event_bridge::UiProjection;

fn phase9_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scenario-testing/fixtures/phase9.json")
}

#[test]
fn cli_runs_the_shared_phase9_scenario_through_its_product_event_bridge() {
    let document = load_scenarios(phase9_fixture()).expect("phase 9 scenarios");
    for scenario in &document.scenarios {
        let mut projection = UiProjection::from_snapshot(initial_snapshot());
        for event in &scenario.events {
            projection.apply_product_event(event);
        }
        let terminal = semantic_state(projection.product_for_tests());
        assert_eq!(terminal, scenario.expected, "{}", scenario.scenario.name);
        if scenario.scenario.reconnect.replay_last_event {
            let event = scenario.events.last().expect("reconnect event");
            projection.apply_product_event(event);
            assert_eq!(semantic_state(projection.product_for_tests()), terminal);
        }
        let replay = replay_terminal(scenario).expect("virtual terminal replay");
        assert!(replay.output.contains("MCP server connected"));
        assert!(projection.drain().len() >= 5);
    }
}
