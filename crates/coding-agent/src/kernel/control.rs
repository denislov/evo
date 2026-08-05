use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PromptControlCommand {
    Abort {
        reason: String,
    },
    Steer {
        text: String,
    },
    SteerContent {
        content: Vec<ai_protocol::api::conversation::ContentBlock>,
    },
    FollowUp {
        text: String,
    },
    FollowUpContent {
        content: Vec<ai_protocol::api::conversation::ContentBlock>,
    },
}

pub(crate) type PromptControlReceiver = mpsc::Receiver<PromptControlCommand>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PromptControlGeneration(pub(crate) u64);
