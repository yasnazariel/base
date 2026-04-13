//! Forkchoice synchronization with the execution layer.
//!
//! The [`EngineHandle::synchronize_forkchoice`] method performs `engine_forkchoiceUpdated` calls
//! to synchronize the EL's forkchoice state with the rollup node's view. This is the core
//! primitive used by insert, consolidate, finalize, and reset.

use alloy_rpc_types_engine::{INVALID_FORK_CHOICE_STATE_ERROR, PayloadStatusEnum};
use tokio::time::Instant;

use super::EngineHandle;
use crate::{EngineClient, EngineState, EngineSyncStateUpdate, SynchronizeTaskError};

impl<C: EngineClient> EngineHandle<C> {
    /// Synchronizes the engine's forkchoice state with the execution layer.
    ///
    /// Applies the provided [`EngineSyncStateUpdate`] and sends a forkchoice update
    /// to the EL. State is only committed if the EL responds with `Valid`; a `Syncing`
    /// response leaves state unchanged to prevent the unsafe head from advancing beyond
    /// what the EL can serve.
    pub(super) async fn synchronize_forkchoice(
        &self,
        state: &mut EngineState,
        update: EngineSyncStateUpdate,
    ) -> Result<(), SynchronizeTaskError> {
        let new_sync_state = state.sync_state.apply_update(update);

        // A forkchoice update is not needed if:
        // 1. The engine state is not default (initial forkchoice has been emitted), and
        // 2. The new sync state is the same as the current sync state (no changes).
        if state.sync_state != Default::default() && state.sync_state == new_sync_state {
            debug!(target: "engine", ?new_sync_state, "No forkchoice update needed");
            return Ok(());
        }

        // Validate: unsafe head must not be behind finalized head.
        if new_sync_state.unsafe_head().block_info.number
            < new_sync_state.finalized_head().block_info.number
        {
            return Err(SynchronizeTaskError::FinalizedAheadOfUnsafe(
                new_sync_state.unsafe_head().block_info.number,
                new_sync_state.finalized_head().block_info.number,
            ));
        }

        let fcu_time_start = Instant::now();
        let forkchoice = new_sync_state.create_forkchoice_state();

        // NOTE: it doesn't matter which version we use here, because we're not sending any
        // payload attributes. The forkchoice updated call is version agnostic if no payload
        // attributes are provided.
        let response =
            self.inner.client.fork_choice_updated_v3(forkchoice, None).await.map_err(|e| {
                let error = e
                    .as_error_resp()
                    .and_then(|e| {
                        (e.code == INVALID_FORK_CHOICE_STATE_ERROR as i64)
                            .then_some(SynchronizeTaskError::InvalidForkchoiceState)
                    })
                    .unwrap_or_else(|| SynchronizeTaskError::ForkchoiceUpdateFailed(e));
                debug!(target: "engine", error = ?error, "Unexpected forkchoice update error");
                error
            })?;

        // Check the forkchoice status and conditionally apply the state update.
        let was_syncing = !state.el_sync_finished;
        let confirmed = match &response.payload_status.status {
            PayloadStatusEnum::Valid => {
                if was_syncing {
                    info!(target: "engine", "Finished execution layer sync.");
                    state.el_sync_finished = true;
                }
                true
            }
            PayloadStatusEnum::Syncing => {
                // The EL stored the block but cannot validate it yet. We intentionally
                // do NOT apply the sync-state update so that unsafe_head stays at the
                // last *confirmed* value. This prevents a gap between the node's logical
                // unsafe head and what the EL can actually serve.
                debug!(target: "engine", "Forkchoice update returned Syncing; state not advanced");
                false
            }
            s => return Err(SynchronizeTaskError::UnexpectedPayloadStatus(s.clone())),
        };

        // Only apply the sync-state update when the EL confirmed the forkchoice (`Valid`).
        if confirmed {
            let old_safe_head = state.sync_state.safe_head();
            state.sync_state = new_sync_state;

            // Emit SyncCompleted when el_sync_finished transitions from false to true.
            if was_syncing {
                let _ = self.inner.events_tx.send(super::EngineEvent::SyncCompleted {
                    safe_head: state.sync_state.safe_head(),
                });
            }

            // Emit SafeHeadUpdated when the safe head changes.
            if state.sync_state.safe_head() != old_safe_head {
                let _ = self.inner.events_tx.send(super::EngineEvent::SafeHeadUpdated {
                    safe_head: state.sync_state.safe_head(),
                });
            }
        }

        let fcu_duration = fcu_time_start.elapsed();
        debug!(
            target: "engine",
            fcu_duration = ?fcu_duration,
            forkchoice = ?forkchoice,
            ?confirmed,
            response = ?response,
            "Forkchoice updated"
        );

        Ok(())
    }
}
