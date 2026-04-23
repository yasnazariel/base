use alloy_primitives::{Address, TxHash, U256};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a transaction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransactionId {
    /// The sender address.
    pub sender: Address,
    /// The transaction nonce.
    pub nonce: U256,
    /// The transaction hash.
    pub hash: TxHash,
}

/// Unique identifier for a bundle.
pub type BundleId = Uuid;

/// Reason a bundle was dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DropReason {
    /// Bundle timed out.
    TimedOut,
    /// Bundle transaction reverted.
    Reverted,
}

/// A transaction with its data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    /// Transaction identifier.
    pub id: TransactionId,
    /// Raw transaction data.
    pub data: Bytes,
}
