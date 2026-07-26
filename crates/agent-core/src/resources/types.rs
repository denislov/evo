// ── Resource types ─────────────────────────────────

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub location: String,
    pub content: String,
    pub disable_model_invocation: bool,
}

#[derive(Debug, Clone)]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    pub content: String,
    pub location: String,
}

#[derive(Debug, Clone, Default)]
pub struct AgentResources {
    pub skills: Vec<Skill>,
    pub prompt_templates: Vec<PromptTemplate>,
}

impl AgentResources {
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty() && self.prompt_templates.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct ResourceDiagnostic {
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub path: std::path::PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
}

/// Provenance for a [`Skill`] or [`PromptTemplate`] loaded by the
/// `load_sourced_*` helpers. Mirrors the `source` parameter of TS
/// `loadSourcedSkills` (`pi/packages/agent/src/harness/skills.ts:83`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTag {
    /// Original input path the entry was loaded from.
    pub source_path: std::path::PathBuf,
    /// Caller-defined provenance label (e.g. "project", "user", "builtin").
    pub source_type: String,
}

/// A [`Skill`] paired with the [`SourceTag`] of the input it was loaded from.
#[derive(Debug, Clone)]
pub struct SourcedSkill {
    pub skill: Skill,
    pub source: SourceTag,
}

/// A [`PromptTemplate`] paired with the [`SourceTag`] of the input it was
/// loaded from.
#[derive(Debug, Clone)]
pub struct SourcedPromptTemplate {
    pub template: PromptTemplate,
    pub source: SourceTag,
}

/// A [`ResourceDiagnostic`] carrying the [`SourceTag`] of the input that
/// produced it.
#[derive(Debug, Clone)]
pub struct SourcedResourceDiagnostic {
    pub diagnostic: ResourceDiagnostic,
    pub source: SourceTag,
}
