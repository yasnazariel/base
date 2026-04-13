//! Fetching a sealed payload from the engine without inserting it.
//!
//! Unlike [`seal`](super::seal), this only performs `engine_getPayload` and returns
//! the envelope. No `new_payload` or FCU calls are made. This enables the sequencer
//! to commit to the conductor before engine insertion.

use alloy_rpc_types_engine::{ExecutionPayload, PayloadId};
use base_common_rpc_types_engine::{BaseExecutionPayload, BaseExecutionPayloadEnvelope};
use base_protocol::AttributesWithParent;

use super::EngineHandle;
use crate::{
    EngineClient, EngineGetPayloadVersion, EngineState, EngineTaskError, Metrics, SealTaskError,
};

impl<C: EngineClient> EngineHandle<C> {
    /// Fetches a sealed payload from the engine without inserting it.
    ///
    /// Validates that the unsafe head hasn't changed since the build was started,
    /// then fetches the payload using the appropriate `engine_getPayload` version.
    pub async fn get_payload(
        &self,
        payload_id: PayloadId,
        attrs: AttributesWithParent,
    ) -> Result<BaseExecutionPayloadEnvelope, SealTaskError> {
        let state = self.inner.state.lock().await;
        let result = self.do_get_payload(&state, payload_id, &attrs).await;
        // get_payload does not mutate state.
        self.broadcast(&state);

        match &result {
            Ok(_) => Metrics::engine_task_count(Metrics::GET_PAYLOAD_TASK_LABEL).increment(1),
            Err(e) => {
                Metrics::engine_task_failure(
                    Metrics::GET_PAYLOAD_TASK_LABEL,
                    e.severity().as_label(),
                )
                .increment(1);
            }
        }

        result
    }

    /// Internal `get_payload` logic. Does not acquire the Mutex.
    pub(super) async fn do_get_payload(
        &self,
        state: &EngineState,
        payload_id: PayloadId,
        attrs: &AttributesWithParent,
    ) -> Result<BaseExecutionPayloadEnvelope, SealTaskError> {
        let unsafe_block_info = state.sync_state.unsafe_head().block_info;
        let parent_block_info = attrs.parent.block_info;

        if unsafe_block_info.hash != parent_block_info.hash
            || unsafe_block_info.number != parent_block_info.number
        {
            error!(
                target: "engine",
                unsafe_block_info = ?unsafe_block_info,
                parent_block_info = ?parent_block_info,
                "GetPayload attributes parent does not match unsafe head, returning rebuild error"
            );
            Metrics::sequencer_unsafe_head_changed_total().increment(1);
            return Err(SealTaskError::UnsafeHeadChangedSinceBuild);
        }

        self.seal_payload(payload_id, attrs).await
    }

    /// Version-dispatch logic for `engine_getPayload`.
    ///
    /// Shared between [`seal`](Self::do_seal) and [`get_payload`](Self::do_get_payload).
    pub(super) async fn seal_payload(
        &self,
        payload_id: PayloadId,
        attrs: &AttributesWithParent,
    ) -> Result<BaseExecutionPayloadEnvelope, SealTaskError> {
        let payload_timestamp = attrs.attributes().payload_attributes.timestamp;

        debug!(
            target: "engine",
            payload_id = %payload_id,
            l2_time = payload_timestamp,
            "Fetching payload"
        );

        let get_payload_version =
            EngineGetPayloadVersion::from_cfg(&self.inner.config, payload_timestamp);

        match get_payload_version {
            EngineGetPayloadVersion::V5 => {
                let payload = self.inner.client.get_payload_v5(payload_id).await.map_err(|e| {
                    error!(target: "engine", error = %e, "Payload fetch failed");
                    SealTaskError::GetPayloadFailed(e)
                })?;

                Ok(BaseExecutionPayloadEnvelope {
                    parent_beacon_block_root: attrs
                        .attributes()
                        .payload_attributes
                        .parent_beacon_block_root,
                    execution_payload: BaseExecutionPayload::V4(payload.execution_payload),
                })
            }
            EngineGetPayloadVersion::V4 => {
                let payload = self.inner.client.get_payload_v4(payload_id).await.map_err(|e| {
                    error!(target: "engine", error = %e, "Payload fetch failed");
                    SealTaskError::GetPayloadFailed(e)
                })?;

                Ok(BaseExecutionPayloadEnvelope {
                    parent_beacon_block_root: Some(payload.parent_beacon_block_root),
                    execution_payload: BaseExecutionPayload::V4(payload.execution_payload),
                })
            }
            EngineGetPayloadVersion::V3 => {
                let payload = self.inner.client.get_payload_v3(payload_id).await.map_err(|e| {
                    error!(target: "engine", error = %e, "Payload fetch failed");
                    SealTaskError::GetPayloadFailed(e)
                })?;

                Ok(BaseExecutionPayloadEnvelope {
                    parent_beacon_block_root: Some(payload.parent_beacon_block_root),
                    execution_payload: BaseExecutionPayload::V3(payload.execution_payload),
                })
            }
            EngineGetPayloadVersion::V2 => {
                let payload = self.inner.client.get_payload_v2(payload_id).await.map_err(|e| {
                    error!(target: "engine", error = %e, "Payload fetch failed");
                    SealTaskError::GetPayloadFailed(e)
                })?;

                Ok(BaseExecutionPayloadEnvelope {
                    parent_beacon_block_root: None,
                    execution_payload: match payload.execution_payload.into_payload() {
                        ExecutionPayload::V1(payload) => BaseExecutionPayload::V1(payload),
                        ExecutionPayload::V2(payload) => BaseExecutionPayload::V2(payload),
                        _ => unreachable!("the response should be a V1 or V2 payload"),
                    },
                })
            }
        }
    }
}
