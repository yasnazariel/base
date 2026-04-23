//! Bundle lifecycle events for audit tracking.

use alloy_consensus::transaction::{SignerRecoverable, Transaction as ConsensusTransaction};
use alloy_primitives::{B256, TxHash, U256, keccak256};
use base_bundles::{AcceptedBundle, BundleExtensions};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::{BundleId, DropReason, TransactionId};

/// Bundle lifecycle event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum BundleEvent {
    /// Bundle was received.
    Received {
        /// Bundle identifier.
        bundle_id: BundleId,
        /// The accepted bundle.
        bundle: Box<AcceptedBundle>,
    },
    /// Bundle was cancelled.
    Cancelled {
        /// Bundle identifier.
        bundle_id: BundleId,
    },
    /// Bundle was included by a builder.
    BuilderIncluded {
        /// Bundle identifier.
        bundle_id: BundleId,
        /// Builder identifier.
        builder: String,
        /// Block number.
        block_number: u64,
        /// Flashblock index.
        flashblock_index: u64,
    },
    /// Bundle was included in a block.
    BlockIncluded {
        /// Bundle identifier.
        bundle_id: BundleId,
        /// Block number.
        block_number: u64,
        /// Block hash.
        block_hash: TxHash,
    },
    /// Bundle was dropped.
    Dropped {
        /// Bundle identifier.
        bundle_id: BundleId,
        /// Drop reason.
        reason: DropReason,
    },
    /// Transaction was forwarded from mempool to builder.
    MempoolForwarded {
        /// Transaction hash.
        tx_hash: B256,
    },
    /// Transaction was dropped from mempool forwarding.
    MempoolDropped {
        /// Transaction hash.
        tx_hash: B256,
    },
}

impl BundleEvent {
    /// Returns the bundle ID for this event.
    ///
    /// For mempool events, derives a synthetic UUID from the transaction hash.
    pub fn bundle_id(&self) -> BundleId {
        match self {
            Self::Received { bundle_id, .. }
            | Self::Cancelled { bundle_id, .. }
            | Self::BuilderIncluded { bundle_id, .. }
            | Self::BlockIncluded { bundle_id, .. }
            | Self::Dropped { bundle_id, .. } => *bundle_id,
            Self::MempoolForwarded { tx_hash } | Self::MempoolDropped { tx_hash } => {
                Uuid::new_v5(&Uuid::NAMESPACE_OID, tx_hash.as_slice())
            }
        }
    }

    /// Returns transaction IDs from this event (only for Received events).
    pub fn transaction_ids(&self) -> Vec<TransactionId> {
        match self {
            Self::Received { bundle, .. } => bundle
                .txs
                .iter()
                .filter_map(|envelope| {
                    envelope.recover_signer().ok().map(|sender| TransactionId {
                        sender,
                        nonce: U256::from(envelope.nonce()),
                        hash: *envelope.hash(),
                    })
                })
                .collect(),
            Self::Cancelled { .. }
            | Self::BuilderIncluded { .. }
            | Self::BlockIncluded { .. }
            | Self::Dropped { .. }
            | Self::MempoolForwarded { .. }
            | Self::MempoolDropped { .. } => vec![],
        }
    }

    /// Returns the `bundle_hash` for events that carry bundle data.
    ///
    /// For mempool events, computes `keccak256(tx_hash)` to match the
    /// single-element bundle hash computation.
    pub fn bundle_hash(&self) -> Option<B256> {
        match self {
            Self::Received { bundle, .. } => Some(bundle.bundle_hash()),
            Self::MempoolForwarded { tx_hash } | Self::MempoolDropped { tx_hash } => {
                Some(keccak256(tx_hash.as_slice()))
            }
            Self::Cancelled { .. }
            | Self::BuilderIncluded { .. }
            | Self::BlockIncluded { .. }
            | Self::Dropped { .. } => None,
        }
    }

    /// Generates the event key used as both the Kafka message key and S3 object name.
    ///
    /// For `Received` events, derived from `bundle_hash` so that the same
    /// bundle on different ingress pods produces the same key.
    ///
    /// For mempool events, uses `mpool-received-{bundle_hash}` format.
    pub fn generate_event_key(&self) -> String {
        match self {
            Self::Received { bundle, .. } => {
                let hash = bundle.bundle_hash();
                format!("received-{hash}")
            }
            Self::BlockIncluded { bundle_id, block_hash, .. } => {
                format!("block-included-{bundle_id}-{block_hash}")
            }
            Self::MempoolForwarded { tx_hash } | Self::MempoolDropped { tx_hash } => {
                let hash = keccak256(tx_hash.as_slice());
                format!("mpool-received-{hash}")
            }
            Self::Cancelled { .. } | Self::BuilderIncluded { .. } | Self::Dropped { .. } => {
                let id = self.bundle_id();
                let event_type = match self {
                    Self::Cancelled { .. } => "cancelled",
                    Self::BuilderIncluded { .. } => "builder-included",
                    Self::Dropped { .. } => "dropped",
                    _ => unreachable!(),
                };
                format!("{event_type}-{id}")
            }
        }
    }

    /// Returns the full S3 key for this event: `bundles/{prefix}/{event_key}`.
    pub fn s3_event_key(&self) -> String {
        let prefix = match self {
            Self::Received { bundle, .. } => format!("{}", bundle.bundle_hash()),
            Self::MempoolForwarded { tx_hash } | Self::MempoolDropped { tx_hash } => {
                format!("{}", keccak256(tx_hash.as_slice()))
            }
            Self::Cancelled { .. }
            | Self::BuilderIncluded { .. }
            | Self::BlockIncluded { .. }
            | Self::Dropped { .. } => format!("{}", self.bundle_id()),
        };
        format!("bundles/{prefix}/{}", self.generate_event_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mempool_forwarded_bundle_id() {
        let tx_hash = B256::from([1u8; 32]);
        let event = BundleEvent::MempoolForwarded { tx_hash };
        let id = event.bundle_id();
        assert_eq!(id, Uuid::new_v5(&Uuid::NAMESPACE_OID, tx_hash.as_slice()));
    }

    #[test]
    fn test_mempool_forwarded_bundle_hash() {
        let tx_hash = B256::from([1u8; 32]);
        let event = BundleEvent::MempoolForwarded { tx_hash };
        let hash = event.bundle_hash();
        assert_eq!(hash, Some(keccak256(tx_hash.as_slice())));
    }

    #[test]
    fn test_mempool_forwarded_event_key() {
        let tx_hash = B256::from([1u8; 32]);
        let event = BundleEvent::MempoolForwarded { tx_hash };
        let key = event.generate_event_key();
        let expected_hash = keccak256(tx_hash.as_slice());
        assert_eq!(key, format!("mpool-received-{expected_hash}"));
    }

    #[test]
    fn test_mempool_forwarded_s3_key() {
        let tx_hash = B256::from([1u8; 32]);
        let event = BundleEvent::MempoolForwarded { tx_hash };
        let s3_key = event.s3_event_key();
        let expected_hash = keccak256(tx_hash.as_slice());
        assert_eq!(s3_key, format!("bundles/{expected_hash}/mpool-received-{expected_hash}"));
    }

    #[test]
    fn test_mempool_dropped_same_hash_as_forwarded() {
        let tx_hash = B256::from([2u8; 32]);
        let forwarded = BundleEvent::MempoolForwarded { tx_hash };
        let dropped = BundleEvent::MempoolDropped { tx_hash };

        // Same tx_hash should produce same bundle_hash
        assert_eq!(forwarded.bundle_hash(), dropped.bundle_hash());
        // And same event key (both are mpool-received)
        assert_eq!(forwarded.generate_event_key(), dropped.generate_event_key());
    }

    #[test]
    fn test_transaction_ids_empty_for_mempool_events() {
        let tx_hash = B256::from([1u8; 32]);
        let event = BundleEvent::MempoolForwarded { tx_hash };
        assert!(event.transaction_ids().is_empty());
    }

    #[test]
    fn test_mempool_dropped_bundle_id() {
        let tx_hash = B256::from([3u8; 32]);
        let event = BundleEvent::MempoolDropped { tx_hash };
        let id = event.bundle_id();
        assert_eq!(id, Uuid::new_v5(&Uuid::NAMESPACE_OID, tx_hash.as_slice()));
    }

    #[test]
    fn test_mempool_dropped_s3_key() {
        let tx_hash = B256::from([3u8; 32]);
        let event = BundleEvent::MempoolDropped { tx_hash };
        let s3_key = event.s3_event_key();
        let expected_hash = keccak256(tx_hash.as_slice());
        assert_eq!(s3_key, format!("bundles/{expected_hash}/mpool-received-{expected_hash}"));
    }
}
