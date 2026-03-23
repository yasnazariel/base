//! EIP-8130 intrinsic gas calculation.

use alloy_rlp::Encodable;

use super::{
    AccountChangeEntry, TxEip8130,
    constants::{
        AA_BASE_COST, BYTECODE_BASE_GAS, BYTECODE_PER_BYTE_GAS, CONFIG_CHANGE_OP_GAS,
        CONFIG_CHANGE_SKIP_GAS, EOA_AUTH_GAS, NONCE_KEY_COLD_GAS, NONCE_KEY_WARM_GAS, SLOAD_GAS,
    },
};

/// Computes the intrinsic gas for an AA transaction.
///
/// ```text
/// intrinsic_gas = AA_BASE_COST
///               + tx_payload_cost
///               + sender_auth_cost
///               + payer_auth_cost
///               + nonce_key_cost
///               + bytecode_cost
///               + account_changes_cost
/// ```
///
/// The `nonce_key_is_warm` parameter indicates whether the nonce channel has been
/// used before (affects the SSTORE cost).
pub fn intrinsic_gas(tx: &TxEip8130, nonce_key_is_warm: bool, chain_id: u64) -> u64 {
    let mut gas = AA_BASE_COST;

    gas += tx_payload_cost(tx);
    gas += sender_auth_cost(tx);
    gas += payer_auth_cost(tx);
    gas += nonce_key_cost(nonce_key_is_warm);
    gas += bytecode_cost(tx);
    gas += account_changes_cost(tx, chain_id);

    gas
}

/// Standard EIP-2028 calldata cost: 16 gas per non-zero byte, 4 per zero byte,
/// computed over the full RLP encoding of the transaction.
pub fn tx_payload_cost(tx: &TxEip8130) -> u64 {
    let mut buf = alloc::vec::Vec::with_capacity(tx.length());
    tx.encode(&mut buf);
    calldata_gas(&buf)
}

/// Sender authentication cost.
///
/// - EOA (ecrecover): flat 6 000 gas.
/// - Configured: SLOAD (owner_config read) + verifier resolution cost.
///   The verifier execution cost is metered separately at runtime.
pub fn sender_auth_cost(tx: &TxEip8130) -> u64 {
    if tx.is_eoa() {
        EOA_AUTH_GAS
    } else {
        SLOAD_GAS
    }
}

/// Payer authentication cost: 0 for self-pay, same model as sender for sponsored.
pub fn payer_auth_cost(tx: &TxEip8130) -> u64 {
    if tx.is_self_pay() {
        0
    } else {
        SLOAD_GAS
    }
}

/// Nonce key cost: 22 100 for a new channel, 5 000 for an existing one.
pub fn nonce_key_cost(is_warm: bool) -> u64 {
    if is_warm { NONCE_KEY_WARM_GAS } else { NONCE_KEY_COLD_GAS }
}

/// Bytecode deployment cost: 32 000 base + 200/byte if a create entry is present.
pub fn bytecode_cost(tx: &TxEip8130) -> u64 {
    for entry in &tx.account_changes {
        if let AccountChangeEntry::Create(create) = entry {
            if create.bytecode.is_empty() {
                return BYTECODE_BASE_GAS;
            }
            return BYTECODE_BASE_GAS + BYTECODE_PER_BYTE_GAS * create.bytecode.len() as u64;
        }
    }
    0
}

/// Configuration change cost.
///
/// - Entries targeting the current `chain_id` (or chain_id == 0 for multi-chain): `CONFIG_CHANGE_OP_GAS` per operation.
/// - Entries for a different chain: `CONFIG_CHANGE_SKIP_GAS` (SLOAD to verify and skip).
pub fn account_changes_cost(tx: &TxEip8130, chain_id: u64) -> u64 {
    let mut gas = 0u64;
    for entry in &tx.account_changes {
        if let AccountChangeEntry::ConfigChange(cc) = entry {
            if cc.chain_id == 0 || cc.chain_id == chain_id {
                gas += CONFIG_CHANGE_OP_GAS * cc.operations.len() as u64;
            } else {
                gas += CONFIG_CHANGE_SKIP_GAS;
            }
        }
    }
    gas
}

/// EIP-2028 calldata gas: 16 per non-zero byte, 4 per zero byte.
fn calldata_gas(data: &[u8]) -> u64 {
    data.iter().fold(0u64, |acc, &byte| acc + if byte == 0 { 4 } else { 16 })
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, Bytes, U256};

    use super::*;
    use crate::transaction::eip8130::types::{ConfigChangeEntry, ConfigOperation, CreateEntry, Owner};

    #[test]
    fn calldata_gas_basic() {
        assert_eq!(calldata_gas(&[0, 0, 1, 2]), 4 + 4 + 16 + 16);
        assert_eq!(calldata_gas(&[]), 0);
    }

    #[test]
    fn nonce_key_cost_warm_vs_cold() {
        assert_eq!(nonce_key_cost(false), 22_100);
        assert_eq!(nonce_key_cost(true), 5_000);
    }

    #[test]
    fn bytecode_cost_no_create() {
        let tx = TxEip8130::default();
        assert_eq!(bytecode_cost(&tx), 0);
    }

    #[test]
    fn bytecode_cost_with_create() {
        let tx = TxEip8130 {
            account_changes: vec![AccountChangeEntry::Create(CreateEntry {
                user_salt: Default::default(),
                bytecode: Bytes::from_static(&[0x60; 100]),
                initial_owners: vec![Owner {
                    verifier: Address::repeat_byte(1),
                    owner_id: Default::default(),
                    scope: 0,
                }],
            })],
            ..Default::default()
        };
        assert_eq!(bytecode_cost(&tx), 32_000 + 200 * 100);
    }

    #[test]
    fn account_changes_cost_applied_vs_skipped() {
        let tx = TxEip8130 {
            account_changes: vec![
                AccountChangeEntry::ConfigChange(ConfigChangeEntry {
                    chain_id: 8453,
                    sequence: 0,
                    operations: vec![
                        ConfigOperation {
                            op_type: 0x01,
                            verifier: Address::repeat_byte(1),
                            owner_id: Default::default(),
                            scope: 0,
                        },
                        ConfigOperation {
                            op_type: 0x01,
                            verifier: Address::repeat_byte(2),
                            owner_id: Default::default(),
                            scope: 0,
                        },
                    ],
                    authorizer_auth: Bytes::new(),
                }),
                AccountChangeEntry::ConfigChange(ConfigChangeEntry {
                    chain_id: 1, // different chain — will be skipped
                    sequence: 0,
                    operations: vec![ConfigOperation {
                        op_type: 0x01,
                        verifier: Address::repeat_byte(3),
                        owner_id: Default::default(),
                        scope: 0,
                    }],
                    authorizer_auth: Bytes::new(),
                }),
            ],
            ..Default::default()
        };
        let cost = account_changes_cost(&tx, 8453);
        assert_eq!(cost, 2 * CONFIG_CHANGE_OP_GAS + CONFIG_CHANGE_SKIP_GAS);
    }

    #[test]
    fn eoa_auth_cost() {
        let mut tx = TxEip8130::default();
        tx.from = Address::ZERO; // EOA
        assert_eq!(sender_auth_cost(&tx), EOA_AUTH_GAS);

        tx.from = Address::repeat_byte(1); // Configured
        assert_eq!(sender_auth_cost(&tx), SLOAD_GAS);
    }

    #[test]
    fn payer_auth_cost_self_pay_vs_sponsored() {
        let mut tx = TxEip8130::default();
        assert_eq!(payer_auth_cost(&tx), 0); // self-pay

        tx.payer = Address::repeat_byte(0xCC);
        assert_eq!(payer_auth_cost(&tx), SLOAD_GAS);
    }

    #[test]
    fn intrinsic_gas_smoke() {
        let tx = TxEip8130 {
            chain_id: 8453,
            from: Address::repeat_byte(1),
            nonce_key: U256::ZERO,
            nonce_sequence: 0,
            sender_auth: Bytes::from_static(&[0x01; 65]),
            ..Default::default()
        };
        let gas = intrinsic_gas(&tx, true, 8453);
        assert!(gas >= AA_BASE_COST, "intrinsic gas must be at least AA_BASE_COST");
    }
}
