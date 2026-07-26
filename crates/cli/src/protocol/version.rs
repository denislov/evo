use serde::Deserialize;
use std::fmt;

pub use coding_agent::api::client::{ProtocolFamilyVersion, UI_SNAPSHOT_PROTOCOL_VERSION};
pub use coding_agent::api::event::PRODUCT_EVENT_PROTOCOL_VERSION;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RequestedProtocolVersion {
    pub family: String,
    pub major: u32,
    pub minor: u32,
}

impl fmt::Display for RequestedProtocolVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}.{}", self.family, self.major, self.minor)
    }
}

pub const RPC_PROTOCOL_VERSION: ProtocolFamilyVersion = ProtocolFamilyVersion::new("rpc", 3, 0);

pub fn is_compatible_with(
    supported: ProtocolFamilyVersion,
    requested: &RequestedProtocolVersion,
) -> bool {
    supported.family == requested.family
        && supported.major == requested.major
        && requested.minor <= supported.minor
}
