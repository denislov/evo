use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::capability::CapabilityGeneration;
use super::snapshot::SnapshotCoordinator;
use crate::runtime::facade::CodingSessionError;
use crate::runtime::operation::OperationClass;

pub(crate) const DEFAULT_RUNTIME_ROOT_LIMIT: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationKind {
    Prompt,
    Compact,
    DelegationConfirmation,
    BranchSummary,
    AgentInvocation,
    AgentTeam,
    Export,
    ForkSession,
    SwitchActiveLeaf,
    SetSessionTreeLabel,
    SetSessionName,
    SetDefaultAgentProfile,
    SelfHealingEdit,
}

impl OperationKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Compact => "compact",
            Self::DelegationConfirmation => "delegation_confirmation",
            Self::BranchSummary => "branch_summary",
            Self::AgentInvocation => "agent_invocation",
            Self::AgentTeam => "agent_team",
            Self::Export => "export",
            Self::ForkSession => "fork_session",
            Self::SwitchActiveLeaf => "switch_active_leaf",
            Self::SetSessionTreeLabel => "set_session_tree_label",
            Self::SetSessionName => "set_session_name",
            Self::SetDefaultAgentProfile => "set_default_agent_profile",
            Self::SelfHealingEdit => "self_healing_edit",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        Some(match value {
            "prompt" => Self::Prompt,
            "compact" => Self::Compact,
            "delegation_confirmation" => Self::DelegationConfirmation,
            "branch_summary" => Self::BranchSummary,
            "agent_invocation" => Self::AgentInvocation,
            "agent_team" => Self::AgentTeam,
            "export" => Self::Export,
            "fork_session" => Self::ForkSession,
            "switch_active_leaf" => Self::SwitchActiveLeaf,
            "set_session_tree_label" => Self::SetSessionTreeLabel,
            "set_session_name" => Self::SetSessionName,
            "set_default_agent_profile" => Self::SetDefaultAgentProfile,
            "self_healing_edit" => Self::SelfHealingEdit,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PromptControlCommand {
    Abort {
        reason: String,
    },
    Steer {
        text: String,
    },
    SteerContent {
        content: Vec<ai::api::conversation::ContentBlock>,
    },
    FollowUp {
        text: String,
    },
    FollowUpContent {
        content: Vec<ai::api::conversation::ContentBlock>,
    },
}

pub(crate) type PromptControlReceiver = mpsc::Receiver<PromptControlCommand>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PromptControlGeneration(pub(crate) u64);

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
        content: Vec<ai::api::conversation::ContentBlock>,
    ) -> Result<(), CodingSessionError> {
        self.send(PromptControlCommand::SteerContent { content })
    }

    pub(crate) fn follow_up(&self, text: impl Into<String>) -> Result<(), CodingSessionError> {
        self.send(PromptControlCommand::FollowUp { text: text.into() })
    }

    pub(crate) fn follow_up_content(
        &self,
        content: Vec<ai::api::conversation::ContentBlock>,
    ) -> Result<(), CodingSessionError> {
        self.send(PromptControlCommand::FollowUpContent { content })
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
        let Ok(mut shared) = self.shared.lock() else {
            return;
        };
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
        let mut shared = self
            .shared
            .lock()
            .expect("prompt control state lock poisoned");
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

    fn current(&self) -> Option<PromptControlRegistration> {
        self.shared
            .lock()
            .expect("prompt control state lock poisoned")
            .active
            .as_ref()
            .map(|active| PromptControlRegistration {
                generation: active.generation,
                handle: active.handle.clone(),
            })
    }

    fn take_receiver(&self) -> Option<PromptControlReceiver> {
        self.shared
            .lock()
            .expect("prompt control state lock poisoned")
            .active
            .as_mut()
            .and_then(|active| active.receiver.take())
    }

    fn clear(&self) {
        self.shared
            .lock()
            .expect("prompt control state lock poisoned")
            .active = None;
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
        let shared = self.shared.lock().expect("operation state lock poisoned");
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
        let mut shared = self.shared.lock().expect("operation state lock poisoned");
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

    fn cancel_open_operations(&self) -> Vec<String> {
        let mut cancelled = Vec::new();
        for root in self.root_identities() {
            if root.cancellation_open && !root.cancellation.is_cancelled() {
                root.cancellation.cancel();
                cancelled.push(root.operation_id.clone());
            }
        }
        for child in &self.children {
            if child.cancellation_open && !child.cancellation.is_cancelled() {
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
        CodingSessionError::UnsupportedCapability {
            capability: match self {
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
            },
        }
    }
}

impl OperationState {
    pub(crate) fn with_snapshot_coordinator(
        snapshot_coordinator: Arc<SnapshotCoordinator>,
    ) -> Self {
        Self {
            shared: Arc::new(Mutex::new(OperationStateInner {
                session_write: None,
                non_session_roots: Vec::new(),
                runtime_write: None,
                children: Vec::new(),
                non_session_root_limit: DEFAULT_RUNTIME_ROOT_LIMIT,
                next_generation: 1,
            })),
            snapshot_coordinator,
        }
    }

    pub(crate) fn activity(&self) -> OperationActivity {
        self.shared
            .lock()
            .expect("operation state lock poisoned")
            .activity()
    }

    fn ensure_active_target(
        &self,
        expected_kind: OperationKind,
        expected_operation_id: &str,
    ) -> Result<(), OperationIdentityRejection> {
        let shared = self.shared.lock().expect("operation state lock poisoned");
        let Some(active) = shared.root_identities().find(|active| {
            !active.owner_released
                && active.kind == expected_kind
                && active.operation_id == expected_operation_id
        }) else {
            if let Some(active) = shared
                .root_identities()
                .find(|active| !active.owner_released && active.kind == expected_kind)
            {
                return Err(OperationIdentityRejection::TargetMismatch {
                    kind: expected_kind,
                    expected_operation_id: expected_operation_id.to_owned(),
                    active_operation_id: active.operation_id.clone(),
                });
            }
            if let Some(active) = shared
                .root_identities()
                .find(|active| !active.owner_released)
            {
                return Err(OperationIdentityRejection::KindMismatch {
                    expected_kind,
                    active_kind: active.kind,
                    expected_operation_id: expected_operation_id.to_owned(),
                });
            }
            return Err(OperationIdentityRejection::NoActiveOperation {
                expected_kind,
                expected_operation_id: expected_operation_id.to_owned(),
            });
        };
        debug_assert_eq!(active.kind, expected_kind);
        debug_assert_eq!(active.operation_id, expected_operation_id);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn ensure_session_write_idle(&self) -> Result<(), CodingSessionError> {
        if let Some(active) = self.activity().session_write_blocker() {
            return Err(CodingSessionError::Busy {
                operation: active.as_str().into(),
            });
        }

        Ok(())
    }

    pub(crate) fn begin_root_with_capability_generation(
        &self,
        class: OperationClass,
        kind: OperationKind,
        operation_id: String,
        capability_generation: CapabilityGeneration,
    ) -> Result<OperationGuard, CodingSessionError> {
        self.begin_root_inner(class, kind, operation_id, Some(capability_generation))
    }

    fn begin_root_inner(
        &self,
        class: OperationClass,
        kind: OperationKind,
        operation_id: String,
        capability_generation: Option<CapabilityGeneration>,
    ) -> Result<OperationGuard, CodingSessionError> {
        let mut shared = self.shared.lock().expect("operation state lock poisoned");
        if capability_generation.is_some_and(|generation| {
            generation < self.snapshot_coordinator.current_capability_generation()
        }) {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "operation {operation_id} was admitted with a stale capability generation"
                ),
            });
        }
        if let Some(active_kind) = shared.operation_kind_for_id(&operation_id) {
            return Err(CodingSessionError::Busy {
                operation: active_kind.as_str().into(),
            });
        }
        let activity = shared.activity();
        let blocker = match class {
            OperationClass::SessionWriteRoot => activity.session_write_blocker(),
            OperationClass::NonSessionRoot => activity.non_session_root_blocker(),
            OperationClass::RuntimeWrite => activity.runtime_write_blocker(),
            OperationClass::Query | OperationClass::ReadOnly | OperationClass::Child => {
                return Err(CodingSessionError::UnsupportedCapability {
                    capability: format!("{class:?} does not occupy a root operation slot"),
                });
            }
        };
        if let Some(active) = blocker {
            return Err(CodingSessionError::Busy {
                operation: active.as_str().into(),
            });
        }
        let previous_primary = activity.primary();
        let generation = shared.next_generation;
        shared.next_generation = shared.next_generation.saturating_add(1);
        let cancellation = CancellationToken::new();
        let identity = ActiveOperationIdentity {
            kind,
            operation_id: operation_id.clone(),
            generation,
            capability_generation,
            cancellation: cancellation.clone(),
            cancellation_open: true,
            owner_released: false,
        };
        match class {
            OperationClass::SessionWriteRoot => shared.session_write = Some(identity),
            OperationClass::NonSessionRoot => shared.non_session_roots.push(identity),
            OperationClass::RuntimeWrite => shared.runtime_write = Some(identity),
            OperationClass::Query | OperationClass::ReadOnly | OperationClass::Child => {
                unreachable!("root class validated above")
            }
        }
        let current_primary = shared.activity().primary();
        drop(shared);
        if previous_primary != current_primary {
            self.snapshot_coordinator
                .set_active_operation(current_primary);
        }
        if let Some(capability_generation) = capability_generation {
            self.snapshot_coordinator.register_operation_event_context(
                operation_id.clone(),
                kind,
                capability_generation,
                None,
                operation_id.clone(),
            );
        }
        Ok(OperationGuard {
            shared: Arc::clone(&self.shared),
            snapshot_coordinator: Arc::clone(&self.snapshot_coordinator),
            class,
            kind,
            operation_id,
            generation,
            cancellation: Some(cancellation),
        })
    }

    #[cfg(test)]
    pub(crate) fn begin_child(
        &self,
        kind: OperationKind,
        operation_id: String,
        parent_operation_id: String,
    ) -> Result<ChildOperationGuard, CodingSessionError> {
        self.begin_child_inner(kind, operation_id, parent_operation_id, None)
    }

    pub(crate) fn begin_child_with_capability_generation(
        &self,
        kind: OperationKind,
        operation_id: String,
        parent_operation_id: String,
        capability_generation: CapabilityGeneration,
    ) -> Result<ChildOperationGuard, CodingSessionError> {
        self.begin_child_inner(
            kind,
            operation_id,
            parent_operation_id,
            Some(capability_generation),
        )
    }

    fn begin_child_inner(
        &self,
        kind: OperationKind,
        operation_id: String,
        parent_operation_id: String,
        capability_generation: Option<CapabilityGeneration>,
    ) -> Result<ChildOperationGuard, CodingSessionError> {
        let mut shared = self.shared.lock().expect("operation state lock poisoned");
        if capability_generation.is_some_and(|generation| {
            generation < self.snapshot_coordinator.current_capability_generation()
        }) {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "child operation {operation_id} was admitted with a stale capability generation"
                ),
            });
        }
        if let Some(active_kind) = shared.operation_kind_for_id(&operation_id) {
            return Err(CodingSessionError::Busy {
                operation: active_kind.as_str().into(),
            });
        }
        if !shared.parent_is_active(&parent_operation_id) {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "child operation {operation_id} requires active parent {parent_operation_id}"
                ),
            });
        }
        let root_operation_id = shared
            .root_operation_id_for(&parent_operation_id)
            .expect("active parent must resolve to a root operation");
        let generation = shared.next_generation;
        shared.next_generation = shared.next_generation.saturating_add(1);
        let cancellation = CancellationToken::new();
        shared.children.push(ActiveChildOperation {
            kind,
            operation_id: operation_id.clone(),
            parent_operation_id: parent_operation_id.clone(),
            generation,
            capability_generation,
            cancellation: cancellation.clone(),
            cancellation_open: true,
            owner_released: false,
        });
        if let Some(capability_generation) = capability_generation {
            self.snapshot_coordinator.register_operation_event_context(
                operation_id.clone(),
                kind,
                capability_generation,
                Some(parent_operation_id.clone()),
                root_operation_id.clone(),
            );
        }
        Ok(ChildOperationGuard {
            shared: Arc::clone(&self.shared),
            snapshot_coordinator: Arc::clone(&self.snapshot_coordinator),
            kind,
            operation_id,
            parent_operation_id,
            root_operation_id,
            generation,
            cancellation,
        })
    }

    pub(crate) fn cancel_capability_generations_before(
        &self,
        generation: CapabilityGeneration,
    ) -> Vec<String> {
        self.shared
            .lock()
            .expect("operation state lock poisoned")
            .cancel_capability_generations_before(generation)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OperationControl {
    state: OperationState,
    prompt_control: PromptControlState,
}

impl OperationControl {
    pub(crate) fn with_snapshot_coordinator(
        snapshot_coordinator: Arc<SnapshotCoordinator>,
    ) -> Self {
        Self {
            state: OperationState::with_snapshot_coordinator(snapshot_coordinator),
            prompt_control: PromptControlState::new(),
        }
    }

    pub(crate) fn activity(&self) -> OperationActivity {
        self.state.activity()
    }

    pub(crate) fn begin_root_with_capability_generation(
        &self,
        class: OperationClass,
        kind: OperationKind,
        operation_id: String,
        capability_generation: CapabilityGeneration,
    ) -> Result<OperationGuard, CodingSessionError> {
        self.state.begin_root_with_capability_generation(
            class,
            kind,
            operation_id,
            capability_generation,
        )
    }

    pub(crate) fn begin_child_with_capability_generation(
        &self,
        kind: OperationKind,
        operation_id: String,
        parent_operation_id: String,
        capability_generation: CapabilityGeneration,
    ) -> Result<ChildOperationGuard, CodingSessionError> {
        self.state.begin_child_with_capability_generation(
            kind,
            operation_id,
            parent_operation_id,
            capability_generation,
        )
    }

    pub(crate) fn cancel_capability_generations_before(
        &self,
        generation: CapabilityGeneration,
    ) -> Vec<String> {
        self.state.cancel_capability_generations_before(generation)
    }

    pub(crate) fn cancel_open_operations_for_shutdown(&self) -> Vec<String> {
        self.state
            .shared
            .lock()
            .expect("operation state lock poisoned")
            .cancel_open_operations()
    }

    pub(crate) fn current_prompt_control_registration(&self) -> Option<PromptControlRegistration> {
        self.prompt_control.current()
    }

    pub(crate) fn prompt_control_registration_for(
        &mut self,
        operation_id: &str,
    ) -> Result<PromptControlRegistration, CodingSessionError> {
        self.state
            .ensure_active_target(OperationKind::Prompt, operation_id)
            .map_err(OperationIdentityRejection::into_error)?;
        match self.prompt_control.current() {
            Some(registration) => Ok(registration),
            None => self.prompt_control.create(),
        }
    }

    pub(crate) fn prompt_control_cleanup(&self) -> PromptControlCleanup {
        self.prompt_control.cleanup()
    }

    pub(crate) fn take_prompt_control_receiver(&mut self) -> Option<PromptControlReceiver> {
        self.prompt_control.take_receiver()
    }

    pub(crate) fn clear_prompt_control_receiver(&mut self) {
        self.prompt_control.clear();
    }
}

#[derive(Debug)]
#[must_use = "dropping OperationGuard clears the active operation"]
pub(crate) struct OperationGuard {
    shared: Arc<Mutex<OperationStateInner>>,
    snapshot_coordinator: Arc<SnapshotCoordinator>,
    class: OperationClass,
    kind: OperationKind,
    operation_id: String,
    generation: u64,
    cancellation: Option<CancellationToken>,
}

impl OperationGuard {
    pub(crate) fn cancellation_token(&self) -> Option<CancellationToken> {
        self.cancellation.clone()
    }

    pub(crate) fn cancellation_handle(&self) -> OperationCancellationHandle {
        OperationCancellationHandle {
            shared: Arc::clone(&self.shared),
            operation_id: self.operation_id.clone(),
        }
    }

    pub(crate) fn bind_capability_generation(&mut self, generation: CapabilityGeneration) {
        let mut shared = self.shared.lock().expect("operation state lock poisoned");
        let matches = |active: &ActiveOperationIdentity| {
            active.kind == self.kind
                && active.operation_id == self.operation_id
                && active.generation == self.generation
        };
        let active = match self.class {
            OperationClass::SessionWriteRoot => shared
                .session_write
                .as_mut()
                .filter(|active| matches(active)),
            OperationClass::NonSessionRoot => shared
                .non_session_roots
                .iter_mut()
                .find(|active| matches(active)),
            OperationClass::RuntimeWrite => shared
                .runtime_write
                .as_mut()
                .filter(|active| matches(active)),
            OperationClass::Query | OperationClass::ReadOnly | OperationClass::Child => None,
        };
        active
            .expect("operation guard must retain its active identity")
            .capability_generation = Some(generation);
        drop(shared);
        self.snapshot_coordinator.register_operation_event_context(
            self.operation_id.clone(),
            self.kind,
            generation,
            None,
            self.operation_id.clone(),
        );
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        let Ok(mut shared) = self.shared.lock() else {
            return;
        };
        let previous_primary = shared.activity().primary();
        let matches = |active: &ActiveOperationIdentity| {
            active.kind == self.kind
                && active.operation_id == self.operation_id
                && active.generation == self.generation
        };
        let released = match self.class {
            OperationClass::SessionWriteRoot => {
                if let Some(active) = shared
                    .session_write
                    .as_mut()
                    .filter(|active| matches(active))
                {
                    active.owner_released = true;
                    true
                } else {
                    false
                }
            }
            OperationClass::NonSessionRoot => {
                if let Some(active) = shared
                    .non_session_roots
                    .iter_mut()
                    .find(|active| matches(active))
                {
                    active.owner_released = true;
                    true
                } else {
                    false
                }
            }
            OperationClass::RuntimeWrite => {
                if let Some(active) = shared
                    .runtime_write
                    .as_mut()
                    .filter(|active| matches(active))
                {
                    active.owner_released = true;
                    true
                } else {
                    false
                }
            }
            OperationClass::Query | OperationClass::ReadOnly | OperationClass::Child => false,
        };
        if released {
            shared.cancel_descendants(&self.operation_id);
            let removed = shared.remove_released_roots_without_descendants();
            let current_primary = shared.activity().primary();
            drop(shared);
            for (operation_id, generation) in removed {
                self.snapshot_coordinator
                    .clear_operation_event_context_if(&operation_id, generation);
            }
            self.snapshot_coordinator
                .clear_operation_cancellation_if(&self.operation_id);
            if previous_primary != current_primary {
                self.snapshot_coordinator
                    .set_active_operation(current_primary);
            }
        } else {
            drop(shared);
        }
    }
}

#[derive(Debug)]
#[must_use = "dropping ChildOperationGuard releases the child operation"]
pub(crate) struct ChildOperationGuard {
    shared: Arc<Mutex<OperationStateInner>>,
    snapshot_coordinator: Arc<SnapshotCoordinator>,
    kind: OperationKind,
    operation_id: String,
    parent_operation_id: String,
    root_operation_id: String,
    generation: u64,
    cancellation: CancellationToken,
}

impl ChildOperationGuard {
    pub(crate) fn parent_operation_id(&self) -> &str {
        &self.parent_operation_id
    }

    pub(crate) fn root_operation_id(&self) -> &str {
        &self.root_operation_id
    }

    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub(crate) fn cancellation_handle(&self) -> OperationCancellationHandle {
        OperationCancellationHandle {
            shared: Arc::clone(&self.shared),
            operation_id: self.operation_id.clone(),
        }
    }

    pub(crate) fn bind_capability_generation(&mut self, generation: CapabilityGeneration) {
        let mut shared = self.shared.lock().expect("operation state lock poisoned");
        shared
            .children
            .iter_mut()
            .find(|active| {
                active.kind == self.kind
                    && active.operation_id == self.operation_id
                    && active.generation == self.generation
            })
            .expect("child guard must retain its active identity")
            .capability_generation = Some(generation);
        drop(shared);
        self.snapshot_coordinator.register_operation_event_context(
            self.operation_id.clone(),
            self.kind,
            generation,
            Some(self.parent_operation_id.clone()),
            self.root_operation_id.clone(),
        );
    }
}

impl Drop for ChildOperationGuard {
    fn drop(&mut self) {
        let Ok(mut shared) = self.shared.lock() else {
            return;
        };
        let previous_primary = shared.activity().primary();
        let matches = |active: &ActiveChildOperation| {
            active.kind == self.kind
                && active.operation_id == self.operation_id
                && active.generation == self.generation
        };
        let Some(child) = shared.children.iter_mut().find(|child| matches(child)) else {
            return;
        };
        child.owner_released = true;
        shared.cancel_descendants(&self.operation_id);
        let mut removed = shared.remove_released_children_without_descendants();
        removed.extend(shared.remove_released_roots_without_descendants());
        let current_primary = shared.activity().primary();
        drop(shared);
        for (operation_id, generation) in removed {
            self.snapshot_coordinator
                .clear_operation_event_context_if(&operation_id, generation);
        }
        if previous_primary != current_primary {
            self.snapshot_coordinator
                .set_active_operation(current_primary);
        }
    }
}
