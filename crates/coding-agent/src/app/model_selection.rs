use crate::app::error::ApplicationError;
use agent_core::api::agent::ThinkingLevel;
use globset::{Glob, GlobMatcher};

#[derive(Debug)]
pub struct ModelRotation {
    pub entries: Vec<ModelRotationEntry>,
}

#[derive(Debug)]
pub struct ModelRotationEntry {
    matcher: GlobMatcher,
}

impl ModelRotation {
    pub fn matches(&self, model_id: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.matcher.is_match(model_id))
    }
}

pub fn parse_model_rotation(value: &str) -> Result<ModelRotation, ApplicationError> {
    let mut entries = Vec::new();
    for raw in value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let (pattern, thinking) = match raw.rsplit_once(':') {
            Some((pattern, level)) if !pattern.is_empty() && !level.is_empty() => {
                let thinking: ThinkingLevel =
                    level.parse().map_err(ApplicationError::InvalidInput)?;
                (pattern.to_string(), Some(thinking))
            }
            _ => (raw.to_string(), None),
        };
        let matcher = Glob::new(&pattern)
            .map_err(|error| {
                ApplicationError::InvalidInput(format!("invalid model glob {pattern}: {error}"))
            })?
            .compile_matcher();
        entries.push(ModelRotationEntry {
            matcher,
        });
        let _ = thinking;
    }
    if entries.is_empty() {
        return Err(ApplicationError::InvalidInput(
            "--models cannot be empty".into(),
        ));
    }
    Ok(ModelRotation { entries })
}
