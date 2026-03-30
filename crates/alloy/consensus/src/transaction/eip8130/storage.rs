//! Storage slot derivation for EIP-8130 system contracts.
//!
//! Provides deterministic slot computation for owner configuration, nonce
//! manager, lock state, and change sequence data.

use alloy_primitives::{Address, B256, U256, keccak256};

/// Base storage slot for the `_ownerConfigs` mapping in `AccountConfiguration`.
///
/// `_ownerConfigs[account][ownerId]` →
///   `keccak256(ownerId . keccak256(account . OWNER_CONFIG_BASE_SLOT))`
///
/// Solidity packing (right-aligned): `zeros(11) | scope(1) | verifier(20)`.
pub const OWNER_CONFIG_BASE_SLOT: U256 = U256::ZERO;

/// Base storage slot for the `_accountLocks` mapping in `AccountConfiguration`.
///
/// `_accountLocks[account]` → `keccak256(account . LOCK_BASE_SLOT)`
///
/// Solidity `AccountLock { bool locked; uint32 unlockDelay; uint32 unlockRequestedAt; }`.
/// Packing (right-aligned): `zeros(23) | unlockRequestedAt(4) | unlockDelay(4) | locked(1)`.
pub const LOCK_BASE_SLOT: U256 = U256::from_limbs([1, 0, 0, 0]);

/// Base storage slot for the `_changeSequences` mapping in `AccountConfiguration`.
///
/// `_changeSequences[account]` → `keccak256(account . SEQUENCE_BASE_SLOT)`
///
/// Solidity `ChangeSequences { uint64 multichain; uint64 local; }`.
/// Packing (right-aligned): `zeros(16) | local(8) | multichain(8)`.
pub const SEQUENCE_BASE_SLOT: U256 = U256::from_limbs([2, 0, 0, 0]);

/// Base storage slot for the `nonce` mapping in `NonceManager` (precompile at 0xAa02).
///
/// `nonce[account][nonceKey]` →
///   `keccak256(nonceKey . keccak256(account . NONCE_BASE_SLOT))`
///
/// NonceManager is a separate precompile at a different address; this slot
/// does not overlap with AccountConfiguration storage.
pub const NONCE_BASE_SLOT: U256 = U256::from_limbs([1, 0, 0, 0]);

/// Computes the storage slot for `owner_config[account][ownerId]`.
pub fn owner_config_slot(account: Address, owner_id: B256) -> B256 {
    let inner = {
        let mut buf = [0u8; 64];
        buf[12..32].copy_from_slice(account.as_slice());
        OWNER_CONFIG_BASE_SLOT.to_be_bytes::<32>().as_slice().iter().enumerate().for_each(
            |(i, &b)| {
                buf[32 + i] = b;
            },
        );
        keccak256(buf)
    };

    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(owner_id.as_slice());
    buf[32..64].copy_from_slice(inner.as_slice());
    keccak256(buf)
}

/// Computes the storage slot for `nonce[account][nonceKey]`.
pub fn nonce_slot(account: Address, nonce_key: U256) -> B256 {
    let inner = {
        let mut buf = [0u8; 64];
        buf[12..32].copy_from_slice(account.as_slice());
        NONCE_BASE_SLOT.to_be_bytes::<32>().as_slice().iter().enumerate().for_each(|(i, &b)| {
            buf[32 + i] = b;
        });
        keccak256(buf)
    };

    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(&nonce_key.to_be_bytes::<32>());
    buf[32..64].copy_from_slice(inner.as_slice());
    keccak256(buf)
}

/// Computes the storage slot for `lock_state[account]`.
pub fn lock_slot(account: Address) -> B256 {
    let mut buf = [0u8; 64];
    buf[12..32].copy_from_slice(account.as_slice());
    LOCK_BASE_SLOT.to_be_bytes::<32>().as_slice().iter().enumerate().for_each(|(i, &b)| {
        buf[32 + i] = b;
    });
    keccak256(buf)
}

/// Computes the base storage slot for `_changeSequences[account]`.
///
/// The Solidity struct `ChangeSequences { uint64 multichain; uint64 local; }` packs
/// both fields into this single slot (right-aligned):
///   bits [0,  64)  = multichain (chain_id 0)
///   bits [64, 128) = local      (chain_id == block.chainid)
pub fn sequence_base_slot(account: Address) -> B256 {
    let mut buf = [0u8; 64];
    buf[12..32].copy_from_slice(account.as_slice());
    SEQUENCE_BASE_SLOT.to_be_bytes::<32>().as_slice().iter().enumerate().for_each(|(i, &b)| {
        buf[32 + i] = b;
    });
    keccak256(buf)
}

/// Reads the `multichain` or `local` sequence from a packed slot value.
///
/// `is_multichain` = true  → reads the low 8 bytes  (chain_id 0)
/// `is_multichain` = false → reads bytes [16..24]    (local chain)
pub fn read_sequence(slot_value: U256, is_multichain: bool) -> u64 {
    if is_multichain { slot_value.as_limbs()[0] } else { (slot_value >> 64_u8).as_limbs()[0] }
}

/// Writes a sequence value into a packed slot, preserving the other field.
pub fn write_sequence(current: U256, is_multichain: bool, new_value: u64) -> U256 {
    let mask_low = U256::from(u64::MAX);
    let mask_high = mask_low << 64_u8;
    if is_multichain {
        (current & mask_high) | U256::from(new_value)
    } else {
        (current & mask_low) | (U256::from(new_value) << 64_u8)
    }
}

/// Legacy helper: computes the slot as if it were a nested mapping.
///
/// **Deprecated** – use [`sequence_base_slot`] + [`read_sequence`]/[`write_sequence`]
/// for compatibility with the deployed AccountConfiguration contract.
#[deprecated(note = "use sequence_base_slot + read_sequence/write_sequence instead")]
pub fn sequence_slot(account: Address, chain_id: u64) -> B256 {
    let inner = {
        let mut buf = [0u8; 64];
        buf[12..32].copy_from_slice(account.as_slice());
        SEQUENCE_BASE_SLOT.to_be_bytes::<32>().as_slice().iter().enumerate().for_each(|(i, &b)| {
            buf[32 + i] = b;
        });
        keccak256(buf)
    };

    let mut buf = [0u8; 64];
    buf[24..32].copy_from_slice(&chain_id.to_be_bytes());
    buf[32..64].copy_from_slice(inner.as_slice());
    keccak256(buf)
}

/// Parses an `owner_config` storage slot value into `(verifier, scope)`.
///
/// Solidity right-aligned packing for `OwnerConfig { address verifier; uint8 scope; }`:
///   `zeros(11) | scope(1) | verifier(20)`
///
/// In the 32-byte big-endian word:
///   bytes [12..32] = verifier (20 bytes, low-order)
///   byte  [11]     = scope
///   bytes [0..11]  = zeros
pub fn parse_owner_config(slot_value: B256) -> (Address, u8) {
    let bytes = slot_value.as_slice();
    let verifier = Address::from_slice(&bytes[12..32]);
    let scope = bytes[11];
    (verifier, scope)
}

/// Encodes `(verifier, scope)` into an `owner_config` storage slot value.
///
/// Must match Solidity's right-aligned struct packing so the deployed
/// `AccountConfiguration` contract can read protocol-written storage.
pub fn encode_owner_config(verifier: Address, scope: u8) -> B256 {
    let mut bytes = [0u8; 32];
    bytes[12..32].copy_from_slice(verifier.as_slice());
    bytes[11] = scope;
    B256::from(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_config_deterministic() {
        let account = Address::repeat_byte(0x01);
        let owner_id = B256::repeat_byte(0x02);
        let slot1 = owner_config_slot(account, owner_id);
        let slot2 = owner_config_slot(account, owner_id);
        assert_eq!(slot1, slot2);
    }

    #[test]
    fn different_accounts_different_slots() {
        let owner_id = B256::repeat_byte(0x02);
        let slot_a = owner_config_slot(Address::repeat_byte(0x01), owner_id);
        let slot_b = owner_config_slot(Address::repeat_byte(0x03), owner_id);
        assert_ne!(slot_a, slot_b);
    }

    #[test]
    fn different_owners_different_slots() {
        let account = Address::repeat_byte(0x01);
        let slot_a = owner_config_slot(account, B256::repeat_byte(0x02));
        let slot_b = owner_config_slot(account, B256::repeat_byte(0x03));
        assert_ne!(slot_a, slot_b);
    }

    #[test]
    fn nonce_slot_deterministic() {
        let account = Address::repeat_byte(0x01);
        let nonce_key = U256::from(42);
        let slot1 = nonce_slot(account, nonce_key);
        let slot2 = nonce_slot(account, nonce_key);
        assert_eq!(slot1, slot2);
    }

    #[test]
    fn different_nonce_keys_different_slots() {
        let account = Address::repeat_byte(0x01);
        let slot_a = nonce_slot(account, U256::from(1));
        let slot_b = nonce_slot(account, U256::from(2));
        assert_ne!(slot_a, slot_b);
    }

    #[test]
    fn lock_slot_deterministic() {
        let slot1 = lock_slot(Address::repeat_byte(0x01));
        let slot2 = lock_slot(Address::repeat_byte(0x01));
        assert_eq!(slot1, slot2);
    }

    #[test]
    fn different_accounts_different_lock_slots() {
        let slot_a = lock_slot(Address::repeat_byte(0x01));
        let slot_b = lock_slot(Address::repeat_byte(0x02));
        assert_ne!(slot_a, slot_b);
    }

    #[test]
    fn sequence_base_slot_deterministic() {
        let account = Address::repeat_byte(0x01);
        let slot1 = sequence_base_slot(account);
        let slot2 = sequence_base_slot(account);
        assert_eq!(slot1, slot2);
    }

    #[test]
    fn read_write_sequence_multichain() {
        let packed = U256::ZERO;
        let updated = write_sequence(packed, true, 42);
        assert_eq!(read_sequence(updated, true), 42);
        assert_eq!(read_sequence(updated, false), 0);
    }

    #[test]
    fn read_write_sequence_local() {
        let packed = U256::ZERO;
        let updated = write_sequence(packed, false, 99);
        assert_eq!(read_sequence(updated, false), 99);
        assert_eq!(read_sequence(updated, true), 0);
    }

    #[test]
    fn read_write_sequence_preserves_other() {
        let packed = write_sequence(U256::ZERO, true, 10);
        let packed = write_sequence(packed, false, 20);
        assert_eq!(read_sequence(packed, true), 10);
        assert_eq!(read_sequence(packed, false), 20);
    }

    #[test]
    fn parse_encode_roundtrip() {
        let verifier = Address::repeat_byte(0xAA);
        let scope = 0x0F;
        let encoded = encode_owner_config(verifier, scope);
        let (decoded_verifier, decoded_scope) = parse_owner_config(encoded);
        assert_eq!(decoded_verifier, verifier);
        assert_eq!(decoded_scope, scope);
    }

    #[test]
    fn parse_zero_slot_returns_zero_verifier() {
        let (verifier, scope) = parse_owner_config(B256::ZERO);
        assert_eq!(verifier, Address::ZERO);
        assert_eq!(scope, 0);
    }
}
