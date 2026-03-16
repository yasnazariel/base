//! Contains the [`Metadata`] type used in Flashblocks.

use std::collections::HashMap;

use alloy_primitives::{Address, B256, Bytes};
use serde::{Deserialize, Serialize};

/// Metadata associated with a flashblock.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq, Default)]
pub struct Metadata {
    /// Block number this flashblock belongs to.
    pub block_number: u64,
    /// Per-transaction receipts keyed by tx hash string.
    ///
    /// Only present in v0.5.0+ flashblock format; absent in older versions.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub receipts: HashMap<String, ReceiptEnvelope>,
}

impl Metadata {
    /// Returns all log entries across all transaction receipts in this flashblock.
    pub fn collect_logs(&self) -> Vec<&ReceiptLog> {
        self.receipts.values().flat_map(|env| env.logs()).collect()
    }
}

/// A receipt in flashblock metadata.
///
/// Two wire formats are supported via [`serde(untagged)`]:
///
/// - **Production format** (`v0.5.0+`): externally type-tagged map, e.g.
///   `{"Deposit": {"logs": [...]}}` or `{"Eip1559": {"logs": [...]}}`
/// - **Legacy format** (`v0.5.0-rc.3`): flat object, e.g.
///   `{"type": "0x2", "status": true, "logs": [...]}`
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum ReceiptEnvelope {
    /// Production format: outer key is the tx type name.
    Tagged(HashMap<String, ReceiptData>),
    /// Legacy format: receipt fields directly in the object.
    Flat(ReceiptData),
}

impl ReceiptEnvelope {
    /// Returns all logs from this receipt regardless of format.
    pub fn logs(&self) -> Vec<&ReceiptLog> {
        match self {
            Self::Tagged(map) => map.values().flat_map(|d| d.logs.iter()).collect(),
            Self::Flat(data) => data.logs.iter().collect(),
        }
    }
}

/// Partial receipt data — only the fields needed for event log filtering.
///
/// Unknown fields (status, cumulativeGasUsed, etc.) are intentionally ignored.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq, Default)]
pub struct ReceiptData {
    /// Logs emitted by this transaction.
    #[serde(default)]
    pub logs: Vec<ReceiptLog>,
}

/// A single log entry from a transaction receipt.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq, Default)]
pub struct ReceiptLog {
    /// The contract address that emitted this log.
    pub address: Address,
    /// Indexed log topics. `topics[0]` is the event signature hash (keccak256 of the ABI signature).
    #[serde(default)]
    pub topics: Vec<B256>,
    /// Non-indexed ABI-encoded event data. Used for decoding transfer amounts and swap volumes.
    #[serde(default)]
    pub data: Bytes,
}
