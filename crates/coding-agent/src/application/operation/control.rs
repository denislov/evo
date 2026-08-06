use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::application::snapshot::SnapshotCoordinator;
use crate::kernel::capability::CapabilityGeneration;
pub(crate) use crate::kernel::control::{
    PromptControlCommand, PromptControlGeneration, PromptControlReceiver,
};
use crate::kernel::error::CodingSessionError;
use crate::kernel::operation::OperationClass;
pub(crate) use crate::kernel::operation::OperationKind;
use crate::mutex::MutexExt;

pub(crate) const DEFAULT_RUNTIME_ROOT_LIMIT: usize = 4;

#[derive(Debug, Clone)]
pub(crate) struct PromptControlHandle {
    sender: mpsc::Sender<PromptControlCommand>,
}

impl PromptControlHandle {
    pub(crate) fn abort(&self, reason: impl Into<String>) -> Result<(), CodingSessionError> {
        self.send(PromptControlCommand::Abort {
            reason: reason.into(),
        })
    }

    pub(crate) fn steer(&self, text: impl Into<String>) -> Result<(), CodingSessionError> {
        self.send(PromptControlCommand::Steer { text: text.into() })
    }

    pub(crate) fn steer_content(
        &self,
        content: Vec<ai_protocol::api::conversation::ContentBlock>,
    ) -> Result<(), CodingSessionError> {
        self.send(PromptControlCommand::SteerContent { content })
    }

    pub(crate) fn follow_up(&self, text: impl Into<String>) -> Result<(), CodingSessionError> {
        self.send(PromptControlCommand::FollowUp { text: text.into() })
    }

    pub(crate) fn follow_up_content(
        &self,
        content: Vec<ai_protocol::api::conversation::ContentBlock>,
    ) -> Result<(), CodingSessionError> {
        self.send(PromptControlCommand::FollowUpContent { content })
    }

    pub(crate) fn interject(&self, text: impl Into<String>) -> Result<(), CodingSessionError> {
        self.send(PromptControlCommand::Interject { text: text.into() })
    }

    pub(crate) fn interject_content(
        &self,
        content: Vec<ai_protocol::api::conversation::ContentBlock>,
    ) -> Result<(), CodingSessionError> {
        self.send(PromptControlCommand::InterjectContent { content })
    }

    fn send(&self, command: PromptControlCommand) -> Result<(), CodingSessionError> {
        self.sender.try_send(command).map_err(|error| match error {
            mpsc::error::TrySendError::Closed(_) => CodingSessionError::Session {
                message: "prompt control receiver is closed".into(),
            },
            mpsc::error::TrySendError::Full(_) => CodingSessionError::Busy {
                operation: "prompt_control_queue".into(),
            },
        })
    }
}

pub(crate) fn prompt_control_channel() -> (PromptControlHandle, PromptControlReceiver) {
    let (sender, receiver) = mpsc::channel(64);
    (PromptControlHandle { sender }, receiver)
}

#[derive(Debug, Clone)]
pub(crate) struct PromptControlRegistration {
    pub(crate) generation: PromptControlGeneration,
    pub(crate) handle: PromptControlHandle,
}

#[derive(Debug)]
struct PromptControlOwnership {
    generation: PromptControlGeneration,
    handle: PromptControlHandle,
    receiver: Option<PromptControlReceiver>,
}

#[derive(Debug)]
struct PromptControlStateInner {
    next_generation: u64,
    active: Option<PromptControlOwnership>,
}

#[derive(Debug, Clone)]
pub(crate) struct PromptControlCleanup {
    shared: Arc<Mutex<PromptControlStateInner>>,
}

impl PromptControlCleanup {
    pub(crate) fn clear_if_generation(&self, generation: PromptControlGeneration) {
        // Called by PromptControlCleanupGuard::drop.
        let mut shared = self.shared.lock_or_recover("prompt control state");
        if shared
            .active
            .as_ref()
            .is_some_and(|active| active.generation == generation)
        {
            shared.active = None;
        }
    }
}

#[derive(Debug, Clone)]
struct PromptControlState {
    shared: Arc<Mutex<PromptControlStateInner>>,
}

impl PromptControlState {
    fn new() -> Self {
        Self {
            shared: Arc::new(Mutex::new(PromptControlStateInner {
                next_generation: 1,
                active: None,
            })),
        }
    }

    fn create(&self) -> Result<PromptControlRegistration, CodingSessionError> {
        let mut shared = self.shared.lock_resource("prompt control state")?;
        if shared
            .active
            .as_ref()
            .is_some_and(|active| active.receiver.is_some())
        {
            return Err(CodingSessionError::Busy {
                operation: "prompt_control".into(),
            });
        }
        let generation = PromptControlGeneration(shared.next_generation);
        shared.next_generation = shared.next_generation.saturating_add(1);
        let (handle, receiver) = prompt_control_channel();
        shared.active = Some(PromptControlOwnership {
            generation,
            handle: handle.clone(),
            receiver: Some(receiver),
        });
        Ok(PromptControlRegistration { generation, handle })
    }

    fn current(&self) -> Result<Option<PromptControlRegistration>, CodingSessionError> {
        Ok(self
            .shared
            .lock_resource("prompt control state")?
            .active
            .as_ref()
            .map(|active| PromptControlRegistration {
                generation: active.generation,
                handle: active.handle.clone(),
            }))
    }

    fn take_receiver(&self) -> Result<Option<PromptControlReceiver>, CodingSessionError> {
        Ok(self
            .shared
            .lock_resource("prompt control state")?
            .active
            .as_mut()
            .and_then(|active| active.receiver.take()))
    }

    fn clear(&self) -> Result<(), CodingSessionError> {
        self.shared.lock_resource("prompt control state")?.active = None;
        Ok(())
    }

    fn cleanup(&self) -> PromptControlCleanup {
        PromptControlCleanup {
            shared: Arc::clone(&self.shared),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OperationState {
    shared: Arc<Mutex<OperationStateInner>>,
    snapshot_coordinator: Arc<SnapshotCoordinator>,
}

#[derive(Debug, Clone)]
pub(crate) struct OperationCancellationHandle {
    shared: Arc<Mutex<OperationStateInner>>,
    operation_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationCancellationOutcome {
    Requested { kind: OperationKind },
    AlreadyRequested { kind: OperationKind },
}

impl OperationCancellationHandle {
    pub(crate) fn request(
        &self,
    ) -> Result<OperationCancellationOutcome, OperationIdentityRejection> {
        let shared = self.shared.lock_resource("operation state")?;
        let root = shared
            .root_identities()
            .find(|active| active.operation_id == self.operation_id);
        let child = shared
            .children
            .iter()
            .find(|active| active.operation_id == self.operation_id);
        let (kind, cancellation, cancellation_open, owner_released) = match (root, child) {
            (Some(active), _) => (
                active.kind,
                active.cancellation.clone(),
                active.cancellation_open,
                active.owner_released,
            ),
            (None, Some(active)) => (
                active.kind,
                active.cancellation.clone(),
                active.cancellation_open,
                active.owner_released,
            ),
            (None, None) => {
                return Err(OperationIdentityRejection::NoActiveOperation {
                    expected_kind: OperationKind::Prompt,
                    expected_operation_id: self.operation_id.clone(),
                });
            }
        };
        if owner_released {
            return Err(OperationIdentityRejection::NoActiveOperation {
                expected_kind: kind,
                expected_operation_id: self.operation_id.clone(),
            });
        }
        if !cancellation_open {
            return Err(OperationIdentityRejection::CancellationClosed {
                kind,
                operation_id: self.operation_id.clone(),
            });
        }
        if cancellation.is_cancelled() {
            return Ok(OperationCancellationOutcome::AlreadyRequested { kind });
        }
        cancellation.cancel();
        shared.cancel_descendants(&self.operation_id);
        Ok(OperationCancellationOutcome::Requested { kind })
    }

    pub(crate) fn close(&self) -> Result<(), CodingSessionError> {
        let mut shared = self.shared.lock_resource("operation state")?;
        if let Some(active) = shared
            .session_write
            .as_mut()
            .filter(|active| active.operation_id == self.operation_id && !active.owner_released)
        {
            if active.cancellation.is_cancelled() {
                return Err(CodingSessionError::Cancelled);
            }
            active.cancellation_open = false;
            return Ok(());
        }
        if let Some(active) = shared
            .non_session_roots
            .iter_mut()
            .find(|active| active.operation_id == self.operation_id && !active.owner_released)
        {
            if active.cancellation.is_cancelled() {
                return Err(CodingSessionError::Cancelled);
            }
            active.cancellation_open = false;
            return Ok(());
        }
        if let Some(active) = shared
            .runtime_write
            .as_mut()
            .filter(|active| active.operation_id == self.operation_id && !active.owner_released)
        {
            if active.cancellation.is_cancelled() {
                return Err(CodingSessionError::Cancelled);
            }
            active.cancellation_open = false;
            return Ok(());
        }
        if let Some(active) = shared
            .children
            .iter_mut()
            .find(|active| active.operation_id == self.operation_id && !active.owner_released)
        {
            if active.cancellation.is_cancelled() {
                return Err(CodingSessionError::Cancelled);
            }
            active.cancellation_open = false;
            return Ok(());
        }
        Err(CodingSessionError::UnsupportedCapability {
            capability: format!("operation {} is not running", self.operation_id),
        })
    }

    /// Reopens interactive cancellation after a mutation's atomic section.
    /// A shutdown requested while the section was closed stays latched in the
    /// token and is surfaced immediately as `Cancelled`.
    pub(crate) fn reopen(&self) -> Result<(), CodingSessionError> {
        let mut shared = self.shared.lock_resource("operation state")?;
        if let Some(active) = shared
            .session_write
            .as_mut()
            .filter(|active| active.operation_id == self.operation_id && !active.owner_released)
        {
            active.cancellation_open = true;
            return cancellation_reopened(active.cancellation.is_cancelled());
        }
        if let Some(active) = shared
            .non_session_roots
            .iter_mut()
            .find(|active| active.operation_id == self.operation_id && !active.owner_released)
        {
            active.cancellation_open = true;
            return cancellation_reopened(active.cancellation.is_cancelled());
        }
        if let Some(active) = shared
            .runtime_write
            .as_mut()
            .filter(|active| active.operation_id == self.operation_id && !active.owner_released)
        {
            active.cancellation_open = true;
            return cancellation_reopened(active.cancellation.is_cancelled());
        }
        if let Some(active) = shared
            .children
            .iter_mut()
            .find(|active| active.operation_id == self.operation_id && !active.owner_released)
        {
            active.cancellation_open = true;
            return cancellation_reopened(active.cancellation.is_cancelled());
        }
        Err(CodingSessionError::UnsupportedCapability {
            capability: format!("operation {} is not running", self.operation_id),
        })
    }
}

fn cancellation_reopened(cancelled: bool) -> Result<(), CodingSessionError> {
    if cancelled {
        Err(CodingSessionError::Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Debug)]
struct OperationStateInner {
    session_write: Option<ActiveOperationIdentity>,
    non_session_roots: Vec<ActiveOperationIdentity>,
    runtime_write: Option<ActiveOperationIdentity>,
    children: Vec<ActiveChildOperation>,
    non_session_root_limit: usize,
    next_generation: u64,
}

#[derive(Debug, Clone)]
struct ActiveOperationIdentity {
    kind: OperationKind,
    operation_id: String,
    generation: u64,
    capability_generation: Option<CapabilityGeneration>,
    cancellation: CancellationToken,
    cancellation_open: bool,
    owner_released: bool,
}

#[derive(Debug, Clone)]
struct ActiveChildOperation {
    kind: OperationKind,
    operation_id: String,
    parent_operation_id: String,
    generation: u64,
    capability_generation: Option<CapabilityGeneration>,
    cancellation: CancellationToken,
    cancellation_open: bool,
    owner_released: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationActivity {
    session_write: Option<OperationKind>,
    non_session_roots: Vec<OperationKind>,
    runtime_write: Option<OperationKind>,
    non_session_root_limit: usize,
}

impl OperationActivity {
    pub(crate) fn from_session_write(session_write: Option<OperationKind>) -> Self {
        Self {
            session_write,
            non_session_roots: Vec::new(),
            runtime_write: None,
            non_session_root_limit: DEFAULT_RUNTIME_ROOT_LIMIT,
        }
    }

    pub(crate) fn primary(&self) -> Option<OperationKind> {
        self.runtime_write
            .or(self.session_write)
            .or_else(|| self.non_session_roots.first().copied())
    }

    pub(crate) fn session_write(&self) -> Option<OperationKind> {
        self.session_write
    }

    pub(crate) fn session_write_blocker(&self) -> Option<OperationKind> {
        self.runtime_write.or(self.session_write)
    }

    pub(crate) fn non_session_root_blocker(&self) -> Option<OperationKind> {
        self.runtime_write.or_else(|| {
            (self.non_session_roots.len() >= self.non_session_root_limit)
                .then(|| self.non_session_roots[0])
        })
    }

    pub(crate) fn runtime_write_blocker(&self) -> Option<OperationKind> {
        self.primary()
    }
}

impl OperationStateInner {
    fn activity(&self) -> OperationActivity {
        OperationActivity {
            session_write: self.session_write.as_ref().map(|active| active.kind),
            non_session_roots: self
                .non_session_roots
                .iter()
                .map(|active| active.kind)
                .collect(),
            runtime_write: self.runtime_write.as_ref().map(|active| active.kind),
            non_session_root_limit: self.non_session_root_limit,
        }
    }

    fn root_identities(&self) -> impl Iterator<Item = &ActiveOperationIdentity> {
        self.session_write
            .iter()
            .chain(self.non_session_roots.iter())
            .chain(self.runtime_write.iter())
    }

    fn operation_kind_for_id(&self, operation_id: &str) -> Option<OperationKind> {
        self.root_identities()
            .find(|active| active.operation_id == operation_id)
            .map(|active| active.kind)
            .or_else(|| {
                self.children
                    .iter()
                    .find(|active| active.operation_id == operation_id)
                    .map(|active| active.kind)
            })
    }

    fn parent_is_active(&self, operation_id: &str) -> bool {
        self.root_identities()
            .any(|active| active.operation_id == operation_id && !active.owner_released)
            || self.children.iter().any(|active| {
                active.operation_id == operation_id
                    && !active.owner_released
                    && !active.cancellation.is_cancelled()
            })
    }

    fn root_operation_id_for(&self, operation_id: &str) -> Option<String> {
        if self
            .root_identities()
            .any(|active| active.operation_id == operation_id)
        {
            return Some(operation_id.to_owned());
        }
        let mut current = self
            .children
            .iter()
            .find(|child| child.operation_id == operation_id)?;
        for _ in 0..=self.children.len() {
            if self
                .root_identities()
                .any(|root| root.operation_id == current.parent_operation_id)
            {
                return Some(current.parent_operation_id.clone());
            }
            current = self
                .children
                .iter()
                .find(|child| child.operation_id == current.parent_operation_id)?;
        }
        None
    }

    fn child_descends_from(&self, child: &ActiveChildOperation, ancestor_id: &str) -> bool {
        let mut parent_id = child.parent_operation_id.as_str();
        for _ in 0..=self.children.len() {
            if parent_id == ancestor_id {
                return true;
            }
            let Some(parent) = self
                .children
                .iter()
                .find(|candidate| candidate.operation_id == parent_id)
            else {
                return false;
            };
            parent_id = parent.parent_operation_id.as_str();
        }
        false
    }

    fn cancel_descendants(&self, operation_id: &str) {
        for child in &self.children {
            if self.child_descends_from(child, operation_id) {
                child.cancellation.cancel();
            }
        }
    }

    fn cancel_capability_generations_before(
        &self,
        generation: CapabilityGeneration,
    ) -> Vec<String> {
        let mut cancelled = Vec::new();
        for root in self.root_identities() {
            if root
                .capability_generation
                .is_some_and(|active| active < generation)
            {
                if !root.cancellation.is_cancelled() {
                    root.cancellation.cancel();
                }
                self.cancel_descendants(&root.operation_id);
                cancelled.push(root.operation_id.clone());
            }
        }
        for child in &self.children {
            if child
                .capability_generation
                .is_some_and(|active| active < generation)
            {
                if !child.cancellation.is_cancelled() {
                    child.cancellation.cancel();
                }
                cancelled.push(child.operation_id.clone());
            }
        }
        cancelled.sort();
        cancelled.dedup();
        cancelled
    }

    fn cancel_operations_for_shutdown(&self) -> Vec<String> {
        let mut cancelled = Vec::new();
        for root in self.root_identities() {
            // Shutdown is stronger than an interactive cancellation request:
            // it must be remembered even while a mutation temporarily closes
            // its cancellation window, otherwise the following check/process
            // can keep runtime drain blocked forever.
            if !root.cancellation.is_cancelled() {
                root.cancellation.cancel();
                cancelled.push(root.operation_id.clone());
            }
        }
        for child in &self.children {
            if !child.cancellation.is_cancelled() {
                child.cancellation.cancel();
                cancelled.push(child.operation_id.clone());
            }
        }
        cancelled.sort();
        cancelled.dedup();
        cancelled
    }

    fn has_descendants(&self, operation_id: &str) -> bool {
        self.children
            .iter()
            .any(|child| self.child_descends_from(child, operation_id))
    }

    fn remove_released_roots_without_descendants(&mut self) -> Vec<(String, CapabilityGeneration)> {
        let mut removed = Vec::new();
        let retained_by_children = self
            .root_identities()
            .filter(|root| self.has_descendants(&root.operation_id))
            .map(|root| root.operation_id.clone())
            .collect::<Vec<_>>();
        let retain = |root: &ActiveOperationIdentity| {
            !root.owner_released || retained_by_children.contains(&root.operation_id)
        };
        if self
            .session_write
            .as_ref()
            .is_some_and(|root| !retain(root))
        {
            let root = self.session_write.take().unwrap();
            if let Some(generation) = root.capability_generation {
                removed.push((root.operation_id, generation));
            }
        }
        let mut retained = Vec::with_capacity(self.non_session_roots.len());
        for root in self.non_session_roots.drain(..) {
            if retain(&root) {
                retained.push(root);
            } else if let Some(generation) = root.capability_generation {
                removed.push((root.operation_id, generation));
            }
        }
        self.non_session_roots = retained;
        if self
            .runtime_write
            .as_ref()
            .is_some_and(|root| !retain(root))
        {
            let root = self.runtime_write.take().unwrap();
            if let Some(generation) = root.capability_generation {
                removed.push((root.operation_id, generation));
            }
        }
        removed
    }

    fn remove_released_children_without_descendants(
        &mut self,
    ) -> Vec<(String, CapabilityGeneration)> {
        let mut removed = Vec::new();
        loop {
            let removable = self.children.iter().position(|child| {
                child.owner_released && !self.has_descendants(&child.operation_id)
            });
            let Some(index) = removable else {
                break;
            };
            let child = self.children.remove(index);
            if let Some(generation) = child.capability_generation {
                removed.push((child.operation_id, generation));
            }
        }
        removed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OperationIdentityRejection {
    Resource {
        message: String,
    },
    NoActiveOperation {
        expected_kind: OperationKind,
        expected_operation_id: String,
    },
    KindMismatch {
        expected_kind: OperationKind,
        active_kind: OperationKind,
        expected_operation_id: String,
    },
    TargetMismatch {
        kind: OperationKind,
        expected_operation_id: String,
        active_operation_id: String,
    },
    CancellationClosed {
        kind: OperationKind,
        operation_id: String,
    },
}

impl OperationIdentityRejection {
    fn into_error(self) -> CodingSessionError {
        match self {
            Self::Resource { message } => CodingSessionError::Resource { message },
            other => CodingSessionError::UnsupportedCapability {
                capability: match other {
                    Self::NoActiveOperation {
                        expected_kind,
                        expected_operation_id,
                    } => format!(
                        "{} control target {} is not running",
                        expected_kind.as_str(),
                        expected_operation_id
                    ),
                    Self::KindMismatch {
                        expected_kind,
                        active_kind,
                        expected_operation_id,
                    } => format!(
                        "{} control target {} does not match active {} operation",
                        expected_kind.as_str(),
                        expected_operation_id,
                        active_kind.as_str()
                    ),
                    Self::TargetMismatch {
                        kind,
                        expected_operation_id,
                        active_operation_id,
                    } => format!(
                        "{} control target {} does not match active operation {}",
                        kind.as_str(),
                        expected_operation_id,
                        active_operation_id
                    ),
                    Self::CancellationClosed { kind, operation_id } => format!(
                        "{} control target {} is no longer cancellable",
                        kind.as_str(),
                        operation_id
                    ),
                    Self::Resource { .. } => unreachable!("resource rejection handled above"),
                },
            },
        }
    }
}

impl From<CodingSessionError> for OperationIdentityRejection {
    fn from(error: CodingSessionError) -> Self {
        Self::Resource {
            message: error.to_string(),
        }
    }
}

mod state;

mod service;

pub(crate) use service::{ChildOperationGuard, OperationGuard};
pub(crate) use state::OperationControl;
