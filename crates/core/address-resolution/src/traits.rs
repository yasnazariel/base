//! Traits for virtual address registration and resolution.

use alloy_primitives::{Address, B256};

use crate::MasterId;

/// Resolves an address to its effective recipient.
///
/// Implementations detect virtual addresses and return the registered master.
/// Non-virtual addresses pass through unchanged.
pub trait AddressResolver {
    /// The error type returned by this resolver.
    type Error;

    /// Resolve `addr` to the effective recipient.
    ///
    /// - If `addr` is a virtual address with a registered master, returns the master.
    /// - If `addr` is not a virtual address, returns `addr` unchanged.
    /// - If `addr` is virtual but unregistered, returns an error.
    fn resolve_recipient(&self, addr: Address) -> Result<Address, Self::Error>;
}

/// Registry mapping master IDs to master addresses.
///
/// Implementors manage the storage of `MasterId → Address` mappings and enforce
/// registration invariants (proof-of-work, collision detection, address validity).
pub trait MasterRegistry {
    /// The error type returned by this registry.
    type Error;

    /// Register the caller as a virtual-address master.
    ///
    /// Computes `masterId` from `keccak256(abi.encodePacked(caller, salt))`,
    /// validates the 32-bit proof-of-work, checks for collisions, and stores
    /// the mapping.
    ///
    /// Returns the newly registered [`MasterId`].
    fn register(&mut self, caller: Address, salt: B256) -> Result<MasterId, Self::Error>;

    /// Look up the master address for a given `MasterId`.
    ///
    /// Returns `None` if the `MasterId` is not registered.
    fn get_master(&self, master_id: MasterId) -> Result<Option<Address>, Self::Error>;
}
