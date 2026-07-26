use crate::agent::types::{
    DiagnosticSeverity, ResourceDiagnostic, Skill, SourceTag, SourcedResourceDiagnostic,
    SourcedSkill,
};
use crate::resources::loader::{
    BlockingLoadGuard, ResourceLoadBudget, error_diagnostic, path_metadata,
};
use crate::resources::{ResourceLoadError, ResourceLoadPolicy, parse_frontmatter_at_path};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

pub fn load_skills(paths: &[PathBuf]) -> (Vec<Skill>, Vec<ResourceDiagnostic>) {
    match load_skills_with_policy(paths, ResourceLoadPolicy::default(), None) {
        Ok(loaded) => loaded,
        Err(error) => (Vec::new(), vec![error_diagnostic(&error)]),
    }
}

/// Blocking skill loader. Call it only from synchronous startup code or from
/// an explicitly owned blocking worker; async callers should use
/// [`load_skills_async`].
pub fn load_skills_with_policy(
    paths: &[PathBuf],
    policy: ResourceLoadPolicy,
    cancellation: Option<&CancellationToken>,
) -> Result<(Vec<Skill>, Vec<ResourceDiagnostic>), ResourceLoadError> {
    let mut budget = ResourceLoadBudget::new(policy, cancellation)?;
    load_skills_bounded(paths, &mut budget)
}

pub async fn load_skills_async(
    paths: Vec<PathBuf>,
    policy: ResourceLoadPolicy,
    cancellation: CancellationToken,
) -> Result<(Vec<Skill>, Vec<ResourceDiagnostic>), ResourceLoadError> {
    if cancellation.is_cancelled() {
        return Err(ResourceLoadError::Cancelled);
    }
    let worker_guard = BlockingLoadGuard::new();
    let worker_cancellation = worker_guard.token();
    let mut worker = tokio::task::spawn_blocking(move || {
        load_skills_with_policy(&paths, policy, Some(&worker_cancellation))
    });
    tokio::select! {
        result = &mut worker => result.map_err(|_| ResourceLoadError::Worker)?,
        _ = cancellation.cancelled() => {
            worker_guard.cancel();
            let _ = worker.await;
            Err(ResourceLoadError::Cancelled)
        }
    }
}

fn load_skills_bounded(
    paths: &[PathBuf],
    budget: &mut ResourceLoadBudget<'_>,
) -> Result<(Vec<Skill>, Vec<ResourceDiagnostic>), ResourceLoadError> {
    let mut skills = Vec::new();
    let mut diagnostics = Vec::new();

    for root in paths {
        budget.visit_root(root)?;
        let Some(metadata) = path_metadata(root, &mut diagnostics) else {
            continue;
        };

        if metadata.is_dir() {
            load_skills_from_dir(root, budget, &mut skills, &mut diagnostics)?;
        } else if metadata.is_file()
            && let Some(ext) = root.extension()
            && ext == "md"
        {
            let path = root.clone();
            if let Some(skill) = load_skill_file(&path, budget, &mut diagnostics)? {
                skills.push(skill);
            }
        }
    }

    Ok((skills, diagnostics))
}

/// Load skills from sourced inputs. Each entry's input path is loaded with
/// [`load_skills`]; every resulting skill and diagnostic is tagged with the
/// associated [`SourceTag`]. Mirrors TS `loadSourcedSkills`
/// (`pi/packages/agent/src/harness/skills.ts:83`).
pub fn load_sourced_skills(
    inputs: &[(PathBuf, SourceTag)],
) -> (Vec<SourcedSkill>, Vec<SourcedResourceDiagnostic>) {
    match load_sourced_skills_with_policy(inputs, ResourceLoadPolicy::default(), None) {
        Ok(loaded) => loaded,
        Err(error) => {
            let diagnostics = inputs
                .first()
                .map(|(_, source)| {
                    vec![SourcedResourceDiagnostic {
                        diagnostic: error_diagnostic(&error),
                        source: source.clone(),
                    }]
                })
                .unwrap_or_default();
            (Vec::new(), diagnostics)
        }
    }
}

pub fn load_sourced_skills_with_policy(
    inputs: &[(PathBuf, SourceTag)],
    policy: ResourceLoadPolicy,
    cancellation: Option<&CancellationToken>,
) -> Result<(Vec<SourcedSkill>, Vec<SourcedResourceDiagnostic>), ResourceLoadError> {
    let mut sourced_skills = Vec::new();
    let mut sourced_diagnostics = Vec::new();
    let mut budget = ResourceLoadBudget::new(policy, cancellation)?;
    for (path, source) in inputs {
        let (skills, diagnostics) = load_skills_bounded(std::slice::from_ref(path), &mut budget)?;
        for skill in skills {
            sourced_skills.push(SourcedSkill {
                skill,
                source: source.clone(),
            });
        }
        for diagnostic in diagnostics {
            sourced_diagnostics.push(SourcedResourceDiagnostic {
                diagnostic,
                source: source.clone(),
            });
        }
    }
    Ok((sourced_skills, sourced_diagnostics))
}

pub async fn load_sourced_skills_async(
    inputs: Vec<(PathBuf, SourceTag)>,
    policy: ResourceLoadPolicy,
    cancellation: CancellationToken,
) -> Result<(Vec<SourcedSkill>, Vec<SourcedResourceDiagnostic>), ResourceLoadError> {
    if cancellation.is_cancelled() {
        return Err(ResourceLoadError::Cancelled);
    }
    let worker_guard = BlockingLoadGuard::new();
    let worker_cancellation = worker_guard.token();
    let mut worker = tokio::task::spawn_blocking(move || {
        load_sourced_skills_with_policy(&inputs, policy, Some(&worker_cancellation))
    });
    tokio::select! {
        result = &mut worker => result.map_err(|_| ResourceLoadError::Worker)?,
        _ = cancellation.cancelled() => {
            worker_guard.cancel();
            let _ = worker.await;
            Err(ResourceLoadError::Cancelled)
        }
    }
}

fn load_skills_from_dir(
    root: &PathBuf,
    budget: &mut ResourceLoadBudget<'_>,
    skills: &mut Vec<Skill>,
    diagnostics: &mut Vec<ResourceDiagnostic>,
) -> Result<(), ResourceLoadError> {
    let walker = WalkBuilder::new(root)
        .git_ignore(true)
        .hidden(false)
        .follow_links(false)
        .max_depth(Some(budget.policy().max_depth))
        .build();

    for entry in walker {
        budget.check_cancelled()?;
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                diagnostics.push(ResourceDiagnostic {
                    severity: DiagnosticSeverity::Warning,
                    code: "resource_walk_error".into(),
                    message: format!("failed to traverse resource directory: {error}"),
                    path: root.clone(),
                });
                continue;
            }
        };
        let path = entry.path().to_path_buf();
        budget.visit_entry(&path)?;
        if path == *root {
            continue;
        }
        let Some(metadata) = path_metadata(&path, diagnostics) else {
            continue;
        };
        let is_skill_file = entry.file_name() == "SKILL.md";
        let is_direct_markdown =
            entry.depth() == 1 && path.extension().is_some_and(|extension| extension == "md");
        if metadata.is_file()
            && (is_skill_file || is_direct_markdown)
            && let Some(skill) = load_skill_file(&path, budget, diagnostics)?
        {
            skills.push(skill);
        }
    }
    Ok(())
}

fn load_skill_file(
    path: &Path,
    budget: &mut ResourceLoadBudget<'_>,
    diagnostics: &mut Vec<ResourceDiagnostic>,
) -> Result<Option<Skill>, ResourceLoadError> {
    let Some(content) = budget.read_text(path, "skill_read_error", diagnostics)? else {
        return Ok(None);
    };

    let (meta, body) = parse_frontmatter_at_path(&content, path, diagnostics);

    let parent_dir_name = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string());

    let frontmatter_name = meta.get("name").and_then(|v| v.as_str()).map(str::to_owned);
    let name = frontmatter_name
        .clone()
        .map(|value| value.chars().take(64).collect())
        .unwrap_or_else(|| fallback_name(path));

    // Validate name against TS `validateName` rules.
    if let Some(ref parent_name) = parent_dir_name
        && let Some(ref fm_name) = frontmatter_name
        && fm_name.as_str() != parent_name.as_str()
    {
        diagnostics.push(ResourceDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "invalid_metadata".into(),
            message: format!(
                "name \"{fm_name}\" does not match parent directory \"{parent_name}\""
            ),
            path: path.to_path_buf(),
        });
    }
    for error in validate_skill_name(frontmatter_name.as_deref().unwrap_or(&name)) {
        diagnostics.push(ResourceDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "invalid_metadata".into(),
            message: error,
            path: path.to_path_buf(),
        });
    }

    let description_raw = meta
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let description = description_raw
        .clone()
        .map(|value| value.chars().take(1024).collect::<String>());

    // Reject skills with empty description (TS behavior).
    if description.as_deref().is_none_or(|d| d.trim().is_empty()) {
        diagnostics.push(ResourceDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "invalid_metadata".into(),
            message: "description is required".into(),
            path: path.to_path_buf(),
        });
        return Ok(None);
    }

    if let Some(ref desc) = description_raw
        && desc.chars().count() > 1024
    {
        diagnostics.push(ResourceDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "invalid_metadata".into(),
            message: format!(
                "description exceeds {} characters ({})",
                1024,
                desc.chars().count()
            ),
            path: path.to_path_buf(),
        });
    }
    let description = description.unwrap(); // safe: we returned None above if None/empty

    let disable_model_invocation = meta
        .get("disable-model-invocation")
        .or_else(|| meta.get("disableModelInvocation"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let location = path.to_string_lossy().to_string();

    Ok(Some(Skill {
        name,
        description,
        location,
        content: body,
        disable_model_invocation,
    }))
}

/// Validate a skill name against TS `validateName` rules:
/// - only lowercase a-z, 0-9, hyphens
/// - no leading or trailing hyphens
/// - no consecutive hyphens
/// - max 64 characters
fn validate_skill_name(name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let character_count = name.chars().count();
    if character_count > 64 {
        errors.push(format!("name exceeds 64 characters ({character_count})"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        errors.push(
            "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)".into(),
        );
    }
    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".into());
    }
    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".into());
    }
    errors
}

fn fallback_name(path: &Path) -> String {
    if let Some(stem) = path.file_stem() {
        let s = stem.to_string_lossy();
        let capped: String = s.chars().take(64).collect();
        return capped;
    }
    if let Some(parent) = path.parent()
        && let Some(name) = parent.file_name()
    {
        let s = name.to_string_lossy();
        let capped: String = s.chars().take(64).collect();
        return capped;
    }
    "unnamed".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn loads_skill_md_from_directory() {
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join("rust");
        std::fs::create_dir(&skill_dir).unwrap();
        let skill_md = skill_dir.join("SKILL.md");
        let mut f = std::fs::File::create(&skill_md).unwrap();
        writeln!(
            f,
            "---\nname: rust\ndescription: Rust programming\n---\n\nRust skill content."
        )
        .unwrap();

        let (skills, diags) = load_skills(&[skill_dir]);
        assert!(diags.is_empty());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "rust");
        assert_eq!(skills[0].description, "Rust programming");
        assert!(skills[0].content.contains("Rust skill content"));
        assert!(!skills[0].disable_model_invocation);
    }

    #[test]
    fn skips_ignored_directories() {
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join("visible");
        std::fs::create_dir(&skill_dir).unwrap();
        let skill_md = skill_dir.join("SKILL.md");
        std::fs::write(
            &skill_md,
            "---\nname: visible\ndescription: A visible skill\n---\n\ncontent",
        )
        .unwrap();

        let hidden_dir = dir.path().join("hidden");
        std::fs::create_dir(&hidden_dir).unwrap();
        let gitignore = dir.path().join(".gitignore");
        std::fs::write(&gitignore, "hidden/").unwrap();

        let (skills, diags) = load_skills(&[dir.path().to_path_buf()]);
        let _ = diags;
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "visible");
    }

    #[test]
    fn missing_root_is_skipped() {
        let (skills, diags) = load_skills(&[PathBuf::from("/nonexistent/path/12345")]);
        assert!(diags.is_empty());
        assert!(skills.is_empty());
    }

    #[test]
    fn rejects_skill_with_empty_description() {
        let dir = TempDir::new().unwrap();
        let skill_md = dir.path().join("SKILL.md");
        std::fs::write(&skill_md, "---\nname: noskill\n---\n\ncontent").unwrap();
        let (skills, diags) = load_skills(&[dir.path().to_path_buf()]);
        assert!(
            skills.is_empty(),
            "skill with no description should be rejected"
        );
        assert!(!diags.is_empty());
        assert!(diags.iter().any(|d| d.message.contains("description")));
    }

    #[test]
    fn validates_skill_name_rules() {
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join("bad-name");
        std::fs::create_dir(&skill_dir).unwrap();
        let skill_md = skill_dir.join("SKILL.md");
        std::fs::write(
            &skill_md,
            "---\nname: BAD_NAME\ndescription: test\n---\n\ncontent",
        )
        .unwrap();
        let (skills, diags) = load_skills(&[dir.path().to_path_buf()]);
        // Name is invalid but skill is still loaded (TS emits warning, not rejection)
        assert_eq!(skills.len(), 1);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("invalid characters"))
        );
    }

    #[test]
    fn validates_unicode_name_and_description_lengths_by_characters() {
        let dir = TempDir::new().unwrap();
        let skill_md = dir.path().join("SKILL.md");
        let long_name = "a".repeat(65);
        let long_description = "界".repeat(1025);
        std::fs::write(
            &skill_md,
            format!("---\nname: {long_name}\ndescription: {long_description}\n---\n\ncontent"),
        )
        .unwrap();
        let (skills, diags) = load_skills(&[dir.path().to_path_buf()]);
        assert_eq!(skills.len(), 1);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("name exceeds 64 characters (65)"))
        );
        assert!(diags.iter().any(|d| {
            d.message
                .contains("description exceeds 1024 characters (1025)")
        }));
        assert_eq!(skills[0].description.chars().count(), 1024);
    }
}
