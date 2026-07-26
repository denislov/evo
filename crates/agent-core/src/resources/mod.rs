mod loader;
mod types;

pub use loader::{
    MAX_RESOURCE_DEPTH, MAX_RESOURCE_ENTRIES, MAX_RESOURCE_FILE_BYTES, MAX_RESOURCE_FILES,
    MAX_RESOURCE_ROOTS, MAX_RESOURCE_TOTAL_BYTES, ResourceLoadError, ResourceLoadLimit,
    ResourceLoadPolicy,
};
pub use types::{
    AgentResources, DiagnosticSeverity, PromptTemplate, ResourceDiagnostic, Skill, SourceTag,
    SourcedPromptTemplate, SourcedResourceDiagnostic, SourcedSkill,
};

pub mod frontmatter;
pub mod prompt_templates;
pub mod skills;
pub mod system_prompt;

pub use frontmatter::parse_frontmatter;
pub use prompt_templates::{
    load_prompt_templates, load_prompt_templates_async, load_prompt_templates_with_policy,
    load_sourced_prompt_templates, load_sourced_prompt_templates_async,
    load_sourced_prompt_templates_with_policy,
};
pub use skills::{
    load_skills, load_skills_async, load_skills_with_policy, load_sourced_skills,
    load_sourced_skills_async, load_sourced_skills_with_policy,
};
pub use system_prompt::{
    format_prompt_template_invocation, format_skill_invocation, format_skills_for_system_prompt,
    parse_command_args, substitute_args,
};

pub(crate) fn parse_frontmatter_at_path(
    content: &str,
    path: &std::path::Path,
    diagnostics: &mut Vec<ResourceDiagnostic>,
) -> (serde_yaml::Value, String) {
    let (meta, body, mut meta_diags) = parse_frontmatter(content);
    for d in &mut meta_diags {
        d.path = path.to_path_buf();
    }
    diagnostics.append(&mut meta_diags);
    (meta, body)
}
