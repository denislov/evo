use std::collections::BTreeSet;

use crate::kernel::error::CodingSessionError;
use crate::kernel::ids::ProfileId;
use tool_contract::api::definition::ToolId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CapabilityGeneration(pub(crate) u64);

impl CapabilityGeneration {
    pub(crate) fn new(value: u64) -> Self {
        Self(value.max(1))
    }

    pub(crate) fn get(self) -> u64 {
        self.0
    }

    pub(crate) fn next(self) -> Result<Self, CodingSessionError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| CodingSessionError::UnsupportedCapability {
                capability: "capability generation is exhausted".into(),
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActorId {
    Client,
    ChildOperation(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelCapability {
    pub(crate) profile_id: Option<ProfileId>,
}

impl ModelCapability {
    pub(crate) fn require<'a>(
        value: Option<&'a ModelCapability>,
        runtime_profile_id: Option<&ProfileId>,
    ) -> Result<&'a ModelCapability, CodingSessionError> {
        let capability = value.ok_or_else(|| CodingSessionError::UnsupportedCapability {
            capability: "model capability is not granted".into(),
        })?;
        if capability.profile_id.as_ref() != runtime_profile_id {
            return Err(CodingSessionError::UnsupportedCapability {
                capability: format!(
                    "model capability profile mismatch: granted={}, runtime={}",
                    capability
                        .profile_id
                        .as_ref()
                        .map(ProfileId::as_str)
                        .unwrap_or("<none>"),
                    runtime_profile_id
                        .map(ProfileId::as_str)
                        .unwrap_or("<none>")
                ),
            });
        }
        Ok(capability)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ToolCapabilitySet {
    allow_all: bool,
    allowed: BTreeSet<ToolId>,
}

impl ToolCapabilitySet {
    pub(crate) fn from_ids(ids: impl IntoIterator<Item = ToolId>) -> Self {
        Self {
            allow_all: false,
            allowed: ids.into_iter().collect(),
        }
    }

    pub(crate) fn allows(&self, id: &ToolId) -> bool {
        self.allow_all || self.allowed.contains(id)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CommandCapabilitySet {
    allowed: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionReadCapability {
    pub(crate) persistent: bool,
}

impl SessionReadCapability {
    pub(crate) fn require(
        value: Option<&SessionReadCapability>,
    ) -> Result<&SessionReadCapability, CodingSessionError> {
        value.ok_or_else(|| CodingSessionError::UnsupportedCapability {
            capability: "session read capability is not granted".into(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionWriteCapability {
    pub(crate) persistent: bool,
}

impl SessionWriteCapability {
    pub(crate) fn require(
        value: Option<&SessionWriteCapability>,
    ) -> Result<&SessionWriteCapability, CodingSessionError> {
        value.ok_or_else(|| CodingSessionError::UnsupportedCapability {
            capability: "session write capability is not granted".into(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UiCapability;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityRevocationPolicy {
    RequestCancelOlderOperations,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledCapabilityGeneration {
    pub(crate) generation: CapabilityGeneration,
    pub(crate) revocation: CapabilityRevocationPolicy,
    pub(crate) cancellation_requested_operation_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionCapabilityAccess {
    None,
    Read,
    Write,
}
