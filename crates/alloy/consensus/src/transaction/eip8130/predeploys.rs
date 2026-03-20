//! Predeploy addresses for EIP-8130 system contracts and precompiles.
//!
//! These addresses are reserved in the genesis state for the account abstraction
//! infrastructure. The exact values are provisional and will be finalized before
//! mainnet activation.

use alloy_primitives::{Address, address};

/// Account configuration system contract.
/// Manages owner registrations, account creation, config changes, and locks.
pub const ACCOUNT_CONFIG_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa01");

/// Nonce Manager precompile address.
/// Provides read-only access to 2D nonces; writes are protocol-only.
pub const NONCE_MANAGER_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa02");

/// Transaction context precompile address.
/// Exposes the current AA transaction's metadata during execution.
pub const TX_CONTEXT_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa03");

/// Default account (wallet) contract.
/// Auto-delegated for bare EOAs that submit AA transactions.
pub const DEFAULT_ACCOUNT_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa04");

/// Native K1 (secp256k1 ECDSA) verifier contract.
pub const K1_VERIFIER_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa10");

/// Native P256 raw ECDSA verifier contract.
pub const P256_RAW_VERIFIER_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa11");

/// Native P256 WebAuthn verifier contract.
pub const P256_WEBAUTHN_VERIFIER_ADDRESS: Address =
    address!("0x000000000000000000000000000000000000aa12");

/// Native delegate verifier contract (1-hop delegation).
pub const DELEGATE_VERIFIER_ADDRESS: Address =
    address!("0x000000000000000000000000000000000000aa13");
