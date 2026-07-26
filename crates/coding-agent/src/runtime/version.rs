use serde::Serialize;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProtocolFamilyVersion {
    pub family: &'static str,
    pub major: u32,
    pub minor: u32,
}

impl ProtocolFamilyVersion {
    pub const fn new(family: &'static str, major: u32, minor: u32) -> Self {
        Self {
            family,
            major,
            minor,
        }
    }
}

impl fmt::Display for ProtocolFamilyVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}.{}", self.family, self.major, self.minor)
    }
}

pub const PRODUCT_EVENT_PROTOCOL_VERSION: ProtocolFamilyVersion =
    ProtocolFamilyVersion::new("product_event", 3, 0);
pub const UI_SNAPSHOT_PROTOCOL_VERSION: ProtocolFamilyVersion =
    ProtocolFamilyVersion::new("ui_snapshot", 3, 0);
