use std::path::PathBuf;

use coding_agent::api::client::CodingAgentClientProjection;
use scenario_testing::{
    MockInferenceSseServer, ScenarioLoadError, apply_scenario, fetch_sse, initial_snapshot,
    load_scenarios, replay_terminal,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

#[test]
fn json_and_yaml_load_the_same_versioned_scenario() {
    let json = load_scenarios(fixture("phase9.json")).expect("JSON scenario");
    let yaml = load_scenarios(fixture("phase9.yaml")).expect("YAML scenario");
    assert_eq!(json.scenarios.len(), 1);
    assert_eq!(json.scenarios[0].scenario, yaml.scenarios[0].scenario);
    assert_eq!(json.scenarios[0].events, yaml.scenarios[0].events);
    assert_eq!(json.scenarios[0].expected, yaml.scenarios[0].expected);
}

#[test]
fn invalid_contract_version_fails_before_referenced_fixtures_are_loaded() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("invalid.json");
    std::fs::write(&path, r#"{"version":2,"scenarios":[]}"#).expect("write fixture");
    let error = load_scenarios(path).expect_err("invalid version");
    assert!(matches!(error, ScenarioLoadError::Contract(_)));
    assert!(error.to_string().contains("version 2"));
}

#[test]
fn product_runner_reaches_the_reviewed_semantic_terminal_state() {
    let document = load_scenarios(fixture("phase9.json")).expect("scenario document");
    for scenario in &document.scenarios {
        let mut projection =
            CodingAgentClientProjection::new(initial_snapshot()).expect("initial projection");
        let actual = apply_scenario(&mut projection, scenario).expect("scenario applies");
        assert_eq!(actual, scenario.expected, "{}", scenario.scenario.name);
    }
}

#[test]
fn terminal_and_mock_inference_sse_replay_are_deterministic() {
    let document = load_scenarios(fixture("phase9.yaml")).expect("scenario document");
    let scenario = &document.scenarios[0];
    let terminal = replay_terminal(scenario).expect("terminal replay");
    assert_eq!(terminal.size.columns, 100);
    assert_eq!(terminal.checkpoints.len(), scenario.scenario.terminal.len());

    let server = MockInferenceSseServer::spawn(scenario.scenario.sse.clone()).expect("SSE server");
    let actual = fetch_sse(server.address(), "/v1/responses").expect("SSE response");
    let request = server.finish().expect("SSE server completion");
    assert_eq!(actual, scenario.scenario.sse);
    assert!(request.starts_with("POST /v1/responses HTTP/1.1"));
}
