//! Production-actor derivation-node test harness.

use std::sync::Arc;

use alloy_provider::RootProvider;
use base_consensus_engine::{BaseEngineClient, Engine, EngineClientBuilder, EngineState};
use base_consensus_genesis::RollupConfig;
use base_consensus_node::{
    DerivationActor, DerivationActorRequest, EngineActor, EngineProcessor, EngineRpcProcessor,
    NetworkActor, NetworkInboundData, NodeActor, QueuedDerivationEngineClient,
    QueuedEngineDerivationClient, QueuedNetworkEngineClient,
};
use base_consensus_safedb::{SafeDB, SafeDBError, SafeDBReader, SafeHeadResponse};
use base_protocol::{BlockInfo, L2BlockInfo};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{
    ActionEngineClient, HarnessEngineServer, HarnessL1Server, SupervisedP2P, TestGossipTransport,
    VerifierPipeline,
};

type ProdEngineClient = BaseEngineClient<RootProvider, RootProvider<base_common_network::Base>>;

/// An action test harness that drives the production [`DerivationActor`] and
/// [`EngineActor`] with an HTTP-backed [`BaseEngineClient`] and a real
/// derivation pipeline.
///
/// Unlike [`crate::TestRollupNode`], which steps the pipeline manually via
/// `step()` / `run_until_idle()`, `TestActorDerivationNode` runs the exact
/// production actor stack end-to-end: derivation-state-machine gating,
/// reorg detection, safe-head SafeDB writes, finalization tracking, and the
/// full `InsertTask` → `ConsolidateTask` engine flow all execute through the
/// real code paths.
///
/// The engine actors communicate with the execution layer over localhost HTTP
/// via a real [`BaseEngineClient`], so the JWT-authenticated JSON-RPC transport
/// path is also exercised.  The [`ActionEngineClient`] held on `engine` is the
/// same instance that the HTTP server delegates to, so test assertions on
/// `engine.safe_head()` / `engine.unsafe_head()` reflect the post-RPC state.
///
/// # Usage
///
/// 1. Build and push L1 data (batches) via [`crate::Batcher`] and
///    [`crate::SharedL1Chain`].
/// 2. Call [`initialize`] once to send the genesis activation and EL-sync
///    completion signals.
/// 3. Call [`act_l1_head_signal`] with the L1 tip to trigger derivation.
/// 4. Call [`tick`] / [`sync_until_safe`] to advance until the safe head
///    reaches the desired target.
///
/// # Tests
///
/// Tests using this harness should use a regular `#[tokio::test]` (no
/// `start_paused`), as the `DerivationActor` is purely event-driven with no
/// timer loops.
///
/// [`initialize`]: TestActorDerivationNode::initialize
/// [`act_l1_head_signal`]: TestActorDerivationNode::act_l1_head_signal
/// [`tick`]: TestActorDerivationNode::tick
/// [`sync_until_safe`]: TestActorDerivationNode::sync_until_safe
#[derive(Debug)]
pub struct TestActorDerivationNode {
    /// The engine client — query state here after ticking.
    pub engine: ActionEngineClient,
    /// Safe-head DB for asserting on [`optimism_safeHeadAtL1Block`] results.
    pub safe_db: Arc<SafeDB>,
    /// The rollup config used by the derivation actor.
    pub rollup_config: Arc<RollupConfig>,
    /// Sender for injecting gossip blocks directly into the network actor.
    pub gossip_tx: SupervisedP2P,
    /// Sender side of the derivation actor's inbound request channel.
    derivation_actor_tx: mpsc::Sender<DerivationActorRequest>,
    /// Cancellation token to stop the actors on drop.
    cancel: CancellationToken,
    /// Handle to the spawned network actor task.
    _network_handle: JoinHandle<()>,
    /// Handle to the spawned derivation actor task.
    _derivation_handle: JoinHandle<()>,
    /// Handle to the spawned engine actor task.
    _engine_handle: JoinHandle<()>,
    /// Keeps the network actor's inbound channels alive for the lifetime of the test.
    _network_inbound: NetworkInboundData,
    /// Keeps the in-process engine API HTTP server alive for the duration of the test.
    _engine_server: HarnessEngineServer,
    /// Keeps the in-process L1 JSON-RPC HTTP server alive for the duration of the test.
    _l1_server: HarnessL1Server,
    /// Temporary directory that holds the SafeDB files — dropped last to
    /// keep the DB alive for the duration of the test.
    _safedb_dir: tempfile::TempDir,
}

impl TestActorDerivationNode {
    /// Create a new [`TestActorDerivationNode`] wiring real production actors.
    ///
    /// Spawns an in-process Engine API HTTP server and an L1 JSON-RPC HTTP
    /// server, builds a production [`BaseEngineClient`] that talks to them,
    /// then wires a [`DerivationActor`] and an [`EngineActor`] using that
    /// client.  This mirrors the wiring in [`base_consensus_node::RollupNode`]
    /// while still allowing the test to observe engine state via the
    /// [`ActionEngineClient`] held on the returned struct.
    ///
    /// Call [`initialize`] before any derivation calls.
    ///
    /// [`initialize`]: Self::initialize
    pub async fn new(
        config: Arc<RollupConfig>,
        engine: ActionEngineClient,
        pipeline: VerifierPipeline,
        _genesis_safe_head: L2BlockInfo,
    ) -> Self {
        let safedb_dir = tempfile::TempDir::new()
            .expect("TestActorDerivationNode: failed to create tempdir");
        let safe_db = Arc::new(
            SafeDB::open(safedb_dir.path().join("safedb"))
                .expect("TestActorDerivationNode: failed to open SafeDB"),
        );
        let cancel = CancellationToken::new();

        let engine_server = HarnessEngineServer::spawn(Arc::new(engine.clone()))
            .await
            .expect("TestActorDerivationNode: failed to spawn engine server");
        let l1_server = HarnessL1Server::spawn(engine.l1_chain())
            .await
            .expect("TestActorDerivationNode: failed to spawn L1 server");

        let http_client = Arc::new(
            EngineClientBuilder {
                l2: engine_server.url.clone(),
                l2_jwt: engine_server.jwt,
                l1_rpc: l1_server.url.clone(),
                cfg: Arc::clone(&config),
            }
            .build()
            .await
            .expect("TestActorDerivationNode: failed to build BaseEngineClient"),
        );

        let (derivation_actor_tx, derivation_actor_rx) = mpsc::channel(1024);
        let (engine_actor_tx, engine_actor_rx) = mpsc::channel(1024);

        let (gossip_tx, transport) = TestGossipTransport::channel();
        let (network_inbound, network_actor) = NetworkActor::with_transport(
            QueuedNetworkEngineClient { engine_actor_request_tx: engine_actor_tx.clone() },
            cancel.clone(),
            transport,
        );
        let network_handle = tokio::spawn(async move {
            let _ = network_actor.start(()).await;
        });

        let engine_state = EngineState::default();
        let (engine_state_tx, engine_state_rx) = watch::channel(engine_state);
        let (engine_queue_length_tx, engine_queue_length_rx) = watch::channel(0usize);

        // A watch channel is required so that `EngineProcessor` selects
        // `bootstrap_active_sequencer` (not `bootstrap_validator`). At genesis,
        // `bootstrap_active_sequencer` calls `engine.reset()`, which FCUs to
        // genesis, sets `finalized_head = genesis ≠ default`, and sets
        // `el_sync_finished = true`. The first `drain()` then fires
        // `mark_el_sync_complete_and_notify_derivation_actor()`, which sees
        // `finalized_head ≠ default`, skips the inner reset, and sends exactly
        // one `ProcessEngineSyncCompletionRequest` to the derivation actor.
        let (unsafe_head_tx, _) = watch::channel(L2BlockInfo::default());

        let engine_obj =
            Engine::new(engine_state, engine_state_tx, engine_queue_length_tx);
        let engine_processor = EngineProcessor::new(
            Arc::clone(&http_client) as Arc<ProdEngineClient>,
            Arc::clone(&config),
            QueuedEngineDerivationClient::new(derivation_actor_tx.clone()),
            engine_obj,
            Some(unsafe_head_tx),
            None,  // no conductor
            false, // sequencer_stopped irrelevant for derivation mode
        );
        let engine_rpc_processor = EngineRpcProcessor::new(
            Arc::clone(&http_client) as Arc<ProdEngineClient>,
            Arc::clone(&config),
            engine_state_rx,
            engine_queue_length_rx,
        );
        let engine_actor = EngineActor::new(
            cancel.clone(),
            engine_actor_rx,
            engine_processor,
            engine_rpc_processor,
        );

        let derivation_actor = DerivationActor::new(
            QueuedDerivationEngineClient { engine_actor_request_tx: engine_actor_tx.clone() },
            cancel.clone(),
            derivation_actor_rx,
            pipeline,
            Arc::clone(&safe_db) as Arc<dyn base_consensus_safedb::SafeHeadListener>,
        );

        let engine_handle = tokio::spawn(async move {
            let _ = engine_actor.start(()).await;
        });
        let derivation_handle = tokio::spawn(async move {
            let _ = derivation_actor.start(()).await;
        });

        Self {
            engine,
            safe_db,
            rollup_config: Arc::clone(&config),
            gossip_tx,
            derivation_actor_tx,
            cancel,
            _network_handle: network_handle,
            _derivation_handle: derivation_handle,
            _engine_handle: engine_handle,
            _network_inbound: network_inbound,
            _engine_server: engine_server,
            _l1_server: l1_server,
            _safedb_dir: safedb_dir,
        }
    }

    /// Initialize the derivation actor.
    ///
    /// Yields control enough times for the [`EngineProcessor`] bootstrap to
    /// complete. The bootstrap path (`bootstrap_active_sequencer` at genesis)
    /// calls `engine.reset()`, which FCUs to genesis, sets
    /// `el_sync_finished = true`, and — on the first `drain()` — sends a
    /// single [`ProcessEngineSyncCompletionRequest`] to the derivation actor,
    /// transitioning it from `AwaitingELSyncCompletion` to `Deriving`.
    ///
    /// The pipeline must already be activated (via [`ActivationSignal`] applied
    /// directly to the pipeline) before this is called — that is handled by
    /// [`ActionTestHarness::create_actor_derivation_node`].
    ///
    /// Must be called once before any [`act_l1_head_signal`] calls.
    ///
    /// [`act_l1_head_signal`]: Self::act_l1_head_signal
    /// [`ProcessEngineSyncCompletionRequest`]: DerivationActorRequest::ProcessEngineSyncCompletionRequest
    /// [`ActivationSignal`]: base_consensus_derive::ActivationSignal
    pub async fn initialize(&self) {
        for _ in 0..500 {
            tokio::task::yield_now().await;
        }
    }

    /// Signal the derivation actor that a new L1 head block is available.
    ///
    /// Sends [`ProcessL1HeadUpdateRequest`] to the derivation actor's inbox.
    /// The actor transitions from `AwaitingL1Data` to `Deriving` and calls
    /// `attempt_derivation`, which steps the pipeline until EOF or an error.
    ///
    /// [`ProcessL1HeadUpdateRequest`]: DerivationActorRequest::ProcessL1HeadUpdateRequest
    pub async fn act_l1_head_signal(&self, l1_block: BlockInfo) {
        self.derivation_actor_tx
            .send(DerivationActorRequest::ProcessL1HeadUpdateRequest(Box::new(l1_block)))
            .await
            .expect("TestActorDerivationNode: L1 head signal channel closed");
    }

    /// Signal the derivation actor that an L1 block has been finalized.
    ///
    /// Sends [`ProcessFinalizedL1Block`] to the actor's inbox. The
    /// [`L2Finalizer`] scans its pending queue for L2 blocks whose L1 origin
    /// is at or below this block and promotes the highest matching L2 block to
    /// finalized.
    ///
    /// [`ProcessFinalizedL1Block`]: DerivationActorRequest::ProcessFinalizedL1Block
    /// [`L2Finalizer`]: base_consensus_node::L2Finalizer
    pub async fn act_l1_finalized_signal(&self, l1_block: BlockInfo) {
        self.derivation_actor_tx
            .send(DerivationActorRequest::ProcessFinalizedL1Block(Box::new(l1_block)))
            .await
            .expect("TestActorDerivationNode: finalized signal channel closed");
    }

    /// Yield many times to allow the actor stack to process pending messages.
    ///
    /// Unlike [`crate::TestActorFollowNode::tick`], this method does not
    /// advance tokio's wall clock, because `DerivationActor` has no timer
    /// loop — it is purely event-driven.
    pub async fn tick(&self) {
        for _ in 0..500 {
            tokio::task::yield_now().await;
        }
    }

    /// Send an L1 head signal and tick until the engine's safe head reaches
    /// `target` block number.
    ///
    /// After the target is reached, one additional [`tick`] is performed to
    /// allow the [`DerivationActor`] to process the final
    /// [`ProcessEngineSafeHeadUpdateRequest`] confirmation and write the entry
    /// to the [`SafeDB`]. Without this extra tick, callers that query the
    /// [`SafeDB`] immediately after this call may observe the previous entry
    /// because the actor runs asynchronously from the engine.
    ///
    /// Panics if `target` is not reached within 200 ticks.
    ///
    /// [`tick`]: Self::tick
    /// [`SafeDB`]: base_consensus_safedb::SafeDB
    /// [`ProcessEngineSafeHeadUpdateRequest`]: DerivationActorRequest::ProcessEngineSafeHeadUpdateRequest
    pub async fn sync_until_safe(&self, target: u64, l1_tip: BlockInfo) {
        self.act_l1_head_signal(l1_tip).await;
        for _ in 0..200 {
            let safe = self.engine.safe_head().block_info.number;
            if safe >= target {
                self.tick().await;
                self.tick().await;
                self.tick().await;
                return;
            }
            self.tick().await;
        }
        panic!(
            "sync_until_safe({target}): stuck at safe block {}",
            self.engine.safe_head().block_info.number
        );
    }

    /// Query the safe head recorded for a given L1 block number from the
    /// persistent [`SafeDB`].
    ///
    /// Returns the most recent safe head entry whose L1 block number is ≤
    /// `l1_block_num`. Pass `u64::MAX` to retrieve the latest entry regardless
    /// of L1 block height.
    pub async fn safe_head_at_l1(
        &self,
        l1_block_num: u64,
    ) -> Result<SafeHeadResponse, SafeDBError> {
        self.safe_db.safe_head_at_l1(l1_block_num).await
    }

    /// Cancel the actors, allowing their tasks to terminate.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

impl Drop for TestActorDerivationNode {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
