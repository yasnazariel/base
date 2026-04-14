//! Trait abstraction for resource metering providers.

use core::fmt::Debug;
use std::sync::Arc;

use alloy_primitives::TxHash;
use base_bundles::MeterBundleResponse;

/// Trait abstracting resource metering data retrieval and management for the builder.
pub trait MeteringProvider: Debug + Send + Sync + 'static {
    /// Retrieves the metering data for a given transaction hash.
    fn get(&self, tx_hash: &TxHash) -> Option<MeterBundleResponse>;

    /// Returns whether resource metering is currently enabled.
    fn is_enabled(&self) -> bool {
        false
    }

    /// Inserts metering information for a transaction.
    fn insert(&self, _tx_hash: TxHash, _metering: MeterBundleResponse) {}

    /// Removes metering data for the given transaction hashes.
    ///
    /// Used for generic eviction paths where the builder no longer wants to retain
    /// the cached metering entries.
    fn remove(&self, _tx_hashes: &[TxHash]) {}

    /// Marks that metering data was needed for a transaction but not yet available.
    ///
    /// Implementations can use this hook to measure how long metering arrives after the builder
    /// first encounters a `MeteringDataPending` decision.
    fn mark_metering_data_pending(&self, _tx_hash: TxHash) {}

    /// Marks transactions as included in the current payload and evicts their metering data.
    ///
    /// Implementations can use this hook to observe metering updates that arrive after payload
    /// inclusion without conflating them with other evictions, such as permanently rejected
    /// transactions.
    fn mark_payload_included(&self, tx_hashes: &[TxHash]) {
        self.remove(tx_hashes);
    }

    /// Clears all stored metering data.
    fn clear(&self) {}

    /// Enables or disables resource metering.
    fn set_enabled(&self, _enabled: bool) {}
}

/// A no-op provider that always returns no metering data.
#[derive(Debug, Clone)]
pub struct NoopMeteringProvider;

impl MeteringProvider for NoopMeteringProvider {
    fn get(&self, _tx_hash: &TxHash) -> Option<MeterBundleResponse> {
        None
    }
}

/// Type alias for the shared, type-erased metering provider.
pub type SharedMeteringProvider = Arc<dyn MeteringProvider>;
