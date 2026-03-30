//! 2D-nonce-aware transaction pool for EIP-8130 AA transactions.
//!
//! Reth's standard pool uses `(sender, nonce_sequence)` as the identity key.
//! Two AA transactions sharing `(sender, nonce_sequence)` but with different
//! `nonce_key` values are both valid on-chain yet collide in that pool.
//!
//! This module provides an [`Eip8130Pool`] that stores AA transactions with
//! `nonce_key != 0` in a separate index keyed by the full 2D identity
//! `(sender, nonce_key, nonce_sequence)`. Transactions with `nonce_key == 0`
//! continue to use the standard pool, preserving compatibility with Reth's
//! existing ordering and nonce-gap logic.
//!
//! Modeled on Tempo's `AA2dPool` architecture.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_primitives::{Address, B256, U256};
use base_alloy_consensus::lock_slot;
use parking_lot::RwLock;
use reth_transaction_pool::{
    EthPoolTransaction, PoolTransaction, TransactionOrigin, ValidPoolTransaction,
    error::InvalidPoolTransactionError,
    identifier::{SenderId, TransactionId},
};

/// Identifies a nonce sequence lane: `(sender, nonce_key)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Eip8130SequenceId {
    /// Transaction sender.
    pub sender: Address,
    /// Nonce key (non-zero for 2D nonce lanes).
    pub nonce_key: U256,
}

/// Full 2D identity for an AA transaction.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Eip8130TxId {
    /// Transaction sender.
    pub sender: Address,
    /// Nonce key.
    pub nonce_key: U256,
    /// Nonce sequence within the key.
    pub nonce_sequence: u64,
}

impl Eip8130TxId {
    /// Returns the sequence lane this tx belongs to.
    pub fn sequence_id(&self) -> Eip8130SequenceId {
        Eip8130SequenceId { sender: self.sender, nonce_key: self.nonce_key }
    }
}

/// An entry stored in the pool alongside the full transaction.
///
/// No trait bounds on `T` at the struct level — bounds are only required on
/// impl blocks that interact with `ValidPoolTransaction` or `PoolTransaction`.
struct PooledEntry<T> {
    id: Eip8130TxId,
    transaction: T,
    origin: TransactionOrigin,
    timestamp: Instant,
}

impl<T: core::fmt::Debug> core::fmt::Debug for PooledEntry<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PooledEntry")
            .field("id", &self.id)
            .field("transaction", &self.transaction)
            .field("origin", &self.origin)
            .field("timestamp", &self.timestamp)
            .finish()
    }
}

/// Per-sequence state: on-chain nonce and ordered map of pending transactions.
struct SequenceState<T> {
    next_nonce: u64,
    pending: BTreeMap<u64, PooledEntry<T>>,
}

impl<T: core::fmt::Debug> core::fmt::Debug for SequenceState<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SequenceState")
            .field("next_nonce", &self.next_nonce)
            .field("pending", &self.pending)
            .finish()
    }
}

impl<T> Default for SequenceState<T> {
    fn default() -> Self {
        Self { next_nonce: 0, pending: BTreeMap::new() }
    }
}

/// Throughput tier for a sender, determined during validation from on-chain
/// state (account lock status + payer bytecode).
///
/// Each tier maps to a different per-sender transaction cap, configured via
/// [`Eip8130PoolConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum SenderThroughputTier {
    /// Sender is not locked — default throughput.
    #[default]
    Default,
    /// Sender account is locked — elevated throughput.
    Locked,
    /// Sender is locked and the payer has trusted bytecode — highest throughput.
    LockedTrustedPayer,
}

/// Configuration for the EIP-8130 2D nonce pool.
#[derive(Debug, Clone)]
pub struct Eip8130PoolConfig {
    /// Maximum pending transactions per sequence lane.
    pub max_txs_per_sequence: usize,
    /// Maximum total transactions in the pool.
    pub max_pool_size: usize,
    /// Per-sender cap when the sender is **not** locked.
    pub default_max_txs_per_sender: usize,
    /// Per-sender cap when the sender account is **locked**.
    pub locked_max_txs_per_sender: usize,
    /// Per-sender cap when the sender is locked **and** the payer has trusted
    /// bytecode.
    pub locked_trusted_payer_max_txs_per_sender: usize,
    /// How long a cached throughput tier remains valid before the pool
    /// re-evaluates on the next insertion. Account locks can change (e.g.
    /// an unlock request completing), so entries must expire periodically.
    pub tier_cache_ttl: Duration,
}

impl Default for Eip8130PoolConfig {
    fn default() -> Self {
        Self {
            max_txs_per_sequence: 16,
            max_pool_size: 4096,
            default_max_txs_per_sender: 16,
            locked_max_txs_per_sender: 64,
            locked_trusted_payer_max_txs_per_sender: 128,
            tier_cache_ttl: Duration::from_secs(300),
        }
    }
}

impl Eip8130PoolConfig {
    /// Returns the per-sender transaction cap for the given tier.
    pub fn max_txs_for_tier(&self, tier: SenderThroughputTier) -> usize {
        match tier {
            SenderThroughputTier::Default => self.default_max_txs_per_sender,
            SenderThroughputTier::Locked => self.locked_max_txs_per_sender,
            SenderThroughputTier::LockedTrustedPayer => {
                self.locked_trusted_payer_max_txs_per_sender
            }
        }
    }
}

/// Cached throughput tier with an expiry timestamp.
#[derive(Debug, Clone, Copy)]
struct CachedTier {
    tier: SenderThroughputTier,
    cached_at: Instant,
}

struct PoolInner<T> {
    sequences: HashMap<Eip8130SequenceId, SequenceState<T>>,
    by_hash: HashMap<B256, Eip8130TxId>,
    /// Reverse index: nonce storage slot → sequence ID.
    /// Populated at insertion time so that block-maintenance can map
    /// `NONCE_MANAGER_ADDRESS` storage diffs back to sequence lanes.
    slot_to_seq: HashMap<B256, Eip8130SequenceId>,
    /// Per-sender transaction count for DoS protection.
    txs_by_sender: HashMap<Address, usize>,
    /// Cached throughput tier per sender with TTL, populated on first
    /// insertion and invalidated when the sender's lock slot changes
    /// on-chain or when the TTL expires.
    sender_tiers: HashMap<Address, CachedTier>,
    /// Reverse map: lock storage slot → sender address. Used by the
    /// maintenance task to identify which senders need tier
    /// invalidation when `ACCOUNT_CONFIG_ADDRESS` lock slots change.
    lock_slot_to_sender: HashMap<B256, Address>,
}

impl<T: core::fmt::Debug> core::fmt::Debug for PoolInner<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PoolInner")
            .field("sequences", &self.sequences)
            .field("by_hash_len", &self.by_hash.len())
            .field("slot_to_seq_len", &self.slot_to_seq.len())
            .field("txs_by_sender_len", &self.txs_by_sender.len())
            .finish()
    }
}

impl<T> Default for PoolInner<T> {
    fn default() -> Self {
        Self {
            sequences: HashMap::new(),
            by_hash: HashMap::new(),
            slot_to_seq: HashMap::new(),
            txs_by_sender: HashMap::new(),
            sender_tiers: HashMap::new(),
            lock_slot_to_sender: HashMap::new(),
        }
    }
}

/// A 2D-nonce-aware pool for EIP-8130 transactions with `nonce_key != 0`.
///
/// Thread-safe via interior `RwLock`. All public methods acquire the lock
/// internally, so callers do not need external synchronization.
///
/// The type parameter `T` is the pool transaction type (e.g.
/// [`BasePooledTransaction`](crate::BasePooledTransaction)). No trait bounds
/// are required on the struct itself, only on the methods that need them.
pub struct Eip8130Pool<T> {
    inner: RwLock<PoolInner<T>>,
    config: Eip8130PoolConfig,
}

impl<T: core::fmt::Debug> core::fmt::Debug for Eip8130Pool<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Eip8130Pool")
            .field("config", &self.config)
            .field("inner", &*self.inner.read())
            .finish()
    }
}

impl<T> Default for Eip8130Pool<T> {
    fn default() -> Self {
        Self::with_config(Eip8130PoolConfig::default())
    }
}

/// Returns `true` if a transaction with the given nonce key would be routed
/// to the 2D nonce pool (i.e. `nonce_key != 0`).
pub fn is_2d_nonce(nonce_key: U256) -> bool {
    !nonce_key.is_zero()
}

impl<T> Eip8130Pool<T> {
    /// Creates an empty pool.
    pub fn new() -> Self {
        Self::with_config(Eip8130PoolConfig::default())
    }

    /// Creates a pool with the given configuration.
    pub fn with_config(config: Eip8130PoolConfig) -> Self {
        Self { inner: RwLock::new(PoolInner::default()), config }
    }

    /// Returns a reference to the pool configuration.
    pub fn config(&self) -> &Eip8130PoolConfig {
        &self.config
    }

    /// Returns `true` if the pool contains a transaction with the given hash.
    pub fn contains(&self, hash: &B256) -> bool {
        self.inner.read().by_hash.contains_key(hash)
    }

    /// Returns the 2D identity for a transaction hash, if present.
    pub fn get_id(&self, hash: &B256) -> Option<Eip8130TxId> {
        self.inner.read().by_hash.get(hash).cloned()
    }

    /// Number of transactions currently in the pool.
    pub fn len(&self) -> usize {
        self.inner.read().by_hash.len()
    }

    /// Returns `true` if the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.read().by_hash.is_empty()
    }

    /// Looks up the sequence ID for a nonce storage slot, if tracked.
    pub fn seq_id_for_slot(&self, slot: &B256) -> Option<Eip8130SequenceId> {
        self.inner.read().slot_to_seq.get(slot).cloned()
    }

    /// Resets the cached throughput tier for a sender, forcing re-evaluation
    /// on the next insertion.
    ///
    /// Called by the invalidation task when the sender's lock slot changes
    /// on-chain (e.g. the account was unlocked).
    pub fn invalidate_sender_tier(&self, sender: &Address) {
        self.inner.write().sender_tiers.remove(sender);
    }

    /// Checks a set of changed `ACCOUNT_CONFIG_ADDRESS` storage slots against
    /// the pool's lock-slot reverse map and invalidates the cached throughput
    /// tier for any matching senders.
    ///
    /// Returns the number of senders whose tiers were invalidated.
    pub fn invalidate_tiers_for_lock_slots(&self, changed_slots: &[B256]) -> usize {
        let mut inner = self.inner.write();
        let mut invalidated = 0;
        for slot in changed_slots {
            if let Some(sender) = inner.lock_slot_to_sender.get(slot).copied() {
                if inner.sender_tiers.remove(&sender).is_some() {
                    invalidated += 1;
                }
            }
        }
        invalidated
    }

    /// Returns all transaction hashes in the pool.
    pub fn all_hashes(&self) -> Vec<B256> {
        self.inner.read().by_hash.keys().copied().collect()
    }

    /// Removes a transaction by hash. Returns the id if found.
    pub fn remove_transaction(&self, hash: &B256) -> Option<Eip8130TxId> {
        let mut inner = self.inner.write();
        Self::remove_from_inner(&mut inner, hash)
    }

    /// Removes multiple transactions by hash. Returns the hashes that were
    /// actually present.
    pub fn remove_transactions(&self, hashes: &[B256]) -> Vec<B256> {
        let mut inner = self.inner.write();
        hashes.iter().filter_map(|h| Self::remove_from_inner(&mut inner, h).map(|_| *h)).collect()
    }

    /// Updates the known on-chain nonce for a sequence lane and removes any
    /// transactions with `nonce_sequence < new_nonce`.
    ///
    /// Returns the hashes of pruned transactions.
    pub fn update_sequence_nonce(&self, seq_id: &Eip8130SequenceId, new_nonce: u64) -> Vec<B256>
    where
        T: PoolTransaction,
    {
        let mut inner = self.inner.write();
        let mut removed_hashes = Vec::new();

        if let Some(seq) = inner.sequences.get_mut(seq_id) {
            seq.next_nonce = new_nonce;
            let stale: Vec<u64> = seq.pending.range(..new_nonce).map(|(&nonce, _)| nonce).collect();
            for nonce in stale {
                if let Some(entry) = seq.pending.remove(&nonce) {
                    removed_hashes.push(*entry.transaction.hash());
                }
            }
        }

        for hash in &removed_hashes {
            inner.by_hash.remove(hash);
        }

        if !removed_hashes.is_empty() {
            Self::decrement_sender_count(&mut inner, &seq_id.sender, removed_hashes.len());
        }

        if inner.sequences.get(seq_id).is_some_and(|s| s.pending.is_empty()) {
            inner.sequences.remove(seq_id);
            inner.slot_to_seq.retain(|_, v| v != seq_id);
        }

        removed_hashes
    }

    fn remove_from_inner(inner: &mut PoolInner<T>, hash: &B256) -> Option<Eip8130TxId> {
        let id = inner.by_hash.remove(hash)?;
        let seq_id = id.sequence_id();
        inner.sequences.get_mut(&seq_id)?.pending.remove(&id.nonce_sequence);
        Self::decrement_sender_count(inner, &id.sender, 1);
        if inner.sequences.get(&seq_id).is_some_and(|s| s.pending.is_empty()) {
            inner.sequences.remove(&seq_id);
            inner.slot_to_seq.retain(|_, v| v != &seq_id);
        }
        Some(id)
    }

    /// Decrements the transaction count for a sender, removing the entry (and
    /// the cached tier) when it reaches zero.
    fn decrement_sender_count(inner: &mut PoolInner<T>, sender: &Address, n: usize) {
        use std::collections::hash_map::Entry;
        if let Entry::Occupied(mut entry) = inner.txs_by_sender.entry(*sender) {
            let count = entry.get_mut();
            *count = count.saturating_sub(n);
            if *count == 0 {
                entry.remove();
                inner.sender_tiers.remove(sender);
                inner.lock_slot_to_sender.remove(&lock_slot(*sender));
            }
        }
    }
}

impl<T: PoolTransaction> Eip8130Pool<T> {
    /// Attempts to add a validated transaction to the pool.
    ///
    /// The caller must provide the `nonce_storage_slot` (output of
    /// `nonce_slot(sender, nonce_key)`) so the pool can build the reverse
    /// index used during block-maintenance nonce updates.
    pub fn add_transaction(
        &self,
        id: Eip8130TxId,
        transaction: T,
        origin: TransactionOrigin,
        nonce_storage_slot: B256,
        tier: SenderThroughputTier,
    ) -> Result<(), Eip8130PoolError> {
        let hash = *transaction.hash();
        let mut inner = self.inner.write();

        if inner.by_hash.contains_key(&hash) {
            return Err(Eip8130PoolError::DuplicateHash(hash));
        }

        if inner.by_hash.len() >= self.config.max_pool_size {
            return Err(Eip8130PoolError::PoolFull);
        }

        let sender = id.sender;
        let now = Instant::now();

        // Check cached tier — if it exists and hasn't expired, use the
        // max of cached vs incoming tier. Otherwise, start fresh from
        // the tier provided by the validator.
        let cached_tier = inner
            .sender_tiers
            .get(&sender)
            .filter(|c| now.duration_since(c.cached_at) < self.config.tier_cache_ttl)
            .map(|c| c.tier);

        let effective_tier = cached_tier.unwrap_or(SenderThroughputTier::Default).max(tier);
        inner.sender_tiers.insert(sender, CachedTier { tier: effective_tier, cached_at: now });
        inner.lock_slot_to_sender.entry(lock_slot(sender)).or_insert(sender);

        let sender_count = inner.txs_by_sender.get(&sender).copied().unwrap_or(0);
        if sender_count >= self.config.max_txs_for_tier(effective_tier) {
            return Err(Eip8130PoolError::SenderCapacityExceeded(sender));
        }

        let seq_id = id.sequence_id();
        let seq = inner.sequences.entry(seq_id.clone()).or_default();

        if seq.pending.len() >= self.config.max_txs_per_sequence {
            return Err(Eip8130PoolError::SequenceFull);
        }

        if seq.pending.contains_key(&id.nonce_sequence) {
            return Err(Eip8130PoolError::NonceAlreadyPending {
                sender,
                nonce_key: id.nonce_key,
                nonce_sequence: id.nonce_sequence,
            });
        }

        let entry = PooledEntry { id: id.clone(), transaction, origin, timestamp: Instant::now() };
        seq.pending.insert(id.nonce_sequence, entry);
        inner.by_hash.insert(hash, id);
        inner.slot_to_seq.entry(nonce_storage_slot).or_insert(seq_id);
        *inner.txs_by_sender.entry(sender).or_insert(0) += 1;

        Ok(())
    }

    /// Returns the validated pool transaction for the given hash, if present.
    pub fn get(&self, hash: &B256) -> Option<Arc<ValidPoolTransaction<T>>>
    where
        T: Clone,
    {
        let inner = self.inner.read();
        let id = inner.by_hash.get(hash)?;
        let seq_id = id.sequence_id();
        let entry = inner.sequences.get(&seq_id)?.pending.get(&id.nonce_sequence)?;
        Some(Self::wrap_entry(entry))
    }

    /// Returns `(pending, queued)` transaction counts.
    ///
    /// Pending = nonce_sequence forms a contiguous run from `next_nonce`.
    /// Queued = nonce_sequence has a gap relative to `next_nonce`.
    pub fn pending_and_queued_count(&self) -> (usize, usize) {
        let inner = self.inner.read();
        let mut pending = 0;
        let mut queued = 0;
        for seq in inner.sequences.values() {
            let mut next = seq.next_nonce;
            for &nonce in seq.pending.keys() {
                if nonce == next {
                    pending += 1;
                    next += 1;
                } else {
                    queued += 1;
                }
            }
        }
        (pending, queued)
    }

    /// Returns the count of transactions from a specific sender across all
    /// nonce-key lanes.
    pub fn sender_tx_count(&self, sender: &Address) -> usize {
        let inner = self.inner.read();
        inner
            .sequences
            .iter()
            .filter(|(seq_id, _)| &seq_id.sender == sender)
            .map(|(_, state)| state.pending.len())
            .sum()
    }

    /// Returns all transactions from a specific sender across all nonce lanes.
    pub fn get_transactions_by_sender(&self, sender: &Address) -> Vec<Arc<ValidPoolTransaction<T>>>
    where
        T: Clone,
    {
        let inner = self.inner.read();
        inner
            .sequences
            .iter()
            .filter(|(seq_id, _)| &seq_id.sender == sender)
            .flat_map(|(_, state)| state.pending.values().map(|e| Self::wrap_entry(e)))
            .collect()
    }

    /// Returns all pending (ready) transactions.
    pub fn pending_transactions(&self) -> Vec<Arc<ValidPoolTransaction<T>>>
    where
        T: Clone,
    {
        let inner = self.inner.read();
        let mut result = Vec::new();
        for seq in inner.sequences.values() {
            let mut next = seq.next_nonce;
            for (&nonce, entry) in &seq.pending {
                if nonce == next {
                    result.push(Self::wrap_entry(entry));
                    next += 1;
                } else {
                    break;
                }
            }
        }
        result
    }

    /// Returns all queued (not yet ready) transactions.
    pub fn queued_transactions(&self) -> Vec<Arc<ValidPoolTransaction<T>>>
    where
        T: Clone,
    {
        let inner = self.inner.read();
        let mut result = Vec::new();
        for seq in inner.sequences.values() {
            let mut next = seq.next_nonce;
            let mut in_gap = false;
            for (&nonce, entry) in &seq.pending {
                if nonce == next && !in_gap {
                    next += 1;
                } else {
                    in_gap = true;
                    result.push(Self::wrap_entry(entry));
                }
            }
        }
        result
    }

    /// Returns all validated transactions in the pool (regardless of readiness).
    pub fn all_transactions(&self) -> Vec<Arc<ValidPoolTransaction<T>>>
    where
        T: Clone,
    {
        let inner = self.inner.read();
        inner
            .sequences
            .values()
            .flat_map(|seq| seq.pending.values().map(|e| Self::wrap_entry(e)))
            .collect()
    }

    /// Wraps a pool entry in a `ValidPoolTransaction` for external consumption.
    fn wrap_entry(entry: &PooledEntry<T>) -> Arc<ValidPoolTransaction<T>>
    where
        T: Clone,
    {
        let sender_id_val =
            u64::from_be_bytes(entry.id.sender.as_slice()[..8].try_into().unwrap_or_default());
        Arc::new(ValidPoolTransaction {
            transaction: entry.transaction.clone(),
            transaction_id: TransactionId::new(
                SenderId::from(sender_id_val),
                entry.id.nonce_sequence,
            ),
            propagate: true,
            timestamp: entry.timestamp,
            origin: entry.origin,
            authority_ids: None,
        })
    }
}

impl<T: EthPoolTransaction + Clone> Eip8130Pool<T> {
    /// Snapshots the ready (executable) transactions across all sequences.
    ///
    /// A transaction is ready when its `nonce_sequence == next_nonce` for the
    /// sequence lane, forming a contiguous chain from the on-chain nonce.
    /// Results are sorted by effective priority (max_priority_fee descending).
    pub fn best_transactions(&self) -> BestEip8130Transactions<T> {
        let inner = self.inner.read();
        let mut ready = Vec::new();

        for seq in inner.sequences.values() {
            let mut next = seq.next_nonce;
            for (&nonce, entry) in &seq.pending {
                if nonce != next {
                    break;
                }
                ready.push(Self::wrap_entry(entry));
                next += 1;
            }
        }

        ready.sort_by(|a, b| {
            let a_prio = a.transaction.max_priority_fee_per_gas().unwrap_or_default();
            let b_prio = b.transaction.max_priority_fee_per_gas().unwrap_or_default();
            b_prio.cmp(&a_prio)
        });

        BestEip8130Transactions { ready: ready.into(), invalid: HashSet::new() }
    }
}

/// Iterator over ready 2D-nonce transactions, sorted by priority.
///
/// Implements [`reth_transaction_pool::BestTransactions`] so it can be merged
/// with the standard pool's iterator during block building.
pub struct BestEip8130Transactions<T: PoolTransaction> {
    ready: VecDeque<Arc<ValidPoolTransaction<T>>>,
    invalid: HashSet<Address>,
}

impl<T: PoolTransaction> core::fmt::Debug for BestEip8130Transactions<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BestEip8130Transactions")
            .field("ready_len", &self.ready.len())
            .field("invalid_len", &self.invalid.len())
            .finish()
    }
}

impl<T: EthPoolTransaction> Iterator for BestEip8130Transactions<T> {
    type Item = Arc<ValidPoolTransaction<T>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let tx = self.ready.pop_front()?;
            if self.invalid.contains(&tx.sender()) {
                continue;
            }
            return Some(tx);
        }
    }
}

impl<T: EthPoolTransaction> reth_transaction_pool::BestTransactions for BestEip8130Transactions<T> {
    fn mark_invalid(&mut self, transaction: &Self::Item, _kind: &InvalidPoolTransactionError) {
        self.invalid.insert(transaction.sender());
    }

    fn no_updates(&mut self) {}

    fn skip_blobs(&mut self) {}

    fn set_skip_blobs(&mut self, _skip: bool) {}
}

/// Errors returned by [`Eip8130Pool::add_transaction`].
#[derive(Debug, Clone)]
pub enum Eip8130PoolError {
    /// Transaction hash already exists in the pool.
    DuplicateHash(B256),
    /// The sequence lane `(sender, nonce_key)` has too many pending transactions.
    SequenceFull,
    /// A transaction with the same 2D nonce is already pending.
    NonceAlreadyPending {
        /// Sender address.
        sender: Address,
        /// Nonce key.
        nonce_key: U256,
        /// Nonce sequence within the key.
        nonce_sequence: u64,
    },
    /// Pool has reached its maximum capacity.
    PoolFull,
    /// Sender already has the maximum number of pending transactions across
    /// all nonce-key lanes.
    SenderCapacityExceeded(Address),
}

impl core::fmt::Display for Eip8130PoolError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DuplicateHash(hash) => write!(f, "duplicate transaction hash {hash}"),
            Self::SequenceFull => write!(f, "sequence lane is full"),
            Self::NonceAlreadyPending { sender, nonce_key, nonce_sequence } => write!(
                f,
                "nonce already pending: sender={sender}, nonce_key={nonce_key}, \
                 nonce_sequence={nonce_sequence}"
            ),
            Self::PoolFull => write!(f, "2D nonce pool is full"),
            Self::SenderCapacityExceeded(sender) => {
                write!(f, "sender {sender} exceeded per-sender capacity")
            }
        }
    }
}

impl std::error::Error for Eip8130PoolError {}

impl reth_transaction_pool::error::PoolTransactionError for Eip8130PoolError {
    fn is_bad_transaction(&self) -> bool {
        false
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Shared handle to an [`Eip8130Pool`].
pub type SharedEip8130Pool<T> = Arc<Eip8130Pool<T>>;

#[cfg(test)]
mod tests {
    use alloy_consensus::TxEip1559;
    use alloy_primitives::{Signature, TxKind};
    use base_alloy_consensus::OpTypedTransaction;
    use base_execution_primitives::OpTransactionSigned;
    use reth_primitives_traits::Recovered;
    use reth_transaction_pool::EthPoolTransaction;

    use super::*;
    use crate::BasePooledTransaction;

    type TestPool = Eip8130Pool<BasePooledTransaction>;

    fn cfg() -> Eip8130PoolConfig {
        Eip8130PoolConfig::default()
    }

    fn make_id(sender_byte: u8, nonce_key: u64, nonce_sequence: u64) -> Eip8130TxId {
        Eip8130TxId {
            sender: Address::repeat_byte(sender_byte),
            nonce_key: U256::from(nonce_key),
            nonce_sequence,
        }
    }

    fn make_slot(sender_byte: u8, nonce_key: u64) -> B256 {
        let mut buf = [0u8; 32];
        buf[0] = sender_byte;
        buf[24..32].copy_from_slice(&nonce_key.to_be_bytes());
        B256::from(buf)
    }

    fn make_tx(sender_byte: u8, nonce: u64, priority_fee: u128) -> BasePooledTransaction {
        let sender = Address::repeat_byte(sender_byte);
        let tx = TxEip1559 {
            chain_id: 1,
            nonce,
            gas_limit: 21_000,
            max_fee_per_gas: 1000,
            max_priority_fee_per_gas: priority_fee,
            to: TxKind::Call(Address::repeat_byte(0xFF)),
            value: U256::ZERO,
            access_list: Default::default(),
            input: Default::default(),
        };
        let sig = Signature::new(
            U256::from(sender_byte as u64 * 1000 + nonce),
            U256::from(priority_fee),
            false,
        );
        let signed = OpTransactionSigned::new_unhashed(OpTypedTransaction::Eip1559(tx), sig);
        let recovered = Recovered::new_unchecked(signed, sender);
        let len = recovered.encoded_2718_len();
        BasePooledTransaction::new(recovered, len)
    }

    // ------------------------------------------------------------------ //
    //  Basic identity / routing
    // ------------------------------------------------------------------ //

    #[test]
    fn is_2d_nonce_routing() {
        assert!(!is_2d_nonce(U256::ZERO));
        assert!(is_2d_nonce(U256::from(1)));
        assert!(is_2d_nonce(U256::from(u64::MAX)));
    }

    #[test]
    fn seq_id_construction() {
        let id = make_id(0x01, 1, 5);
        let seq = id.sequence_id();
        assert_eq!(seq.sender, Address::repeat_byte(0x01));
        assert_eq!(seq.nonce_key, U256::from(1));
    }

    // ------------------------------------------------------------------ //
    //  Empty pool
    // ------------------------------------------------------------------ //

    #[test]
    fn empty_pool_properties() {
        let pool = TestPool::new();
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
        assert_eq!(pool.pending_and_queued_count(), (0, 0));
        assert!(pool.all_hashes().is_empty());
    }

    // ------------------------------------------------------------------ //
    //  add_transaction
    // ------------------------------------------------------------------ //

    #[test]
    fn add_single_transaction() {
        let pool = TestPool::new();
        let id = make_id(0x01, 1, 0);
        let tx = make_tx(0x01, 0, 10);
        let hash = *tx.hash();
        let slot = make_slot(0x01, 1);

        pool.add_transaction(
            id,
            tx,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Default,
        )
        .unwrap();

        assert_eq!(pool.len(), 1);
        assert!(pool.contains(&hash));
        assert!(!pool.is_empty());
    }

    #[test]
    fn add_duplicate_hash_rejected() {
        let pool = TestPool::new();
        let tx = make_tx(0x01, 0, 10);
        let id = make_id(0x01, 1, 0);
        let slot = make_slot(0x01, 1);

        pool.add_transaction(
            id.clone(),
            tx.clone(),
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Default,
        )
        .unwrap();

        let result = pool.add_transaction(
            id,
            tx,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Default,
        );
        assert!(matches!(result, Err(Eip8130PoolError::DuplicateHash(_))));
    }

    #[test]
    fn add_nonce_collision_rejected() {
        let pool = TestPool::new();
        let slot = make_slot(0x01, 1);

        let tx1 = make_tx(0x01, 0, 10);
        let id1 = make_id(0x01, 1, 0);
        pool.add_transaction(
            id1,
            tx1,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Default,
        )
        .unwrap();

        let tx2 = make_tx(0x01, 100, 20);
        let id2 = make_id(0x01, 1, 0);
        let result = pool.add_transaction(
            id2,
            tx2,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Default,
        );
        assert!(matches!(result, Err(Eip8130PoolError::NonceAlreadyPending { .. })));
    }

    #[test]
    fn sequence_full_rejected() {
        let c = cfg();
        let pool = TestPool::new();
        let slot = make_slot(0x01, 1);

        for seq in 0..c.max_txs_per_sequence as u64 {
            let tx = make_tx(0x01, seq, 10);
            let id = make_id(0x01, 1, seq);
            pool.add_transaction(
                id,
                tx,
                TransactionOrigin::External,
                slot,
                SenderThroughputTier::Default,
            )
            .unwrap();
        }

        let tx = make_tx(0x01, c.max_txs_per_sequence as u64, 10);
        let id = make_id(0x01, 1, c.max_txs_per_sequence as u64);
        let result = pool.add_transaction(
            id,
            tx,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Default,
        );
        assert!(matches!(result, Err(Eip8130PoolError::SequenceFull)));
    }

    #[test]
    fn per_sender_limit_rejects_excess() {
        let c = cfg();
        let pool = TestPool::new();

        for key in 1..=c.default_max_txs_per_sender as u64 {
            let tx = make_tx(0x01, key, 10);
            let id = make_id(0x01, key, 0);
            let slot = make_slot(0x01, key);
            pool.add_transaction(
                id,
                tx,
                TransactionOrigin::External,
                slot,
                SenderThroughputTier::Default,
            )
            .unwrap();
        }

        let key = c.default_max_txs_per_sender as u64 + 1;
        let tx = make_tx(0x01, key, 10);
        let id = make_id(0x01, key, 0);
        let slot = make_slot(0x01, key);
        let result = pool.add_transaction(
            id,
            tx,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Default,
        );
        assert!(matches!(result, Err(Eip8130PoolError::SenderCapacityExceeded(_))));
    }

    #[test]
    fn per_sender_limit_freed_after_removal() {
        let c = cfg();
        let pool = TestPool::new();

        let mut hashes = Vec::new();
        for key in 1..=c.default_max_txs_per_sender as u64 {
            let tx = make_tx(0x01, key, 10);
            hashes.push(*tx.hash());
            let id = make_id(0x01, key, 0);
            let slot = make_slot(0x01, key);
            pool.add_transaction(
                id,
                tx,
                TransactionOrigin::External,
                slot,
                SenderThroughputTier::Default,
            )
            .unwrap();
        }

        pool.remove_transaction(&hashes[0]);

        let key = c.default_max_txs_per_sender as u64 + 1;
        let tx = make_tx(0x01, key, 10);
        let id = make_id(0x01, key, 0);
        let slot = make_slot(0x01, key);
        pool.add_transaction(
            id,
            tx,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Default,
        )
        .expect("should succeed after freeing a slot");
    }

    #[test]
    fn per_sender_limit_independent_across_senders() {
        let c = cfg();
        let pool = TestPool::new();

        for key in 1..=c.default_max_txs_per_sender as u64 {
            let tx = make_tx(0x01, key, 10);
            let id = make_id(0x01, key, 0);
            let slot = make_slot(0x01, key);
            pool.add_transaction(
                id,
                tx,
                TransactionOrigin::External,
                slot,
                SenderThroughputTier::Default,
            )
            .unwrap();
        }

        let tx = make_tx(0x02, 1, 10);
        let id = make_id(0x02, 1, 0);
        let slot = make_slot(0x02, 1);
        pool.add_transaction(
            id,
            tx,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Default,
        )
        .expect("different sender should not be affected");
    }

    // ------------------------------------------------------------------ //
    //  Throughput tiers
    // ------------------------------------------------------------------ //

    #[test]
    fn locked_tier_allows_more_than_default() {
        let c = cfg();
        let pool = TestPool::new();

        for key in 1..=c.default_max_txs_per_sender as u64 {
            let tx = make_tx(0x01, key, 10);
            let id = make_id(0x01, key, 0);
            let slot = make_slot(0x01, key);
            pool.add_transaction(
                id,
                tx,
                TransactionOrigin::External,
                slot,
                SenderThroughputTier::Locked,
            )
            .unwrap();
        }

        let key = c.default_max_txs_per_sender as u64 + 1;
        let tx = make_tx(0x01, key, 10);
        let id = make_id(0x01, key, 0);
        let slot = make_slot(0x01, key);
        pool.add_transaction(
            id,
            tx,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Locked,
        )
        .expect("locked sender should accept more than the default limit");
    }

    #[test]
    fn tier_promotes_to_highest_seen() {
        let c = cfg();
        let pool = TestPool::new();

        let tx1 = make_tx(0x01, 1, 10);
        let id1 = make_id(0x01, 1, 0);
        let slot1 = make_slot(0x01, 1);
        pool.add_transaction(
            id1,
            tx1,
            TransactionOrigin::External,
            slot1,
            SenderThroughputTier::Default,
        )
        .unwrap();

        for key in 2..=c.default_max_txs_per_sender as u64 + 1 {
            let tx = make_tx(0x01, key, 10);
            let id = make_id(0x01, key, 0);
            let slot = make_slot(0x01, key);
            pool.add_transaction(
                id,
                tx,
                TransactionOrigin::External,
                slot,
                SenderThroughputTier::Locked,
            )
            .unwrap();
        }

        assert!(
            pool.len() > c.default_max_txs_per_sender,
            "tier promotion should allow exceeding default limit"
        );
    }

    #[test]
    fn tier_defaults_ordered() {
        let c = cfg();
        assert!(c.default_max_txs_per_sender < c.locked_max_txs_per_sender);
        assert!(c.locked_max_txs_per_sender < c.locked_trusted_payer_max_txs_per_sender);
    }

    #[test]
    fn pool_full_rejected() {
        let c = cfg();
        let pool = TestPool::new();

        for i in 0..c.max_pool_size {
            let sender = (i / 256) as u8;
            let nonce = (i % 256) as u64;
            let key = nonce + 1;
            let tx = make_tx(sender, i as u64, 10);
            let id = make_id(sender, key, 0);
            let slot = make_slot(sender, key);
            pool.add_transaction(
                id,
                tx,
                TransactionOrigin::External,
                slot,
                SenderThroughputTier::Default,
            )
            .unwrap();
        }

        assert_eq!(pool.len(), c.max_pool_size);
        let tx = make_tx(0xFF, 9999, 10);
        let id = make_id(0xFF, 9999, 0);
        let slot = make_slot(0xFF, 9999);
        let result = pool.add_transaction(
            id,
            tx,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Default,
        );
        assert!(matches!(result, Err(Eip8130PoolError::PoolFull)));
    }

    #[test]
    fn tier_cache_expires() {
        let config = Eip8130PoolConfig {
            tier_cache_ttl: Duration::from_millis(50),
            ..Eip8130PoolConfig::default()
        };
        let pool = Eip8130Pool::<BasePooledTransaction>::with_config(config.clone());

        let tx = make_tx(0x01, 1, 10);
        let id = make_id(0x01, 1, 0);
        let slot = make_slot(0x01, 1);
        pool.add_transaction(
            id,
            tx,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Locked,
        )
        .unwrap();

        {
            let inner = pool.inner.read();
            assert_eq!(
                inner.sender_tiers.get(&Address::repeat_byte(0x01)).unwrap().tier,
                SenderThroughputTier::Locked,
            );
        }

        std::thread::sleep(Duration::from_millis(60));

        let tx2 = make_tx(0x01, 2, 10);
        let id2 = make_id(0x01, 2, 0);
        let slot2 = make_slot(0x01, 2);
        pool.add_transaction(
            id2,
            tx2,
            TransactionOrigin::External,
            slot2,
            SenderThroughputTier::Default,
        )
        .unwrap();

        {
            let inner = pool.inner.read();
            assert_eq!(
                inner.sender_tiers.get(&Address::repeat_byte(0x01)).unwrap().tier,
                SenderThroughputTier::Default,
                "after TTL expiry, the lower tier from the new tx should take effect"
            );
        }
    }

    // ------------------------------------------------------------------ //
    //  remove_transaction
    // ------------------------------------------------------------------ //

    #[test]
    fn remove_transaction_cleans_up() {
        let pool = TestPool::new();
        let tx = make_tx(0x01, 0, 10);
        let hash = *tx.hash();
        let id = make_id(0x01, 1, 0);
        let slot = make_slot(0x01, 1);

        pool.add_transaction(
            id,
            tx,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Default,
        )
        .unwrap();
        assert_eq!(pool.len(), 1);

        let removed_id = pool.remove_transaction(&hash);
        assert!(removed_id.is_some());
        assert!(pool.is_empty());
        assert!(!pool.contains(&hash));

        let inner = pool.inner.read();
        assert!(inner.slot_to_seq.is_empty(), "slot_to_seq should be cleaned up");
    }

    #[test]
    fn remove_nonexistent_returns_none() {
        let pool = TestPool::new();
        assert!(pool.remove_transaction(&B256::ZERO).is_none());
    }

    #[test]
    fn remove_transactions_batch() {
        let pool = TestPool::new();
        let slot = make_slot(0x01, 1);
        let mut hashes = Vec::new();

        for seq in 0..3u64 {
            let tx = make_tx(0x01, seq, 10);
            hashes.push(*tx.hash());
            let id = make_id(0x01, 1, seq);
            pool.add_transaction(
                id,
                tx,
                TransactionOrigin::External,
                slot,
                SenderThroughputTier::Default,
            )
            .unwrap();
        }

        let removed = pool.remove_transactions(&hashes[..2]);
        assert_eq!(removed.len(), 2);
        assert_eq!(pool.len(), 1);
        assert!(pool.contains(&hashes[2]));
    }

    // ------------------------------------------------------------------ //
    //  update_sequence_nonce
    // ------------------------------------------------------------------ //

    #[test]
    fn update_sequence_nonce_prunes_stale() {
        let pool = TestPool::new();
        let slot = make_slot(0x01, 1);

        for seq in 0..5u64 {
            let tx = make_tx(0x01, seq, 10);
            let id = make_id(0x01, 1, seq);
            pool.add_transaction(
                id,
                tx,
                TransactionOrigin::External,
                slot,
                SenderThroughputTier::Default,
            )
            .unwrap();
        }
        assert_eq!(pool.len(), 5);

        let seq_id =
            Eip8130SequenceId { sender: Address::repeat_byte(0x01), nonce_key: U256::from(1) };
        let pruned = pool.update_sequence_nonce(&seq_id, 3);
        assert_eq!(pruned.len(), 3);
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn update_sequence_nonce_removes_empty_sequence() {
        let pool = TestPool::new();
        let slot = make_slot(0x01, 1);

        let tx = make_tx(0x01, 0, 10);
        let id = make_id(0x01, 1, 0);
        pool.add_transaction(
            id,
            tx,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Default,
        )
        .unwrap();

        let seq_id =
            Eip8130SequenceId { sender: Address::repeat_byte(0x01), nonce_key: U256::from(1) };
        pool.update_sequence_nonce(&seq_id, 1);
        assert!(pool.is_empty());

        let inner = pool.inner.read();
        assert!(inner.sequences.is_empty());
        assert!(inner.slot_to_seq.is_empty());
    }

    // ------------------------------------------------------------------ //
    //  pending / queued classification
    // ------------------------------------------------------------------ //

    #[test]
    fn pending_and_queued_with_gap() {
        let pool = TestPool::new();
        let slot = make_slot(0x01, 1);

        for seq in [0, 1, 3, 4] {
            let tx = make_tx(0x01, seq, 10);
            let id = make_id(0x01, 1, seq);
            pool.add_transaction(
                id,
                tx,
                TransactionOrigin::External,
                slot,
                SenderThroughputTier::Default,
            )
            .unwrap();
        }

        let (pending, queued) = pool.pending_and_queued_count();
        assert_eq!(pending, 2, "nonces 0,1 are contiguous from next_nonce=0");
        assert_eq!(queued, 2, "nonces 3,4 have a gap");
    }

    // ------------------------------------------------------------------ //
    //  best_transactions
    // ------------------------------------------------------------------ //

    #[test]
    fn best_transactions_ordered_by_priority() {
        let pool = TestPool::new();

        let tx_low = make_tx(0x01, 0, 5);
        let id_low = make_id(0x01, 1, 0);
        let slot1 = make_slot(0x01, 1);
        pool.add_transaction(
            id_low,
            tx_low,
            TransactionOrigin::External,
            slot1,
            SenderThroughputTier::Default,
        )
        .unwrap();

        let tx_high = make_tx(0x02, 0, 50);
        let id_high = make_id(0x02, 2, 0);
        let slot2 = make_slot(0x02, 2);
        pool.add_transaction(
            id_high,
            tx_high,
            TransactionOrigin::External,
            slot2,
            SenderThroughputTier::Default,
        )
        .unwrap();

        let mut best = pool.best_transactions();
        let first = best.next().unwrap();
        let second = best.next().unwrap();

        let first_prio = first.max_priority_fee_per_gas().unwrap_or_default();
        let second_prio = second.max_priority_fee_per_gas().unwrap_or_default();
        assert!(first_prio >= second_prio, "first={first_prio}, second={second_prio}");
        assert!(best.next().is_none());
    }

    #[test]
    fn best_transactions_skips_gapped() {
        let pool = TestPool::new();
        let slot = make_slot(0x01, 1);

        let tx = make_tx(0x01, 2, 10);
        let id = make_id(0x01, 1, 2);
        pool.add_transaction(
            id,
            tx,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Default,
        )
        .unwrap();

        let mut best = pool.best_transactions();
        assert!(best.next().is_none(), "nonce 2 has a gap from next_nonce=0");
    }

    #[test]
    fn best_transactions_mark_invalid_skips_sender() {
        let pool = TestPool::new();

        let tx1 = make_tx(0x01, 0, 50);
        let id1 = make_id(0x01, 1, 0);
        let slot1 = make_slot(0x01, 1);
        pool.add_transaction(
            id1,
            tx1,
            TransactionOrigin::External,
            slot1,
            SenderThroughputTier::Default,
        )
        .unwrap();

        let tx2 = make_tx(0x01, 1, 40);
        let id2 = make_id(0x01, 1, 1);
        pool.add_transaction(
            id2,
            tx2,
            TransactionOrigin::External,
            slot1,
            SenderThroughputTier::Default,
        )
        .unwrap();

        let tx3 = make_tx(0x02, 0, 30);
        let id3 = make_id(0x02, 2, 0);
        let slot2 = make_slot(0x02, 2);
        pool.add_transaction(
            id3,
            tx3,
            TransactionOrigin::External,
            slot2,
            SenderThroughputTier::Default,
        )
        .unwrap();

        let mut best = pool.best_transactions();
        let first = best.next().unwrap();
        assert_eq!(first.sender(), Address::repeat_byte(0x01));

        use reth_transaction_pool::BestTransactions;
        let err = InvalidPoolTransactionError::Other(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            "test",
        )));
        best.mark_invalid(&first, &err);

        let next = best.next().unwrap();
        assert_eq!(
            next.sender(),
            Address::repeat_byte(0x02),
            "should skip remaining txs from sender 0x01"
        );
        assert!(best.next().is_none());
    }

    // ------------------------------------------------------------------ //
    //  slot_to_seq reverse index
    // ------------------------------------------------------------------ //

    #[test]
    fn slot_to_seq_lookup() {
        let pool = TestPool::new();
        let slot = make_slot(0x01, 1);
        let tx = make_tx(0x01, 0, 10);
        let id = make_id(0x01, 1, 0);

        pool.add_transaction(
            id,
            tx,
            TransactionOrigin::External,
            slot,
            SenderThroughputTier::Default,
        )
        .unwrap();

        let seq_id = pool.seq_id_for_slot(&slot).unwrap();
        assert_eq!(seq_id.sender, Address::repeat_byte(0x01));
        assert_eq!(seq_id.nonce_key, U256::from(1));
    }

    // ------------------------------------------------------------------ //
    //  sender queries
    // ------------------------------------------------------------------ //

    #[test]
    fn sender_tx_count_across_lanes() {
        let pool = TestPool::new();

        let tx1 = make_tx(0x01, 0, 10);
        let id1 = make_id(0x01, 1, 0);
        let slot1 = make_slot(0x01, 1);
        pool.add_transaction(
            id1,
            tx1,
            TransactionOrigin::External,
            slot1,
            SenderThroughputTier::Default,
        )
        .unwrap();

        let tx2 = make_tx(0x01, 1, 10);
        let id2 = make_id(0x01, 2, 0);
        let slot2 = make_slot(0x01, 2);
        pool.add_transaction(
            id2,
            tx2,
            TransactionOrigin::External,
            slot2,
            SenderThroughputTier::Default,
        )
        .unwrap();

        assert_eq!(pool.sender_tx_count(&Address::repeat_byte(0x01)), 2);
        assert_eq!(pool.sender_tx_count(&Address::repeat_byte(0x02)), 0);
    }
}
