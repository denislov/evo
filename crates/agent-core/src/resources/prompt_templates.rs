use crate::agent::types::{
    DiagnosticSeverity, PromptTemplate, ResourceDiagnostic, SourceTag, SourcedPromptTemplate,
    SourcedResourceDiagnostic,
};
use crate::resources::loader::{
    BlockingLoadGuard, ResourceLoadBudget, error_diagnostic, path_metadata,
};
use crate::resources::{ResourceLoadError, ResourceLoadPolicy, parse_frontmatter_at_path};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

pub fn load_prompt_templates(paths: &[PathBuf]) -> (Vec<PromptTemplate>, Vec<ResourceDiagnostic>) {
    match load_prompt_templates_with_policy(paths, ResourceLoadPolicy::default(), None) {
        Ok(loaded) => loaded,
        Err(error) => (Vec::new(), vec![error_diagnostic(&error)]),
    }
}

/// Blocking prompt-template loader. Async callers should use
/// [`load_prompt_templates_async`] so filesystem traversal never blocks their
/// executor worker.
pub fn load_prompt_templates_with_policy(
    paths: &[PathBuf],
    policy: ResourceLoadPolicy,
    cancellation: Option<&CancellationToken>,
) -> Result<(Vec<PromptTemplate>, Vec<ResourceDiagnostic>), ResourceLoadError> {
    let mut budget = ResourceLoadBudget::new(policy, cancellation)?;
    load_prompt_templates_bounded(paths, &mut budget)
}

pub async fn load_prompt_templates_async(
    paths: Vec<PathBuf>,
    policy: ResourceLoadPolicy,
    cancellation: CancellationToken,
) -> Result<(Vec<PromptTemplate>, Vec<ResourceDiagnostic>), ResourceLoadError> {
    if cancellation.is_cancelled() {
        return Err(ResourceLoadError::Cancelled);
    }
    let worker_guard = BlockingLoadGuard::new();
    let worker_cancellation = worker_guard.token();
    let mut worker = tokio::task::spawn_blocking(move || {
        load_prompt_templates_with_policy(&paths, policy, Some(&worker_cancellation))
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

fn load_prompt_templates_bounded(
    paths: &[PathBuf],
    budget: &mut ResourceLoadBudget<'_>,
) -> Result<(Vec<PromptTemplate>, Vec<ResourceDiagnostic>), ResourceLoadError> {
    let mut templates = Vec::new();
    let mut diagnostics = Vec::new();

    for path in paths {
        budget.visit_root(path)?;
        let Some(metadata) = path_metadata(path, &mut diagnostics) else {
            continue;
        };

        if metadata.is_file() {
            if path.extension().is_some_and(|e| e == "md")
                && let Some(t) = load_template_file(path, budget, &mut diagnostics)?
            {
                templates.push(t);
            }
        } else if metadata.is_dir() {
            let entries = match std::fs::read_dir(path) {
                Ok(entries) => entries,
                Err(error) => {
                    diagnostics.push(ResourceDiagnostic {
                        severity: DiagnosticSeverity::Warning,
                        code: "template_read_dir_error".into(),
                        message: format!(
                            "failed to read template directory {}: {error}",
                            path.display()
                        ),
                        path: path.clone(),
                    });
                    continue;
                }
            };
            let mut files = Vec::new();
            for entry in entries {
                budget.check_cancelled()?;
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        diagnostics.push(ResourceDiagnostic {
                            severity: DiagnosticSeverity::Warning,
                            code: "template_read_dir_error".into(),
                            message: format!(
                                "failed to read an entry in {}: {error}",
                                path.display()
                            ),
                            path: path.clone(),
                        });
                        continue;
                    }
                };
                budget.visit_entry(&entry.path())?;
                files.push(entry);
            }
            files.sort_by_key(|e| e.file_name());
            for entry in files {
                let p = entry.path();
                let Some(metadata) = path_metadata(&p, &mut diagnostics) else {
                    continue;
                };
                if metadata.is_file()
                    && p.extension().is_some_and(|e| e == "md")
                    && let Some(t) = load_template_file(&p, budget, &mut diagnostics)?
                {
                    templates.push(t);
                }
            }
        }
    }

    // Deduplicate by name (first wins, TS behavior). Duplicates produce
    // collision diagnostics so users know a later template shadowed an
    // earlier one.
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut deduped: Vec<PromptTemplate> = Vec::new();
    for template in templates {
        if let Some(&existing_idx) = seen.get(&template.name) {
            let existing_loc = deduped[existing_idx].location.clone();
            diagnostics.push(ResourceDiagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "prompt_collision".into(),
                message: format!(
                    "name \"/{}\" collision (using {}, ignoring {})",
                    template.name, existing_loc, template.location
                ),
                path: PathBuf::from(&template.location),
            });
        } else {
            let idx = deduped.len();
            seen.insert(template.name.clone(), idx);
            deduped.push(template);
        }
    }

    Ok((deduped, diagnostics))
}

/// Load prompt templates from sourced inputs. Mirrors
/// [`crate::resources::skills::load_sourced_skills`].
pub fn load_sourced_prompt_templates(
    inputs: &[(PathBuf, SourceTag)],
) -> (Vec<SourcedPromptTemplate>, Vec<SourcedResourceDiagnostic>) {
    match load_sourced_prompt_templates_with_policy(inputs, ResourceLoadPolicy::default(), None) {
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

pub fn load_sourced_prompt_templates_with_policy(
    inputs: &[(PathBuf, SourceTag)],
    policy: ResourceLoadPolicy,
    cancellation: Option<&CancellationToken>,
) -> Result<(Vec<SourcedPromptTemplate>, Vec<SourcedResourceDiagnostic>), ResourceLoadError> {
    let mut sourced_templates = Vec::new();
    let mut sourced_diagnostics = Vec::new();
    let mut budget = ResourceLoadBudget::new(policy, cancellation)?;
    for (path, source) in inputs {
        let (templates, diagnostics) =
            load_prompt_templates_bounded(std::slice::from_ref(path), &mut budget)?;
        for template in templates {
            sourced_templates.push(SourcedPromptTemplate {
                template,
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
    Ok((sourced_templates, sourced_diagnostics))
}

pub async fn load_sourced_prompt_templates_async(
    inputs: Vec<(PathBuf, SourceTag)>,
    policy: ResourceLoadPolicy,
    cancellation: CancellationToken,
) -> Result<(Vec<SourcedPromptTemplate>, Vec<SourcedResourceDiagnostic>), ResourceLoadError> {
    if cancellation.is_cancelled() {
        return Err(ResourceLoadError::Cancelled);
    }
    let worker_guard = BlockingLoadGuard::new();
    let worker_cancellation = worker_guard.token();
    let mut worker = tokio::task::spawn_blocking(move || {
        load_sourced_prompt_templates_with_policy(&inputs, policy, Some(&worker_cancellation))
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

fn load_template_file(
    path: &std::path::Path,
    budget: &mut ResourceLoadBudget<'_>,
    diagnostics: &mut Vec<ResourceDiagnostic>,
) -> Result<Option<PromptTemplate>, ResourceLoadError> {
    let Some(content) = budget.read_text(path, "template_read_error", diagnostics)? else {
        return Ok(None);
    };

    let (meta, body) = parse_frontmatter_at_path(&content, path, diagnostics);

    let name = meta
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "unnamed".to_string())
        });

    let description = meta
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| {
            let capped: String = s.chars().take(60).collect();
            if s.len() > 60 {
                format!("{}...", capped)
            } else {
                capped
            }
        })
        .unwrap_or_else(|| {
            let first_line = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            let capped: String = first_line.chars().take(60).collect();
            if first_line.len() > 60 {
                format!("{}...", capped)
            } else {
                capped
            }
        });

    Ok(Some(PromptTemplate {
        name,
        description,
        content: body,
        location: path.display().to_string(),
    }))
}
