use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use coding_agent::api::event::CodingAgentProductEvent;

use crate::{
    SCENARIO_CONTRACT_VERSION, Scenario, ScenarioDocument, ScenarioTag, SemanticTerminalState,
};

#[derive(Debug, Clone)]
pub struct LoadedScenario {
    pub source: PathBuf,
    pub scenario: Scenario,
    pub events: Vec<CodingAgentProductEvent>,
    pub expected: SemanticTerminalState,
}

#[derive(Debug, Clone)]
pub struct LoadedScenarioDocument {
    pub source: PathBuf,
    pub scenarios: Vec<LoadedScenario>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScenarioLoadError {
    #[error("could not read scenario fixture {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("unsupported scenario fixture extension for {0}")]
    UnsupportedExtension(PathBuf),
    #[error("invalid JSON scenario fixture {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("invalid YAML scenario fixture {path}: {source}")]
    Yaml {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    #[error("invalid scenario contract: {0}")]
    Contract(String),
    #[error("invalid product event fixture {path}: {source}")]
    Events {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("invalid semantic state fixture {path}: {source}")]
    Expected {
        path: PathBuf,
        source: serde_json::Error,
    },
}

pub fn load_scenarios(path: impl AsRef<Path>) -> Result<LoadedScenarioDocument, ScenarioLoadError> {
    let path = path.as_ref();
    let source = read(path)?;
    let document = match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => {
            serde_json::from_str(&source).map_err(|source| ScenarioLoadError::Json {
                path: path.to_owned(),
                source,
            })?
        }
        Some("yaml" | "yml") => {
            serde_yaml::from_str(&source).map_err(|source| ScenarioLoadError::Yaml {
                path: path.to_owned(),
                source,
            })?
        }
        _ => return Err(ScenarioLoadError::UnsupportedExtension(path.to_owned())),
    };
    validate(&document)?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let scenarios = document
        .scenarios
        .into_iter()
        .map(|scenario| load_scenario(path, base, scenario))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(LoadedScenarioDocument {
        source: path.to_owned(),
        scenarios,
    })
}

fn load_scenario(
    source: &Path,
    base: &Path,
    scenario: Scenario,
) -> Result<LoadedScenario, ScenarioLoadError> {
    let events_path = base.join(&scenario.event_fixture);
    let expected_path = base.join(&scenario.expected_fixture);
    let events =
        serde_json::from_str(&read(&events_path)?).map_err(|source| ScenarioLoadError::Events {
            path: events_path,
            source,
        })?;
    let expected = serde_json::from_str(&read(&expected_path)?).map_err(|source| {
        ScenarioLoadError::Expected {
            path: expected_path,
            source,
        }
    })?;
    Ok(LoadedScenario {
        source: source.to_owned(),
        scenario,
        events,
        expected,
    })
}

fn read(path: &Path) -> Result<String, ScenarioLoadError> {
    std::fs::read_to_string(path).map_err(|source| ScenarioLoadError::Read {
        path: path.to_owned(),
        source,
    })
}

fn validate(document: &ScenarioDocument) -> Result<(), ScenarioLoadError> {
    if document.version != SCENARIO_CONTRACT_VERSION {
        return Err(ScenarioLoadError::Contract(format!(
            "version {} is not supported; expected {SCENARIO_CONTRACT_VERSION}",
            document.version
        )));
    }
    if document.scenarios.is_empty() {
        return Err(ScenarioLoadError::Contract(
            "at least one scenario is required".into(),
        ));
    }
    let mut names = HashSet::new();
    let mut coverage = BTreeSet::new();
    for scenario in &document.scenarios {
        if scenario.name.trim().is_empty() || !names.insert(scenario.name.as_str()) {
            return Err(ScenarioLoadError::Contract(format!(
                "scenario names must be non-empty and unique: {:?}",
                scenario.name
            )));
        }
        if scenario.tags.is_empty() {
            return Err(ScenarioLoadError::Contract(format!(
                "scenario {} has no tags",
                scenario.name
            )));
        }
        if scenario.event_fixture.trim().is_empty() || scenario.expected_fixture.trim().is_empty() {
            return Err(ScenarioLoadError::Contract(format!(
                "scenario {} must name event and expected fixtures",
                scenario.name
            )));
        }
        for tag in &scenario.tags {
            if scenario
                .checkpoints
                .get(tag)
                .is_none_or(|value| value.trim().is_empty())
            {
                return Err(ScenarioLoadError::Contract(format!(
                    "scenario {} has no checkpoint for {tag:?}",
                    scenario.name
                )));
            }
            coverage.insert(*tag);
        }
    }
    let required = ScenarioTag::ALL.into_iter().collect::<BTreeSet<_>>();
    if coverage != required {
        return Err(ScenarioLoadError::Contract(format!(
            "scenario matrix coverage mismatch: expected {required:?}, got {coverage:?}"
        )));
    }
    Ok(())
}
