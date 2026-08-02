#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfHealingEditReplacement {
    pub old_text: String,
    pub new_text: String,
}

impl SelfHealingEditReplacement {
    pub fn new(old_text: impl Into<String>, new_text: impl Into<String>) -> Self {
        Self {
            old_text: old_text.into(),
            new_text: new_text.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfHealingEditDiagnostic {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfHealingEditCheckOutput {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfHealingEditRepairAttempt {
    pub attempt: usize,
    pub replacements: Vec<SelfHealingEditReplacement>,
    pub diagnostics: Vec<SelfHealingEditDiagnostic>,
    pub check_output: Option<SelfHealingEditCheckOutput>,
}
