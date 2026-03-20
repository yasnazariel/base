//! Storage slot derivation for EIP-8130 system contracts.
//!
//! Provides deterministic slot computation for owner configuration, nonce
//! manager, lock state, and change sequence data.

use alloy_primitives::{Address, B256, U256, keccak256};

/// Base storage slot for the `owner_config` mapping in `AccountConfig`.
///
/// `owner_config[account][ownerId]` →
///   `keccak256(ownerId . keccak256(account . OWNER_CONFIG_BASE_SLOT))`
///
/// Each slot packs: `verifier (20 bytes) | scope (1 byte) | reserved (11 bytes)`.
pub const OWNER_CONFIG_BASE_SLOT: U256 = U256::ZERO;

/// Base storage slot for the `nonce` mapping in `NonceManager`.
///
/// `nonce[account][nonceKey]` →
///   `keccak256(nonceKey . keccak256(account . NONCE_BASE_SLOT))`
pub const NONCE_BASE_SLOT: U256 = U256::from_limbs([1, 0, 0, 0]);

/// Base storage slot for the `lock_state` mapping in `AccountConfig`.
///
/// `lock_state[account]` → `keccak256(account . LOCK_BASE_SLOT)`
///
/// Packs: `locked (bool) | unlock_delay (u64) | unlock_requested_at (u64)`.
pub const LOCK_BASE_SLOT: U256 = U256::from_limbs([2, 0, 0, 0]);

/// Base storage slot for the `change_sequence` mapping in `AccountConfig`.
///
/// `change_sequence[account][chainId]` →
///   `keccak256(chainId . keccak256(account . SEQUENCE_BASE_SLOT))`
pub const SEQUENCE_BASE_SLOT: U256 = U256::from_limbs([3, 0, 0, 0]);

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

/// Computes the storage slot for `change_sequence[account][chainId]`.
pub fn sequence_slot(account: Address, chain_id: u64) -> B256 {
    let inner = {
        let mut buf = [0u8; 64];
        buf[12..32].copy_from_slice(account.as_slice());
        SEQUENCE_BASE_SLOT.to_be_bytes::<32>().as_slice().iter().enumerate().for_each(
            |(i, &b)| {
                buf[32 + i] = b;
            },
        );
        keccak256(buf)
    };

    let mut buf = [0u8; 64];
    buf[24..32].copy_from_slice(&chain_id.to_be_bytes());
    buf[32..64].copy_from_slice(inner.as_slice());
    keccak256(buf)
}

/// Parses an `owner_config` storage slot value into `(verifier, scope)`.
///
/// Layout: `verifier (20 bytes) | scope (1 byte) | reserved (11 bytes)`.
pub fn parse_owner_config(slot_value: B256) -> (Address, u8) {
    let bytes = slot_value.as_slice();
    let verifier = Address::from_slice(&bytes[..20]);
    let scope = bytes[20];
    (verifier, scope)
}

/// Encodes `(verifier, scope)` into an `owner_config` storage slot value.
pub fn encode_owner_config(verifier: Address, scope: u8) -> B256 {
    let mut bytes = [0u8; 32];
    bytes[..20].copy_from_slice(verifier.as_slice());
    bytes[20] = scope;
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
    fn sequence_slot_deterministic() {
        let account = Address::repeat_byte(0x01);
        let slot1 = sequence_slot(account, 1);
        let slot2 = sequence_slot(account, 1);
        assert_eq!(slot1, slot2);
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
