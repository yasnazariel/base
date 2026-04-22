//! Delegates EL bootnode execution to reth's built-in `p2p bootnode` command.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use reth_cli_commands::p2p::bootnode::Command as RethBootnodeCommand;
use reth_net_nat::NatResolver;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::BootnodeResult;

/// Default UDP/TCP listen port for the EL bootnode (matches reth's default).
pub const DEFAULT_EL_BOOTNODE_PORT: u16 = 30301;

/// Configuration for [`ElBootnode`].
#[derive(Debug, Clone)]
pub struct ElBootnodeConfig {
    /// Combined UDP/TCP listen address.
    pub addr: SocketAddr,
    /// Optional path to a hex-encoded secp256k1 secret key. Generated and persisted to
    /// this path if it does not exist; ephemeral if `None`
    /// (with a warning, since bootnode ENRs should be stable across restarts).
    pub secret_key_path: Option<PathBuf>,
    /// Strategy for resolving the externally-advertised IP.
    pub nat: NatResolver,
    /// Whether to additionally run discv5 alongside discv4.
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

/// Runs reth's `p2p bootnode` command until `cancel` is triggered.
#[derive(Debug)]
pub struct ElBootnode {
    config: ElBootnodeConfig,
}

impl ElBootnode {
    /// Creates a new [`ElBootnode`] from the given config.
    pub const fn new(config: ElBootnodeConfig) -> Self {
        Self { config }
    }

    /// Runs the EL bootnode until `cancel` is triggered.
    pub async fn run(self, cancel: CancellationToken) -> BootnodeResult<()> {
        info!(
            target: "bootnode::el",
            addr = %self.config.addr,
            nat = %self.config.nat,
            discv5 = self.config.enable_v5,
            "starting EL bootnode"
        );

        let cmd = RethBootnodeCommand {
            addr: self.config.addr,
            p2p_secret_key: self.config.secret_key_path,
            nat: self.config.nat,
            v5: self.config.enable_v5,
        };

        tokio::select! {
            result = cmd.execute() => result.map_err(crate::BootnodeError::El),
            () = cancel.cancelled() => {
                info!(target: "bootnode::el", "shutdown requested");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::Duration,
    };

    use reth_net_nat::NatResolver;

    use super::*;

    #[tokio::test]
    async fn shuts_down_on_cancel() {
        let config = ElBootnodeConfig {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            secret_key_path: None,
            nat: NatResolver::None,
            enable_v5: false,
        };
        let bootnode = ElBootnode::new(config);
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();

        let task = tokio::spawn(async move { bootnode.run(cancel_for_task).await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("EL bootnode did not exit within timeout")
            .expect("EL bootnode task panicked");
        result.expect("EL bootnode returned error");
    }
}
