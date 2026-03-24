//! EIP-8130 AA transaction data carried through the execution pipeline.
//!
//! These types mirror the subset of [`TxEip8130`] fields the handler needs for
//! phased call execution, auto-delegation, and pre-execution storage writes.
//! They use only primitive types to avoid a circular dependency on
//! `base-alloy-consensus`.

use std::vec::Vec;

use revm::primitives::{Address, B256, Bytes, Log, LogData, U256, keccak256};

/// A single call within an AA transaction phase.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Eip8130Call {
    /// Target address.
    pub to: Address,
    /// Calldata.
    pub data: Bytes,
    /// Value to transfer.
    pub value: U256,
}

/// A pre-execution storage write (nonce increment, owner registration, etc.).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Eip8130StorageWrite {
    /// Contract address holding the storage.
    pub address: Address,
    /// Storage slot key.
    pub slot: U256,
    /// New value to write.
    pub value: U256,
}

/// Code placement for auto-delegation (EIP-7702 style delegation designator).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Eip8130CodePlacement {
    /// Address to place code at.
    pub address: Address,
    /// Bytecode to set.
    pub code: Bytes,
}

/// Per-phase execution result.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Eip8130PhaseResult {
    /// Whether the phase succeeded.
    pub success: bool,
    /// Gas consumed by the phase.
    pub gas_used: u64,
}

/// A config-change sequence update applied as a read-modify-write on the
/// packed `ChangeSequences { uint64 multichain; uint64 local }` slot.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Eip8130SequenceUpdate {
    /// Pre-computed storage slot for `_changeSequences[account]`.
    pub slot: U256,
    /// `true` = update the multichain (chain_id 0) field, `false` = local.
    pub is_multichain: bool,
    /// The new sequence value to write (old + 1).
    pub new_value: u64,
}

impl Eip8130SequenceUpdate {
    /// Applies this update to the current packed slot value.
    pub fn apply(&self, current: U256) -> U256 {
        let mask_low = U256::from(u64::MAX);
        let mask_high = mask_low << 64_u8;
        if self.is_multichain {
            (current & mask_high) | U256::from(self.new_value)
        } else {
            (current & mask_low) | (U256::from(self.new_value) << 64_u8)
        }
    }
}

/// Aggregated AA execution data populated during transaction conversion.
///
/// Built from a [`TxEip8130`] in `evm_compat` and consumed by the handler during
/// execution. Non-AA transactions carry a default (empty) instance.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Eip8130Parts {
    /// The effective sender address.
    pub sender: Address,
    /// The effective payer address (same as sender if self-pay).
    pub payer: Address,
    /// Authenticated owner identifier from sender auth.
    pub owner_id: B256,
    /// Nonce key for 2D nonce slot calculation.
    pub nonce_key: U256,
    /// Whether the tx includes a create entry (determines auto-delegation skip).
    pub has_create_entry: bool,
    /// Auto-delegation code (`0xef0100 || DEFAULT_ACCOUNT_ADDRESS`) if applicable.
    /// Empty if auto-delegation is not needed.
    pub auto_delegation_code: Bytes,
    /// Pre-execution storage writes (nonce increment, owner registration, config changes).
    pub pre_writes: Vec<Eip8130StorageWrite>,
    /// Sequence updates requiring read-modify-write on packed storage slots.
    pub sequence_updates: Vec<Eip8130SequenceUpdate>,
    /// Code placements for account creation (runtime bytecode at CREATE2-derived addresses).
    pub code_placements: Vec<Eip8130CodePlacement>,
    /// Phased call batches. Each inner `Vec` is one atomic phase.
    pub call_phases: Vec<Vec<Eip8130Call>>,
}

/// Encodes phase results into the output bytes of an AA transaction.
///
/// Format: one byte per phase, `0x01` = success, `0x00` = failure.
pub fn encode_phase_statuses(results: &[Eip8130PhaseResult]) -> Bytes {
    Bytes::from(results.iter().map(|r| u8::from(r.success)).collect::<Vec<_>>())
}

/// Decodes phase statuses from AA transaction output bytes.
pub fn decode_phase_statuses(output: &[u8]) -> Vec<bool> {
    output.iter().map(|&b| b != 0).collect()
}

/// System log topic for persisting per-phase execution statuses in receipts.
///
/// `keccak256("Eip8130PhaseStatuses(bytes)")`
pub fn phase_statuses_log_topic() -> B256 {
    keccak256(b"Eip8130PhaseStatuses(bytes)")
}

/// Creates a system log carrying per-phase execution statuses.
///
/// Emitted from `emitter_address` (typically the TxContext precompile) so that
/// phase statuses survive in the receipt's log list and can be recovered at RPC time.
pub fn phase_statuses_system_log(emitter: Address, results: &[Eip8130PhaseResult]) -> Log {
    let data = Bytes::from(results.iter().map(|r| u8::from(r.success)).collect::<Vec<_>>());
    Log {
        address: emitter,
        data: LogData::new_unchecked(vec![phase_statuses_log_topic()], data),
    }
}

/// Extracts per-phase statuses from a system log emitted during EIP-8130 execution.
///
/// Scans receipt logs for the `Eip8130PhaseStatuses` topic from the expected emitter address.
/// Returns `None` if no matching log is found.
pub fn extract_phase_statuses_from_logs<T: AsRef<Log>>(
    logs: &[T],
    emitter: Address,
) -> Option<Vec<bool>> {
    let topic = phase_statuses_log_topic();
    for log in logs {
        let log = log.as_ref();
        if log.address == emitter && log.topics().first() == Some(&topic) {
            return Some(decode_phase_statuses(&log.data.data));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_all_success() {
        let results = vec![
            Eip8130PhaseResult { success: true, gas_used: 100 },
            Eip8130PhaseResult { success: true, gas_used: 200 },
        ];
        let encoded = encode_phase_statuses(&results);
        assert_eq!(&encoded[..], &[0x01, 0x01]);
    }

    #[test]
    fn encode_all_failure() {
        let results = vec![
            Eip8130PhaseResult { success: false, gas_used: 50 },
            Eip8130PhaseResult { success: false, gas_used: 75 },
        ];
        let encoded = encode_phase_statuses(&results);
        assert_eq!(&encoded[..], &[0x00, 0x00]);
    }

    #[test]
    fn encode_mixed() {
        let results = vec![
            Eip8130PhaseResult { success: true, gas_used: 10 },
            Eip8130PhaseResult { success: false, gas_used: 20 },
            Eip8130PhaseResult { success: true, gas_used: 30 },
        ];
        let encoded = encode_phase_statuses(&results);
        assert_eq!(&encoded[..], &[0x01, 0x00, 0x01]);
    }

    #[test]
    fn encode_empty() {
        let encoded = encode_phase_statuses(&[]);
        assert!(encoded.is_empty());
    }

    #[test]
    fn decode_roundtrip() {
        let results = vec![
            Eip8130PhaseResult { success: true, gas_used: 0 },
            Eip8130PhaseResult { success: false, gas_used: 0 },
            Eip8130PhaseResult { success: true, gas_used: 0 },
            Eip8130PhaseResult { success: false, gas_used: 0 },
        ];
        let encoded = encode_phase_statuses(&results);
        let decoded = decode_phase_statuses(&encoded);
        assert_eq!(decoded, vec![true, false, true, false]);
    }

    #[test]
    fn decode_empty() {
        let decoded = decode_phase_statuses(&[]);
        assert!(decoded.is_empty());
    }

    #[test]
    fn parts_default_is_empty() {
        let parts = Eip8130Parts::default();
        assert_eq!(parts.sender, Address::ZERO);
        assert_eq!(parts.payer, Address::ZERO);
        assert_eq!(parts.owner_id, B256::ZERO);
        assert_eq!(parts.nonce_key, U256::ZERO);
        assert!(!parts.has_create_entry);
        assert!(parts.auto_delegation_code.is_empty());
        assert!(parts.pre_writes.is_empty());
        assert!(parts.call_phases.is_empty());
    }
}
