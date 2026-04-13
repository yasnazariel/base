//! Inserting unsafe payloads into the execution engine.
//!
//! Sends `engine_newPayload` followed by a forkchoice update to canonicalize the block.

use std::time::Instant;

use alloy_eips::eip7685::EMPTY_REQUESTS_HASH;
use alloy_rpc_types_engine::{
    CancunPayloadFields, ExecutionPayloadInputV2, PayloadStatusEnum, PraguePayloadFields,
};
use base_common_consensus::BaseBlock;
use base_common_rpc_types_engine::{
    BaseExecutionPayload, BaseExecutionPayloadEnvelope, BaseExecutionPayloadSidecar,
};
use base_protocol::L2BlockInfo;

use super::{EngineEvent, EngineHandle};
use crate::{
    EngineClient, EngineState, EngineSyncStateUpdate, EngineTaskError, EngineTaskErrorSeverity,
    InsertTaskError, Metrics,
};

impl<C: EngineClient> EngineHandle<C> {
    /// Inserts an unsafe payload into the execution engine.
    ///
    /// Sends `engine_newPayload` with the payload data, then a forkchoice update to
    /// canonicalize the new block. If `is_derived` is true, the safe head is also advanced.
    ///
    /// On Reset-severity errors, the engine auto-resets and emits [`EngineEvent::Reset`].
    /// On Flush-severity errors, emits [`EngineEvent::Flush`] and returns the error.
    /// Temporary and Critical errors are returned directly to the caller.
    pub async fn insert(
        &self,
        payload: BaseExecutionPayloadEnvelope,
        is_derived: bool,
    ) -> Result<(), InsertTaskError> {
        let mut state = self.inner.state.lock().await;
        let result = self.do_insert(&mut state, &payload, is_derived).await;
        self.broadcast(&state);

        match &result {
            Ok(()) => {
                Metrics::engine_task_count(Metrics::INSERT_TASK_LABEL).increment(1);
            }
            Err(e) => {
                let severity = e.severity();
                Metrics::engine_task_failure(Metrics::INSERT_TASK_LABEL, severity.as_label())
                    .increment(1);

                match severity {
                    EngineTaskErrorSeverity::Reset => {
                        warn!(target: "engine", error = %e, "Insert triggered engine reset");
                        match self.do_reset(&mut state).await {
                            Ok(safe_head) => {
                                self.broadcast(&state);
                                let _ = self.inner.events_tx.send(EngineEvent::Reset { safe_head });
                                return Ok(());
                            }
                            Err(reset_err) => {
                                error!(target: "engine", error = ?reset_err, "Engine reset failed after insert");
                            }
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

    /// Internal insert logic. Does not acquire the Mutex.
    pub(super) async fn do_insert(
        &self,
        state: &mut EngineState,
        envelope: &BaseExecutionPayloadEnvelope,
        is_payload_safe: bool,
    ) -> Result<(), InsertTaskError> {
        let time_start = Instant::now();

        let parent_beacon_block_root = envelope.parent_beacon_block_root.unwrap_or_default();
        let insert_time_start = Instant::now();
        let (response, block): (_, BaseBlock) = match envelope.execution_payload.clone() {
            BaseExecutionPayload::V1(payload) => (
                self.inner.client.new_payload_v1(payload).await,
                envelope
                    .execution_payload
                    .clone()
                    .try_into_block()
                    .map_err(InsertTaskError::FromBlockError)?,
            ),
            BaseExecutionPayload::V2(payload) => {
                let payload_input = ExecutionPayloadInputV2 {
                    execution_payload: payload.payload_inner,
                    withdrawals: Some(payload.withdrawals),
                };
                (
                    self.inner.client.new_payload_v2(payload_input).await,
                    envelope
                        .execution_payload
                        .clone()
                        .try_into_block()
                        .map_err(InsertTaskError::FromBlockError)?,
                )
            }
            BaseExecutionPayload::V3(payload) => (
                self.inner.client.new_payload_v3(payload, parent_beacon_block_root).await,
                envelope
                    .execution_payload
                    .clone()
                    .try_into_block_with_sidecar(&BaseExecutionPayloadSidecar::v3(
                        CancunPayloadFields::new(parent_beacon_block_root, vec![]),
                    ))
                    .map_err(InsertTaskError::FromBlockError)?,
            ),
            BaseExecutionPayload::V4(payload) => (
                self.inner.client.new_payload_v4(payload, parent_beacon_block_root).await,
                envelope
                    .execution_payload
                    .clone()
                    .try_into_block_with_sidecar(&BaseExecutionPayloadSidecar::v4(
                        CancunPayloadFields::new(parent_beacon_block_root, vec![]),
                        PraguePayloadFields::new(EMPTY_REQUESTS_HASH),
                    ))
                    .map_err(InsertTaskError::FromBlockError)?,
            ),
        };

        let response = match response {
            Ok(resp) => resp,
            Err(e) => {
                warn!(target: "engine", error = %e, "Failed to insert new payload");
                return Err(InsertTaskError::InsertFailed(e));
            }
        };
        if !matches!(response.status, PayloadStatusEnum::Valid | PayloadStatusEnum::Syncing) {
            return Err(InsertTaskError::UnexpectedPayloadStatus(response.status));
        }
        let insert_duration = insert_time_start.elapsed();

        let new_unsafe_ref =
            L2BlockInfo::from_block_and_genesis(&block, &self.inner.config.genesis)
                .map_err(InsertTaskError::L2BlockInfoConstruction)?;

        // Send a FCU to canonicalize the imported block.
        self.synchronize_forkchoice(
            state,
            EngineSyncStateUpdate {
                unsafe_head: Some(new_unsafe_ref),
                safe_head: is_payload_safe.then_some(new_unsafe_ref),
                ..Default::default()
            },
        )
        .await?;

        let total_duration = time_start.elapsed();
        info!(
            target: "engine",
            hash = %new_unsafe_ref.block_info.hash,
            number = new_unsafe_ref.block_info.number,
            total_duration = ?total_duration,
            insert_duration = ?insert_duration,
            "Inserted new unsafe block"
        );

        Ok(())
    }
}
