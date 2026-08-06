use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Why a resolved address was blocked. Each variant maps to an independent
/// guard rule so tests can assert the exact policy hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    /// 127.0.0.0/8, ::1
    Loopback,
    /// 10/8, 172.16/12, 192.168/16
    Rfc1918,
    /// 169.254/16, fe80::/10
    LinkLocal,
    /// 169.254.169.254 (cloud metadata endpoint)
    CloudMetadata,
    /// ::ffff:0:0/96 carrying an embedded IPv4 address
    Ipv4Mapped,
    /// 0.0.0.0/8, ::
    Unspecified,
    /// 224.0.0.0/4, ff00::/8
    Multicast,
    /// 255.255.255.255
    Broadcast,
    /// fc00::/7 (unique local addresses)
    Ula,
}

impl BlockReason {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Loopback => "loopback address",
            Self::Rfc1918 => "RFC1918 private address",
            Self::LinkLocal => "link-local address",
            Self::CloudMetadata => "cloud metadata endpoint",
            Self::Ipv4Mapped => "IPv4-mapped IPv6 address",
            Self::Unspecified => "unspecified address",
            Self::Multicast => "multicast address",
            Self::Broadcast => "broadcast address",
            Self::Ula => "unique local address",
        }
    }
}

/// Reject every address class that must never be the target of a fetch.
/// Resolution results are validated in full: a hostname resolving to any
/// blocked address fails closed even when other records are public.
pub fn validate_ip(ip: IpAddr) -> Result<(), BlockReason> {
    match ip {
        IpAddr::V4(v4) => validate_ipv4(v4),
        IpAddr::V6(v6) => validate_ipv6(v6),
    }
}

fn validate_ipv4(ip: Ipv4Addr) -> Result<(), BlockReason> {
    if ip.is_loopback() {
        return Err(BlockReason::Loopback);
    }
    if ip.is_private() {
        return Err(BlockReason::Rfc1918);
    }
    if ip.is_link_local() {
        // 169.254.169.254 is inside the link-local block but gets its own
        // explicit guard and test so cloud metadata blocking stays pinned.
        if ip.octets() == [169, 254, 169, 254] {
            return Err(BlockReason::CloudMetadata);
        }
        return Err(BlockReason::LinkLocal);
    }
    if ip.is_multicast() {
        return Err(BlockReason::Multicast);
    }
    if ip.is_broadcast() {
        return Err(BlockReason::Broadcast);
    }
    if ip.is_unspecified() || ip.octets()[0] == 0 {
        // 0.0.0.0 itself and the rest of the 0.0.0.0/8 "this network" block.
        return Err(BlockReason::Unspecified);
    }
    Ok(())
}

fn validate_ipv6(ip: Ipv6Addr) -> Result<(), BlockReason> {
    if let Some(embedded) = ip.to_ipv4_mapped() {
        // ::ffff:0:0/96 decodes to a real IPv4 address; the mapping itself is
        // rejected regardless of the embedded value.
        let _ = embedded;
        return Err(BlockReason::Ipv4Mapped);
    }
    if ip.is_loopback() {
        return Err(BlockReason::Loopback);
    }
    if ip.is_unspecified() {
        return Err(BlockReason::Unspecified);
    }
    if ip.is_multicast() {
        return Err(BlockReason::Multicast);
    }
    let segments = ip.segments();
    if segments[0] & 0xffc0 == 0xfe80 {
        return Err(BlockReason::LinkLocal);
    }
    if segments[0] & 0xfe00 == 0xfc00 {
        return Err(BlockReason::Ula);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ipv4(octets: [u8; 4]) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(octets))
    }

    fn ipv6(segments: [u16; 8]) -> IpAddr {
        IpAddr::V6(Ipv6Addr::from(segments))
    }

    fn blocked(ip: IpAddr) -> BlockReason {
        validate_ip(ip).expect_err("must be blocked")
    }

    #[test]
    fn loopback_v4_and_v6_are_blocked() {
        assert_eq!(blocked(ipv4([127, 0, 0, 1])), BlockReason::Loopback);
        assert_eq!(blocked(ipv4([127, 255, 255, 254])), BlockReason::Loopback);
        assert_eq!(
            blocked(ipv6([0, 0, 0, 0, 0, 0, 0, 1])),
            BlockReason::Loopback
        );
    }

    #[test]
    fn rfc1918_blocks_are_blocked() {
        assert_eq!(blocked(ipv4([10, 0, 0, 1])), BlockReason::Rfc1918);
        assert_eq!(blocked(ipv4([10, 255, 255, 255])), BlockReason::Rfc1918);
        assert_eq!(blocked(ipv4([172, 16, 0, 1])), BlockReason::Rfc1918);
        assert_eq!(blocked(ipv4([172, 31, 255, 254])), BlockReason::Rfc1918);
        assert_eq!(blocked(ipv4([192, 168, 0, 1])), BlockReason::Rfc1918);
        assert_eq!(blocked(ipv4([192, 168, 255, 255])), BlockReason::Rfc1918);
    }

    #[test]
    fn link_local_v4_and_v6_are_blocked() {
        assert_eq!(blocked(ipv4([169, 254, 0, 1])), BlockReason::LinkLocal);
        assert_eq!(blocked(ipv4([169, 254, 255, 255])), BlockReason::LinkLocal);
        assert_eq!(
            blocked(ipv6([0xfe80, 0, 0, 0, 0, 0, 0, 1])),
            BlockReason::LinkLocal
        );
        assert_eq!(
            blocked(ipv6([0xfebf, 1, 2, 3, 4, 5, 6, 7])),
            BlockReason::LinkLocal
        );
    }

    #[test]
    fn cloud_metadata_gets_its_own_explicit_block() {
        assert_eq!(
            blocked(ipv4([169, 254, 169, 254])),
            BlockReason::CloudMetadata
        );
    }

    #[test]
    fn ipv4_mapped_ipv6_is_blocked_whatever_it_embeds() {
        assert_eq!(
            blocked(ipv6([0, 0, 0, 0, 0, 0xffff, 0x7f00, 1])),
            BlockReason::Ipv4Mapped
        );
        assert_eq!(
            blocked(ipv6([0, 0, 0, 0, 0, 0xffff, 0x0a00, 1])),
            BlockReason::Ipv4Mapped
        );
        assert_eq!(
            blocked(ipv6([0, 0, 0, 0, 0, 0xffff, 0x5d8a, 0xfd5e])),
            BlockReason::Ipv4Mapped
        );
    }

    #[test]
    fn unspecified_multicast_broadcast_and_ula_are_blocked() {
        assert_eq!(blocked(ipv4([0, 0, 0, 0])), BlockReason::Unspecified);
        assert_eq!(blocked(ipv4([0, 0, 0, 7])), BlockReason::Unspecified);
        assert_eq!(blocked(ipv4([224, 0, 0, 1])), BlockReason::Multicast);
        assert_eq!(blocked(ipv4([239, 255, 255, 255])), BlockReason::Multicast);
        assert_eq!(blocked(ipv4([255, 255, 255, 255])), BlockReason::Broadcast);
        assert_eq!(
            blocked(ipv6([0, 0, 0, 0, 0, 0, 0, 0])),
            BlockReason::Unspecified
        );
        assert_eq!(
            blocked(ipv6([0xff00, 0, 0, 0, 0, 0, 0, 1])),
            BlockReason::Multicast
        );
        assert_eq!(
            blocked(ipv6([0xff02, 0, 0, 0, 0, 0, 0, 1])),
            BlockReason::Multicast
        );
        assert_eq!(
            blocked(ipv6([0xfc00, 0, 0, 0, 0, 0, 0, 1])),
            BlockReason::Ula
        );
        assert_eq!(
            blocked(ipv6([0xfdff, 1, 2, 3, 4, 5, 6, 7])),
            BlockReason::Ula
        );
    }

    #[test]
    fn public_addresses_pass() {
        assert!(validate_ip(ipv4([93, 184, 216, 34])).is_ok());
        assert!(validate_ip(ipv4([8, 8, 8, 8])).is_ok());
        assert!(validate_ip(ipv4([1, 1, 1, 1])).is_ok());
        assert!(validate_ip(ipv6([0x2606, 0x4700, 0, 0, 0, 0, 0, 0x1111])).is_ok());
        assert!(validate_ip(ipv4([192, 0, 2, 1])).is_ok());
        assert!(validate_ip(ipv4([198, 51, 100, 1])).is_ok());
    }
}
