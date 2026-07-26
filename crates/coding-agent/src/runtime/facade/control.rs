use super::*;

#[derive(Debug)]
#[must_use = "dropping PendingPromptControlCleanupGuard clears exact pending Prompt control ownership"]
pub(in crate::runtime) struct PendingPromptControlCleanupGuard {
    cleanup: PromptControlCleanup,
    channel_generation: PromptControlGeneration,
}

impl PendingPromptControlCleanupGuard {
    fn new(cleanup: PromptControlCleanup, channel_generation: PromptControlGeneration) -> Self {
        Self {
            cleanup,
            channel_generation,
        }
    }
}

impl Drop for PendingPromptControlCleanupGuard {
    fn drop(&mut self) {
        self.cleanup.clear_if_generation(self.channel_generation);
    }
}

#[derive(Debug)]
#[must_use = "dropping PromptControlCleanupGuard clears exact Prompt control ownership"]
pub(in crate::runtime) struct PromptControlCleanupGuard {
    cleanup: PromptControlCleanup,
    snapshot_coordinator: Arc<SnapshotCoordinator>,
    operation_id: String,
    channel_generation: PromptControlGeneration,
    armed: bool,
}

impl PromptControlCleanupGuard {
    pub(in crate::runtime) fn new(
        cleanup: PromptControlCleanup,
        snapshot_coordinator: Arc<SnapshotCoordinator>,
        operation_id: String,
        channel_generation: PromptControlGeneration,
    ) -> Self {
        Self {
            cleanup,
            snapshot_coordinator,
            operation_id,
            channel_generation,
            armed: true,
        }
    }

    pub(in crate::runtime) fn cleanup(&mut self) {
        if !self.armed {
            return;
        }
        self.snapshot_coordinator
            .clear_prompt_control_if(&self.operation_id, self.channel_generation);
        self.cleanup.clear_if_generation(self.channel_generation);
        self.armed = false;
    }
}

impl Drop for PromptControlCleanupGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

impl CodingAgentSession {
    pub(in crate::runtime) fn pending_prompt_control_cleanup_guard(
        &self,
    ) -> Option<PendingPromptControlCleanupGuard> {
        let registration = self
            .runtime_host
            .operation_supervisor
            .control
            .current_prompt_control_registration()?;
        Some(PendingPromptControlCleanupGuard::new(
            self.runtime_host
                .operation_supervisor
                .control
                .prompt_control_cleanup(),
            registration.generation,
        ))
    }

    #[cfg(test)]
    pub(crate) fn prompt_control_handle(
        &mut self,
    ) -> Result<PromptControlHandle, CodingSessionError> {
        self.runtime_host
            .operation_supervisor
            .control
            .prompt_control_handle()
    }
}
