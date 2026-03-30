//! Storage metadata models.

use reth_codecs::{Compact, add_arbitrary_tests};
use serde::{Deserialize, Serialize};

/// Storage configuration settings for this node.
///
/// Storage configuration settings for this node.
///
/// This is retained for compatibility with call sites that still thread storage settings through
/// APIs, but currently always resolves to the canonical v2 layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Compact, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "arbitrary"), derive(arbitrary::Arbitrary))]
#[add_arbitrary_tests(compact)]
pub struct StorageSettings {
    /// Whether this node uses v2 storage layout.
    ///
    /// Canonical builds always set this to `true`.
    pub storage_v2: bool,
}

impl StorageSettings {
    /// Returns canonical storage settings.
    pub const fn base() -> Self {
        Self::v2()
    }

    /// Creates `StorageSettings` for v2 nodes with all storage features enabled:
    /// - Receipts and transaction senders in static files
    /// - History indices in `RocksDB` (storages, accounts, transaction hashes)
    /// - Account and storage changesets in static files
    /// - Hashed state as canonical state representation
    ///
    /// Use this when the `--storage.v2` CLI flag is set.
    pub const fn v2() -> Self {
        Self { storage_v2: true }
    }

    /// Creates legacy `StorageSettings`.
    ///
    /// Legacy mode is no longer supported; this returns canonical v2 settings.
    pub const fn v1() -> Self {
        Self::v2()
    }

    /// Returns `true` if this node uses v2 storage layout.
    pub const fn is_v2(&self) -> bool {
        self.storage_v2
    }

    /// Whether receipts are stored in static files.
    pub const fn receipts_in_static_files(&self) -> bool {
        self.storage_v2
    }

    /// Whether transaction senders are stored in static files.
    pub const fn transaction_senders_in_static_files(&self) -> bool {
        self.storage_v2
    }

    /// Whether storages history is stored in `RocksDB`.
    pub const fn storages_history_in_rocksdb(&self) -> bool {
        self.storage_v2
    }

    /// Whether transaction hash numbers are stored in `RocksDB`.
    pub const fn transaction_hash_numbers_in_rocksdb(&self) -> bool {
        self.storage_v2
    }

    /// Whether account history is stored in `RocksDB`.
    pub const fn account_history_in_rocksdb(&self) -> bool {
        self.storage_v2
    }

    /// Whether to use hashed state tables (`HashedAccounts`/`HashedStorages`) as the canonical
    /// state representation instead of plain state tables. Implied by v2 storage layout.
    pub const fn use_hashed_state(&self) -> bool {
        self.storage_v2
    }

    /// Returns `true` if any tables are configured to be stored in `RocksDB`.
    pub const fn any_in_rocksdb(&self) -> bool {
        self.storage_v2
    }
}
