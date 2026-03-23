//! Metrics for block processing operations.

use crate::api::{OperationDurations, WriteCounts};

base_metrics::define_metrics_named! {
    BlockMetrics, "optimism_trie.block",

    #[describe("Total time to process a block (end-to-end) in seconds")]
    total_duration_seconds: histogram,
    #[describe("Time spent executing the block (EVM) in seconds")]
    execution_duration_seconds: histogram,
    #[describe("Time spent calculating state root in seconds")]
    state_root_duration_seconds: histogram,
    #[describe("Time spent writing trie updates to storage in seconds")]
    write_duration_seconds: histogram,
    #[describe("Number of trie updates written")]
    account_trie_updates_written_total: counter,
    #[describe("Number of storage trie updates written")]
    storage_trie_updates_written_total: counter,
    #[describe("Number of hashed accounts written")]
    hashed_accounts_written_total: counter,
    #[describe("Number of hashed storages written")]
    hashed_storages_written_total: counter,
    #[describe("Earliest block number that the proofs storage has stored")]
    earliest_number: gauge,
    #[describe("Latest block number that the proofs storage has stored")]
    latest_number: gauge,
}

impl BlockMetrics {
    /// Record duration metrics for a block processing operation.
    pub fn record_operation_durations(durations: &OperationDurations) {
        Self::total_duration_seconds().record(durations.total_duration_seconds);
        Self::execution_duration_seconds().record(durations.execution_duration_seconds);
        Self::state_root_duration_seconds().record(durations.state_root_duration_seconds);
        Self::write_duration_seconds().record(durations.write_duration_seconds);
    }

    /// Increment write counts of historical trie updates for a single block.
    pub fn increment_write_counts(counts: &WriteCounts) {
        Self::account_trie_updates_written_total()
            .increment(counts.account_trie_updates_written_total);
        Self::storage_trie_updates_written_total()
            .increment(counts.storage_trie_updates_written_total);
        Self::hashed_accounts_written_total().increment(counts.hashed_accounts_written_total);
        Self::hashed_storages_written_total().increment(counts.hashed_storages_written_total);
    }
}
