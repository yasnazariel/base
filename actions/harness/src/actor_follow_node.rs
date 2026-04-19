//! Production-actor follow-node test harness.

use std::{sync::Arc, time::Duration};

use alloy_genesis::ChainConfig as GenesisChainConfig;
use alloy_provider::RootProvider;
use base_consensus_engine::{BaseEngineClient, EngineClientBuilder};
use base_consensus_genesis::RollupConfig;
use base_consensus_node::{
    EngineConfig, FollowNode, L1Config, NetworkActor, NetworkInboundData, NodeMode, NodeActor,
    QueuedNetworkEngineClient,
};
use base_consensus_providers::OnlineBeaconClient;
use base_protocol::L2BlockInfo;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{
    ActionEngineClient, ActionL2LocalProvider, ActionL2SourceBridge, HarnessEngineServer,
    HarnessL1Server, SupervisedP2P, TestGossipTransport,
};

type ProdEngineClient = BaseEngineClient<RootProvider, RootProvider<base_common_network::Base>>;

/// An action test harness that drives the production [`FollowNode`] supervisor with an
/// HTTP-backed [`BaseEngineClient`].
///
/// Unlike [`crate::TestFollowNode`], which reimplements the sync loop manually,
/// `TestActorFollowNode` runs the exact production actor stack: proofs-gating,
/// fork-divergence detection, FCU forwarding, and L1 watching all execute
/// through the real [`FollowNode::start_with_engine_client`] code path.
///
/// The engine actors communicate with the execution layer over localhost HTTP
/// via a real [`BaseEngineClient`], so the JWT-authenticated JSON-RPC transport
/// path is also exercised.  The [`ActionEngineClient`] held on `engine` is the
/// same instance that the HTTP server delegates to, so test assertions on
/// `engine.unsafe_head()` reflect the post-RPC state.
///
/// A real [`NetworkActor`] with a [`TestGossipTransport`] is also wired
/// alongside the [`FollowNode`] actors. Use [`Self::gossip_tx`] to inject
/// gossip blocks into the network actor's transport channel.
///
/// # Timing
///
/// Actors are driven by tokio's time-controlled runtime. Tests must use
/// `#[tokio::test(start_paused = true)]` and call [`Self::tick`] to advance the
/// derivation actor's poll interval. The HTTP engine server uses async I/O (not
/// timers), so it continues to function correctly with paused wall-clock time.
#[derive(Debug)]
pub struct TestActorFollowNode {
    /// Source of L2 blocks — push blocks here before ticking.
    pub source: ActionL2SourceBridge,
    /// The local engine client — query state here after ticking.
    pub engine: ActionEngineClient,
    /// The rollup config used by the derivation actor.
    pub rollup_config: Arc<RollupConfig>,
    /// Sender for injecting gossip blocks directly into the network actor.
    pub gossip_tx: SupervisedP2P,
    /// Cancellation token to stop the NetworkActor on drop.
    cancel: CancellationToken,
    /// Handle to the spawned [`FollowNode`] task — aborted on drop to stop all
    /// internal actors (DelegateL2DerivationActor, EngineActor, L1WatcherActor).
    _follow_handle: JoinHandle<()>,
    /// Handle to the spawned network actor task.
    _network_handle: JoinHandle<()>,
    /// Keeps the network actor's inbound channels alive for the lifetime of the test.
    _network_inbound: NetworkInboundData,
    /// Keeps the in-process engine API HTTP server alive for the duration of the test.
    _engine_server: HarnessEngineServer,
    /// Keeps the in-process L1 JSON-RPC HTTP server alive for the duration of the test.
    _l1_server: HarnessL1Server,
}

impl TestActorFollowNode {
    /// Creates a new [`TestActorFollowNode`] wiring the real [`FollowNode`] supervisor.
    ///
    /// Spawns an in-process Engine API HTTP server and an L1 JSON-RPC HTTP
    /// server, then calls [`FollowNode::start_with_engine_client`] so that the
    /// real production actor wiring — [`DelegateL2DerivationActor`],
    /// [`EngineActor`], [`L1WatcherActor`], and [`L1WatcherQueryProcessor`] —
    /// runs exactly as in production, with only the [`ActionEngineClient`] and
    /// [`ActionL2SourceBridge`] injected at the trait boundaries.
    ///
    /// A [`NetworkActor`] with a [`TestGossipTransport`] is wired separately
    /// (outside [`FollowNode`], which explicitly omits P2P) so the gossip path
    /// remains exercisable from tests via [`Self::gossip_tx`].
    pub async fn new(
        config: Arc<RollupConfig>,
        engine: ActionEngineClient,
        source: ActionL2SourceBridge,
        local_provider: ActionL2LocalProvider,
        proofs_enabled: bool,
        proofs_max_blocks_ahead: u64,
    ) -> Self {
        let cancel = CancellationToken::new();

        let engine_server = HarnessEngineServer::spawn(Arc::new(engine.clone()))
            .await
            .expect("TestActorFollowNode: failed to spawn engine server");
        let l1_server = HarnessL1Server::spawn(engine.l1_chain())
            .await
            .expect("TestActorFollowNode: failed to spawn L1 server");

        let http_client = Arc::new(
            EngineClientBuilder {
                l2: engine_server.url.clone(),
                l2_jwt: engine_server.jwt,
                l1_rpc: l1_server.url.clone(),
                cfg: Arc::clone(&config),
            }
            .build()
            .await
            .expect("TestActorFollowNode: failed to build BaseEngineClient"),
        );

        // Wire NetworkActor with TestGossipTransport alongside FollowNode.
        // FollowNode explicitly omits P2P — the network actor is added here to
        // maintain the Change B production-path fidelity guarantee.
        let (engine_actor_request_tx, engine_actor_request_rx) = mpsc::channel(1024);
        let (gossip_tx, transport) = TestGossipTransport::channel();
        let (network_inbound, network_actor) = NetworkActor::with_transport(
            QueuedNetworkEngineClient { engine_actor_request_tx: engine_actor_request_tx.clone() },
            cancel.clone(),
            transport,
        );
        let network_handle = tokio::spawn(async move {
            let _ = network_actor.start(()).await;
        });
        // The engine actor request receiver is consumed by FollowNode internally;
        // drop the dummy channel we created for the NetworkActor's send side only.
        drop(engine_actor_request_rx);

        // Build FollowNode. EngineConfig values are provided for completeness but
        // are not used by start_with_engine_client (the engine client is injected
        // directly). L1Config.engine_provider points at HarnessL1Server so the
        // real L1WatcherActor can poll L1 head/finalized blocks during the test.
        let engine_config = EngineConfig {
            config: Arc::clone(&config),
            l2_url: engine_server.url.clone(),
            l2_jwt_secret: engine_server.jwt,
            l1_url: l1_server.url.clone(),
            mode: NodeMode::Validator,
        };
        let l1_config = L1Config {
            chain_config: Arc::new(GenesisChainConfig::default()),
            trust_rpc: false,
            beacon_client: OnlineBeaconClient::new_http("http://localhost:1".to_string()),
            engine_provider: RootProvider::new_http(l1_server.url.clone()),
            finalized_poll_interval: Duration::from_secs(2),
            verifier_l1_confs: 0,
        };

        let follow_node = FollowNode::new(
            Arc::clone(&config),
            engine_config,
            Arc::new(local_provider) as Arc<dyn base_consensus_node::LocalL2Provider>,
            source.clone(),
            None,
            l1_config,
        )
        .with_proofs(proofs_enabled)
        .with_proofs_max_blocks_ahead(proofs_max_blocks_ahead);

        let follow_client = Arc::clone(&http_client) as Arc<ProdEngineClient>;
        let follow_handle = tokio::spawn(async move {
            if let Err(e) = follow_node.start_with_engine_client(follow_client).await {
                eprintln!("TestActorFollowNode: FollowNode exited with error: {e}");
            }
        });

        Self {
            source,
            engine,
            rollup_config: Arc::clone(&config),
            gossip_tx,
            cancel,
            _follow_handle: follow_handle,
            _network_handle: network_handle,
            _network_inbound: network_inbound,
            _engine_server: engine_server,
            _l1_server: l1_server,
        }
    }

    /// Advance the derivation actor's poll clock by one 2-second interval.
    ///
    /// Must be called from a test with `start_paused = true`. Yields many
    /// times after advancing time to allow the actor's select loop to process
    /// the tick, spawn the sync sub-task, route messages through the engine
    /// actor and engine processor tasks, and execute `InsertTask` + FCU.
    ///
    /// The yield count is intentionally generous: each yield gives one
    /// spawned task one scheduling turn, and the full chain (derivation actor →
    /// sync sub-task → engine actor → engine processor → InsertTask) now
    /// includes a localhost HTTP round-trip via the real [`BaseEngineClient`].
    pub async fn tick(&self) {
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        for _ in 0..500 {
            tokio::task::yield_now().await;
        }
    }

    /// Tick until the engine's unsafe head reaches `target` block number.
    ///
    /// Panics if `target` is not reached within 100 ticks.
    pub async fn sync_to_unsafe(&self, target: u64) {
        for _ in 0..100 {
            if self.engine.unsafe_head().block_info.number >= target {
                return;
            }
            self.tick().await;
        }
        panic!(
            "sync_to_unsafe({target}): stuck at block {}",
            self.engine.unsafe_head().block_info.number
        );
    }

    /// Return the current unsafe head tracked by the local engine.
    pub fn unsafe_head(&self) -> L2BlockInfo {
        self.engine.unsafe_head()
    }

    /// Cancel the NetworkActor and abort the FollowNode task.
    pub fn cancel(&self) {
        self.cancel.cancel();
        self._follow_handle.abort();
    }
}

impl Drop for TestActorFollowNode {
    fn drop(&mut self) {
        self.cancel.cancel();
        self._follow_handle.abort();
    }
}
