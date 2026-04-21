//! Execution-layer bootnode that runs reth's discv4 (and optionally discv5).

use reth_discv4::{DiscoveryUpdate, Discv4, Discv4Config};
use reth_discv5::{Config as Discv5Config, Discv5, discv5::Event as Discv5Event};
use reth_network_peers::NodeRecord;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::{BootnodeError, BootnodeResult, ElBootnodeConfig, ElKeyLoader};

/// Runs reth's discv4 bootnode (and optionally a paired discv5 service) until
/// cancelled.
#[derive(Debug)]
pub struct ElBootnode {
    config: ElBootnodeConfig,
}

impl ElBootnode {
    /// Creates a new [`ElBootnode`] from the given config.
    pub const fn new(config: ElBootnodeConfig) -> Self {
        Self { config }
    }

    /// Runs the EL bootnode until `cancel` is triggered or both discovery
    /// streams end.
    pub async fn run(self, cancel: CancellationToken) -> BootnodeResult<()> {
        let ElBootnodeConfig { addr, secret_key_path, nat, enable_v5 } = self.config;

        let sk = ElKeyLoader::load_or_generate(secret_key_path.as_deref())?;
        let local_enr = NodeRecord::from_secret_key(addr, &sk);

        let discv4_config = Discv4Config::builder().external_ip_resolver(Some(nat)).build();
        let (_discv4, mut discv4_service) = Discv4::bind(addr, local_enr, sk, discv4_config)
            .await
            .map_err(BootnodeError::ElDiscv4)?;
        info!(
            target: "bootnode::el",
            addr = %addr,
            peer_id = ?local_enr.id,
            "started discv4"
        );

        let mut discv4_updates = discv4_service.update_stream();
        discv4_service.spawn();

        // Hoist the discv5 handle to function scope so its `Drop` doesn't tear the service down
        // when the `if` block exits — only the update receiver escapes the inner scope, not the
        // service handle itself.
        let _discv5_handle;
        let mut discv5_updates = if enable_v5 {
            let discv5_config = Discv5Config::builder(addr).build();
            let (handle, updates) = Discv5::start(&sk, discv5_config)
                .await
                .map_err(|e| BootnodeError::ElDiscv5(Box::new(e)))?;
            info!(target: "bootnode::el", addr = %addr, "started discv5");
            _discv5_handle = Some(handle);
            Some(updates)
        } else {
            _discv5_handle = None;
            None
        };

        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    info!(target: "bootnode::el", "shutdown requested");
                    return Ok(());
                }
                update = discv4_updates.next() => {
                    let Some(update) = update else {
                        info!(target: "bootnode::el", "discv4 update stream ended");
                        return Ok(());
                    };
                    match update {
                        DiscoveryUpdate::Added(record) => {
                            debug!(target: "bootnode::el", peer_id = ?record.id, "discv4 peer added");
                        }
                        DiscoveryUpdate::Removed(peer_id) => {
                            debug!(target: "bootnode::el", peer_id = ?peer_id, "discv4 peer removed");
                        }
                        _ => {}
                    }
                }
                update = async {
                    if let Some(updates) = &mut discv5_updates {
                        updates.recv().await
                    } else {
                        futures::future::pending().await
                    }
                } => {
                    let Some(event) = update else {
                        info!(target: "bootnode::el", "discv5 update stream ended");
                        return Ok(());
                    };
                    if let Discv5Event::SessionEstablished(enr, _) = event {
                        debug!(target: "bootnode::el", peer_id = ?enr.id(), "discv5 peer added");
                    }
                }
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

    use super::*;

    #[tokio::test]
    async fn shuts_down_on_cancel() {
        let config = ElBootnodeConfig {
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            secret_key_path: None,
            nat: reth_net_nat::NatResolver::None,
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
            .expect("bootnode task did not exit within timeout")
            .expect("bootnode task panicked");
        result.expect("bootnode returned error");
    }
}
