//! Contains the [`RollupNode`] implementation.
use std::{ops::Not as _, path::PathBuf, sync::Arc, time::Duration};

use alloy_eips::BlockNumberOrTag;
use alloy_genesis::ChainConfig;
use alloy_provider::RootProvider;
use base_common_chains::BaseChainConfig;
use base_common_network::Base;
use base_consensus_derive::{Pipeline, ResetSignal, Signal, SignalReceiver, StatefulAttributesBuilder};
use base_consensus_engine::{BootstrapRole, EngineClient, EngineEvent, EngineHandle, EngineQueries};
use base_consensus_genesis::RollupConfig;
use base_consensus_providers::{
    AlloyChainProvider, AlloyL2ChainProvider, OnlineBeaconClient, OnlineBlobProvider,
    OnlinePipeline,
};
use base_consensus_rpc::RpcBuilder;
use base_consensus_safedb::{DisabledSafeDB, SafeDB, SafeDBReader, SafeHeadListener};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

type EngineComponents<E> = (
    EngineHandle<E>,
    mpsc::UnboundedReceiver<base_consensus_engine::EngineEvent>,
    mpsc::Sender<EngineQueries>,
    mpsc::Receiver<EngineQueries>,
    EngineRpcProcessor<E>,
);

use crate::{
    AlloyL1BlockFetcher, Conductor, ConductorClient, DelayedL1OriginSelectorProvider,
    DelegateDerivationActor, DerivationActor, DerivationDelegateClient, DerivationError,
    EngineConfig, EngineRpcProcessor, EngineRpcRequestReceiver, L1OriginSelector, L1WatcherActor,
    NetworkActor, NetworkBuilder, NetworkConfig, NodeActor, NodeMode, PayloadBuilder,
    QueuedEngineRpcClient, QueuedL1WatcherDerivationClient, QueuedSequencerAdminAPIClient,
    RecoveryModeGuard, RpcActor, RpcContext, SequencerActor, SequencerConfig,
    actors::{BlockStream, NetworkInboundData, QueuedUnsafePayloadGossipClient},
};

const DERIVATION_PROVIDER_CACHE_SIZE: usize = 1024;
/// Poll interval in seconds for the head block stream.
pub const HEAD_STREAM_POLL_INTERVAL: u64 = 4;

/// The configuration for the L1 chain.
#[derive(Debug, Clone)]
pub struct L1Config {
    /// The L1 chain configuration.
    pub chain_config: Arc<ChainConfig>,
    /// Whether to trust the L1 RPC.
    pub trust_rpc: bool,
    /// The L1 beacon client.
    pub beacon_client: OnlineBeaconClient,
    /// The L1 engine provider.
    pub engine_provider: RootProvider,
    /// How frequently to poll L1 for a new finalized block.
    ///
    /// The right value depends on the L1 finality cadence:
    /// - Ethereum mainnet/Sepolia: one epoch (~384 s = 32 slots × 12 s)
    /// - Devnet local L1: near-instant finality, poll aggressively (~2 s)
    pub finalized_poll_interval: Duration,
}

impl L1Config {
    /// Returns the recommended finalized-block poll interval for the given L1 chain.
    pub const fn default_finalized_poll_interval(l1_chain_id: u64) -> Duration {
        const ETH_MAINNET_L1: u64 = BaseChainConfig::mainnet().l1_chain_id;
        const ETH_SEPOLIA_L1: u64 = BaseChainConfig::sepolia().l1_chain_id;
        const DEVNET_L1: u64 = BaseChainConfig::devnet().l1_chain_id;

        match l1_chain_id {
            // Ethereum mainnet and Sepolia: poll once per L1 epoch (32 slots × 12 s).
            ETH_MAINNET_L1 | ETH_SEPOLIA_L1 => Duration::from_secs(384),
            // Devnet local L1: near-instant finality, poll aggressively.
            DEVNET_L1 => Duration::from_secs(2),
            // Unknown chains: fall back to a conservative default.
            _ => Duration::from_secs(60),
        }
    }
}

/// The standard implementation of the [`RollupNode`] service, using the governance approved Base
/// configuration of components.
#[derive(Debug)]
pub struct RollupNode {
    /// The rollup configuration.
    pub config: Arc<RollupConfig>,
    /// The L1 configuration.
    pub l1_config: L1Config,
    /// The L2 EL provider.
    pub l2_provider: RootProvider<Base>,
    /// Whether to trust the L2 RPC.
    pub l2_trust_rpc: bool,
    /// The [`EngineConfig`] for the node.
    pub engine_config: EngineConfig,
    /// The [`RpcBuilder`] for the node.
    pub rpc_builder: Option<RpcBuilder>,
    /// The P2P [`NetworkConfig`] for the node.
    pub p2p_config: NetworkConfig,
    /// The [`SequencerConfig`] for the node.
    pub sequencer_config: SequencerConfig,
    /// Optional derivation delegate provider.
    pub derivation_delegate_provider: Option<DerivationDelegateClient>,
    /// Optional path to the safe head database.
    ///
    /// When set, the node records L1→L2 safe head mappings to a persistent redb database and
    /// serves them via the `optimism_safeHeadAtL1Block` RPC endpoint. When `None`, safe head
    /// tracking is disabled and that RPC method returns an error.
    ///
    /// If the path is set but the database cannot be opened (e.g., bad permissions, disk
    /// error, or corrupted file), the node **fails to start** with an error.
    pub safedb_path: Option<PathBuf>,
}

/// A RollupNode-level derivation actor wrapper.
///
/// This type selects the concrete derivation actor implementation
/// based on `RollupNode` configuration. It is generic over the pipeline
/// type `P` to support both the online production pipeline and any
/// pre-built pipeline (e.g., an in-memory test pipeline).
///
/// It is not intended to be generic or reusable outside the
/// `RollupNode` wiring logic.
enum ConfiguredDerivationActor<E: EngineClient + std::fmt::Debug + 'static, P>
where
    P: Pipeline + SignalReceiver + Send + Sync + 'static,
{
    Delegate(Box<DelegateDerivationActor<EngineHandle<E>>>),
    Normal(Box<DerivationActor<EngineHandle<E>, P>>),
}

#[async_trait::async_trait]
impl<E, P> NodeActor for ConfiguredDerivationActor<E, P>
where
    E: EngineClient + std::fmt::Debug + 'static,
    P: Pipeline + SignalReceiver + Send + Sync + 'static,
    DelegateDerivationActor<EngineHandle<E>>: NodeActor<StartData = (), Error = DerivationError>,
    DerivationActor<EngineHandle<E>, P>: NodeActor<StartData = (), Error = DerivationError>,
{
    type StartData = ();
    type Error = DerivationError;

    async fn start(self, ctx: ()) -> Result<(), Self::Error> {
        match self {
            Self::Delegate(a) => a.start(ctx).await,
            Self::Normal(a) => a.start(ctx).await,
        }
    }
}

impl RollupNode {
    /// The mode of operation for the node.
    const fn mode(&self) -> NodeMode {
        self.engine_config.mode
    }

    /// Creates a network builder for the node.
    fn network_builder(&self) -> NetworkBuilder {
        NetworkBuilder::from(self.p2p_config.clone())
    }

    /// Returns an engine builder for the node.
    fn engine_config(&self) -> EngineConfig {
        self.engine_config.clone()
    }

    /// Returns an rpc builder for the node.
    fn rpc_builder(&self) -> Option<RpcBuilder> {
        self.rpc_builder.clone()
    }

    /// Returns the sequencer builder for the node.
    fn create_attributes_builder(
        &self,
    ) -> StatefulAttributesBuilder<AlloyChainProvider, AlloyL2ChainProvider> {
        let l1_derivation_provider = AlloyChainProvider::new_with_trust(
            self.l1_config.engine_provider.clone(),
            DERIVATION_PROVIDER_CACHE_SIZE,
            self.l1_config.trust_rpc,
        );
        let l2_derivation_provider = AlloyL2ChainProvider::new_with_trust(
            self.l2_provider.clone(),
            Arc::clone(&self.config),
            DERIVATION_PROVIDER_CACHE_SIZE,
            self.l2_trust_rpc,
        );

        StatefulAttributesBuilder::new(
            Arc::clone(&self.config),
            Arc::clone(&self.l1_config.chain_config),
            l2_derivation_provider,
            l1_derivation_provider,
        )
    }

    async fn create_pipeline(&self) -> OnlinePipeline {
        // Create the caching L1/L2 EL providers for derivation.
        let l1_derivation_provider = AlloyChainProvider::new_with_trust(
            self.l1_config.engine_provider.clone(),
            DERIVATION_PROVIDER_CACHE_SIZE,
            self.l1_config.trust_rpc,
        );
        let l2_derivation_provider = AlloyL2ChainProvider::new_with_trust(
            self.l2_provider.clone(),
            Arc::clone(&self.config),
            DERIVATION_PROVIDER_CACHE_SIZE,
            self.l2_trust_rpc,
        );

        OnlinePipeline::new_polled(
            Arc::clone(&self.config),
            Arc::clone(&self.l1_config.chain_config),
            OnlineBlobProvider::init(self.l1_config.beacon_client.clone()).await,
            l1_derivation_provider,
            l2_derivation_provider,
        )
    }

    /// Creates the [`EngineHandle`] and RPC processor.
    ///
    /// The handle is distributed to all actors. The RPC processor runs independently,
    /// receiving queries through its own channel.
    fn create_engine<E: EngineClient + std::fmt::Debug + 'static>(
        &self,
        engine_client: Arc<E>,
    ) -> EngineComponents<E> {
        let (engine_handle, engine_events_rx) =
            EngineHandle::new(Arc::clone(&engine_client), Arc::clone(&self.config));

        let (rpc_query_tx, rpc_query_rx) = mpsc::channel(1024);
        let (_, queue_length_rx) = watch::channel(0usize);
        let engine_rpc_processor = EngineRpcProcessor::new(
            engine_client,
            Arc::clone(&self.config),
            engine_handle.subscribe(),
            queue_length_rx,
        );

        (engine_handle, engine_events_rx, rpc_query_tx, rpc_query_rx, engine_rpc_processor)
    }

    /// Starts the rollup node service.
    ///
    /// The rollup node, in validator mode, listens to two sources of information to sync the L2
    /// chain:
    ///
    /// 1. The data availability layer, with a watcher that listens for new updates. L2 inputs (L2
    ///    transaction batches + deposits) are then derived from the DA layer.
    /// 2. The L2 sequencer, which produces unsafe L2 blocks and sends them to the network over p2p
    ///    gossip.
    ///
    /// From these two sources, the node imports `unsafe` blocks from the L2 sequencer, `safe`
    /// blocks from the L2 derivation pipeline into the L2 execution layer via the Engine API,
    /// and finalizes `safe` blocks that it has derived when L1 finalized block updates are
    /// received.
    ///
    /// In sequencer mode, the node is responsible for producing unsafe L2 blocks and sending them
    /// to the network over p2p gossip. The node also listens for L1 finalized block updates and
    /// finalizes `safe` blocks that it has derived when L1 finalized block updates are
    /// received.
    pub async fn start(&self) -> Result<(), String> {
        let pipeline = self.create_pipeline().await;
        let engine_client = Arc::new(self.engine_config().build_engine_client());
        self.start_inner(engine_client, pipeline).await
    }

    /// Starts the rollup node service with a pre-built derivation pipeline.
    ///
    /// This is the underlying implementation of [`Self::start`]. It accepts any pipeline
    /// implementing [`Pipeline`] and [`SignalReceiver`], enabling callers to substitute
    /// in-memory or test pipelines without modifying `RollupNode` itself.
    ///
    /// Production callers should use [`Self::start`], which constructs the standard
    /// [`OnlinePipeline`] automatically.
    pub async fn start_with<P>(&self, pipeline: P) -> Result<(), String>
    where
        P: Pipeline + SignalReceiver + Send + Sync + 'static,
    {
        let engine_client = Arc::new(self.engine_config().build_engine_client());
        self.start_inner(engine_client, pipeline).await
    }

    /// Starts the rollup node with a pre-built engine client.
    ///
    /// This method enables dependency injection of the engine client, useful for testing
    /// scenarios where a mock or in-memory engine client should be used instead of
    /// connecting to a live L2 Engine API.
    pub async fn start_with_engine_client<E: EngineClient + std::fmt::Debug + 'static>(
        &self,
        engine_client: Arc<E>,
    ) -> Result<(), String> {
        let pipeline = self.create_pipeline().await;
        self.start_inner(engine_client, pipeline).await
    }

    async fn start_inner<E, P>(&self, engine_client: Arc<E>, pipeline: P) -> Result<(), String>
    where
        E: EngineClient + std::fmt::Debug + 'static,
        P: Pipeline + SignalReceiver + Send + Sync + 'static,
    {
        let cancellation = CancellationToken::new();

        // Build the safe head DB pair.
        let (safe_head_listener, safe_db_reader): (
            Arc<dyn SafeHeadListener>,
            Arc<dyn SafeDBReader>,
        ) = if self.derivation_delegate_provider.is_none() {
            if let Some(path) = &self.safedb_path {
                let db = Arc::new(
                    SafeDB::open(path)
                        .map_err(|e| format!("failed to open safe head database: {e}"))?,
                );
                (Arc::clone(&db) as Arc<dyn SafeHeadListener>, db as Arc<dyn SafeDBReader>)
            } else {
                let db = Arc::new(DisabledSafeDB);
                (Arc::clone(&db) as Arc<dyn SafeHeadListener>, db as Arc<dyn SafeDBReader>)
            }
        } else {
            let db = Arc::new(DisabledSafeDB);
            (Arc::clone(&db) as Arc<dyn SafeHeadListener>, db as Arc<dyn SafeDBReader>)
        };

        let (derivation_actor_request_tx, derivation_actor_request_rx) = mpsc::channel(1024);

        // Create the EngineHandle.
        let (engine_handle, engine_events_rx, rpc_query_tx, rpc_query_rx, engine_rpc_processor) =
            self.create_engine(Arc::clone(&engine_client));

        // Bridge engine events to the derivation actor's request channel.
        {
            let deriv_tx = derivation_actor_request_tx.clone();
            let cancel = cancellation.clone();
            tokio::spawn(async move {
                let mut rx = engine_events_rx;
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        event = rx.recv() => {
                            let Some(event) = event else { break };
                            let req = match event {
                                EngineEvent::Reset { safe_head } => {
                                    info!(target: "engine", safe_head = safe_head.block_info.number, "Engine reset, signaling derivation");
                                    crate::DerivationActorRequest::ProcessEngineSignalRequest(
                                        Box::new(ResetSignal { l2_safe_head: safe_head }.signal()),
                                    )
                                }
                                EngineEvent::Flush => {
                                    info!(target: "engine", "Engine flush, signaling derivation");
                                    crate::DerivationActorRequest::ProcessEngineSignalRequest(
                                        Box::new(Signal::FlushChannel),
                                    )
                                }
                                EngineEvent::SyncCompleted { safe_head } => {
                                    info!(target: "engine", safe_head = safe_head.block_info.number, "EL sync completed");
                                    crate::DerivationActorRequest::ProcessEngineSyncCompletionRequest(
                                        Box::new(safe_head),
                                    )
                                }
                                EngineEvent::SafeHeadUpdated { safe_head } => {
                                    crate::DerivationActorRequest::ProcessEngineSafeHeadUpdateRequest(
                                        Box::new(safe_head),
                                    )
                                }
                            };
                            if deriv_tx.send(req).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }

        // Create the conductor client.
        let conductor: Option<ConductorClient> = self
            .sequencer_config
            .conductor_rpc_url
            .clone()
            .map(ConductorClient::new_http)
            .transpose()
            .map_err(|e| format!("Failed to create conductor client: {e}"))?;

        // Bootstrap the engine.
        let bootstrap_role = if !self.mode().is_sequencer() {
            BootstrapRole::Validator
        } else if self.sequencer_config.sequencer_stopped {
            BootstrapRole::ConductorFollower
        } else {
            // Check conductor leadership if configured.
            match &conductor {
                None => BootstrapRole::ActiveSequencer,
                Some(c) => match c.leader().await {
                    Ok(true) => BootstrapRole::ActiveSequencer,
                    _ => BootstrapRole::ConductorFollower,
                },
            }
        };
        engine_handle
            .bootstrap(bootstrap_role)
            .await
            .map_err(|e| format!("Engine bootstrap failed: {e}"))?;

        // Select the concrete derivation actor implementation.
        // The EngineHandle implements DerivationEngineClient, so it can be used directly.
        let derivation: ConfiguredDerivationActor<E, P> =
            if let Some(provider) = self.derivation_delegate_provider.clone() {
                let l1_provider = AlloyChainProvider::new(
                    self.l1_config.engine_provider.clone(),
                    DERIVATION_PROVIDER_CACHE_SIZE,
                );
                ConfiguredDerivationActor::Delegate(Box::new(DelegateDerivationActor::<_>::new(
                    engine_handle.clone(),
                    cancellation.clone(),
                    derivation_actor_request_rx,
                    provider,
                    l1_provider,
                )))
            } else {
                ConfiguredDerivationActor::Normal(Box::new(DerivationActor::<_, P>::new(
                    engine_handle.clone(),
                    cancellation.clone(),
                    derivation_actor_request_rx,
                    pipeline,
                    safe_head_listener,
                )))
            };

        // Create the p2p actor. EngineHandle implements NetworkEngineClient.
        let (
            NetworkInboundData {
                signer,
                p2p_rpc: network_rpc,
                gossip_payload_tx,
                admin_rpc: net_admin_rpc,
            },
            network,
        ) = NetworkActor::new(engine_handle.clone(), cancellation.clone(), self.network_builder())
            .await
            .map_err(|e| format!("Failed to start network actor: {e}"))?;

        let (l1_head_updates_tx, l1_head_updates_rx) = watch::channel(None);
        let delayed_l1_provider = DelayedL1OriginSelectorProvider::new(
            self.l1_config.engine_provider.clone(),
            l1_head_updates_rx,
            self.sequencer_config.l1_conf_delay,
        );

        let delayed_origin_selector =
            L1OriginSelector::new(Arc::clone(&self.config), delayed_l1_provider);

        // Create the L1 Watcher actor

        // A channel to send queries about the state of L1.
        let (l1_query_tx, l1_query_rx) = mpsc::channel(1024);

        let head_stream = BlockStream::new_as_stream(
            self.l1_config.engine_provider.clone(),
            BlockNumberOrTag::Latest,
            Duration::from_secs(HEAD_STREAM_POLL_INTERVAL),
        )?;
        let finalized_stream = BlockStream::new_as_stream(
            self.l1_config.engine_provider.clone(),
            BlockNumberOrTag::Finalized,
            self.l1_config.finalized_poll_interval,
        )?;

        // Create the [`L1WatcherActor`]. Previously known as the DA watcher actor.
        let l1_watcher = L1WatcherActor::new(
            Arc::clone(&self.config),
            AlloyL1BlockFetcher(self.l1_config.engine_provider.clone()),
            l1_query_rx,
            l1_head_updates_tx.clone(),
            QueuedL1WatcherDerivationClient { derivation_actor_request_tx },
            Some(signer),
            cancellation.clone(),
            head_stream,
            finalized_stream,
        );

        // Create the sequencer if needed. EngineHandle implements SequencerEngineClient.
        let (sequencer_actor, sequencer_admin_client) = if self.mode().is_sequencer() {
            let (sequencer_admin_api_tx, sequencer_admin_api_rx) = mpsc::channel(1024);
            let queued_gossip_client =
                QueuedUnsafePayloadGossipClient::new(gossip_payload_tx.clone());

            let recovery_mode =
                RecoveryModeGuard::new(self.sequencer_config.sequencer_recovery_mode);
            let seq_engine_client = Arc::new(engine_handle.clone());
            (
                Some(SequencerActor {
                    admin_api_rx: sequencer_admin_api_rx,
                    builder: PayloadBuilder {
                        attributes_builder: self.create_attributes_builder(),
                        engine_client: Arc::clone(&seq_engine_client),
                        origin_selector: delayed_origin_selector,
                        recovery_mode: recovery_mode.clone(),
                        rollup_config: Arc::clone(&self.config),
                    },
                    cancellation_token: cancellation.clone(),
                    conductor,
                    engine_client: seq_engine_client,
                    is_active: self.sequencer_config.sequencer_stopped.not(),
                    recovery_mode,
                    rollup_config: Arc::clone(&self.config),
                    unsafe_payload_gossip_client: queued_gossip_client,
                    sealer: None,
                    pending_stop: None,
                    next_build_parent: None,
                }),
                Some(QueuedSequencerAdminAPIClient::new(sequencer_admin_api_tx)),
            )
        } else {
            (None, None)
        };

        // Create the RPC server actor.
        let rpc = self.rpc_builder().map(|b| {
            RpcActor::new(
                b,
                QueuedEngineRpcClient::new(rpc_query_tx),
                sequencer_admin_client,
                safe_db_reader,
            )
        });

        // Spawn the RPC processor as a separate task.
        let _rpc_handle = engine_rpc_processor.start(rpc_query_rx);

        crate::service::spawn_and_wait!(
            cancellation,
            actors = [
                rpc.map(|r| (
                    r,
                    RpcContext {
                        cancellation: cancellation.clone(),
                        p2p_network: Some(network_rpc),
                        network_admin: Some(net_admin_rpc),
                        l1_watcher_queries: l1_query_tx,
                    }
                )),
                sequencer_actor.map(|s| (s, ())),
                Some((network, ())),
                Some((l1_watcher, ())),
                Some((derivation, ())),
            ]
        );
        Ok(())
    }
}
