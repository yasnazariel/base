//! Addresses for EIP-8130 system contracts and precompiles.
//!
//! # Deployment model
//!
//! **Precompiles** (native code, no EVM bytecode, fixed addresses):
//!   - `NonceManager` (`0x…aa02`)  — 2D nonce reads
//!   - `TxContext`     (`0x…aa03`)  — AA transaction metadata
//!
//! **Predeploy candidate** (can be injected at genesis once AccountConfig
//! address is finalized; currently deployed alongside AccountConfig via the
//! same forge script):
//!   - `DefaultAccount` (`0x…aa04`) — wallet implementation for auto-delegation
//!
//! **Deployed contracts** (Solidity, deployed via CREATE2 with `salt = 0`):
//!   - `AccountConfiguration` — owner registrations, account creation, locks
//!   - `K1Verifier`, `P256Verifier`, `WebAuthnVerifier`, `DelegateVerifier`
//!
//! Deployed contract addresses are deterministic (CREATE2 from the deploy
//! script in `contracts/eip-8130/script/Deploy.s.sol`). The constants below
//! are provisional values used during development; run `deploy-8130.sh` and
//! update these to match the actual deterministic addresses.

use alloy_primitives::{Address, address};

// ── Precompiles (native, fixed addresses) ─────────────────────────

/// Nonce Manager precompile. Read-only 2D nonce access; writes are
/// protocol-only (handler pre-execution storage writes).
pub const NONCE_MANAGER_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa02");

/// Transaction context precompile. Exposes the current AA transaction's
/// `owner_id`, phase index, and call metadata during execution.
pub const TX_CONTEXT_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa03");

// ── Predeploy (bytecode placed at genesis) ────────────────────────

/// Default account (wallet) implementation contract. Bare EOAs that submit
/// AA transactions are auto-delegated to this address via EIP-7702.
pub const DEFAULT_ACCOUNT_ADDRESS: Address = address!("0xb080bA38C82F824137A12Db1Ac53baeDa70e4a03");

// ── Deployed contracts (CREATE2, addresses are provisional) ───────

/// Account configuration system contract.
/// Manages owner registrations, account creation, config changes, and locks.
///
/// Deployed via `Deploy.s.sol` with `salt = 0`.
pub const ACCOUNT_CONFIG_ADDRESS: Address = address!("0x0F127193b72E0f8546A6F4E471b6F8241900932B");

/// K1 (secp256k1 ECDSA) verifier contract.
pub const K1_VERIFIER_ADDRESS: Address = address!("0x167Ad053B3d786C6a6dC90aCa456DE98625EE31C");

/// P256 raw ECDSA verifier contract.
pub const P256_RAW_VERIFIER_ADDRESS: Address =
    address!("0x0D8D9D476D39764D9C0eC19449497FE1F39c673B");

/// P256 WebAuthn verifier contract.
pub const P256_WEBAUTHN_VERIFIER_ADDRESS: Address =
    address!("0x895650b7dd7C5Bd1c31006A7790b353A8dB73F7D");

/// Delegate verifier contract (1-hop delegation).
pub const DELEGATE_VERIFIER_ADDRESS: Address =
    address!("0x1Bc0F6e1496420590fD4981Dd7b844525F32B1D1");
