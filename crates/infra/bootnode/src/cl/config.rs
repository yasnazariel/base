//! Configuration for the consensus-layer bootnode.

use std::{
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
};

/// Default UDP listen port for the CL bootnode.
pub const DEFAULT_CL_BOOTNODE_PORT: u16 = 9222;

/// Configuration for a [`super::ClBootnode`].
#[derive(Debug, Clone)]
pub struct ClBootnodeConfig {
    /// L2 chain ID. Drives ENR encoding and the default bootnode list.
    pub chain_id: u64,
    /// IP to bind the discv5 socket to.
    pub listen_ip: IpAddr,
    /// UDP port to bind the discv5 socket to.
    pub listen_udp_port: u16,
    /// IP to advertise in the local ENR.
    pub advertise_ip: IpAddr,
    /// TCP port to advertise in the local ENR.
    pub advertise_tcp_port: u16,
    /// UDP port to advertise in the local ENR.
    pub advertise_udp_port: u16,
    /// Optional path to a hex-encoded secp256k1 secret key. Generated and
    /// persisted to this path if it does not exist; ephemeral if `None`
    /// (with a warning, since bootnode ENRs should be stable across restarts).
    pub secret_key_path: Option<PathBuf>,
    /// Optional override for the on-disk bootstore path. Defaults to
    /// `~/.base/<chain_id>/bootstore.json`.
    pub bootstore_path: Option<PathBuf>,
    /// User-supplied bootnodes that, when non-empty, replace the chain default
    /// list. Each entry must be either an `enr:` or `enode://` string.
    pub bootnodes: Vec<String>,
    /// If `true`, disable ENR auto-update so the advertised IP is stable.
    pub static_ip: bool,
}

impl ClBootnodeConfig {
    /// Returns a config skeleton bound to all interfaces on the default port for the given chain.
    ///
    /// **`advertise_ip` is left as `0.0.0.0` and MUST be replaced with the node's
    /// externally-routable IP before passing to [`super::ClBootnode`].**
    /// [`super::ClBootnode::run`] will immediately return
    /// [`super::super::BootnodeError::UnroutableClAdvertiseIp`] if the field is left unset.
    pub const fn for_chain(chain_id: u64) -> Self {
        Self {
            chain_id,
            listen_ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            listen_udp_port: DEFAULT_CL_BOOTNODE_PORT,
            advertise_ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            advertise_tcp_port: DEFAULT_CL_BOOTNODE_PORT,
            advertise_udp_port: DEFAULT_CL_BOOTNODE_PORT,
            secret_key_path: None,
            bootstore_path: None,
            bootnodes: Vec::new(),
            static_ip: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_chain_uses_default_port() {
        let cfg = ClBootnodeConfig::for_chain(8453);
        assert_eq!(cfg.chain_id, 8453);
        assert_eq!(cfg.listen_udp_port, DEFAULT_CL_BOOTNODE_PORT);
        assert_eq!(cfg.advertise_udp_port, DEFAULT_CL_BOOTNODE_PORT);
        assert!(cfg.bootnodes.is_empty());
        // for_chain is a skeleton — callers must set advertise_ip to a routable address.
        assert!(cfg.advertise_ip.is_unspecified());
    }
}
