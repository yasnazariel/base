//! Engine bootstrap and reset logic.
//!
//! Bootstrap is called once at startup to establish the initial engine state
//! based on the node's role (Validator, `ActiveSequencer`, `ConductorFollower`).
//! Reset finds a plausible sync starting point and FCUs the engine to it.

use alloy_eips::eip1898::BlockNumberOrTag;
use base_protocol::L2BlockInfo;

use super::{BootstrapRole, EngineHandle};
use crate::{
    EngineClient, EngineResetError, EngineState, EngineSyncStateUpdate, EngineTaskError,
    EngineTaskErrorSeverity, Metrics, SynchronizeTaskError, find_starting_forkchoice,
};

impl<C: EngineClient> EngineHandle<C> {
    /// Bootstraps the engine based on the node's role.
    ///
    /// Must be called once at startup before any operations.
    pub async fn bootstrap(&self, role: BootstrapRole) -> Result<(), EngineResetError> {
        let mut state = self.inner.state.lock().await;

        let reth_head = self.inner.client.l2_block_info_by_label(BlockNumberOrTag::Latest).await;
        let at_genesis = match &reth_head {
            Ok(Some(head)) => head.block_info.hash == self.inner.config.genesis.l2.hash,
            Ok(None) => true,
            Err(err) => {
                warn!(target: "engine", ?err, "Bootstrap: failed to query reth head, falling back to reset");
                true
            }
        };

        let opt_head = reth_head.ok().flatten();
        match role {
            BootstrapRole::Validator => self.bootstrap_validator(&mut state, opt_head),
            BootstrapRole::ConductorFollower => {
                self.bootstrap_conductor_follower(&mut state, opt_head).await
            }
            BootstrapRole::ActiveSequencer => {
                self.bootstrap_active_sequencer(&mut state, opt_head, at_genesis).await?
            }
        }

        self.broadcast(&state);
        Ok(())
    }

    /// Bootstrap path for pure validators.
    ///
    /// Seeds engine state from reth's current head so `op_syncStatus` never returns
    /// zeros, but intentionally skips sending a forkchoice update.
    fn bootstrap_validator(&self, state: &mut EngineState, head: Option<L2BlockInfo>) {
        let Some(head) = head else { return };
        let seed = EngineSyncStateUpdate { unsafe_head: Some(head), ..Default::default() };
        state.sync_state = state.sync_state.apply_update(seed);
        info!(
            target: "engine",
            unsafe_head = %head.block_info.number,
            "Bootstrap: validator seeded engine state, awaiting gossip for EL sync"
        );
    }

    /// Bootstrap path for conductor followers and stopped sequencers.
    ///
    /// Probes the EL with reth's current head as unsafe, but zeroed safe/finalized.
    async fn bootstrap_conductor_follower(
        &self,
        state: &mut EngineState,
        head: Option<L2BlockInfo>,
    ) {
        let Some(head) = head else { return };

        let follower_update =
            EngineSyncStateUpdate { unsafe_head: Some(head), ..Default::default() };

        let el_confirmed = match self.probe_el_sync(state, follower_update).await {
            Ok(c) => c,
            Err(err) => {
                warn!(
                    target: "engine",
                    error = ?err,
                    "Bootstrap: conductor follower probe failed, seeding state"
                );
                false
            }
        };

        if !el_confirmed {
            state.sync_state = state.sync_state.apply_update(follower_update);
        }

        info!(
            target: "engine",
            el_confirmed,
            unsafe_head = %head.block_info.number,
            "Bootstrap: conductor follower probed EL sync"
        );
    }

    /// Bootstrap path for the active sequencer.
    ///
    /// - At genesis: calls `reset` to FCU with all heads set to genesis.
    /// - Beyond genesis: probes the EL with reth's own safe/finalized labels.
    async fn bootstrap_active_sequencer(
        &self,
        state: &mut EngineState,
        head: Option<L2BlockInfo>,
        at_genesis: bool,
    ) -> Result<(), EngineResetError> {
        if at_genesis {
            match self.do_reset(state).await {
                Ok(_) => {}
                Err(err) => {
                    warn!(target: "engine", ?err, "Engine startup bootstrap failed; will initialize on first task");
                }
            }
        } else if let Some(head) = head {
            let safe = self
                .inner
                .client
                .l2_block_info_by_label(BlockNumberOrTag::Safe)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            let finalized = self
                .inner
                .client
                .l2_block_info_by_label(BlockNumberOrTag::Finalized)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();

            let probe_update = EngineSyncStateUpdate {
                unsafe_head: Some(head),
                safe_head: Some(safe),
                finalized_head: Some(finalized),
            };

            let el_confirmed = match self.probe_el_sync(state, probe_update).await {
                Ok(c) => c,
                Err(err) => {
                    warn!(
                        target: "engine",
                        error = ?err,
                        "Bootstrap: FCU probe failed, treating EL as syncing"
                    );
                    false
                }
            };

            if !el_confirmed {
                state.sync_state = state.sync_state.apply_update(probe_update);
            }

            if el_confirmed {
                info!(
                    target: "engine",
                    unsafe_head = %head.block_info.number,
                    "Bootstrap: EL confirmed canonical chain, el_sync_finished = true"
                );
            } else {
                info!(
                    target: "engine",
                    unsafe_head = %head.block_info.number,
                    "Bootstrap: EL sync pending, seeded engine state"
                );
            }
        }

        Ok(())
    }

    /// Probes the EL with a bare FCU to determine whether a snap-sync is in progress.
    ///
    /// Returns `true` if the EL confirmed the chain (`Valid`), `false` if still syncing.
    pub(super) async fn probe_el_sync(
        &self,
        state: &mut EngineState,
        update: EngineSyncStateUpdate,
    ) -> Result<bool, SynchronizeTaskError> {
        self.synchronize_forkchoice(state, update).await?;
        Ok(state.el_sync_finished)
    }

    /// Seeds the engine sync state from an external source without sending a forkchoice update.
    ///
    /// Pre-populates the [`EngineState`] so that callers such as `op_syncStatus`
    /// never observe zeros during the bootstrap window.
    pub fn seed_state(state: &mut EngineState, update: EngineSyncStateUpdate) {
        state.sync_state = state.sync_state.apply_update(update);
    }

    /// Resets the engine by finding a plausible sync starting point and FCU-ing to it.
    ///
    /// Returns the new safe head after the reset. If the EL has not finished syncing,
    /// returns `Err` to avoid aborting an in-progress snap sync.
    pub async fn reset(&self) -> Result<L2BlockInfo, EngineResetError> {
        let mut state = self.inner.state.lock().await;

        // Do not reset while the EL is still syncing. A Reset sends a forkchoice_updated
        // to reth pointing at the sync-start block, which will return Valid and cause reth
        // to set that stale block as canonical, aborting any in-progress snap sync.
        if !state.el_sync_finished {
            warn!(target: "engine", "Deferring engine reset: EL sync not yet complete");
            return Err(EngineResetError::ELSyncing);
        }

        let result = self.do_reset(&mut state).await;
        self.broadcast(&state);
        result
    }

    /// Internal reset. Does not acquire the Mutex.
    pub(super) async fn do_reset(
        &self,
        state: &mut EngineState,
    ) -> Result<L2BlockInfo, EngineResetError> {
        let mut start =
            find_starting_forkchoice(&self.inner.config, self.inner.client.as_ref()).await?;

        // Retry synchronize until success or critical error.
        loop {
            match self
                .synchronize_forkchoice(
                    state,
                    EngineSyncStateUpdate {
                        unsafe_head: Some(start.un_safe),
                        safe_head: Some(start.safe),
                        finalized_head: Some(start.finalized),
                    },
                )
                .await
            {
                Ok(()) => break,
                Err(err) => match err.severity() {
                    EngineTaskErrorSeverity::Temporary
                    | EngineTaskErrorSeverity::Flush
                    | EngineTaskErrorSeverity::Reset => {
                        warn!(target: "engine", ?err, "Forkchoice update failed during reset. Trying again...");
                        start = find_starting_forkchoice(
                            &self.inner.config,
                            self.inner.client.as_ref(),
                        )
                        .await?;
                    }
                    EngineTaskErrorSeverity::Critical => {
                        return Err(EngineResetError::Forkchoice(err));
                    }
                },
            }
        }

        Metrics::engine_reset_count().increment(1);
        Ok(start.safe)
    }
}
