use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::kernel::error::CodingSessionError;
use crate::limits;
use crate::platform::io::bounded as bounded_io;

pub use crate::kernel::ids::ProfileId;

const PROFILE_SCHEMA_VERSION: u32 = 1;
const AGENT_PROFILE_DIR: &str = "agents";
const TEAM_PROFILE_DIR: &str = "teams";
const PROFILE_FILE_EXTENSION: &str = "toml";
const BUILT_IN_HELPER_AGENT_IDS: [&str; 3] = ["explore", "review", "check"];
const BUILT_IN_READ_ONLY_TOOL_NAMES: [&str; 4] = ["read", "grep", "find", "ls"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSource {
    BuiltIn,
    User,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    Agent,
    Team,
}

impl ProfileKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Team => "team",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileDiagnostic {
    pub source: ProfileSource,
    pub kind: ProfileKind,
    pub path: Option<PathBuf>,
    pub profile_id: Option<ProfileId>,
    pub message: String,
}

impl ProfileDiagnostic {
    fn new(
        source: ProfileSource,
        kind: ProfileKind,
        path: Option<PathBuf>,
        profile_id: Option<ProfileId>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            source,
            kind,
            path,
            profile_id,
            message: message.into(),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisionPolicy {
    #[default]
    Session,
    SelfReview,
    LlmSupervisor,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationConfirmationMode {
    Never,
    #[default]
    Writes,
    Always,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct DelegationPolicy {
    pub allow_delegate_agent: bool,
    pub allow_delegate_team: bool,
    pub max_depth: usize,
    pub max_parallel_children: usize,
    pub require_confirmation: DelegationConfirmationMode,
    pub allowed_agents: Vec<ProfileId>,
    pub allowed_teams: Vec<ProfileId>,
}

impl Default for DelegationPolicy {
    fn default() -> Self {
        Self {
            allow_delegate_agent: false,
            allow_delegate_team: false,
            max_depth: 0,
            max_parallel_children: 1,
            require_confirmation: DelegationConfirmationMode::Writes,
            allowed_agents: Vec::new(),
            allowed_teams: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TeamSupervisor {
    Deterministic,
    Agent(ProfileId),
}

impl<'de> Deserialize<'de> for TeamSupervisor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value == "deterministic" {
            Ok(Self::Deterministic)
        } else {
            ProfileId::new(value)
                .map(Self::Agent)
                .map_err(de::Error::custom)
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamStrategy {
    #[default]
    PlanExecuteReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentProfile {
    pub schema_version: u32,
    pub id: ProfileId,
    pub display_name: String,
    pub description: Option<String>,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub tools: Vec<String>,
    pub skills: Vec<String>,
    pub supervision: SupervisionPolicy,
    pub delegation: DelegationPolicy,
    pub source: ProfileSource,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TeamProfile {
    pub schema_version: u32,
    pub id: ProfileId,
    pub display_name: String,
    pub description: Option<String>,
    pub supervisor: TeamSupervisor,
    pub strategy: TeamStrategy,
    pub members: Vec<ProfileId>,
    pub delegation: DelegationPolicy,
    pub source: ProfileSource,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileRegistryOptions {
    user_roots: Vec<PathBuf>,
    project_roots: Vec<PathBuf>,
}

impl ProfileRegistryOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_user_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.user_roots.push(root.into());
        self
    }

    pub fn with_project_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.project_roots.push(root.into());
        self
    }

    pub fn user_roots(&self) -> &[PathBuf] {
        &self.user_roots
    }

    pub fn project_roots(&self) -> &[PathBuf] {
        &self.project_roots
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileRegistry {
    agents: BTreeMap<ProfileId, AgentProfile>,
    teams: BTreeMap<ProfileId, TeamProfile>,
    diagnostics: Vec<ProfileDiagnostic>,
}

impl ProfileRegistry {
    pub fn load(options: ProfileRegistryOptions) -> Result<Self, CodingSessionError> {
        let mut registry = Self::default();
        for profile in built_in_agent_profiles() {
            registry.insert_agent(profile);
        }
        for root in options.user_roots() {
            registry.load_root(root, ProfileSource::User);
        }
        for root in options.project_roots() {
            registry.load_root(root, ProfileSource::Project);
        }
        Ok(registry)
    }

    pub fn agent(&self, id: &str) -> Option<&AgentProfile> {
        let id = ProfileId::new(id.to_owned()).ok()?;
        self.agents.get(&id)
    }

    pub fn team(&self, id: &str) -> Option<&TeamProfile> {
        let id = ProfileId::new(id.to_owned()).ok()?;
        self.teams.get(&id)
    }

    pub fn agents(&self) -> impl Iterator<Item = &AgentProfile> {
        self.agents.values()
    }

    pub fn teams(&self) -> impl Iterator<Item = &TeamProfile> {
        self.teams.values()
    }

    pub fn diagnostics(&self) -> &[ProfileDiagnostic] {
        &self.diagnostics
    }

    fn load_root(&mut self, root: &Path, source: ProfileSource) {
        self.load_agent_dir(&root.join(AGENT_PROFILE_DIR), source);
        self.load_team_dir(&root.join(TEAM_PROFILE_DIR), source);
    }

    fn load_agent_dir(&mut self, dir: &Path, source: ProfileSource) {
        for path in discover_toml_files(dir, source, ProfileKind::Agent, &mut self.diagnostics) {
            match read_agent_profile(&path, source) {
                Ok(profile) => self.insert_agent(profile),
                Err(diagnostic) => self.diagnostics.push(diagnostic),
            }
        }
    }

    fn load_team_dir(&mut self, dir: &Path, source: ProfileSource) {
        for path in discover_toml_files(dir, source, ProfileKind::Team, &mut self.diagnostics) {
            match read_team_profile(&path, source) {
                Ok(profile) => self.insert_team(profile),
                Err(diagnostic) => self.diagnostics.push(diagnostic),
            }
        }
    }

    fn insert_agent(&mut self, profile: AgentProfile) {
        if let Some(previous) = self.agents.insert(profile.id.clone(), profile.clone()) {
            self.diagnostics.push(ProfileDiagnostic::new(
                profile.source,
                ProfileKind::Agent,
                profile.path.clone(),
                Some(profile.id.clone()),
                format!(
                    "duplicate agent profile id {} from {:?} overrides {:?}",
                    profile.id, profile.source, previous.source
                ),
            ));
        }
    }

    fn insert_team(&mut self, profile: TeamProfile) {
        if let Some(previous) = self.teams.insert(profile.id.clone(), profile.clone()) {
            self.diagnostics.push(ProfileDiagnostic::new(
                profile.source,
                ProfileKind::Team,
                profile.path.clone(),
                Some(profile.id.clone()),
                format!(
                    "duplicate team profile id {} from {:?} overrides {:?}",
                    profile.id, profile.source, previous.source
                ),
            ));
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentProfileFile {
    schema_version: u32,
    id: ProfileId,
    display_name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    supervision: SupervisionPolicy,
    #[serde(default)]
    delegation: DelegationPolicy,
}

impl AgentProfileFile {
    fn into_profile(
        self,
        source: ProfileSource,
        path: PathBuf,
    ) -> Result<AgentProfile, ProfileDiagnostic> {
        validate_schema(
            self.schema_version,
            ProfileKind::Agent,
            source,
            Some(path.clone()),
            Some(self.id.clone()),
        )?;
        Ok(AgentProfile {
            schema_version: self.schema_version,
            id: self.id,
            display_name: self.display_name,
            description: self.description,
            model: self.model,
            system_prompt: self.system_prompt,
            tools: self.tools,
            skills: self.skills,
            supervision: self.supervision,
            delegation: self.delegation,
            source,
            path: Some(path),
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TeamProfileFile {
    schema_version: u32,
    id: ProfileId,
    display_name: String,
    #[serde(default)]
    description: Option<String>,
    supervisor: TeamSupervisor,
    #[serde(default)]
    strategy: TeamStrategy,
    #[serde(default)]
    members: Vec<ProfileId>,
    #[serde(default)]
    delegation: DelegationPolicy,
}

impl TeamProfileFile {
    fn into_profile(
        self,
        source: ProfileSource,
        path: PathBuf,
    ) -> Result<TeamProfile, ProfileDiagnostic> {
        validate_schema(
            self.schema_version,
            ProfileKind::Team,
            source,
            Some(path.clone()),
            Some(self.id.clone()),
        )?;
        Ok(TeamProfile {
            schema_version: self.schema_version,
            id: self.id,
            display_name: self.display_name,
            description: self.description,
            supervisor: self.supervisor,
            strategy: self.strategy,
            members: self.members,
            delegation: self.delegation,
            source,
            path: Some(path),
        })
    }
}

fn built_in_agent_profiles() -> Vec<AgentProfile> {
    let mut profiles = vec![built_in_default_agent_profile()];
    profiles.extend([
        built_in_helper_agent_profile(
            "explore",
            "Explore",
            "Read-only helper for context gathering and codebase exploration",
            "You are a read-only exploration helper. Gather context and summarize findings without making changes.",
        ),
        built_in_helper_agent_profile(
            "review",
            "Review",
            "Read-only helper for review and risk analysis",
            "You are a read-only review helper. Inspect the requested work and report findings without making changes.",
        ),
        built_in_helper_agent_profile(
            "check",
            "Check",
            "Read-only helper for safe verification planning and diagnostics",
            "You are a read-only check helper. Run only safe verification reasoning and report diagnostics without making changes.",
        ),
    ]);
    profiles
}

fn built_in_default_agent_profile() -> AgentProfile {
    AgentProfile {
        schema_version: PROFILE_SCHEMA_VERSION,
        id: ProfileId::from("default"),
        display_name: "Default".into(),
        description: Some("Built-in default coding agent profile".into()),
        model: None,
        system_prompt: None,
        tools: Vec::new(),
        skills: Vec::new(),
        supervision: SupervisionPolicy::Session,
        delegation: DelegationPolicy {
            allow_delegate_agent: true,
            allow_delegate_team: false,
            max_depth: 1,
            max_parallel_children: 1,
            require_confirmation: DelegationConfirmationMode::Never,
            allowed_agents: BUILT_IN_HELPER_AGENT_IDS
                .iter()
                .map(|id| ProfileId::from(*id))
                .collect(),
            allowed_teams: Vec::new(),
        },
        source: ProfileSource::BuiltIn,
        path: None,
    }
}

fn built_in_helper_agent_profile(
    id: &'static str,
    display_name: &'static str,
    description: &'static str,
    system_prompt: &'static str,
) -> AgentProfile {
    AgentProfile {
        schema_version: PROFILE_SCHEMA_VERSION,
        id: ProfileId::from(id),
        display_name: display_name.into(),
        description: Some(description.into()),
        model: None,
        system_prompt: Some(system_prompt.into()),
        tools: BUILT_IN_READ_ONLY_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        skills: Vec::new(),
        supervision: SupervisionPolicy::Session,
        delegation: DelegationPolicy::default(),
        source: ProfileSource::BuiltIn,
        path: None,
    }
}

fn discover_toml_files(
    dir: &Path,
    source: ProfileSource,
    kind: ProfileKind,
    diagnostics: &mut Vec<ProfileDiagnostic>,
) -> Vec<PathBuf> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            diagnostics.push(ProfileDiagnostic::new(
                source,
                kind,
                Some(dir.to_path_buf()),
                None,
                format!(
                    "failed to read {} profile directory {}: {error}",
                    kind.as_str(),
                    dir.display()
                ),
            ));
            return Vec::new();
        }
    };

    let mut paths = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= limits::MAX_PROFILE_FILES_PER_DIRECTORY {
            diagnostics.push(ProfileDiagnostic::new(
                source,
                kind,
                Some(dir.to_path_buf()),
                None,
                format!(
                    "{} profile directory {} exceeds the {} entry safety limit",
                    kind.as_str(),
                    dir.display(),
                    limits::MAX_PROFILE_FILES_PER_DIRECTORY
                ),
            ));
            break;
        }
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path.is_file()
                    && path.extension().and_then(|extension| extension.to_str())
                        == Some(PROFILE_FILE_EXTENSION)
                {
                    paths.push(path);
                }
            }
            Err(error) => diagnostics.push(ProfileDiagnostic::new(
                source,
                kind,
                Some(dir.to_path_buf()),
                None,
                format!(
                    "failed to read {} profile directory entry {}: {error}",
                    kind.as_str(),
                    dir.display()
                ),
            )),
        }
    }
    paths.sort();
    paths
}

fn read_agent_profile(
    path: &Path,
    source: ProfileSource,
) -> Result<AgentProfile, ProfileDiagnostic> {
    let content = read_profile_file(path, source, ProfileKind::Agent)?;
    let parsed = toml::from_str::<AgentProfileFile>(&content).map_err(|error| {
        ProfileDiagnostic::new(
            source,
            ProfileKind::Agent,
            Some(path.to_path_buf()),
            None,
            format!("failed to parse agent profile {}: {error}", path.display()),
        )
    })?;
    parsed.into_profile(source, path.to_path_buf())
}

fn read_team_profile(path: &Path, source: ProfileSource) -> Result<TeamProfile, ProfileDiagnostic> {
    let content = read_profile_file(path, source, ProfileKind::Team)?;
    let parsed = toml::from_str::<TeamProfileFile>(&content).map_err(|error| {
        ProfileDiagnostic::new(
            source,
            ProfileKind::Team,
            Some(path.to_path_buf()),
            None,
            format!("failed to parse team profile {}: {error}", path.display()),
        )
    })?;
    parsed.into_profile(source, path.to_path_buf())
}

fn read_profile_file(
    path: &Path,
    source: ProfileSource,
    kind: ProfileKind,
) -> Result<String, ProfileDiagnostic> {
    bounded_io::read_text(path, limits::MAX_PROFILE_FILE_BYTES).map_err(|error| {
        ProfileDiagnostic::new(
            source,
            kind,
            Some(path.to_path_buf()),
            None,
            format!(
                "failed to read {} profile {}: {error}",
                kind.as_str(),
                path.display()
            ),
        )
    })
}

fn validate_schema(
    schema_version: u32,
    kind: ProfileKind,
    source: ProfileSource,
    path: Option<PathBuf>,
    profile_id: Option<ProfileId>,
) -> Result<(), ProfileDiagnostic> {
    if schema_version == PROFILE_SCHEMA_VERSION {
        return Ok(());
    }
    Err(ProfileDiagnostic::new(
        source,
        kind,
        path,
        profile_id,
        format!(
            "unsupported {} profile schema_version {} (expected {})",
            kind.as_str(),
            schema_version,
            PROFILE_SCHEMA_VERSION
        ),
    ))
}
