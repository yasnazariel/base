//! Compiled bytecodes for test contracts.
//!
//! These are minimal EVM bytecode snippets used in integration tests. For
//! a production deployment, these would be compiled from Solidity source.

use alloy_primitives::{Address, U256};

/// A minimal ERC-20 token contract that resolves virtual addresses via the
/// registry before crediting the recipient.
///
/// For simplicity in this example, the "contract" logic is implemented in
/// Rust and executed via the [`RegistryEvmOverride`]. In a real deployment,
/// this would be a Solidity contract that calls `resolveRecipient()` on the
/// registry precompile.
///
/// Storage layout:
/// - `keccak256(abi.encode(address, 0))` → balance of `address`
/// - slot 1 → total supply
#[derive(Debug)]
pub struct ResolverErc20;

impl ResolverErc20 {
    /// Compute the storage slot for a given address's balance.
    pub fn balance_slot(addr: Address) -> U256 {
        use alloy_primitives::keccak256;
        let mut data = [0u8; 64];
        data[12..32].copy_from_slice(addr.as_slice());
        // slot 0 for balances mapping
        U256::from_be_bytes(keccak256(data).0)
    }

    /// Read the balance of `addr` from the EVM database.
    pub fn balance_of(db: &revm::database::InMemoryDB, token: Address, addr: Address) -> U256 {
        let slot = Self::balance_slot(addr);
        db.cache
            .accounts
            .get(&token)
            .and_then(|acct| acct.storage.get(&slot).copied())
            .unwrap_or(U256::ZERO)
    }
}
