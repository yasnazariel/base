//! The [`EngineHandle`] — a minimal, Mutex-based engine API client.
//!
//! `EngineHandle` is `Clone + Send + Sync` with all `&self` methods. It serializes
//! Engine API calls via a [`tokio::sync::Mutex`], executes them immediately (no queue),
//! updates internal [`EngineState`], and broadcasts state changes via a
//! [`tokio::sync::watch`] channel.
//!
//! Callers receive errors directly — there is no internal retry. This means a
//! `Temporary` error releases the Mutex immediately, allowing higher-priority
//! callers to proceed without waiting. Callers decide their own retry policy.

use std::sync::Arc;

use base_consensus_genesis::RollupConfig;
use base_protocol::L2BlockInfo;
use tokio::sync::{Mutex, mpsc, watch};

use crate::{EngineClient, EngineState};

mod bootstrap;
mod build;
mod consolidate;
mod consumers;
pub use consumers::{
    DerivationEngineClient, EngineClientError, EngineClientResult, NetworkEngineClient,
    SequencerEngineClient,
};
#[cfg(any(test, feature = "test-utils"))]
pub use consumers::{
    MockDerivationEngineClient, MockNetworkEngineClient, MockSequencerEngineClient,
};

mod finalize;
mod get_payload;
mod insert;
mod seal;
mod synchronize;

/// Classifies the bootstrap behavior for the [`EngineHandle`].
///
/// Determined once at startup from the node's configuration and (if applicable)
/// a live conductor leadership check. Each variant maps to a distinct bootstrap
/// path in [`EngineHandle::bootstrap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapRole {
    /// Pure validator — seed engine state from reth's latest head, no forkchoice update.
    Validator,
    /// Active sequencer — drive forkchoice at genesis or probe the EL with real heads.
    ActiveSequencer,
    /// Conductor follower or stopped sequencer — probe the EL with zeroed safe/finalized heads.
    ConductorFollower,
}

/// Signals emitted by the engine for cross-actor coordination.
///
/// Consumed by the derivation actor to drive pipeline resets, flushes, and state updates.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// Engine auto-reset after a Reset-severity error. Derivation should reset its pipeline
    /// starting from the provided safe head.
    Reset {
        /// The new safe head after the reset.
        safe_head: L2BlockInfo,
    },
    /// A Flush-severity error occurred. Derivation should flush the current channel.
    Flush,
    /// The EL has finished syncing. Derivation can start processing.
    SyncCompleted {
        /// The safe head at the time sync completed.
        safe_head: L2BlockInfo,
    },
    /// The safe head has been updated. Derivation should record this in `SafeDB`.
    SafeHeadUpdated {
        /// The new safe head.
        safe_head: L2BlockInfo,
    },
}

/// The engine. `Clone + Send + Sync`. All methods are `&self`.
///
/// A [`Mutex`] serializes EL calls. No channel, no queue, no background task.
/// State changes are broadcast via a [`watch`] channel that subscribers can observe.
pub struct EngineHandle<C: EngineClient> {
    inner: Arc<EngineInner<C>>,
}

// Manual Clone impl to avoid requiring C: Clone (Arc<T> is always Clone).
impl<C: EngineClient> Clone for EngineHandle<C> {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner) }
    }
}

impl<C: EngineClient + std::fmt::Debug> std::fmt::Debug for EngineHandle<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineHandle").field("inner", &self.inner).finish()
    }
}

#[derive(Debug)]
struct EngineInner<C: EngineClient> {
    /// The Engine API client for communicating with the execution layer.
    client: Arc<C>,
    /// The rollup configuration for version selection and genesis info.
    config: Arc<RollupConfig>,
    /// Mutable engine state, protected by a Mutex for serialized access.
    state: Mutex<EngineState>,
    /// Watch channel sender for broadcasting state updates to subscribers.
    state_tx: watch::Sender<EngineState>,
    /// Unbounded channel for emitting engine events (Reset, Flush).
    events_tx: mpsc::UnboundedSender<EngineEvent>,
}

impl<C: EngineClient> EngineHandle<C> {
    /// Creates a new [`EngineHandle`] and returns the events receiver.
    ///
    /// The handle is distributed to all actors that need engine access.
    /// The events receiver should be consumed by the derivation actor.
    pub fn new(
        client: Arc<C>,
        config: Arc<RollupConfig>,
    ) -> (Self, mpsc::UnboundedReceiver<EngineEvent>) {
        let state = EngineState::default();
        let (state_tx, _) = watch::channel(state);
        let (events_tx, events_rx) = mpsc::unbounded_channel();

        let handle = Self {
            inner: Arc::new(EngineInner {
                client,
                config,
                state: Mutex::new(state),
                state_tx,
                events_tx,
            }),
        };

        (handle, events_rx)
    }

    /// Creates a new [`EngineHandle`] with a pre-populated initial state.
    ///
    /// Used for testing and scenarios where state must be seeded before bootstrap.
    pub fn new_with_state(
        client: Arc<C>,
        config: Arc<RollupConfig>,
        initial_state: EngineState,
    ) -> (Self, mpsc::UnboundedReceiver<EngineEvent>) {
        let (state_tx, _) = watch::channel(initial_state);
        let (events_tx, events_rx) = mpsc::unbounded_channel();

        let handle = Self {
            inner: Arc::new(EngineInner {
                client,
                config,
                state: Mutex::new(initial_state),
                state_tx,
                events_tx,
            }),
        };

        (handle, events_rx)
    }

    /// Returns the current [`EngineState`] without acquiring the Mutex.
    ///
    /// Reads from the watch channel, which is always up to date after any operation.
    pub fn state(&self) -> EngineState {
        *self.inner.state_tx.borrow()
    }

    /// Returns a [`watch::Receiver`] to subscribe to engine state changes.
    pub fn subscribe(&self) -> watch::Receiver<EngineState> {
        self.inner.state_tx.subscribe()
    }

    /// Broadcasts the current engine state to all watch channel subscribers.
    fn broadcast(&self, state: &EngineState) {
        self.inner.state_tx.send_replace(*state);
    }

    /// Returns a reference to the inner [`EngineClient`].
    pub fn client(&self) -> &C {
        &self.inner.client
    }

    /// Returns a reference to the inner [`RollupConfig`].
    pub fn config(&self) -> &RollupConfig {
        &self.inner.config
    }
}
