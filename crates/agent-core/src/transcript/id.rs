use std::collections::HashSet;
use std::fmt;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

pub fn create_session_id() -> String {
    Uuid::now_v7().to_string()
}

pub fn create_timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
        .replace("+00:00", "Z")
}

pub fn generate_entry_id(existing: &HashSet<String>) -> String {
    for _ in 0..100 {
        let full = create_session_id();
        let short = full.chars().take(8).collect::<String>();
        if !existing.contains(&short) {
            return short;
        }
    }
    create_session_id()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptIdError {
    message: String,
}

impl TranscriptIdError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for TranscriptIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TranscriptIdError {}

#[derive(Debug, Clone)]
pub struct SessionIdGenerator {
    pub session_id: String,
    pub entry_ids: Vec<String>,
    pub timestamp: String,
}

impl SessionIdGenerator {
    pub fn fixed(session_id: &str, entry_ids: Vec<&str>, timestamp: &str) -> Self {
        Self {
            session_id: session_id.into(),
            entry_ids: entry_ids.into_iter().map(str::to_string).collect(),
            timestamp: timestamp.into(),
        }
    }

    pub fn next_entry_id(&mut self) -> Result<String, TranscriptIdError> {
        if self.entry_ids.is_empty() {
            return Err(TranscriptIdError::new("test entry id generator exhausted"));
        }
        Ok(self.entry_ids.remove(0))
    }
}
