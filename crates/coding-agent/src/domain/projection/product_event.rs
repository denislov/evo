use crate::events::{
    CodingAgentProductEventCheckOutput, CodingAgentProductEventDiagnostic,
    CodingAgentProductEventReplacement, CodingAgentProductEventUsage,
};

impl From<ai::api::conversation::Usage> for CodingAgentProductEventUsage {
    fn from(usage: ai::api::conversation::Usage) -> Self {
        Self {
            input: usage.input,
            output: usage.output,
            reasoning_tokens: usage.reasoning_tokens,
            cache_read: usage.cache_read,
            cache_write: usage.cache_write,
            total_tokens: usage.total_tokens,
            cost_known: usage.cost.known,
            input_cost: usage.cost.input,
            output_cost: usage.cost.output,
            cache_read_cost: usage.cost.cache_read,
            cache_write_cost: usage.cost.cache_write,
        }
    }
}

impl From<crate::operations::self_healing_edit::runner::SelfHealingEditReplacement>
    for CodingAgentProductEventReplacement
{
    fn from(
        replacement: crate::operations::self_healing_edit::runner::SelfHealingEditReplacement,
    ) -> Self {
        Self {
            old_text: replacement.old_text,
            new_text: replacement.new_text,
        }
    }
}

impl From<crate::operations::self_healing_edit::runner::SelfHealingEditDiagnostic>
    for CodingAgentProductEventDiagnostic
{
    fn from(
        diagnostic: crate::operations::self_healing_edit::runner::SelfHealingEditDiagnostic,
    ) -> Self {
        Self {
            message: diagnostic.message,
        }
    }
}

impl From<crate::operations::self_healing_edit::runner::SelfHealingEditCheckOutput>
    for CodingAgentProductEventCheckOutput
{
    fn from(
        output: crate::operations::self_healing_edit::runner::SelfHealingEditCheckOutput,
    ) -> Self {
        Self {
            command: output.command,
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
        }
    }
}
