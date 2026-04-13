use std::{sync::Arc, time::Duration};

use alloy_eips::BlockNumberOrTag;
use alloy_provider::RootProvider;
use base_common_network::Base;
use base_consensus_derive::{ResetSignal, Signal};
use base_consensus_engine::{BootstrapRole, EngineClient, EngineEvent, EngineHandle, EngineQueries};
use base_consensus_genesis::RollupConfig;
use base_consensus_rpc::RpcBuilder;
use base_consensus_safedb::{DisabledSafeDB, SafeDBReader};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::{
    AlloyL1BlockFetcher, BlockStream, DelegateL2Client, DelegateL2DerivationActor, EngineConfig,
    EngineRpcProcessor, EngineRpcRequestReceiver, L1Config, L1WatcherActor, NodeActor,
    QueuedEngineRpcClient, QueuedL1WatcherDerivationClient, RpcActor, RpcContext,
    service::node::HEAD_STREAM_POLL_INTERVAL,
};

/// A lightweight node that follows another L2 node by polling its execution
/// layer RPC and driving the local engine via `NewPayload` + FCU.
///
/// Runs only the [`EngineHandle`] and [`DelegateL2DerivationActor`] — no derivation
/// pipeline, P2P, or sequencer.
#[derive(Debug)]
pub struct FollowNode {
    config: Arc<RollupConfig>,
    engine_config: EngineConfig,
    local_l2_provider: RootProvider<Base>,
    l2_source: DelegateL2Client,
    proofs_enabled: bool,
    proofs_max_blocks_ahead: u64,
    l1_config: L1Config,
    rpc_builder: Option<RpcBuilder>,
}

impl FollowNode {
    /// Creates a new [`FollowNode`].
    pub const fn new(
        config: Arc<RollupConfig>,
        engine_config: EngineConfig,
        local_l2_provider: RootProvider<Base>,
        l2_source: DelegateL2Client,
        rpc_builder: Option<RpcBuilder>,
        l1_config: L1Config,
    ) -> Self {
        Self {
            config,
            engine_config,
            local_l2_provider,
            l2_source,
            rpc_builder,
            l1_config,
            proofs_enabled: false,
            proofs_max_blocks_ahead: 512,
        }
    }

    /// Enables proofs sync gating via `debug_proofsSyncStatus`.
    pub const fn with_proofs(mut self, enabled: bool) -> Self {
        self.proofs_enabled = enabled;
        self
    }

    /// Sets the maximum number of blocks the node may advance beyond the
    /// proofs `ExEx` head.
    pub const fn with_proofs_max_blocks_ahead(mut self, max_blocks_ahead: u64) -> Self {
        self.proofs_max_blocks_ahead = max_blocks_ahead;
        self
    }

    /// Starts the follow node.
    pub async fn start(&self) -> Result<(), String> {
        let engine_client = Arc::new(self.engine_config.clone().build_engine_client());
        self.start_inner(engine_client).await
    }

    /// Starts the follow node with a pre-built engine client.
    pub async fn start_with_engine_client<E: EngineClient + std::fmt::Debug + 'static>(
        &self,
        engine_client: Arc<E>,
    ) -> Result<(), String> {
        self.start_inner(engine_client).await
    }

    async fn start_inner<E: EngineClient + std::fmt::Debug + 'static>(
        &self,
        engine_client: Arc<E>,
    ) -> Result<(), String> {
        let cancellation = CancellationToken::new();

        let (derivation_actor_request_tx, derivation_actor_request_rx) = mpsc::channel(1024);

        // Create the EngineHandle.
        let (engine_handle, engine_events_rx) =
            EngineHandle::new(Arc::clone(&engine_client), Arc::clone(&self.config));

        // Bootstrap as a validator (follow nodes are always validators).
        engine_handle
            .bootstrap(BootstrapRole::Validator)
            .await
            .map_err(|e| format!("Engine bootstrap failed: {e}"))?;

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

        // Set up the RPC processor with its own channel.
        let (rpc_query_tx, rpc_query_rx) = mpsc::channel::<EngineQueries>(1024);
        let (_, queue_length_rx) = watch::channel(0usize);
        let engine_rpc_processor = EngineRpcProcessor::new(
            engine_client,
            Arc::clone(&self.config),
            engine_handle.subscribe(),
            queue_length_rx,
        );
        let _rpc_handle = engine_rpc_processor.start(rpc_query_rx);

        let derivation = DelegateL2DerivationActor::<_>::new(
            engine_handle.clone(),
            cancellation.clone(),
            derivation_actor_request_rx,
            self.local_l2_provider.clone(),
            self.l2_source.clone(),
        )
        .with_proofs(self.proofs_enabled)
        .with_proofs_max_blocks_ahead(self.proofs_max_blocks_ahead);

        // Create the RPC server actor if configured.
        let rpc = self.rpc_builder.clone().map(|b| {
            RpcActor::new(
                b,
                QueuedEngineRpcClient::new(rpc_query_tx),
                None::<crate::QueuedSequencerAdminAPIClient>,
                Arc::new(DisabledSafeDB) as Arc<dyn SafeDBReader>,
            )
        });

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

        let (l1_head_updates_tx, _l1_head_updates_rx) = watch::channel(None);
        let l1_watcher = L1WatcherActor::new(
            Arc::clone(&self.config),
            AlloyL1BlockFetcher(self.l1_config.engine_provider.clone()),
            l1_query_rx,
            l1_head_updates_tx,
            QueuedL1WatcherDerivationClient { derivation_actor_request_tx },
            None,
            cancellation.clone(),
            head_stream,
            finalized_stream,
        );

        crate::service::spawn_and_wait!(
            cancellation,
            actors = [
                rpc.map(|r| (
                    r,
                    RpcContext {
                        cancellation: cancellation.clone(),
                        p2p_network: None,
                        network_admin: None,
                        l1_watcher_queries: l1_query_tx,
                    }
                )),
                Some((derivation, ())),
                Some((l1_watcher, ())),
            ]
        );
        Ok(())
    }
}
