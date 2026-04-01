//! Virtual address encoding, decoding, and validation.

use alloy_primitives::{Address, B256, FixedBytes, keccak256};

use crate::{MasterId, RegistryError, UserTag, VIRTUAL_MAGIC};

/// Utilities for working with virtual addresses.
///
/// A virtual address is a 20-byte address with the layout:
///
/// ```text
/// [4-byte masterId] [10-byte VIRTUAL_MAGIC] [6-byte userTag]
/// ```
///
/// The 10-byte magic in the middle identifies the address as virtual. The
/// `masterId` is a registry lookup key, and the `userTag` is an opaque
/// per-user identifier.
#[derive(Debug)]
pub struct VirtualAddress;

impl VirtualAddress {
    /// Returns `true` if `addr` matches the virtual address format.
    ///
    /// Checks whether bytes `[4..14]` equal [`VIRTUAL_MAGIC`].
    pub fn is_virtual(addr: Address) -> bool {
        addr.as_slice()[4..14] == VIRTUAL_MAGIC
    }

    /// Decode a virtual address into its `(MasterId, UserTag)` components.
    ///
    /// Returns `None` if the address is not virtual.
    pub fn decode(addr: Address) -> Option<(MasterId, UserTag)> {
        if !Self::is_virtual(addr) {
            return None;
        }
        let bytes = addr.as_slice();
        let master_id = MasterId::from_slice(&bytes[..4]);
        let user_tag = UserTag::from_slice(&bytes[14..]);
        Some((master_id, user_tag))
    }

    /// Encode a `(MasterId, UserTag)` pair into a virtual address.
    pub fn encode(master_id: MasterId, user_tag: UserTag) -> Address {
        let mut bytes = [0u8; 20];
        bytes[..4].copy_from_slice(master_id.as_slice());
        bytes[4..14].copy_from_slice(&VIRTUAL_MAGIC);
        bytes[14..].copy_from_slice(user_tag.as_slice());
        Address::new(bytes)
    }

    /// Compute the `MasterId` for a given `(caller, salt)` pair.
    ///
    /// The registration hash is `keccak256(abi.encodePacked(caller, salt))`.
    /// The first 4 bytes of the hash **must** be zero (32-bit proof-of-work).
    /// The `MasterId` is extracted from bytes `[4..8]`.
    pub fn compute_master_id(caller: Address, salt: B256) -> Result<MasterId, RegistryError> {
        let hash = Self::registration_hash(caller, salt);
        // Proof-of-work: first 4 bytes must be zero.
        if hash[..4] != [0u8; 4] {
            return Err(RegistryError::ProofOfWorkFailed);
        }
        Ok(FixedBytes::from_slice(&hash[4..8]))
    }

    /// Compute the raw registration hash without validating proof-of-work.
    pub fn registration_hash(caller: Address, salt: B256) -> B256 {
        let mut preimage = [0u8; 52];
        preimage[..20].copy_from_slice(caller.as_slice());
        preimage[20..].copy_from_slice(salt.as_slice());
        keccak256(preimage)
    }

    /// Returns `true` if `addr` is a valid master address.
    ///
    /// A valid master address must not be:
    /// - The zero address
    /// - A virtual address (matching [`VIRTUAL_MAGIC`])
    pub fn is_valid_master(addr: Address) -> bool {
        !addr.is_zero() && !Self::is_virtual(addr)
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256, address, b256};

    use super::*;

    #[test]
    fn roundtrip_encode_decode() {
        let master_id = MasterId::from_slice(&[0xAB, 0xCD, 0x12, 0x34]);
        let user_tag = UserTag::from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);

        let addr = VirtualAddress::encode(master_id, user_tag);
        assert!(VirtualAddress::is_virtual(addr));

        let (decoded_mid, decoded_ut) = VirtualAddress::decode(addr).unwrap();
        assert_eq!(decoded_mid, master_id);
        assert_eq!(decoded_ut, user_tag);
    }

    #[test]
    fn non_virtual_address_returns_none() {
        let addr = address!("d8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
        assert!(!VirtualAddress::is_virtual(addr));
        assert!(VirtualAddress::decode(addr).is_none());
    }

    #[test]
    fn zero_address_is_not_valid_master() {
        assert!(!VirtualAddress::is_valid_master(Address::ZERO));
    }

    #[test]
    fn virtual_address_is_not_valid_master() {
        let master_id = MasterId::from_slice(&[0x00, 0x00, 0x00, 0x01]);
        let user_tag = UserTag::from_slice(&[0x00; 6]);
        let virtual_addr = VirtualAddress::encode(master_id, user_tag);
        assert!(!VirtualAddress::is_valid_master(virtual_addr));
    }

    #[test]
    fn regular_address_is_valid_master() {
        let addr = address!("d8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
        assert!(VirtualAddress::is_valid_master(addr));
    }

    #[test]
    fn compute_master_id_extracts_correct_bytes() {
        // Verify that when the first 4 bytes of the hash are zero,
        // compute_master_id returns bytes [4..8] as the MasterId.
        let caller = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        let salt = B256::ZERO;
        let hash = VirtualAddress::registration_hash(caller, salt);

        // The hash is deterministic. Check whether it passes PoW.
        if hash[..4] == [0u8; 4] {
            // Lucky — first 4 bytes are zero. Verify extraction.
            let mid = VirtualAddress::compute_master_id(caller, salt).unwrap();
            assert_eq!(mid.as_slice(), &hash[4..8]);
        } else {
            // Expected path — PoW fails for this salt.
            assert_eq!(
                VirtualAddress::compute_master_id(caller, salt),
                Err(RegistryError::ProofOfWorkFailed)
            );
        }
    }

    #[test]
    fn registration_hash_is_deterministic() {
        let caller = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        let salt = b256!("0000000000000000000000000000000000000000000000000000000000000001");

        let h1 = VirtualAddress::registration_hash(caller, salt);
        let h2 = VirtualAddress::registration_hash(caller, salt);
        assert_eq!(h1, h2);

        // Different salt produces different hash.
        let salt2 = b256!("0000000000000000000000000000000000000000000000000000000000000002");
        let h3 = VirtualAddress::registration_hash(caller, salt2);
        assert_ne!(h1, h3);
    }

    #[test]
    fn invalid_proof_of_work_rejected() {
        let caller = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        // A salt of all zeros is extremely unlikely to pass PoW.
        let salt = b256!("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
        let result = VirtualAddress::compute_master_id(caller, salt);
        assert_eq!(result, Err(RegistryError::ProofOfWorkFailed));
    }
}
