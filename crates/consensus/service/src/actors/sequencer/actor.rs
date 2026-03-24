//! The [`SequencerActor`].

use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use base_consensus_derive::AttributesBuilder;
use base_consensus_genesis::RollupConfig;
use tokio::{select, sync::mpsc};
use tokio_util::sync::{CancellationToken, WaitForCancellationFuture};

use crate::{
    CancellableContext, NodeActor, SequencerAdminQuery, UnsafePayloadGossipClient,
    actors::{
        SequencerEngineClient,
        engine::EngineClientError,
        sequencer::{
            build::{PayloadBuilder, UnsealedPayloadHandle},
            conductor::Conductor,
            error::SequencerActorError,
            metrics::{
                inc_seal_error, update_seal_duration_metrics, update_total_transactions_sequenced,
            },
            origin_selector::OriginSelector,
            recovery::RecoveryModeGuard,
        },
    },
};

/// Sealing duration constant, matching op-node's default `SealingDuration`.
/// Replaces the measured `last_seal_duration` heuristic.
const SEALING_DURATION: Duration = Duration::from_millis(50);

/// Maximum number of blocks the unsafe head may lead the safe head before sequencing pauses.
const MAX_SAFE_LAG: u64 = 1800;

/// The [`SequencerActor`] is responsible for building L2 blocks on top of the current unsafe head
/// and scheduling them to be signed and gossipped by the P2P layer, extending the L2 chain with new
/// blocks.
#[derive(Debug)]
pub struct SequencerActor<
    AttributesBuilder_,
    Conductor_,
    OriginSelector_,
    SequencerEngineClient_,
    UnsafePayloadGossipClient_,
> where
    AttributesBuilder_: AttributesBuilder,
    Conductor_: Conductor,
    OriginSelector_: OriginSelector,
    SequencerEngineClient_: SequencerEngineClient,
    UnsafePayloadGossipClient_: UnsafePayloadGossipClient,
{
    /// Receiver for admin API requests.
    pub admin_api_rx: mpsc::Receiver<SequencerAdminQuery>,
    /// Drives L1 origin selection, attribute preparation, and block build initiation.
    pub builder: PayloadBuilder<AttributesBuilder_, OriginSelector_, SequencerEngineClient_>,
    /// The cancellation token, shared between all tasks.
    pub cancellation_token: CancellationToken,
    /// The optional conductor RPC client.
    pub conductor: Option<Conductor_>,
    /// The struct used to interact with the engine.
    pub engine_client: Arc<SequencerEngineClient_>,
    /// Whether the sequencer is active.
    pub is_active: bool,
    /// Shared recovery mode flag.
    pub recovery_mode: RecoveryModeGuard,
    /// The rollup configuration.
    pub rollup_config: Arc<RollupConfig>,
    /// A client to asynchronously sign and gossip built payloads to the network actor.
    pub unsafe_payload_gossip_client: UnsafePayloadGossipClient_,
}

impl<
    AttributesBuilder_,
    Conductor_,
    OriginSelector_,
    SequencerEngineClient_,
    UnsafePayloadGossipClient_,
>
    SequencerActor<
        AttributesBuilder_,
        Conductor_,
        OriginSelector_,
        SequencerEngineClient_,
        UnsafePayloadGossipClient_,
    >
where
    AttributesBuilder_: AttributesBuilder,
    Conductor_: Conductor,
    OriginSelector_: OriginSelector,
    SequencerEngineClient_: SequencerEngineClient,
    UnsafePayloadGossipClient_: UnsafePayloadGossipClient,
{
    /// Runs one complete block cycle: seal (if pre-built) → conductor commit → gossip → insert →
    /// build next.
    ///
    /// Returns the duration until the next tick should fire.
    pub(crate) async fn run_block_pipeline(
        &mut self,
        next_payload: &mut Option<UnsealedPayloadHandle>,
    ) -> Result<Duration, SequencerActorError> {
        // PHASE 1: SEAL the pre-built payload (if any).
        if let Some(handle) = next_payload.take() {
            // Stale detection: if the head has moved (advanced OR rewound), discard the stale
            // build.
            let current_head = self.engine_client.get_unsafe_head().await?;
            if current_head.block_info.number
                != handle.attributes_with_parent.parent().block_info.number
            {
                warn!(
                    target: "sequencer",
                    current_head = current_head.block_info.number,
                    build_parent = handle.attributes_with_parent.parent().block_info.number,
                    "Head moved since build started, discarding stale payload"
                );
                // Fall through to build a fresh payload below.
            } else {
                // Seal: get the completed payload from the engine.
                let seal_start = Instant::now();
                let envelope = match self
                    .engine_client
                    .get_sealed_payload(handle.payload_id, handle.attributes_with_parent.clone())
                    .await
                {
                    Ok(env) => env,
                    Err(EngineClientError::SealError(err)) => {
                        if err.is_fatal() {
                            error!(target: "sequencer", error = ?err, "Fatal seal error");
                            inc_seal_error(true);
                            self.cancellation_token.cancel();
                            return Err(EngineClientError::SealError(err).into());
                        }
                        warn!(target: "sequencer", error = ?err, "Non-fatal seal error, dropping block");
                        inc_seal_error(false);
                        // Fall through to build a fresh payload below.
                        *next_payload = self.builder.build().await?;
                        return Ok(Self::schedule_next_tick(next_payload, &self.rollup_config));
                    }
                    Err(other) => return Err(other.into()),
                };

                update_seal_duration_metrics(seal_start.elapsed());
                update_total_transactions_sequenced(
                    handle.attributes_with_parent.count_transactions(),
                );

                // Conductor commit (blocking, up to 30s). Non-fatal on failure (1s backoff via
                // caller).
                if let Some(conductor) = &self.conductor {
                    conductor.commit_unsafe_payload(&envelope).await.map_err(|e| {
                        warn!(target: "sequencer", error = %e, "Conductor commit failed, will retry");
                        SequencerActorError::ConductorCommitFailed(e)
                    })?;
                }

                // Gossip: best-effort P2P broadcast. Failure is non-fatal; continue to insert.
                if let Err(e) = self
                    .unsafe_payload_gossip_client
                    .schedule_execution_payload_gossip(envelope.clone())
                    .await
                {
                    warn!(target: "sequencer", error = %e, "Gossip failed, continuing to insert");
                }

                // Insert and wait for the unsafe head watch channel to confirm.
                let expected_hash = envelope.execution_payload.block_hash();
                self.engine_client.insert_and_await_head(envelope, expected_hash).await?;
            }
        }

        // Safe-lag guard: pause sequencing if unsafe head is too far ahead of safe head.
        let unsafe_num = self.engine_client.get_unsafe_head().await?.block_info.number;
        let safe_num = self.engine_client.get_safe_head().await?.block_info.number;
        if unsafe_num.saturating_sub(safe_num) > MAX_SAFE_LAG {
            warn!(
                target: "sequencer",
                unsafe_head = unsafe_num,
                safe_head = safe_num,
                max_safe_lag = MAX_SAFE_LAG,
                "Unsafe head too far ahead of safe head, pausing sequencing"
            );
            *next_payload = None;
            return Ok(Duration::from_secs(1));
        }

        // PHASE 2: BUILD the next payload.
        *next_payload = self.builder.build().await?;

        // PHASE 3: SCHEDULE the next tick.
        Ok(Self::schedule_next_tick(next_payload, &self.rollup_config))
    }

    /// Computes the delay until the next build tick should fire.
    pub(crate) fn schedule_next_tick(
        next_payload: &Option<UnsealedPayloadHandle>,
        rollup_config: &RollupConfig,
    ) -> Duration {
        next_payload.as_ref().map_or_else(
            || Duration::from_millis(100),
            |payload| {
                let block_ts = payload
                    .attributes_with_parent
                    .attributes()
                    .payload_attributes
                    .timestamp;
                let seal_time = UNIX_EPOCH + Duration::from_secs(block_ts) - SEALING_DURATION;
                seal_time.duration_since(SystemTime::now()).map_or(Duration::ZERO, |d| d)
            },
        )
    }

    /// Schedules the initial engine reset request and waits for the unsafe head to be updated.
    async fn schedule_initial_reset(&self) -> Result<(), SequencerActorError> {
        // Reset the engine, in order to initialize the engine state.
        // NB: this call waits for confirmation that the reset succeeded and we can proceed with
        // post-reset logic.
        self.engine_client.reset_engine_forkchoice().await.map_err(|err| {
            error!(target: "sequencer", ?err, "Failed to send reset request to engine");
            err.into()
        })
    }
}

#[async_trait]
impl<
    AttributesBuilder_,
    Conductor_,
    OriginSelector_,
    SequencerEngineClient_,
    UnsafePayloadGossipClient_,
> NodeActor
    for SequencerActor<
        AttributesBuilder_,
        Conductor_,
        OriginSelector_,
        SequencerEngineClient_,
        UnsafePayloadGossipClient_,
    >
where
    AttributesBuilder_: AttributesBuilder + Sync + 'static,
    Conductor_: Conductor + Sync + 'static,
    OriginSelector_: OriginSelector + Sync + 'static,
    SequencerEngineClient_: SequencerEngineClient + Sync + 'static,
    UnsafePayloadGossipClient_: UnsafePayloadGossipClient + Sync + 'static,
{
    type Error = SequencerActorError;
    type StartData = ();

    async fn start(mut self, _: Self::StartData) -> Result<(), Self::Error> {
        let mut build_ticker =
            tokio::time::interval(Duration::from_secs(self.rollup_config.block_time));

        self.update_metrics();

        // Reset the engine state prior to beginning block building.
        self.schedule_initial_reset().await?;

        let mut next_payload: Option<UnsealedPayloadHandle> = None;
        loop {
            select! {
                biased;
                _ = self.cancellation_token.cancelled() => {
                    info!(target: "sequencer", "Received shutdown signal. Exiting sequencer task.");
                    return Ok(());
                }
                Some(query) = self.admin_api_rx.recv() => {
                    let active_before = self.is_active;
                    self.handle_admin_query(&mut next_payload, query).await;
                    if !active_before && self.is_active {
                        build_ticker.reset_immediately();
                    }
                }
                _ = build_ticker.tick(), if self.is_active => {
                    // Inner select! ensures shutdown/cancellation remains responsive
                    // during the (potentially long) conductor commit inside run_block_pipeline.
                    let cancel = self.cancellation_token.clone();
                    let result = tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Ok(()),
                        r = self.run_block_pipeline(&mut next_payload) => r,
                    };
                    match result {
                        Ok(next_tick) => build_ticker.reset_after(next_tick),
                        Err(e) if e.is_fatal() => {
                            self.cancellation_token.cancel();
                            return Err(e);
                        }
                        Err(e) => {
                            warn!(target: "sequencer", error = ?e, "Non-fatal pipeline error, backing off");
                            build_ticker.reset_after(Duration::from_secs(1));
                        }
                    }
                }
            }
        }
    }
}

impl<
    AttributesBuilder_,
    Conductor_,
    OriginSelector_,
    SequencerEngineClient_,
    UnsafePayloadGossipClient_,
> CancellableContext
    for SequencerActor<
        AttributesBuilder_,
        Conductor_,
        OriginSelector_,
        SequencerEngineClient_,
        UnsafePayloadGossipClient_,
    >
where
    AttributesBuilder_: AttributesBuilder,
    Conductor_: Conductor,
    OriginSelector_: OriginSelector,
    SequencerEngineClient_: SequencerEngineClient,
    UnsafePayloadGossipClient_: UnsafePayloadGossipClient,
{
    fn cancelled(&self) -> WaitForCancellationFuture<'_> {
        self.cancellation_token.cancelled()
    }
}
