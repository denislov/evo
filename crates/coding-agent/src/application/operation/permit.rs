use tokio_util::sync::CancellationToken;

use super::OperationExecution;
use super::control::{ChildOperationGuard, OperationCancellationHandle, OperationGuard};
use crate::application::capability::OperationCapabilitySnapshot;
use crate::kernel::error::CodingSessionError;

#[derive(Debug)]
#[must_use = "dropping OperationPermit releases any guarded operation"]
pub(crate) struct OperationPermit {
    guard: Option<OperationGuard>,
    child_guard: Option<ChildOperationGuard>,
    execution: OperationExecution,
    cancellation: Option<CancellationToken>,
    cancellation_handle: Option<OperationCancellationHandle>,
}

impl OperationPermit {
    pub(crate) fn guarded(
        mut guard: OperationGuard,
        execution: OperationExecution,
    ) -> Result<Self, CodingSessionError> {
        guard.bind_capability_generation(execution.capability_generation)?;
        let cancellation = guard.cancellation_token();
        let cancellation_handle = Some(guard.cancellation_handle());

        Ok(Self {
            guard: Some(guard),
            child_guard: None,
            execution,
            cancellation,
            cancellation_handle,
        })
    }

    pub(crate) fn unguarded(execution: OperationExecution) -> Self {
        Self {
            guard: None,
            child_guard: None,
            execution,
            cancellation: None,
            cancellation_handle: None,
        }
    }

    pub(crate) fn child(
        execution: OperationExecution,
        mut guard: ChildOperationGuard,
    ) -> Result<Self, CodingSessionError> {
        guard.bind_capability_generation(execution.capability_generation)?;
        let cancellation = Some(guard.cancellation_token());
        let cancellation_handle = Some(guard.cancellation_handle());

        Ok(Self {
            guard: None,
            child_guard: Some(guard),
            execution,
            cancellation,
            cancellation_handle,
        })
    }

    pub(crate) fn capability_snapshot(&self) -> &OperationCapabilitySnapshot {
        &self.execution.capability_snapshot
    }

    pub(crate) fn execution(&self) -> &OperationExecution {
        &self.execution
    }

    pub(crate) fn cancellation_token(&self) -> Option<CancellationToken> {
        self.cancellation.clone()
    }

    pub(crate) fn cancellation_handle(&self) -> Option<OperationCancellationHandle> {
        self.cancellation_handle.clone()
    }

    /// Release admission ownership early while retaining immutable execution
    /// metadata for finalization. Session forking is the sole caller: the old
    /// session guard must be gone before the writer switches to the new session.
    /// Filesystem bindings remain owned by this permit until the operation
    /// itself reaches a terminal exit and the permit is dropped.
    pub(crate) fn release(&mut self) {
        self.guard.take();
        self.child_guard.take();
    }
}

impl Drop for OperationPermit {
    fn drop(&mut self) {
        if let Some(filesystem) = &self.execution.capability_snapshot.filesystem {
            filesystem.discard_operation_bindings(&self.execution.operation_id);
        }
        let _ = self.guard.is_some();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::operation::contract::CodingAgentOperation;
    use crate::application::operation::{OperationExecution, OperationOrigin};
    use crate::kernel::capability::{
        ActorId, CapabilityGeneration, CommandCapabilitySet, ToolCapabilitySet,
    };
    use crate::kernel::operation::OperationKind;
    use tool_contract::api::definition::ToolId;

    #[derive(Clone, Copy)]
    enum TerminalExit {
        Committed,
        Aborted,
        Failed,
    }

    fn execution(
        operation_id: &str,
        filesystem: crate::platform::fs::capability::FilesystemCapability,
    ) -> OperationExecution {
        let capability_snapshot = OperationCapabilitySnapshot {
            generation: CapabilityGeneration::new(1),
            operation_id: operation_id.to_owned(),
            actor: ActorId::Client,
            model: None,
            tools: ToolCapabilitySet::from_ids([ToolId::new("read").unwrap()]),
            commands: CommandCapabilitySet::default(),
            filesystem: Some(filesystem),
            shell: None,
            session_read: None,
            session_write: None,
            ui: None,
        };
        OperationExecution::root(
            OperationKind::Export,
            CodingAgentOperation::ExportCurrent.descriptor(),
            OperationOrigin::ClientRoot,
            None,
            None,
            capability_snapshot,
        )
    }

    fn leave_operation(
        _permit: OperationPermit,
        terminal: TerminalExit,
    ) -> Result<TerminalExit, &'static str> {
        match terminal {
            TerminalExit::Committed => Ok(TerminalExit::Committed),
            TerminalExit::Aborted => Ok(TerminalExit::Aborted),
            TerminalExit::Failed => Err("failed"),
        }
    }

    #[tokio::test]
    async fn every_terminal_exit_discards_all_and_only_its_operation_bindings() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("one.txt"), "one").expect("write one");
        std::fs::write(temp.path().join("two.txt"), "two").expect("write two");
        let filesystem =
            crate::platform::fs::capability::FilesystemCapability::new(temp.path().to_path_buf())
                .expect("filesystem capability");

        filesystem
            .bind_tool_target("survivor", "call-survivor", "read", "one.txt")
            .await
            .expect("bind survivor");
        assert_eq!(filesystem.bound_len(), 1);
        #[cfg(target_os = "linux")]
        let survivor_fd_count = workspace_fd_count(temp.path());

        for (index, terminal) in [
            TerminalExit::Committed,
            TerminalExit::Aborted,
            TerminalExit::Failed,
        ]
        .into_iter()
        .enumerate()
        {
            let operation_id = format!("terminal-{index}");
            for (call_id, path) in [("call-one", "one.txt"), ("call-two", "two.txt")] {
                filesystem
                    .bind_tool_target(&operation_id, call_id, "read", path)
                    .await
                    .expect("bind terminal operation target");
            }
            assert_eq!(filesystem.bound_len(), 3);

            let permit = OperationPermit::unguarded(execution(&operation_id, filesystem.clone()));
            let _ = leave_operation(permit, terminal);
            assert_eq!(filesystem.bound_len(), 1);
            #[cfg(target_os = "linux")]
            assert_eq!(workspace_fd_count(temp.path()), survivor_fd_count);

            for (call_id, path) in [("call-one", "one.txt"), ("call-two", "two.txt")] {
                let error = filesystem
                    .take_bound_tool_target(&operation_id, call_id, "read", path)
                    .expect_err("terminal permit drop must discard every operation binding");
                assert!(error.to_string().contains("no authorization-bound target"));
            }
        }

        filesystem
            .take_bound_tool_target("survivor", "call-survivor", "read", "one.txt")
            .expect("another operation's binding must survive exact cleanup");
        assert_eq!(filesystem.bound_len(), 0);
    }

    #[cfg(target_os = "linux")]
    fn workspace_fd_count(workspace: &std::path::Path) -> usize {
        std::fs::read_dir("/proc/self/fd")
            .expect("read process fd table")
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .filter(|target| target.starts_with(workspace))
            .count()
    }
}
