//! Production-actor derivation-node test harness.

use std::{
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{B256, Signature, U256};
use alloy_provider::RootProvider;
use base_common_consensus::BaseBlock;
use base_common_rpc_types_engine::{BaseExecutionPayload, NetworkPayloadEnvelope, PayloadHash};
use base_consensus_derive::{Pipeline, SignalReceiver};
use base_consensus_engine::{BaseEngineClient, Engine, EngineClientBuilder, EngineState};
use base_consensus_genesis::RollupConfig;
use base_consensus_node::{
    AlloyL1BlockFetcher, BlockStream, DerivationActor, DerivationActorRequest, EngineActor,
    EngineActorRequest, EngineProcessor, EngineRpcProcessor, L1WatcherActor, NetworkActor,
    NetworkInboundData, NodeActor, QueuedDerivationEngineClient, QueuedEngineDerivationClient,
    QueuedL1WatcherDerivationClient, QueuedNetworkEngineClient, ResetRequest,
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
};

type ProdEngineClient = BaseEngineClient<RootProvider, RootProvider<base_common_network::Base>>;

/// An action test harness that drives the production [`DerivationActor`],
/// [`EngineActor`], and [`L1WatcherActor`] with an HTTP-backed
/// [`BaseEngineClient`] and a real derivation pipeline.
///
/// Unlike a manual pipeline-stepping harness, `TestActorDerivationNode` runs the exact
/// production actor stack end-to-end: L1 head delivery via the real
/// [`L1WatcherActor`] polling [`HarnessL1Server`], derivation-state-machine
/// gating, reorg detection, safe-head `SafeDB` writes, finalization tracking,
/// and the full `InsertTask` → `ConsolidateTask` engine flow all execute
/// through the real code paths.
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
/// 3. Call [`tick`] / [`sync_until_safe`] to advance virtual time; the real
///    [`L1WatcherActor`] polls [`HarnessL1Server`] and delivers L1 head
///    updates to the derivation actor automatically.
///
/// # Tests
///
/// Tests using this harness must use `#[tokio::test(start_paused = true)]`
/// because [`L1WatcherActor`] drives derivation via a timer-based poll loop.
/// [`tick`] advances virtual time by the poll interval and yields enough
/// times for the full actor chain to process the resulting messages.
///
/// [`initialize`]: TestActorDerivationNode::initialize
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
    ///
    /// Kept for [`act_l1_finalized_signal`] — L1 finality is not yet driven
    /// automatically by the watcher in test mode.
    ///
    /// [`act_l1_finalized_signal`]: Self::act_l1_finalized_signal
    derivation_actor_tx: mpsc::Sender<DerivationActorRequest>,
    /// Sender side of the engine actor's inbound request channel.
    ///
    /// Used by [`act_reset`] to send [`ResetRequest`] directly to the engine
    /// actor, which fully resets the [`EngineProcessor`]'s internal state and
    /// propagates the reset signal back to the derivation actor.
    ///
    /// [`act_reset`]: Self::act_reset
    engine_actor_tx: mpsc::Sender<EngineActorRequest>,
    /// Cancellation token to stop the actors on drop.
    cancel: CancellationToken,
    /// Handle to the spawned network actor task.
    _network_handle: JoinHandle<()>,
    /// Handle to the spawned derivation actor task.
    _derivation_handle: JoinHandle<()>,
    /// Handle to the spawned engine actor task.
    _engine_handle: JoinHandle<()>,
    /// Handle to the spawned L1 watcher actor task.
    _l1_watcher_handle: JoinHandle<()>,
    /// Keeps the network actor's inbound channels alive for the lifetime of the test.
    _network_inbound: NetworkInboundData,
    /// Keeps the in-process engine API HTTP server alive for the duration of the test.
    _engine_server: HarnessEngineServer,
    /// Keeps the in-process L1 JSON-RPC HTTP server alive for the duration of the test.
    _l1_server: HarnessL1Server,
    /// Temporary directory that holds the `SafeDB` files — dropped last to
    /// keep the DB alive for the duration of the test.
    _safedb_dir: tempfile::TempDir,
}

impl TestActorDerivationNode {
    /// Create a new [`TestActorDerivationNode`] wiring real production actors.
    ///
    /// Spawns an in-process Engine API HTTP server and an L1 JSON-RPC HTTP
    /// server, builds a production [`BaseEngineClient`] that talks to them,
    /// then wires a [`DerivationActor`], [`EngineActor`], and
    /// [`L1WatcherActor`] using that client.  This mirrors the wiring in
    /// [`base_consensus_node::RollupNode`] while still allowing the test to
    /// observe engine state via the [`ActionEngineClient`] held on the
    /// returned struct.
    ///
    /// Call [`initialize`] before any derivation calls.
    ///
    /// [`initialize`]: Self::initialize
    pub async fn new<P>(
        config: Arc<RollupConfig>,
        engine: ActionEngineClient,
        pipeline: P,
        _genesis_safe_head: L2BlockInfo,
    ) -> Self
    where
        P: Pipeline + SignalReceiver + std::fmt::Debug + Send + Sync + 'static,
    {
        let safedb_dir =
            tempfile::TempDir::new().expect("TestActorDerivationNode: failed to create tempdir");
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

        let engine_obj = Engine::new(engine_state, engine_state_tx, engine_queue_length_tx);
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

        // Wire L1WatcherActor so it polls HarnessL1Server and delivers real
        // L1 head updates to the derivation actor. Use a 2-second poll interval
        // to match the tick() step size.
        let (l1_head_updates_tx, _l1_head_updates_rx) = watch::channel::<Option<BlockInfo>>(None);
        let l1_provider = RootProvider::new_http(l1_server.url.clone());
        let head_stream = BlockStream::new_as_stream(
            l1_provider.clone(),
            BlockNumberOrTag::Latest,
            Duration::from_secs(2),
        )
        .expect("TestActorDerivationNode: failed to create head stream");
        let finalized_stream = BlockStream::new_as_stream(
            l1_provider.clone(),
            BlockNumberOrTag::Finalized,
            Duration::from_secs(2),
        )
        .expect("TestActorDerivationNode: failed to create finalized stream");
        let l1_watcher = L1WatcherActor::new(
            Arc::clone(&config),
            AlloyL1BlockFetcher(l1_provider),
            l1_head_updates_tx,
            QueuedL1WatcherDerivationClient {
                derivation_actor_request_tx: derivation_actor_tx.clone(),
            },
            None, // no unsafe block signer
            cancel.clone(),
            head_stream,
            finalized_stream,
            0, // no verifier_l1_confs
            Arc::new(AtomicU64::new(0)),
        );

        let engine_handle = tokio::spawn(async move {
            let _ = engine_actor.start(()).await;
        });
        let derivation_handle = tokio::spawn(async move {
            let _ = derivation_actor.start(()).await;
        });
        let l1_watcher_handle = tokio::spawn(async move {
            let _ = l1_watcher.start(()).await;
        });

        Self {
            engine,
            safe_db,
            rollup_config: Arc::clone(&config),
            gossip_tx,
            derivation_actor_tx,
            engine_actor_tx,
            cancel,
            _network_handle: network_handle,
            _derivation_handle: derivation_handle,
            _engine_handle: engine_handle,
            _l1_watcher_handle: l1_watcher_handle,
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
    /// Must be called once before any [`tick`] / [`sync_until_safe`] calls.
    ///
    /// [`tick`]: Self::tick
    /// [`ProcessEngineSyncCompletionRequest`]: DerivationActorRequest::ProcessEngineSyncCompletionRequest
    /// [`ActivationSignal`]: base_consensus_derive::ActivationSignal
    pub async fn initialize(&self) {
        for _ in 0..500 {
            tokio::task::yield_now().await;
        }
    }

    /// Register a block hash in the engine's shared registry.
    ///
    /// Passing `None` for the state root skips state-root validation for that
    /// block, which is necessary for hardfork upgrade blocks where the pipeline
    /// injects additional deposit transactions that change the EVM state root
    /// relative to what the sequencer computed.
    pub fn register_block_hash(&self, number: u64, hash: B256) {
        self.engine.block_hash_registry().insert(number, hash, None);
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
        for _ in 0..500 {
            tokio::task::yield_now().await;
        }
    }

    /// Trigger a full pipeline reset to `l2_safe_head`.
    ///
    /// Resets [`ActionEngineClient`]'s tracked state to `l2_safe_head` FIRST so
    /// that `find_starting_forkchoice` (called inside `Engine::reset()`) queries
    /// the HTTP server and sees the target block rather than the stale pre-reset
    /// heads. This causes the engine to FCU to `l2_safe_head` (genesis in the
    /// typical reorg case) instead of the old finalized/safe block.
    ///
    /// After the forkchoice reset, the engine propagates [`Signal::Reset`]
    /// one-way to the derivation actor. Yielding 500 times gives the derivation
    /// actor a chance to process the reset before this call returns.
    ///
    /// [`reset_engine_state`]: ActionEngineClient::reset_engine_state
    pub async fn act_reset(&self, l2_safe_head: base_protocol::L2BlockInfo) {
        // Reset ActionEngineClient state BEFORE sending ResetRequest so that
        // find_starting_forkchoice sees the target head, not the old finalized head.
        self.engine.reset_engine_state(l2_safe_head);

        let (result_tx, mut result_rx) = mpsc::channel(1);
        self.engine_actor_tx
            .send(EngineActorRequest::ResetRequest(Box::new(ResetRequest { result_tx })))
            .await
            .expect("TestActorDerivationNode: engine actor channel closed");
        result_rx
            .recv()
            .await
            .expect("TestActorDerivationNode: engine reset result channel closed")
            .expect("TestActorDerivationNode: engine reset failed");
        for _ in 0..500 {
            tokio::task::yield_now().await;
        }
    }

    /// Advance virtual time by the L1 watcher poll interval and yield many
    /// times to let the full actor chain process the resulting messages.
    ///
    /// Must be called from a test with `#[tokio::test(start_paused = true)]`.
    /// Each call advances the tokio clock by 2 seconds, which fires the
    /// [`L1WatcherActor`]'s poll timer, causing it to fetch the latest L1 block
    /// from [`HarnessL1Server`] and forward it to the [`DerivationActor`].
    /// The 500 `yield_now()` calls give every spawned task (watcher → derivation
    /// → engine → `InsertTask`) one scheduling turn before returning.
    pub async fn tick(&self) {
        tokio::time::advance(Duration::from_secs(2)).await;
        for _ in 0..500 {
            tokio::task::yield_now().await;
        }
    }

    /// Tick until the engine's safe head reaches `target` block number.
    ///
    /// The real [`L1WatcherActor`] delivers L1 head updates on each tick;
    /// no manual `act_l1_head_signal` call is required.
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
    pub async fn sync_until_safe(&self, target: u64) {
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

    /// Inject an unsafe gossip block into the network actor.
    ///
    /// Applies the same sequential-number gap guard that the production P2P
    /// stack enforces: if `block.number != unsafe_head + 1` the block is silently
    /// dropped and no tick is performed.
    ///
    /// For sequential blocks, converts `block` into a [`NetworkPayloadEnvelope`]
    /// and sends it via the gossip channel to the real [`NetworkActor`], which
    /// forwards it to the [`EngineActor`] as a `ProcessUnsafeL2Block` request.
    /// One [`tick`] is performed so the actor chain has time to process the payload.
    ///
    /// The gap guard is required because [`ActionEngineClient`] executes blocks
    /// against its in-process EVM: inserting a block whose parent has not yet been
    /// committed would cause a debug assert.  The real P2P layer applies an
    /// equivalent check before forwarding blocks to the engine.
    ///
    /// [`tick`]: Self::tick
    pub async fn act_l2_unsafe_gossip_receive(&self, block: &BaseBlock) {
        let unsafe_head_num = self.engine.unsafe_head().block_info.number;
        if block.header.number != unsafe_head_num + 1 {
            return;
        }
        let block_hash = block.header.hash_slow();
        let (execution_payload, _) = BaseExecutionPayload::from_block_unchecked(block_hash, block);
        let network = NetworkPayloadEnvelope {
            payload: execution_payload,
            signature: Signature::new(U256::ZERO, U256::ZERO, false),
            payload_hash: PayloadHash(B256::ZERO),
            parent_beacon_block_root: block.header.parent_beacon_block_root,
        };
        self.gossip_tx.send(network);
        // Yield without advancing virtual time so the L1WatcherActor timer does
        // not fire. Advancing time here would trigger derivation, which may race
        // with the gossip InsertTask and attempt to reconcile (build) the block
        // before the gossip block is committed — causing the EngineProcessor to
        // fail and drop subsequent gossip payloads.
        for _ in 0..1000 {
            tokio::task::yield_now().await;
        }
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
