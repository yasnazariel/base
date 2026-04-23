//! Background polling loop for bootnode routing-table monitoring.

use std::time::Duration;

use tokio::sync::mpsc;
use tracing::warn;

use crate::prober::{BootnodeProber, BootnodeSnapshot};

/// Runs a continuous loop that queries all listed bootnodes every ~2 seconds.
///
/// Sends a `BootnodeSnapshot` on `tx` after each complete sweep. Stops when `tx` is closed.
pub async fn run_bootnode_poller(
    network_name: String,
    fork_hash: [u8; 4],
    bootnodes: Vec<String>,
    tx: mpsc::Sender<BootnodeSnapshot>,
) {
    let mut prober = match BootnodeProber::new(fork_hash).await {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "failed to create bootnode prober, poller exiting");
            return;
        }
    };

    loop {
        let snapshot = prober.probe_all(&network_name, &bootnodes).await;
        if tx.send(snapshot).await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
