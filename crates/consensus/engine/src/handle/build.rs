//! Block building via forkchoice update with attributes.

use alloy_rpc_types_engine::{PayloadId, PayloadStatusEnum};
use base_protocol::AttributesWithParent;

use super::EngineHandle;
use crate::{
    BuildTaskError, EngineBuildError, EngineClient, EngineForkchoiceVersion, EngineState,
    EngineSyncStateUpdate, EngineTaskError, Metrics,
};

impl<C: EngineClient> EngineHandle<C> {
    /// Starts building a new block by sending a forkchoice update with payload attributes.
    ///
    /// Returns the [`PayloadId`] assigned by the execution layer, which can be used
    /// to later seal or fetch the payload.
    ///
    /// This method does **not** mutate engine state — it only sends the FCU.
    pub async fn build(&self, attrs: AttributesWithParent) -> Result<PayloadId, BuildTaskError> {
        let state = self.inner.state.lock().await;
        let result = self.do_build(&state, &attrs).await;
        self.broadcast(&state);
        match &result {
            Ok(_) => Metrics::engine_task_count(Metrics::BUILD_TASK_LABEL).increment(1),
            Err(e) => {
                Metrics::engine_task_failure(Metrics::BUILD_TASK_LABEL, e.severity().as_label())
                    .increment(1);
            }
        }
        result
    }

    /// Internal build logic. Does not acquire the Mutex.
    pub(super) async fn do_build(
        &self,
        state: &EngineState,
        attrs: &AttributesWithParent,
    ) -> Result<PayloadId, BuildTaskError> {
        // Sanity check: head must not be behind finalized.
        if state.sync_state.unsafe_head().block_info.number
            < state.sync_state.finalized_head().block_info.number
        {
            return Err(BuildTaskError::EngineBuildError(
                EngineBuildError::FinalizedAheadOfUnsafe(
                    state.sync_state.unsafe_head().block_info.number,
                    state.sync_state.finalized_head().block_info.number,
                ),
            ));
        }

        // Advertise the parent as the current unsafe head for the FCU.
        let new_forkchoice = state
            .sync_state
            .apply_update(EngineSyncStateUpdate {
                unsafe_head: Some(attrs.parent),
                ..Default::default()
            })
            .create_forkchoice_state();

        let forkchoice_version = EngineForkchoiceVersion::from_cfg(
            &self.inner.config,
            attrs.attributes.payload_attributes.timestamp,
        );

        let update = match forkchoice_version {
            EngineForkchoiceVersion::V3 => {
                self.inner
                    .client
                    .fork_choice_updated_v3(new_forkchoice, Some(attrs.attributes.clone()))
                    .await
            }
            EngineForkchoiceVersion::V2 => {
                self.inner
                    .client
                    .fork_choice_updated_v2(new_forkchoice, Some(attrs.attributes.clone()))
                    .await
            }
        }
        .map_err(|e| {
            error!(target: "engine", error = %e, "Forkchoice update failed");
            BuildTaskError::EngineBuildError(EngineBuildError::AttributesInsertionFailed(e))
        })?;

        // Validate the forkchoice status.
        match update.payload_status.status {
            PayloadStatusEnum::Valid => {}
            PayloadStatusEnum::Invalid { validation_error } => {
                error!(target: "engine", error = %validation_error, "Forkchoice update failed");
                return Err(BuildTaskError::EngineBuildError(EngineBuildError::InvalidPayload(
                    validation_error,
                )));
            }
            PayloadStatusEnum::Syncing => {
                warn!(target: "engine", "Forkchoice update failed temporarily: EL is syncing");
                return Err(BuildTaskError::EngineBuildError(EngineBuildError::EngineSyncing));
            }
            PayloadStatusEnum::Accepted => {
                return Err(BuildTaskError::EngineBuildError(
                    EngineBuildError::UnexpectedPayloadStatus(update.payload_status.status),
                ));
            }
        }

        debug!(
            target: "engine",
            unsafe_hash = %new_forkchoice.head_block_hash,
            safe_hash = %new_forkchoice.safe_block_hash,
            finalized_hash = %new_forkchoice.finalized_block_hash,
            "Forkchoice update with attributes successful"
        );

        // Extract the payload ID. If missing, the EL failed to initiate the build.
        update
            .payload_id
            .ok_or(BuildTaskError::EngineBuildError(EngineBuildError::MissingPayloadId))
    }
}
