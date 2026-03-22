//! Storage wrapper that records metrics for all operations.

use std::{
    fmt::Debug,
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_eips::{BlockNumHash, eip1898::BlockWithParent};
use alloy_primitives::{B256, U256};
use derive_more::Constructor;
use reth_db::DatabaseError;
use reth_primitives_traits::Account;
use reth_trie::{
    hashed_cursor::{HashedCursor, HashedStorageCursor},
    trie_cursor::{TrieCursor, TrieStorageCursor},
};
use reth_trie_common::{BranchNodeCompact, Nibbles};

use crate::{
    BlockStateDiff, OpProofsStorageResult, OpProofsStore,
    api::{InitialStateAnchor, OpProofsInitialStateStore, OperationDurations, WriteCounts},
    cursor,
};

base_macros::define_metrics! {
    #[scope("optimism_trie_storage_operation")]
    pub struct OperationMetrics {
        #[describe("Duration of storage operations in seconds")]
        #[label("operation", operation)]
        duration_seconds: histogram,
    }
}

base_macros::define_metrics! {
    #[scope("optimism_trie_block")]
    pub struct BlockMetrics {
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
}

/// Types of storage operations that can be tracked.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum StorageOperation {
    /// Store account trie branch
    StoreAccountBranch,
    /// Store storage trie branch
    StoreStorageBranch,
    /// Store hashed account
    StoreHashedAccount,
    /// Store hashed storage
    StoreHashedStorage,
    /// Trie cursor seek exact operation
    TrieCursorSeekExact,
    /// Trie cursor seek
    TrieCursorSeek,
    /// Trie cursor next
    TrieCursorNext,
    /// Trie cursor current
    TrieCursorCurrent,
    /// Hashed cursor seek
    HashedCursorSeek,
    /// Hashed cursor next
    HashedCursorNext,
}

impl StorageOperation {
    /// Returns the operation as a string for metrics labels.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::StoreAccountBranch => "store_account_branch",
            Self::StoreStorageBranch => "store_storage_branch",
            Self::StoreHashedAccount => "store_hashed_account",
            Self::StoreHashedStorage => "store_hashed_storage",
            Self::TrieCursorSeekExact => "trie_cursor_seek_exact",
            Self::TrieCursorSeek => "trie_cursor_seek",
            Self::TrieCursorNext => "trie_cursor_next",
            Self::TrieCursorCurrent => "trie_cursor_current",
            Self::HashedCursorSeek => "hashed_cursor_seek",
            Self::HashedCursorNext => "hashed_cursor_next",
        }
    }

    /// Record an operation with timing.
    pub fn record<R>(&self, f: impl FnOnce() -> R) -> R {
        let start = Instant::now();
        let result = f();
        OperationMetrics::duration_seconds(self.as_str()).record(start.elapsed());
        result
    }

    /// Record a pre-measured duration.
    pub fn record_duration(&self, duration: Duration) {
        OperationMetrics::duration_seconds(self.as_str()).record(duration);
    }

    /// Record a pre-measured duration divided across multiple items.
    pub fn record_duration_per_item(&self, duration: Duration, count_usize: usize) {
        if count_usize > 0
            && let Some(count) = u32::try_from(count_usize).ok()
        {
            let hist = OperationMetrics::duration_seconds(self.as_str());
            let per_item = duration / count;
            for _ in 0..count {
                hist.record(per_item);
            }
        }
    }

    /// Record a storage operation with timing (async version).
    pub async fn record_async<F, R>(&self, f: F) -> R
    where
        F: Future<Output = R>,
    {
        let start = Instant::now();
        let result = f.await;
        OperationMetrics::duration_seconds(self.as_str()).record(start.elapsed());
        result
    }
}

/// Alias for [`OpProofsStorageWithMetrics`].
pub type OpProofsStorage<S> = OpProofsStorageWithMetrics<S>;

/// Alias for [`TrieCursor`](cursor::OpProofsTrieCursor) with metrics layer.
pub type OpProofsTrieCursor<C> = cursor::OpProofsTrieCursor<OpProofsTrieCursorWithMetrics<C>>;

/// Alias for [`OpProofsHashedAccountCursor`](cursor::OpProofsHashedAccountCursor) with metrics
/// layer.
pub type OpProofsHashedAccountCursor<C> =
    cursor::OpProofsHashedAccountCursor<OpProofsHashedCursorWithMetrics<C>>;

/// Alias for [`OpProofsHashedStorageCursor`](cursor::OpProofsHashedStorageCursor) with metrics
/// layer.
pub type OpProofsHashedStorageCursor<C> =
    cursor::OpProofsHashedStorageCursor<OpProofsHashedCursorWithMetrics<C>>;

/// Record operation durations for the processing of a block.
pub fn record_block_operation_durations(durations: &OperationDurations) {
    BlockMetrics::total_duration_seconds().record(durations.total_duration_seconds);
    BlockMetrics::execution_duration_seconds().record(durations.execution_duration_seconds);
    BlockMetrics::state_root_duration_seconds().record(durations.state_root_duration_seconds);
    BlockMetrics::write_duration_seconds().record(durations.write_duration_seconds);
}

/// Increment write counts of historical trie updates for a single block.
pub fn increment_block_write_counts(counts: &WriteCounts) {
    BlockMetrics::account_trie_updates_written_total()
        .increment(counts.account_trie_updates_written_total);
    BlockMetrics::storage_trie_updates_written_total()
        .increment(counts.storage_trie_updates_written_total);
    BlockMetrics::hashed_accounts_written_total().increment(counts.hashed_accounts_written_total);
    BlockMetrics::hashed_storages_written_total().increment(counts.hashed_storages_written_total);
}

/// Wrapper for [`TrieCursor`] that records metrics.
#[derive(Debug, Constructor, Clone)]
pub struct OpProofsTrieCursorWithMetrics<C> {
    cursor: C,
}

impl<C: TrieCursor> TrieCursor for OpProofsTrieCursorWithMetrics<C> {
    #[inline]
    fn seek_exact(
        &mut self,
        path: Nibbles,
    ) -> Result<Option<(Nibbles, BranchNodeCompact)>, DatabaseError> {
        StorageOperation::TrieCursorSeekExact.record(|| self.cursor.seek_exact(path))
    }

    #[inline]
    fn seek(
        &mut self,
        path: Nibbles,
    ) -> Result<Option<(Nibbles, BranchNodeCompact)>, DatabaseError> {
        StorageOperation::TrieCursorSeek.record(|| self.cursor.seek(path))
    }

    #[inline]
    fn next(&mut self) -> Result<Option<(Nibbles, BranchNodeCompact)>, DatabaseError> {
        StorageOperation::TrieCursorNext.record(|| self.cursor.next())
    }

    #[inline]
    fn current(&mut self) -> Result<Option<Nibbles>, DatabaseError> {
        StorageOperation::TrieCursorCurrent.record(|| self.cursor.current())
    }

    #[inline]
    fn reset(&mut self) {
        self.cursor.reset()
    }
}

impl<C: TrieStorageCursor> TrieStorageCursor for OpProofsTrieCursorWithMetrics<C> {
    #[inline]
    fn set_hashed_address(&mut self, _hashed_address: B256) {
        self.cursor.set_hashed_address(_hashed_address)
    }
}

/// Wrapper for [`HashedCursor`] type that records metrics.
#[derive(Debug, Constructor, Clone)]
pub struct OpProofsHashedCursorWithMetrics<C> {
    cursor: C,
}

impl<C: HashedCursor> HashedCursor for OpProofsHashedCursorWithMetrics<C> {
    type Value = C::Value;

    #[inline]
    fn seek(&mut self, key: B256) -> Result<Option<(B256, Self::Value)>, DatabaseError> {
        StorageOperation::HashedCursorSeek.record(|| self.cursor.seek(key))
    }

    #[inline]
    fn next(&mut self) -> Result<Option<(B256, Self::Value)>, DatabaseError> {
        StorageOperation::HashedCursorNext.record(|| self.cursor.next())
    }

    #[inline]
    fn reset(&mut self) {
        self.cursor.reset()
    }
}

impl<C: HashedStorageCursor> HashedStorageCursor for OpProofsHashedCursorWithMetrics<C> {
    #[inline]
    fn is_storage_empty(&mut self) -> Result<bool, DatabaseError> {
        self.cursor.is_storage_empty()
    }

    #[inline]
    fn set_hashed_address(&mut self, _hashed_address: B256) {
        self.cursor.set_hashed_address(_hashed_address)
    }
}

/// Wrapper around [`OpProofsStore`] type that records metrics for all operations.
#[derive(Debug, Clone)]
pub struct OpProofsStorageWithMetrics<S> {
    storage: S,
}

impl<S> OpProofsStorageWithMetrics<S> {
    /// Initializes a new wrapper around the given storage instance.
    pub fn new(storage: S) -> Self {
        Self { storage }
    }

    /// Get the underlying storage.
    pub const fn inner(&self) -> &S {
        &self.storage
    }
}

impl<S> OpProofsStore for OpProofsStorageWithMetrics<S>
where
    S: OpProofsStore,
{
    type StorageTrieCursor<'tx>
        = OpProofsTrieCursorWithMetrics<S::StorageTrieCursor<'tx>>
    where
        Self: 'tx;
    type AccountTrieCursor<'tx>
        = OpProofsTrieCursorWithMetrics<S::AccountTrieCursor<'tx>>
    where
        Self: 'tx;
    type StorageCursor<'tx>
        = OpProofsHashedCursorWithMetrics<S::StorageCursor<'tx>>
    where
        Self: 'tx;
    type AccountHashedCursor<'tx>
        = OpProofsHashedCursorWithMetrics<S::AccountHashedCursor<'tx>>
    where
        Self: 'tx;

    #[inline]
    fn get_earliest_block_number(&self) -> OpProofsStorageResult<Option<(u64, B256)>> {
        self.storage.get_earliest_block_number()
    }

    #[inline]
    fn get_latest_block_number(&self) -> OpProofsStorageResult<Option<(u64, B256)>> {
        self.storage.get_latest_block_number()
    }

    #[inline]
    fn storage_trie_cursor<'tx>(
        &self,
        hashed_address: B256,
        max_block_number: u64,
    ) -> OpProofsStorageResult<Self::StorageTrieCursor<'tx>> {
        let cursor = self.storage.storage_trie_cursor(hashed_address, max_block_number)?;
        Ok(OpProofsTrieCursorWithMetrics::new(cursor))
    }

    #[inline]
    fn account_trie_cursor<'tx>(
        &self,
        max_block_number: u64,
    ) -> OpProofsStorageResult<Self::AccountTrieCursor<'tx>> {
        let cursor = self.storage.account_trie_cursor(max_block_number)?;
        Ok(OpProofsTrieCursorWithMetrics::new(cursor))
    }

    #[inline]
    fn storage_hashed_cursor<'tx>(
        &self,
        hashed_address: B256,
        max_block_number: u64,
    ) -> OpProofsStorageResult<Self::StorageCursor<'tx>> {
        let cursor = self.storage.storage_hashed_cursor(hashed_address, max_block_number)?;
        Ok(OpProofsHashedCursorWithMetrics::new(cursor))
    }

    #[inline]
    fn account_hashed_cursor<'tx>(
        &self,
        max_block_number: u64,
    ) -> OpProofsStorageResult<Self::AccountHashedCursor<'tx>> {
        let cursor = self.storage.account_hashed_cursor(max_block_number)?;
        Ok(OpProofsHashedCursorWithMetrics::new(cursor))
    }

    // metrics are handled by the live trie collector
    #[inline]
    fn store_trie_updates(
        &self,
        block_ref: BlockWithParent,
        block_state_diff: BlockStateDiff,
    ) -> OpProofsStorageResult<WriteCounts> {
        let result = self.storage.store_trie_updates(block_ref, block_state_diff)?;
        BlockMetrics::latest_number().set(block_ref.block.number as f64);
        Ok(result)
    }

    // no metrics for these
    #[inline]
    fn fetch_trie_updates(&self, block_number: u64) -> OpProofsStorageResult<BlockStateDiff> {
        self.storage.fetch_trie_updates(block_number)
    }
    #[inline]
    fn prune_earliest_state(
        &self,
        new_earliest_block_ref: BlockWithParent,
    ) -> OpProofsStorageResult<WriteCounts> {
        BlockMetrics::earliest_number().set(new_earliest_block_ref.block.number as f64);
        self.storage.prune_earliest_state(new_earliest_block_ref)
    }

    #[inline]
    fn unwind_history(&self, to: BlockWithParent) -> OpProofsStorageResult<()> {
        self.storage.unwind_history(to)
    }

    #[inline]
    fn replace_updates(
        &self,
        latest_common_block: BlockNumHash,
        blocks_to_add: Vec<(BlockWithParent, BlockStateDiff)>,
    ) -> OpProofsStorageResult<()> {
        self.storage.replace_updates(latest_common_block, blocks_to_add)
    }

    #[inline]
    fn set_earliest_block_number(
        &self,
        block_number: u64,
        hash: B256,
    ) -> OpProofsStorageResult<()> {
        BlockMetrics::earliest_number().set(block_number as f64);
        self.storage.set_earliest_block_number(block_number, hash)
    }
}

impl<S> OpProofsInitialStateStore for OpProofsStorageWithMetrics<S>
where
    S: OpProofsInitialStateStore,
{
    #[inline]
    fn initial_state_anchor(&self) -> OpProofsStorageResult<InitialStateAnchor> {
        self.storage.initial_state_anchor()
    }

    #[inline]
    fn set_initial_state_anchor(&self, anchor: BlockNumHash) -> OpProofsStorageResult<()> {
        self.storage.set_initial_state_anchor(anchor)
    }

    #[inline]
    fn store_account_branches(
        &self,
        account_nodes: Vec<(Nibbles, Option<BranchNodeCompact>)>,
    ) -> OpProofsStorageResult<()> {
        let count = account_nodes.len();
        let start = Instant::now();
        let result = self.storage.store_account_branches(account_nodes);
        let duration = start.elapsed();

        // Record per-item duration
        if count > 0 {
            StorageOperation::StoreAccountBranch.record_duration_per_item(duration, count);
        }

        result
    }

    #[inline]
    fn store_storage_branches(
        &self,
        hashed_address: B256,
        storage_nodes: Vec<(Nibbles, Option<BranchNodeCompact>)>,
    ) -> OpProofsStorageResult<()> {
        let count = storage_nodes.len();
        let start = Instant::now();
        let result = self.storage.store_storage_branches(hashed_address, storage_nodes);
        let duration = start.elapsed();

        // Record per-item duration
        if count > 0 {
            StorageOperation::StoreStorageBranch.record_duration_per_item(duration, count);
        }

        result
    }

    #[inline]
    fn store_hashed_accounts(
        &self,
        accounts: Vec<(B256, Option<Account>)>,
    ) -> OpProofsStorageResult<()> {
        let count = accounts.len();
        let start = Instant::now();
        let result = self.storage.store_hashed_accounts(accounts);
        let duration = start.elapsed();

        // Record per-item duration
        if count > 0 {
            StorageOperation::StoreHashedAccount.record_duration_per_item(duration, count);
        }

        result
    }

    #[inline]
    fn store_hashed_storages(
        &self,
        hashed_address: B256,
        storages: Vec<(B256, U256)>,
    ) -> OpProofsStorageResult<()> {
        let count = storages.len();
        let start = Instant::now();
        let result = self.storage.store_hashed_storages(hashed_address, storages);
        let duration = start.elapsed();

        // Record per-item duration
        if count > 0 {
            StorageOperation::StoreHashedStorage.record_duration_per_item(duration, count);
        }

        result
    }

    #[inline]
    fn commit_initial_state(&self) -> OpProofsStorageResult<BlockNumHash> {
        let block = self.storage.commit_initial_state()?;
        BlockMetrics::earliest_number().set(block.number as f64);
        Ok(block)
    }
}

impl<S> From<S> for OpProofsStorageWithMetrics<S>
where
    S: OpProofsStore + Clone + 'static,
{
    fn from(storage: S) -> Self {
        Self::new(storage)
    }
}
