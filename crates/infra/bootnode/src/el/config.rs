//! Configuration for the execution-layer bootnode.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use reth_net_nat::NatResolver;

/// Default UDP/TCP listen port for the EL bootnode (matches reth's default).
pub const DEFAULT_EL_BOOTNODE_PORT: u16 = 30301;

/// Configuration for an [`super::ElBootnode`].
#[derive(Debug, Clone)]
pub struct ElBootnodeConfig {
    /// Combined UDP/TCP listen address.
    pub addr: SocketAddr,
    /// Optional path to a hex-encoded secp256k1 secret key. Generated and
    /// persisted to this path if it does not exist; ephemeral if `None`.
    pub secret_key_path: Option<PathBuf>,
    /// Strategy for resolving the externally-advertised IP.
    pub nat: NatResolver,
    /// Whether to additionally start a discv5 service alongside discv4.
    pub enable_v5: bool,
}

impl Default for ElBootnodeConfig {
    fn default() -> Self {
        Self {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_EL_BOOTNODE_PORT),
            secret_key_path: None,
            nat: NatResolver::Any,
            enable_v5: true,
        }
    }
}
