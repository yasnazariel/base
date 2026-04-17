//! Gossip network spam canary action.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base_consensus_gossip::{ConnectionGater, GossipDriver, GossipDriverBuilder};
use base_consensus_rpc::{BaseP2PApiClient, RollupNodeApiClient};
use jsonrpsee::http_client::HttpClientBuilder;
use libp2p::Multiaddr;
use libp2p_identity::Keypair;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use url::Url;

use crate::{ActionOutcome, CanaryAction};

/// Timeout for the rollup config and peer info RPC calls.
const RPC_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for the first peer connection before giving up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// How long to poll the driver for a single event before moving on.
const EVENT_POLL_MS: u64 = 50;

/// Connects as an ephemeral gossip peer and floods the network with garbage
/// messages.
///
/// Each message has valid wire framing (snappy-compressed) but an invalid
/// signature, so peers accept the connection but immediately reject the
/// payload. This exercises the network's spam-rejection path without
/// interfering with real block propagation.
#[derive(Debug)]
pub struct GossipSpamAction {
    cl_rpc_url: Url,
    spam_count: u32,
    spam_interval: Duration,
}

impl GossipSpamAction {
    /// Creates a new [`GossipSpamAction`].
    pub const fn new(cl_rpc_url: Url, spam_count: u32, spam_interval: Duration) -> Self {
        Self { cl_rpc_url, spam_count, spam_interval }
    }

    async fn build_driver(
        cl_rpc_url: &Url,
    ) -> Result<(GossipDriver<ConnectionGater>, Multiaddr), String> {
        let cl_client = HttpClientBuilder::default()
            .build(cl_rpc_url.as_str())
            .map_err(|e| format!("failed to build CL RPC client: {e}"))?;

        let rollup_config = timeout(RPC_TIMEOUT, cl_client.rollup_config())
            .await
            .map_err(|_| "rollup_config RPC timed out".to_string())?
            .map_err(|e| format!("rollup_config RPC failed: {e}"))?;

        let peer_info = timeout(RPC_TIMEOUT, cl_client.opp2p_self())
            .await
            .map_err(|_| "opp2p_self RPC timed out".to_string())?
            .map_err(|e| format!("opp2p_self RPC failed: {e}"))?;

        // Find the first TCP address from peer_info and append the /p2p/{PeerId} component
        // if it is not already present.
        let peer_id_str = peer_info.peer_id.clone();
        let target_addr = peer_info
            .addresses
            .iter()
            .filter_map(|a| a.parse::<Multiaddr>().ok())
            .find(|ma| ma.iter().any(|p| matches!(p, libp2p::multiaddr::Protocol::P2p(_))))
            .or_else(|| {
                // No /p2p component found; try to parse and append manually.
                let pid: libp2p::PeerId = peer_id_str.parse().ok()?;
                peer_info
                    .addresses
                    .iter()
                    .filter_map(|a| a.parse::<Multiaddr>().ok())
                    .map(|mut ma| {
                        ma.push(libp2p::multiaddr::Protocol::P2p(pid));
                        ma
                    })
                    .next()
            })
            .ok_or_else(|| "no usable peer address in opp2p_self response".to_string())?;

        // Random ed25519 identity — distinct from the real block signer.
        let keypair = Keypair::generate_ed25519();

        // Listen on an OS-assigned TCP port.
        let listen_addr: Multiaddr =
            "/ip4/0.0.0.0/tcp/0".parse().map_err(|e| format!("invalid listen addr: {e}"))?;

        let (mut driver, _signer_tx) = GossipDriverBuilder::new(
            rollup_config,
            alloy_primitives::Address::ZERO,
            listen_addr,
            keypair,
        )
        .build()
        .map_err(|e| format!("failed to build gossip driver: {e}"))?;

        driver.start().await.map_err(|e| format!("failed to start gossip driver: {e}"))?;

        Ok((driver, target_addr))
    }
}

#[async_trait]
impl CanaryAction for GossipSpamAction {
    fn name(&self) -> &'static str {
        "gossip_spam"
    }

    async fn execute(&self, cancel: CancellationToken) -> ActionOutcome {
        let start = Instant::now();

        if cancel.is_cancelled() {
            return ActionOutcome::failed("cancelled", start);
        }

        let (mut driver, target_addr) = match Self::build_driver(&self.cl_rpc_url).await {
            Ok(v) => v,
            Err(e) => return ActionOutcome::failed(e, start),
        };

        debug!(target = %target_addr, "dialing devnet gossip peer");
        driver.dial_multiaddr(target_addr);

        // Phase 1: drive events until the peer connects or we time out.
        let connect_deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            if cancel.is_cancelled() {
                return ActionOutcome::failed("cancelled during peer connect", start);
            }
            if Instant::now() >= connect_deadline {
                break;
            }
            let remaining = connect_deadline.saturating_duration_since(Instant::now());
            let poll_dur = remaining.min(Duration::from_millis(EVENT_POLL_MS));
            match timeout(poll_dur, driver.next()).await {
                Ok(Some(ev)) => {
                    driver.handle_event(ev);
                    if driver.connected_peers() > 0 {
                        info!(peers = driver.connected_peers(), "connected to gossip network");
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => {} // poll timeout — try again
            }
        }

        if driver.connected_peers() == 0 {
            return ActionOutcome::failed("failed to connect to any gossip peer", start);
        }

        // Phase 2: publish spam messages as fast as possible, draining the swarm
        // event loop periodically to flush outbound queues.
        let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let topic_hash = driver.handler.topic(now_secs).hash();

        // Raw garbage bytes: snappy-compressing a known pattern means peers decode
        // the outer frame but fail on SSZ parsing — exercising the rejection path.
        let spam_bytes: Vec<u8> = b"base-canary-gossip-spam".to_vec();

        // Drain the swarm every this many publishes so outbound queues don't stall.
        const DRAIN_EVERY: u32 = 50;

        let mut published = 0u32;
        for i in 0..self.spam_count {
            if cancel.is_cancelled() {
                break;
            }

            let _ = driver
                .swarm
                .behaviour_mut()
                .gossipsub
                .publish(topic_hash.clone(), spam_bytes.clone());
            published += 1;

            // Periodically drain swarm events to flush outbound queues.
            if (i + 1) % DRAIN_EVERY == 0 || i + 1 == self.spam_count {
                while let Ok(Some(ev)) =
                    timeout(Duration::from_millis(EVENT_POLL_MS), driver.next()).await
                {
                    driver.handle_event(ev);
                }
            }

            if !self.spam_interval.is_zero() && published < self.spam_count {
                tokio::time::sleep(self.spam_interval).await;
            }
        }

        if published == 0 {
            return ActionOutcome::failed("failed to publish any spam messages", start);
        }

        let peers = driver.connected_peers();
        info!(published, peers, "gossip spam action complete");
        if published < self.spam_count {
            warn!(published, target = self.spam_count, "fewer spam messages published than target");
        }

        ActionOutcome::success(
            format!("published {published}/{} spam msgs to {peers} peer(s)", self.spam_count),
            start,
        )
    }
}
