//! Storage-backed virtual address registry.
//!
//! Implements the `MasterRegistry` and `AddressResolver` traits using raw EVM
//! storage slot reads and writes against a revm `InMemoryDB`.

use alloy_primitives::{Address, B256, U256, keccak256};
use base_address_resolution::{
    AddressResolver, MasterId, MasterRegistry, REGISTRY_ADDRESS, RegistryError, VirtualAddress,
};
use revm::{DatabaseCommit, database::InMemoryDB, state::AccountInfo};

/// Base storage slot for the registry mapping.
///
/// `slot = keccak256(abi.encode(masterId, REGISTRY_SLOT))` stores the packed
/// `masterAddress | reserved(11) | masterType(1)` value.
const REGISTRY_SLOT: U256 = U256::ZERO;

/// A virtual address registry backed by revm `InMemoryDB` storage.
///
/// Stores `masterId → masterAddress` mappings in storage slots under the
/// [`REGISTRY_ADDRESS`]. Compatible with the Tempo TIP-1022 storage layout.
#[derive(Debug)]
pub struct StorageBackedRegistry;

impl StorageBackedRegistry {
    /// Compute the storage slot for a given `MasterId`.
    fn slot_for(master_id: MasterId) -> U256 {
        // slot = keccak256(abi.encode(masterId, REGISTRY_SLOT))
        let mut data = [0u8; 64];
        // masterId is left-padded to 32 bytes (bytes4 in the high bits of a bytes32)
        data[..4].copy_from_slice(master_id.as_slice());
        // REGISTRY_SLOT in the second 32-byte word
        data[32..].copy_from_slice(&REGISTRY_SLOT.to_be_bytes::<32>());
        U256::from_be_bytes(keccak256(data).0)
    }

    /// Read the master address stored for a `MasterId` from the database.
    pub fn read_master(db: &InMemoryDB, master_id: MasterId) -> Option<Address> {
        let slot = Self::slot_for(master_id);
        let value = db
            .cache
            .accounts
            .get(&REGISTRY_ADDRESS)
            .and_then(|acct| acct.storage.get(&slot).copied())
            .unwrap_or(U256::ZERO);

        if value.is_zero() {
            return None;
        }

        // The address is stored in the high 20 bytes of the 32-byte slot.
        let bytes = value.to_be_bytes::<32>();
        Some(Address::from_slice(&bytes[..20]))
    }

    /// Write a `masterId → masterAddress` mapping into the database.
    pub fn write_master(db: &mut InMemoryDB, master_id: MasterId, master: Address) {
        // Ensure the registry account exists.
        if db.cache.accounts.get(&REGISTRY_ADDRESS).is_none() {
            db.insert_account_info(REGISTRY_ADDRESS, AccountInfo::default());
        }

        let slot = Self::slot_for(master_id);

        // Pack: address (20 bytes) | reserved (11 bytes) | type (1 byte = 0x00)
        let mut packed = [0u8; 32];
        packed[..20].copy_from_slice(master.as_slice());
        let value = U256::from_be_bytes(packed);

        // Commit the storage write.
        let mut state = revm::state::EvmState::default();
        let mut account = revm::state::Account::default();
        account.storage.insert(
            slot,
            revm::state::EvmStorageSlot::new_changed(U256::ZERO, value, 0),
        );
        account.mark_touch();
        state.insert(REGISTRY_ADDRESS, account);
        db.commit(state);
    }

    /// Execute a `registerVirtualMaster` call against the database.
    pub fn register(
        db: &mut InMemoryDB,
        caller: Address,
        salt: B256,
    ) -> Result<MasterId, RegistryError> {
        if !VirtualAddress::is_valid_master(caller) {
            return Err(RegistryError::InvalidMasterAddress);
        }

        let master_id = VirtualAddress::compute_master_id(caller, salt)?;

        // Check collision.
        if let Some(existing) = Self::read_master(db, master_id) {
            if existing != caller {
                return Err(RegistryError::MasterIdCollision(existing));
            }
            // Re-registering the same address is a no-op.
            return Ok(master_id);
        }

        Self::write_master(db, master_id, caller);
        Ok(master_id)
    }

    /// Resolve an address: if virtual and registered, return the master.
    pub fn resolve(db: &InMemoryDB, addr: Address) -> Result<Address, RegistryError> {
        let Some((master_id, _user_tag)) = VirtualAddress::decode(addr) else {
            return Ok(addr);
        };

        Self::read_master(db, master_id).ok_or(RegistryError::VirtualAddressUnregistered)
    }
}

impl MasterRegistry for StorageBackedRegistry {
    type Error = RegistryError;

    fn register(&mut self, _caller: Address, _salt: B256) -> Result<MasterId, Self::Error> {
        // This trait impl cannot be used directly because it requires &mut InMemoryDB.
        // Use the static methods instead.
        unimplemented!("use StorageBackedRegistry::register(db, caller, salt) instead")
    }

    fn get_master(&self, _master_id: MasterId) -> Result<Option<Address>, Self::Error> {
        unimplemented!("use StorageBackedRegistry::read_master(db, master_id) instead")
    }
}

impl AddressResolver for StorageBackedRegistry {
    type Error = RegistryError;

    fn resolve_recipient(&self, _addr: Address) -> Result<Address, Self::Error> {
        unimplemented!("use StorageBackedRegistry::resolve(db, addr) instead")
    }
}
