use crate::operations::delegation::DelegationTargetInventory;

use crate::profiles::{ProfileId, ProfileRegistry, ProfileSource, TeamSupervisor};
use crate::public_error::safe_public_summary;
use crate::runtime::facade::{
    CodingAgentPublicDiagnostic, CodingAgentPublicDiagnosticOrigin,
    CodingAgentPublicDiagnosticSeverity,
};

const MAX_PROFILE_ENTRIES: usize = 256;
const MAX_PROFILE_LIST_ITEMS: usize = 128;
const MAX_PROFILE_DIAGNOSTICS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentProfileDelegationSummary {
    pub allow_agents: bool,
    pub allow_teams: bool,
    pub max_depth: u32,
    pub max_parallel_children: u32,
    pub agent_targets: Vec<ProfileId>,
    pub team_targets: Vec<ProfileId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentAgentProfileCatalogEntry {
    pub id: ProfileId,
    pub display_name: String,
    pub description: Option<String>,
    pub source: ProfileSource,
    pub model_id: Option<String>,
    pub tools: Vec<String>,
    pub skills: Vec<String>,
    pub delegation: CodingAgentProfileDelegationSummary,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingAgentTeamProfileCatalogEntry {
    pub id: ProfileId,
    pub display_name: String,
    pub description: Option<String>,
    pub source: ProfileSource,
    pub supervisor: TeamSupervisor,
    pub members: Vec<ProfileId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodingAgentProfileCatalog {
    pub agents: Vec<CodingAgentAgentProfileCatalogEntry>,
    pub teams: Vec<CodingAgentTeamProfileCatalogEntry>,
    pub diagnostics: Vec<CodingAgentPublicDiagnostic>,
    pub truncated: bool,
}

impl CodingAgentProfileCatalog {
    pub(crate) fn from_registry(
        registry: &ProfileRegistry,
        default_agent_profile_id: &ProfileId,
    ) -> Self {
        let mut truncated = false;
        let mut remaining = MAX_PROFILE_ENTRIES;
        let mut agents = Vec::new();
        for profile in registry.agents() {
            if remaining == 0 {
                truncated = true;
                break;
            }
            let inventory = DelegationTargetInventory::from_registry(registry, &profile.delegation);
            let (tools, tools_truncated) = bounded_strings(&profile.tools);
            let (skills, skills_truncated) = bounded_strings(&profile.skills);
            let (agent_targets, agents_truncated) =
                bounded_profile_ids(inventory.agent_ids().cloned());
            let (team_targets, teams_truncated) =
                bounded_profile_ids(inventory.team_ids().cloned());
            truncated |= tools_truncated || skills_truncated || agents_truncated || teams_truncated;
            agents.push(CodingAgentAgentProfileCatalogEntry {
                id: profile.id.clone(),
                display_name: safe_public_summary(&profile.display_name),
                description: profile.description.as_deref().map(safe_public_summary),
                source: profile.source,
                model_id: profile.model.as_deref().map(safe_public_summary),
                tools,
                skills,
                delegation: CodingAgentProfileDelegationSummary {
                    allow_agents: profile.delegation.allow_delegate_agent,
                    allow_teams: profile.delegation.allow_delegate_team,
                    max_depth: u32::try_from(profile.delegation.max_depth).unwrap_or(u32::MAX),
                    max_parallel_children: u32::try_from(profile.delegation.max_parallel_children)
                        .unwrap_or(u32::MAX),
                    agent_targets,
                    team_targets,
                },
                is_default: profile.id == *default_agent_profile_id,
            });
            remaining -= 1;
        }

        let mut teams = Vec::new();
        for profile in registry.teams() {
            if remaining == 0 {
                truncated = true;
                break;
            }
            let (members, members_truncated) = bounded_profile_ids(profile.members.iter().cloned());
            truncated |= members_truncated;
            teams.push(CodingAgentTeamProfileCatalogEntry {
                id: profile.id.clone(),
                display_name: safe_public_summary(&profile.display_name),
                description: profile.description.as_deref().map(safe_public_summary),
                source: profile.source,
                supervisor: profile.supervisor.clone(),
                members,
            });
            remaining -= 1;
        }

        let diagnostic_count = registry.diagnostics().len();
        let diagnostics = registry
            .diagnostics()
            .iter()
            .take(MAX_PROFILE_DIAGNOSTICS)
            .map(|diagnostic| {
                CodingAgentPublicDiagnostic::new(
                    CodingAgentPublicDiagnosticSeverity::Warning,
                    "profile_configuration",
                    &diagnostic.message,
                    CodingAgentPublicDiagnosticOrigin::Profile,
                    None,
                )
            })
            .collect();
        truncated |= diagnostic_count > MAX_PROFILE_DIAGNOSTICS;

        Self {
            agents,
            teams,
            diagnostics,
            truncated,
        }
    }

    pub fn agent(&self, id: &str) -> Option<&CodingAgentAgentProfileCatalogEntry> {
        self.agents.iter().find(|profile| profile.id.as_str() == id)
    }

    pub fn team(&self, id: &str) -> Option<&CodingAgentTeamProfileCatalogEntry> {
        self.teams.iter().find(|profile| profile.id.as_str() == id)
    }

    pub fn sync_default_agent_profile(&mut self, id: &ProfileId) {
        for profile in &mut self.agents {
            profile.is_default = profile.id == *id;
        }
    }
}

fn bounded_strings(values: &[String]) -> (Vec<String>, bool) {
    (
        values
            .iter()
            .take(MAX_PROFILE_LIST_ITEMS)
            .map(|value| safe_public_summary(value))
            .collect(),
        values.len() > MAX_PROFILE_LIST_ITEMS,
    )
}

fn bounded_profile_ids(values: impl Iterator<Item = ProfileId>) -> (Vec<ProfileId>, bool) {
    let mut values = values.take(MAX_PROFILE_LIST_ITEMS + 1).collect::<Vec<_>>();
    let truncated = values.len() > MAX_PROFILE_LIST_ITEMS;
    values.truncate(MAX_PROFILE_LIST_ITEMS);
    (values, truncated)
}
