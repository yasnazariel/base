//! Finalization of L2 blocks.
//!
//! Fetches the block at the given number and dispatches a forkchoice update
//! to advance the finalized head.

use std::time::Instant;

use base_protocol::L2BlockInfo;

use super::{EngineEvent, EngineHandle};
use crate::{
    EngineClient, EngineState, EngineSyncStateUpdate, EngineTaskError, EngineTaskErrorSeverity,
    FinalizeTaskError, Metrics,
};

impl<C: EngineClient> EngineHandle<C> {
    /// Finalizes the L2 block at the given block number.
    ///
    /// Fetches the block from the EL, validates it is at least safe, and dispatches a
    /// forkchoice update to advance the finalized head.
    pub async fn finalize(&self, block_number: u64) -> Result<(), FinalizeTaskError> {
        let mut state = self.inner.state.lock().await;
        let result = self.do_finalize(&mut state, block_number).await;
        self.broadcast(&state);

        match &result {
            Ok(()) => {
                Metrics::engine_task_count(Metrics::FINALIZE_TASK_LABEL).increment(1);
            }
            Err(e) => {
                let severity = e.severity();
                Metrics::engine_task_failure(Metrics::FINALIZE_TASK_LABEL, severity.as_label())
                    .increment(1);

                match severity {
                    EngineTaskErrorSeverity::Reset => {
                        warn!(target: "engine", error = %e, "Finalize triggered engine reset");
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

    /// Internal finalize logic. Does not acquire the Mutex.
    pub(super) async fn do_finalize(
        &self,
        state: &mut EngineState,
        block_number: u64,
    ) -> Result<(), FinalizeTaskError> {
        // Sanity check that the block being finalized is at least safe.
        if state.sync_state.safe_head().block_info.number < block_number {
            return Err(FinalizeTaskError::BlockNotSafe);
        }

        let block_fetch_start = Instant::now();
        let block = self
            .inner
            .client
            .get_l2_block(block_number.into())
            .full()
            .await
            .map_err(FinalizeTaskError::TransportError)?
            .ok_or(FinalizeTaskError::BlockNotFound(block_number))?
            .into_consensus();
        let block_info = L2BlockInfo::from_block_and_genesis(
            &block.map_transactions(|tx| tx.inner.inner.into_inner()),
            &self.inner.config.genesis,
        )
        .map_err(FinalizeTaskError::FromBlock)?;
        let block_fetch_duration = block_fetch_start.elapsed();

        let fcu_start = Instant::now();
        self.synchronize_forkchoice(
            state,
            EngineSyncStateUpdate { finalized_head: Some(block_info), ..Default::default() },
        )
        .await?;
        let fcu_duration = fcu_start.elapsed();
        let total_duration = block_fetch_start.elapsed();
        Metrics::engine_finalize_duration_seconds().record(total_duration.as_secs_f64());

        info!(
            target: "engine",
            hash = %block_info.block_info.hash,
            number = block_info.block_info.number,
            ?block_fetch_duration,
            ?fcu_duration,
            "Updated finalized head"
        );

        Ok(())
    }
}
