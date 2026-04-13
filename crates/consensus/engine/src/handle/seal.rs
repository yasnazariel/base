//! Block sealing and canonicalization.
//!
//! Fetches a built payload via `engine_getPayload`, imports it via `engine_newPayload` +
//! forkchoice update, and handles the Holocene deposits-only fallback.

use std::time::Instant;

use alloy_rpc_types_engine::PayloadId;
use base_common_rpc_types_engine::BaseExecutionPayloadEnvelope;
use base_protocol::{AttributesWithParent, L2BlockInfo};

use super::{EngineEvent, EngineHandle};
use crate::{
    EngineClient, EngineState, EngineTaskError, EngineTaskErrorSeverity, InsertTaskError, Metrics,
    SealTaskError,
};

impl<C: EngineClient> EngineHandle<C> {
    /// Seals and canonicalizes a block.
    ///
    /// Fetches the payload from the EL, imports it, and updates the unsafe head.
    /// Handles Holocene deposits-only fallback if the initial import fails.
    pub async fn seal(
        &self,
        payload_id: PayloadId,
        attrs: AttributesWithParent,
        is_derived: bool,
    ) -> Result<BaseExecutionPayloadEnvelope, SealTaskError> {
        let mut state = self.inner.state.lock().await;
        let result = self.do_seal(&mut state, payload_id, &attrs, is_derived).await;
        self.broadcast(&state);

        match &result {
            Ok(_) => {
                Metrics::engine_task_count(Metrics::SEAL_TASK_LABEL).increment(1);
            }
            Err(e) => {
                let severity = e.severity();
                Metrics::engine_task_failure(Metrics::SEAL_TASK_LABEL, severity.as_label())
                    .increment(1);

                match severity {
                    EngineTaskErrorSeverity::Reset => {
                        warn!(target: "engine", error = %e, "Seal triggered engine reset");
                        if let Ok(safe_head) = self.do_reset(&mut state).await {
                            self.broadcast(&state);
                            let _ = self.inner.events_tx.send(EngineEvent::Reset { safe_head });
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

    /// Internal seal logic. Does not acquire the Mutex.
    pub(super) async fn do_seal(
        &self,
        state: &mut EngineState,
        payload_id: PayloadId,
        attrs: &AttributesWithParent,
        is_derived: bool,
    ) -> Result<BaseExecutionPayloadEnvelope, SealTaskError> {
        debug!(
            target: "engine",
            txs = attrs.attributes().transactions.as_ref().map_or(0, |txs| txs.len()),
            is_deposits = attrs.is_deposits_only(),
            "Starting new seal job"
        );

        let block_import_start_time = Instant::now();

        // Fetch the payload from the EL.
        let new_payload = self.seal_payload(payload_id, attrs).await?;

        let _new_block_ref = L2BlockInfo::from_payload_and_genesis(
            new_payload.execution_payload.clone(),
            attrs.attributes().payload_attributes.parent_beacon_block_root,
            &self.inner.config.genesis,
        )
        .map_err(SealTaskError::FromBlock)?;

        // Insert the payload into the engine with Holocene fallback.
        self.insert_payload_with_fallback(state, &new_payload, attrs, is_derived).await?;

        let block_import_duration = block_import_start_time.elapsed();
        info!(
            target: "engine",
            l2_number = _new_block_ref.block_info.number,
            l2_time = _new_block_ref.block_info.timestamp,
            block_import_duration = ?block_import_duration,
            "Built and imported new {} block",
            if is_derived { "safe" } else { "unsafe" },
        );

        Ok(new_payload)
    }

    /// Inserts a payload into the engine with Holocene fallback support.
    ///
    /// Handles:
    /// 1. Normal insertion via `do_insert`
    /// 2. Deposits-only payload failures (critical error)
    /// 3. Holocene fallback: re-attempt with deposits-only attributes via `do_build_and_seal`
    async fn insert_payload_with_fallback(
        &self,
        state: &mut EngineState,
        new_payload: &BaseExecutionPayloadEnvelope,
        attrs: &AttributesWithParent,
        is_derived: bool,
    ) -> Result<(), SealTaskError> {
        match self.do_insert(state, new_payload, is_derived).await {
            Ok(()) => {
                info!(target: "engine", "Successfully imported payload");
                Ok(())
            }
            Err(InsertTaskError::UnexpectedPayloadStatus(e)) if attrs.is_deposits_only() => {
                error!(target: "engine", error = ?e, "Critical: Deposit-only payload import failed");
                Err(SealTaskError::DepositOnlyPayloadFailed)
            }
            Err(InsertTaskError::UnexpectedPayloadStatus(e))
                if self
                    .inner
                    .config
                    .is_holocene_active(attrs.attributes().payload_attributes.timestamp) =>
            {
                warn!(target: "engine", error = ?e, "Re-attempting payload import with deposits only.");

                // HOLOCENE: Re-attempt with deposits-only attributes.
                let deposits_only_attrs = attrs.as_deposits_only();
                match self.do_build_and_seal(state, &deposits_only_attrs, is_derived).await {
                    Ok(_) => {
                        info!(target: "engine", "Successfully imported deposits-only payload");
                        Err(SealTaskError::HoloceneInvalidFlush)
                    }
                    Err(_) => Err(SealTaskError::DepositOnlyPayloadReattemptFailed),
                }
            }
            Err(e) => {
                error!(target: "engine", error = %e, "Payload import failed");
                Err(Box::new(e).into())
            }
        }
    }

    /// Builds and seals a block in one operation. Used as a fallback path.
    ///
    /// Returns `Ok(())` if both the build and seal succeed.
    /// Uses `Box::pin` to break the async recursion cycle
    /// (`do_seal` -> `insert_payload_with_fallback` -> `do_build_and_seal` -> `do_seal`).
    pub(super) fn do_build_and_seal<'a>(
        &'a self,
        state: &'a mut EngineState,
        attrs: &'a AttributesWithParent,
        is_derived: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SealTaskError>> + Send + 'a>>
    {
        Box::pin(async move {
            let payload_id = self.do_build(state, attrs).await.map_err(|e| {
                error!(target: "engine", error = %e, "Build failed during build-and-seal");
                SealTaskError::DepositOnlyPayloadReattemptFailed
            })?;
            self.do_seal(state, payload_id, attrs, is_derived).await?;
            Ok(())
        })
    }
}
