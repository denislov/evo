use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cost {
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub known: bool,
    pub input: f64,
    pub output: f64,
    #[serde(rename = "cacheRead")]
    pub cache_read: f64,
    #[serde(rename = "cacheWrite")]
    pub cache_write: f64,
}

impl Cost {
    pub fn unknown() -> Self {
        Self {
            known: false,
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        }
    }
}

impl Default for Cost {
    fn default() -> Self {
        Self {
            known: true,
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        }
    }
}

const fn default_true() -> bool {
    true
}

const fn is_true(value: &bool) -> bool {
    *value
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Usage {
    pub input: u32,
    pub output: u32,
    /// Subset of `output` consumed by chain-of-thought generation.
    #[serde(rename = "reasoningTokens", default, skip_serializing_if = "is_zero")]
    pub reasoning_tokens: u32,
    #[serde(rename = "cacheRead")]
    pub cache_read: u32,
    #[serde(rename = "cacheWrite")]
    pub cache_write: u32,
    #[serde(rename = "totalTokens")]
    pub total_tokens: u32,
    pub cost: Cost,
}

const fn is_zero(value: &u32) -> bool {
    *value == 0
}

pub(crate) const fn saturating_token_total(input: u32, output: u32) -> u32 {
    input.saturating_add(output)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StopReason {
    Stop,
    Length,
    #[serde(rename = "toolUse")]
    ToolUse,
    Error,
    Aborted,
}
