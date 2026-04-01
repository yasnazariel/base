//! Core types for virtual address resolution.

use alloy_primitives::{Address, FixedBytes};

/// A 4-byte master identifier derived from `keccak256(abi.encodePacked(caller, salt))`.
///
/// The registration process requires a 32-bit proof-of-work: the first 4 bytes of the
/// hash must be zero, and the `MasterId` is extracted from bytes `[4..8]`.
pub type MasterId = FixedBytes<4>;

/// A 6-byte opaque per-user tag embedded in a virtual address.
///
/// Derived offchain by the master. 48 bits provides ~2.8 × 10¹⁴ unique deposit
/// addresses per master.
pub type UserTag = FixedBytes<6>;

/// 10-byte magic sequence placed at bytes `[4..14]` of a virtual address.
///
/// Any 20-byte address whose bytes `[4..14]` match this constant is treated as virtual.
pub const VIRTUAL_MAGIC: [u8; 10] = [0xFD; 10];

/// Reserved address for the virtual-address registry precompile / contract.
pub const REGISTRY_ADDRESS: Address = Address::new([
    0xFD, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
]);

/// Errors that can occur during virtual address registration or resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// The first 4 bytes of the registration hash are not zero.
    ProofOfWorkFailed,
    /// A different master address is already registered for this `MasterId`.
    MasterIdCollision(Address),
    /// The supplied master address is not valid (zero, virtual, or reserved).
    InvalidMasterAddress,
    /// The virtual address references an unregistered `MasterId`.
    VirtualAddressUnregistered,
}

impl core::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ProofOfWorkFailed => {
                write!(f, "proof-of-work failed: first 4 bytes must be zero")
            }
            Self::MasterIdCollision(addr) => {
                write!(f, "master ID collision: already registered to {addr}")
            }
            Self::InvalidMasterAddress => write!(f, "invalid master address"),
            Self::VirtualAddressUnregistered => write!(f, "virtual address unregistered"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RegistryError {}
