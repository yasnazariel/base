//! Consensus-layer bootnode that wraps `base-consensus-disc` in standalone mode.

use std::net::SocketAddr;

use base_consensus_disc::{Discv5Builder, LocalNode};
use base_consensus_peers::{BootNode, BootNodes, BootStoreFile};
use discv5::ConfigBuilder;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::{BootnodeError, BootnodeResult, ClBootnodeConfig, ClKeyLoader};

/// Runs a CL `discv5` service in bootnode mode (no ENR forwarding to a gossip
/// layer) until cancelled.
#[derive(Debug)]
pub struct ClBootnode {
    config: ClBootnodeConfig,
}

impl ClBootnode {
    /// Creates a new [`ClBootnode`] from the given config.
    pub const fn new(config: ClBootnodeConfig) -> Self {
        Self { config }
    }

    /// Parses the user-supplied bootnode strings. An empty list is forwarded as-is, signalling
    /// the driver to fall back to the chain default list keyed on `chain_id`.
    pub fn resolve_bootnodes(&self) -> BootnodeResult<BootNodes> {
        self.config
            .bootnodes
            .iter()
            .map(|raw| {
                BootNode::parse_bootnode(raw)
                    .map_err(|source| BootnodeError::ClBootnodeParse { raw: raw.clone(), source })
            })
            .collect::<BootnodeResult<Vec<_>>>()
            .map(BootNodes::from)
    }

    /// Runs the CL bootnode until `cancel` is triggered or the discovery ENR
    /// channel closes.
    pub async fn run(self, cancel: CancellationToken) -> BootnodeResult<()> {
        if self.config.advertise_ip.is_unspecified() {
            return Err(BootnodeError::UnroutableClAdvertiseIp { ip: self.config.advertise_ip });
        }

        let signing_key = ClKeyLoader::load_or_generate(self.config.secret_key_path.as_deref())?;
        let local_node = LocalNode::new(
            signing_key,
            self.config.advertise_ip,
            self.config.advertise_tcp_port,
            self.config.advertise_udp_port,
        );

        let listen_addr = SocketAddr::new(self.config.listen_ip, self.config.listen_udp_port);
        let mut config_builder = ConfigBuilder::new(listen_addr.into());
        if self.config.static_ip {
            config_builder.disable_enr_update();
        }
        let discovery_config = config_builder.build();

        let bootnodes = self.resolve_bootnodes()?;
        let bootnodes_len = bootnodes.len();
        let chain_id = self.config.chain_id;
        let bootstore = self
            .config
            .bootstore_path
            .map_or(BootStoreFile::Default { chain_id }, BootStoreFile::Custom);

        let driver = Discv5Builder::new(local_node, chain_id, discovery_config)
            .with_bootnodes(bootnodes)
            .with_bootstore_file(Some(bootstore))
            .replace_chain_defaults()
            .disable_forward()
            .build()?;

        let (handler, mut enr_rx) = driver.start();
        info!(
            target: "bootnode::cl",
            chain_id = %chain_id,
            listen = %listen_addr,
            bootnodes = %bootnodes_len,
            "started CL discv5"
        );

        let outcome = loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    info!(target: "bootnode::cl", "shutdown requested");
                    break Ok(());
                }
                enr = enr_rx.recv() => {
                    match enr {
                        Some(enr) => debug!(target: "bootnode::cl", enr = %enr, "discovered peer"),
                        None => {
                            info!(target: "bootnode::cl", "discv5 ENR channel closed");
                            break Ok(());
                        }
                    }
                }
            }
        };

        drop(handler);
        outcome
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr},
        time::Duration,
    };

    use base_common_chains::ChainConfig;
    use tempfile::TempDir;

    use super::*;

    fn ephemeral_config(chain_id: u64, dir: &TempDir) -> ClBootnodeConfig {
        // Use a fixed high port so the advertised ENR matches reality (discv5 rejects ENRs that
        // advertise port 0). The OS assigns the actual listen port via `port: 0` only at the
        // socket level, but the ENR advertisement must be a real number.
        const TEST_PORT: u16 = 30391;
        ClBootnodeConfig {
            chain_id,
            listen_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            listen_udp_port: TEST_PORT,
            advertise_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            advertise_tcp_port: TEST_PORT,
            advertise_udp_port: TEST_PORT,
            secret_key_path: Some(dir.path().join("cl-secret.key")),
            bootstore_path: Some(dir.path().join("bootstore.json")),
            bootnodes: Vec::new(),
            static_ip: true,
        }
    }

    #[tokio::test]
    async fn shuts_down_on_cancel() {
        let dir = TempDir::new().expect("create tempdir");
        let config = ephemeral_config(ChainConfig::sepolia().chain_id, &dir);
        let bootnode = ClBootnode::new(config);
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();

        let task = tokio::spawn(async move { bootnode.run(cancel_for_task).await });

        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("CL bootnode did not exit within timeout")
            .expect("CL bootnode task panicked");
        result.expect("CL bootnode returned error");
    }

    #[test]
    fn rejects_invalid_bootnode_string() {
        let dir = TempDir::new().expect("create tempdir");
        let mut config = ephemeral_config(ChainConfig::sepolia().chain_id, &dir);
        config.bootnodes = vec!["definitely-not-an-enr".into()];
        let bootnode = ClBootnode::new(config);
        let err = bootnode.resolve_bootnodes().expect_err("should fail to parse");
        assert!(matches!(err, BootnodeError::ClBootnodeParse { .. }));
    }
}
