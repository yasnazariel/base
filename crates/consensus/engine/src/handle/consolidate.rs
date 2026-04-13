//! Engine state consolidation.
//!
//! Consolidation advances the safe head by verifying that existing unsafe blocks match
//! derived attributes (or delegated safe block info). When a mismatch is detected,
//! falls back to building and sealing a new block.

use std::time::Instant;

use base_protocol::L2BlockInfo;

use super::{EngineEvent, EngineHandle};
use crate::{
    ConsolidateInput, ConsolidateTaskError, EngineClient, EngineState, EngineSyncStateUpdate,
    EngineTaskError, EngineTaskErrorSeverity, Metrics,
};

impl<C: EngineClient> EngineHandle<C> {
    /// Consolidates the engine state using derived attributes or safe L2 block info.
    ///
    /// If the unsafe head is ahead of the safe head, attempts to consolidate by checking
    /// the existing block matches the input. On mismatch, falls back to build-and-seal.
    pub async fn consolidate(&self, input: ConsolidateInput) -> Result<(), ConsolidateTaskError> {
        let mut state = self.inner.state.lock().await;
        let result = self.do_consolidate(&mut state, &input).await;
        self.broadcast(&state);

        match &result {
            Ok(()) => {
                Metrics::engine_task_count(Metrics::CONSOLIDATE_TASK_LABEL).increment(1);
            }
            Err(e) => {
                let severity = e.severity();
                Metrics::engine_task_failure(Metrics::CONSOLIDATE_TASK_LABEL, severity.as_label())
                    .increment(1);

                match severity {
                    EngineTaskErrorSeverity::Reset => {
                        warn!(target: "engine", error = %e, "Consolidate triggered engine reset");
                        if let Ok(safe_head) = self.do_reset(&mut state).await {
                            self.broadcast(&state);
                            let _ = self.inner.events_tx.send(EngineEvent::Reset { safe_head });
                            return Ok(());
                        }
                    }
                    EngineTaskErrorSeverity::Flush => {
                        let _ = self.inner.events_tx.send(EngineEvent::Flush);
                    }
                    _ => {}
                }
            }
        }

        result
    }

    /// Internal consolidate logic. Does not acquire the Mutex.
    pub(super) async fn do_consolidate(
        &self,
        state: &mut EngineState,
        input: &ConsolidateInput,
    ) -> Result<(), ConsolidateTaskError> {
        let safe_head_number = match input {
            ConsolidateInput::Attributes { .. } => state.sync_state.safe_head().block_info.number,
            ConsolidateInput::BlockInfo(safe_block_info) => safe_block_info.block_info.number,
        };

        if safe_head_number < state.sync_state.unsafe_head().block_info.number {
            self.consolidate_inner(state, input).await
        } else {
            self.reconcile_unsafe_to_safe(state, input).await
        }
    }

    /// Attempts consolidation: checks if the existing unsafe block matches the input.
    async fn consolidate_inner(
        &self,
        state: &mut EngineState,
        input: &ConsolidateInput,
    ) -> Result<(), ConsolidateTaskError> {
        let global_start = Instant::now();

        let block_num = input.l2_block_number();
        let fetch_start = Instant::now();
        let block = match self.inner.client.l2_block_by_label(block_num.into()).await {
            Ok(Some(block)) => block,
            Ok(None) => {
                warn!(target: "engine", block_num, "Received `None` block");
                return Err(ConsolidateTaskError::MissingUnsafeL2Block(block_num));
            }
            Err(_) => {
                warn!(target: "engine", "Failed to fetch unsafe l2 block for consolidation");
                return Err(ConsolidateTaskError::FailedToFetchUnsafeL2Block);
            }
        };
        let block_fetch_duration = fetch_start.elapsed();
        let block_hash = block.header.hash;

        if input.is_consistent_with_block(&self.inner.config, &block) {
            trace!(
                target: "engine",
                input = ?input,
                block_hash = %block_hash,
                "Consolidating engine state",
            );
            match L2BlockInfo::from_block_and_genesis(
                &block.into_consensus().map_transactions(|tx| tx.inner.inner.into_inner()),
                &self.inner.config.genesis,
            ) {
                // Only issue FCU if the attributes are the last in the span batch.
                // Optimization to avoid sending FCU for every block in the batch.
                Ok(block_info) if !input.is_attributes_last_in_span() => {
                    let total_duration = global_start.elapsed();
                    let old_safe_head = state.sync_state.safe_head();

                    // Apply a transient update to the safe head.
                    state.sync_state = state.sync_state.apply_update(EngineSyncStateUpdate {
                        safe_head: Some(block_info),
                        ..Default::default()
                    });

                    // Emit SafeHeadUpdated for the transient update.
                    if state.sync_state.safe_head() != old_safe_head {
                        let _ = self.inner.events_tx.send(super::EngineEvent::SafeHeadUpdated {
                            safe_head: state.sync_state.safe_head(),
                        });
                    }

                    info!(
                        target: "engine",
                        hash = %block_info.block_info.hash,
                        number = block_info.block_info.number,
                        ?total_duration,
                        ?block_fetch_duration,
                        "Updated safe head via L1 consolidation"
                    );

                    return Ok(());
                }
                Ok(block_info) => {
                    let fcu_start = Instant::now();

                    self.synchronize_forkchoice(
                        state,
                        EngineSyncStateUpdate { safe_head: Some(block_info), ..Default::default() },
                    )
                    .await
                    .map_err(|e| {
                        warn!(target: "engine", error = ?e, "Consolidation failed");
                        e
                    })?;

                    let fcu_duration = fcu_start.elapsed();
                    let total_duration = global_start.elapsed();

                    info!(
                        target: "engine",
                        hash = %block_info.block_info.hash,
                        number = block_info.block_info.number,
                        ?total_duration,
                        ?block_fetch_duration,
                        fcu_duration = ?fcu_duration,
                        "Updated safe head via L1 consolidation"
                    );

                    return Ok(());
                }
                Err(e) => {
                    warn!(target: "engine", error = ?e, "Failed to construct L2BlockInfo, proceeding to build task");
                }
            }
        }

        debug!(
            target: "engine",
            input = ?input,
            block_hash = %block_hash,
            "ConsolidateInput mismatch! Initiating reorg",
        );

        self.reconcile_unsafe_to_safe(state, input).await
    }

    /// Reconciles the unsafe chain to the safe head when safe >= unsafe.
    async fn reconcile_unsafe_to_safe(
        &self,
        state: &mut EngineState,
        input: &ConsolidateInput,
    ) -> Result<(), ConsolidateTaskError> {
        match input {
            ConsolidateInput::Attributes(attributes) => {
                self.do_build_and_seal(state, attributes, true).await?;
                Ok(())
            }
            ConsolidateInput::BlockInfo(safe_l2) => {
                self.reconcile_to_safe_head(state, safe_l2).await
            }
        }
    }

    /// Sets both unsafe and safe heads to the given safe L2 block.
    ///
    /// Used during derivation delegation to ensure the engine observes a self-consistent
    /// head state. Required for correct reorg handling and to trigger EL sync when the
    /// local unsafe head lags behind the safe head.
    async fn reconcile_to_safe_head(
        &self,
        state: &mut EngineState,
        safe_l2: &L2BlockInfo,
    ) -> Result<(), ConsolidateTaskError> {
        warn!(target: "engine", safe_l2 = %safe_l2, "Apply safe head");

        let fcu_start = Instant::now();

        self.synchronize_forkchoice(
            state,
            EngineSyncStateUpdate {
                unsafe_head: Some(*safe_l2),
                safe_head: Some(*safe_l2),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            warn!(target: "engine", error = ?e, "Apply safe head failed");
            e
        })?;

        let fcu_duration = fcu_start.elapsed();
        info!(
            target: "engine",
            hash = %safe_l2.block_info.hash,
            number = safe_l2.block_info.number,
            fcu_duration = ?fcu_duration,
            "Updated safe head via follow safe"
        );

        Ok(())
    }
}
