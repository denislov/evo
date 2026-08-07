use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub const SCENARIO_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioDocument {
    pub version: u32,
    pub scenarios: Vec<Scenario>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    pub tags: BTreeSet<ScenarioTag>,
    pub event_fixture: String,
    pub expected_fixture: String,
    #[serde(default)]
    pub reconnect: ReconnectSpec,
    #[serde(default)]
    pub terminal: Vec<TerminalFrame>,
    #[serde(default)]
    pub sse: Vec<SseFrame>,
    pub checkpoints: BTreeMap<ScenarioTag, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioTag {
    Prompt,
    Tool,
    Authorization,
    Review,
    Rewind,
    Team,
    Background,
    Mcp,
    Reconnect,
}

impl ScenarioTag {
    pub const ALL: [Self; 9] = [
        Self::Prompt,
        Self::Tool,
        Self::Authorization,
        Self::Review,
        Self::Rewind,
        Self::Team,
        Self::Background,
        Self::Mcp,
        Self::Reconnect,
    ];
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconnectSpec {
    #[serde(default)]
    pub replay_last_event: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalFrame {
    pub write: String,
    pub expect: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SseFrame {
    pub event: String,
    pub data: String,
}
