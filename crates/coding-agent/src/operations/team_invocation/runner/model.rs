use ai_protocol::api::conversation::AssistantMessage;

use crate::operations::delegation::DelegationLineageEntry;
use crate::operations::prompt::context::PromptTurnOptions;
use crate::profiles::ProfileId;
use crate::public_error::CodingAgentPublicDiagnostic;

#[derive(Debug, Clone)]
pub struct AgentTeamOptions {
    pub(super) team_id: ProfileId,
    pub(super) task: String,
    pub(super) prompt_options: PromptTurnOptions,
    pub(super) delegation_depth: usize,
    pub(super) delegation_lineage: Vec<DelegationLineageEntry>,
}

impl AgentTeamOptions {
    pub fn new(
        team_id: impl Into<ProfileId>,
        task: impl Into<String>,
        prompt_options: PromptTurnOptions,
    ) -> Self {
        Self {
            team_id: team_id.into(),
            task: task.into(),
            prompt_options,
            delegation_depth: 0,
            delegation_lineage: Vec::new(),
        }
    }

    pub fn with_delegation_depth(mut self, depth: usize) -> Self {
        self.delegation_depth = depth;
        self
    }

    pub(crate) fn with_delegation_lineage(mut self, lineage: Vec<DelegationLineageEntry>) -> Self {
        self.delegation_lineage = lineage;
        self
    }

    pub fn team_id(&self) -> &ProfileId {
        &self.team_id
    }

    pub fn task(&self) -> &str {
        &self.task
    }

    pub fn prompt_options(&self) -> &PromptTurnOptions {
        &self.prompt_options
    }

    pub(crate) fn prompt_options_mut(&mut self) -> &mut PromptTurnOptions {
        &mut self.prompt_options
    }

    pub fn delegation_depth(&self) -> usize {
        self.delegation_depth
    }

    pub(crate) fn delegation_lineage(&self) -> &[DelegationLineageEntry] {
        &self.delegation_lineage
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentTeamMemberOutcome {
    pub profile_id: ProfileId,
    pub operation_id: String,
    pub turn_id: String,
    pub final_text: String,
    pub final_message: AssistantMessage,
    pub diagnostics: Vec<CodingAgentPublicDiagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentTeamOutcome {
    pub operation_id: String,
    pub team_id: ProfileId,
    pub final_text: String,
    pub member_results: Vec<AgentTeamMemberOutcome>,
    pub supervisor_result: Option<Box<AgentTeamMemberOutcome>>,
    pub diagnostics: Vec<CodingAgentPublicDiagnostic>,
}
