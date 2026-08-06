use ai_protocol::api::conversation::ContentBlock;

use crate::agent::runtime::Agent;
use crate::agent::types::{AgentMessage, AgentQueueError};

impl Agent {
    pub fn steer(&self, text: impl Into<String>) -> Result<(), AgentQueueError> {
        self.handle.steer(text.into())
    }

    pub fn steer_content(&self, content: Vec<ContentBlock>) -> Result<(), AgentQueueError> {
        self.handle.steer_content(content)
    }

    pub fn follow_up(&self, text: impl Into<String>) -> Result<(), AgentQueueError> {
        self.handle.follow_up(text.into())
    }

    pub fn follow_up_content(&self, content: Vec<ContentBlock>) -> Result<(), AgentQueueError> {
        self.handle.follow_up_content(content)
    }

    pub fn interject(&self, text: impl Into<String>) -> Result<(), AgentQueueError> {
        self.handle.interject(text.into())
    }

    pub fn interject_content(&self, content: Vec<ContentBlock>) -> Result<(), AgentQueueError> {
        self.handle.interject_content(content)
    }

    pub fn clear_queues(&self) {
        self.handle.clear_queues();
    }

    pub async fn edit_queue_entry(
        &self,
        entry_id: impl Into<String>,
        expected_version: u32,
        new_message: AgentMessage,
    ) -> Result<(), AgentQueueError> {
        self.handle
            .edit_queue_entry(entry_id.into(), expected_version, new_message)
            .await
            .unwrap_or(Err(AgentQueueError::ActorClosed))
    }

    pub async fn remove_queue_entry(
        &self,
        entry_id: impl Into<String>,
        expected_version: u32,
    ) -> Result<(), AgentQueueError> {
        self.handle
            .remove_queue_entry(entry_id.into(), expected_version)
            .await
            .unwrap_or(Err(AgentQueueError::ActorClosed))
    }

    /// Drain and return all queued steering messages.
    pub async fn drain_steering_queue(&self) -> Vec<AgentMessage> {
        self.handle.drain_steering_queue().await.unwrap_or_default()
    }

    /// Drain and return all queued follow-up messages.
    pub async fn drain_follow_up_queue(&self) -> Vec<AgentMessage> {
        self.handle
            .drain_follow_up_queue()
            .await
            .unwrap_or_default()
    }
}
