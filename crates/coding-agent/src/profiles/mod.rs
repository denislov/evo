use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::runtime::error::CodingSessionError;
use crate::{bounded_io, limits};

const PROFILE_SCHEMA_VERSION: u32 = 1;
const AGENT_PROFILE_DIR: &str = "agents";
const TEAM_PROFILE_DIR: &str = "teams";
const PROFILE_FILE_EXTENSION: &str = "toml";
const BUILT_IN_HELPER_AGENT_IDS: [&str; 3] = ["explore", "review", "check"];
const BUILT_IN_READ_ONLY_TOOL_NAMES: [&str; 4] = ["read", "grep", "find", "ls"];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProfileId(String);

impl ProfileId {
    pub fn new(id: impl Into<String>) -> Result<Self, String> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err("profile id must not be empty".into());
        }
        if id.trim() != id {
            return Err(format!(
                "profile id must not have surrounding whitespace: {id:?}"
            ));
        }
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ProfileId {
    fn from(value: &str) -> Self {
        Self::new(value).expect("profile id literal must be valid")
    }
}

impl From<String> for ProfileId {
    fn from(value: String) -> Self {
        Self::new(value).expect("profile id string must be valid")
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ProfileId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProfileId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn built_in_default_agent_profile_exposes_read_only_helper_roster() {
        let registry = ProfileRegistry::load(ProfileRegistryOptions::new()).unwrap();

        let profile = registry
            .agent("default")
            .expect("built-in default profile should resolve");

        assert_eq!(profile.id.as_str(), "default");
        assert_eq!(profile.display_name, "Default");
        assert_eq!(profile.source, ProfileSource::BuiltIn);
        assert_eq!(profile.supervision, SupervisionPolicy::Session);
        assert!(profile.delegation.allow_delegate_agent);
        assert!(!profile.delegation.allow_delegate_team);
        assert_eq!(profile.delegation.max_depth, 1);
        assert_eq!(profile.delegation.max_parallel_children, 1);
        assert_eq!(
            profile.delegation.require_confirmation,
            DelegationConfirmationMode::Never
        );
        assert_eq!(
            profile
                .delegation
                .allowed_agents
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>(),
            vec!["explore", "review", "check"]
        );

        for helper_id in ["explore", "review", "check"] {
            let helper = registry
                .agent(helper_id)
                .expect("built-in helper profile should resolve");
            assert_eq!(helper.source, ProfileSource::BuiltIn);
            assert_eq!(
                helper.tools, BUILT_IN_READ_ONLY_TOOL_NAMES,
                "built-in helper {helper_id} must expose the exact read-only filesystem surface"
            );
            assert!(
                !helper
                    .tools
                    .iter()
                    .any(|tool| matches!(tool.as_str(), "write" | "edit" | "bash"))
            );
            assert!(
                helper.skills.is_empty(),
                "built-in helper {helper_id} must not carry privileged skills by default"
            );
            assert!(!helper.delegation.allow_delegate_agent);
            assert!(!helper.delegation.allow_delegate_team);
        }
    }

    #[test]
    fn custom_default_profile_does_not_inherit_built_in_helper_roster() {
        let root = tempdir().unwrap();
        write_file(
            root.path().join("agents/default.toml"),
            r#"
schema_version = 1
id = "default"
display_name = "Project Default"
"#,
        );

        let registry =
            ProfileRegistry::load(ProfileRegistryOptions::new().with_project_root(root.path()))
                .unwrap();
        let profile = registry.agent("default").unwrap();

        assert_eq!(profile.display_name, "Project Default");
        assert_eq!(profile.source, ProfileSource::Project);
        assert!(!profile.delegation.allow_delegate_agent);
        assert!(profile.delegation.allowed_agents.is_empty());
    }

    #[test]
    fn loads_agent_and_team_profiles_from_toml_roots() {
        let root = tempdir().unwrap();
        write_file(
            root.path().join("agents/coder.toml"),
            r#"
schema_version = 1
id = "coder"
display_name = "Coder"
description = "Implementation agent"
model = "gpt-5-codex"
system_prompt = "You write code."
tools = ["shell", "apply_patch"]
skills = ["superpowers:test-driven-development"]
supervision = "self_review"

[delegation]
allow_delegate_agent = true
allow_delegate_team = false
max_depth = 1
require_confirmation = "writes"
allowed_agents = ["reviewer"]
"#,
        );
        write_file(
            root.path().join("teams/implementation.toml"),
            r#"
schema_version = 1
id = "implementation"
display_name = "Implementation Team"
description = "Planner, coder, reviewer"
supervisor = "planner"
strategy = "plan_execute_review"
members = ["planner", "coder", "reviewer"]

[delegation]
max_parallel_children = 2
max_depth = 1
require_confirmation = "writes"
"#,
        );

        let registry =
            ProfileRegistry::load(ProfileRegistryOptions::new().with_project_root(root.path()))
                .unwrap();

        let coder = registry.agent("coder").expect("agent should load");
        assert_eq!(coder.display_name, "Coder");
        assert_eq!(coder.description.as_deref(), Some("Implementation agent"));
        assert_eq!(coder.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(coder.system_prompt.as_deref(), Some("You write code."));
        assert_eq!(coder.tools, ["shell", "apply_patch"]);
        assert_eq!(
            coder.skills,
            ["superpowers:test-driven-development".to_string()]
        );
        assert_eq!(coder.supervision, SupervisionPolicy::SelfReview);
        assert!(coder.delegation.allow_delegate_agent);
        assert!(!coder.delegation.allow_delegate_team);
        assert_eq!(coder.delegation.max_depth, 1);
        assert_eq!(coder.delegation.allowed_agents[0].as_str(), "reviewer");
        assert_eq!(coder.source, ProfileSource::Project);

        let team = registry.team("implementation").expect("team should load");
        assert_eq!(team.display_name, "Implementation Team");
        assert_eq!(team.supervisor, TeamSupervisor::Agent("planner".into()));
        assert_eq!(team.strategy, TeamStrategy::PlanExecuteReview);
        assert_eq!(team.members.len(), 3);
        assert_eq!(team.members[1].as_str(), "coder");
        assert_eq!(team.delegation.max_parallel_children, 2);
        assert_eq!(team.source, ProfileSource::Project);
        assert!(
            registry.diagnostics().is_empty(),
            "unexpected diagnostics: {registry:#?}"
        );
    }

    #[test]
    fn invalid_profile_files_are_diagnostics_without_blocking_valid_profiles() {
        let root = tempdir().unwrap();
        write_file(
            root.path().join("agents/valid.toml"),
            r#"
schema_version = 1
id = "valid"
display_name = "Valid"
"#,
        );
        write_file(
            root.path().join("agents/invalid.toml"),
            r#"
schema_version = 99
id = "invalid"
display_name = "Invalid"
"#,
        );

        let registry =
            ProfileRegistry::load(ProfileRegistryOptions::new().with_project_root(root.path()))
                .unwrap();

        assert!(registry.agent("valid").is_some());
        assert!(registry.agent("invalid").is_none());
        assert!(
            registry.diagnostics().iter().any(|diagnostic| diagnostic
                .message
                .contains("unsupported agent profile schema_version 99")),
            "expected unsupported schema diagnostic, got {:#?}",
            registry.diagnostics()
        );
    }

    #[test]
    fn duplicate_ids_use_project_over_user_over_builtin_with_diagnostics() {
        let user_root = tempdir().unwrap();
        let project_root = tempdir().unwrap();
        write_file(
            user_root.path().join("agents/default.toml"),
            r#"
schema_version = 1
id = "default"
display_name = "User Default"
"#,
        );
        write_file(
            project_root.path().join("agents/default.toml"),
            r#"
schema_version = 1
id = "default"
display_name = "Project Default"
"#,
        );

        let registry = ProfileRegistry::load(
            ProfileRegistryOptions::new()
                .with_user_root(user_root.path())
                .with_project_root(project_root.path()),
        )
        .unwrap();

        let profile = registry.agent("default").unwrap();
        assert_eq!(profile.display_name, "Project Default");
        assert_eq!(profile.source, ProfileSource::Project);
        assert_eq!(
            registry
                .diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic
                    .message
                    .contains("duplicate agent profile id default"))
                .count(),
            2,
            "expected diagnostics for user overriding built-in and project overriding user"
        );
    }

    #[test]
    fn oversized_profile_is_rejected_before_toml_parsing() {
        let root = tempdir().unwrap();
        let path = root.path().join("agents/large.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = fs::File::create(&path).unwrap();
        file.set_len(limits::MAX_PROFILE_FILE_BYTES as u64 + 1)
            .unwrap();

        let registry =
            ProfileRegistry::load(ProfileRegistryOptions::new().with_project_root(root.path()))
                .unwrap();

        assert!(registry.agent("large").is_none());
        assert!(
            registry
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("safety limit"))
        );
    }

    fn write_file(path: impl AsRef<Path>, content: &str) {
        let path = path.as_ref();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content.trim_start()).unwrap();
    }
}
