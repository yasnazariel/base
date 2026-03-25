//! Addresses for EIP-8130 system contracts and precompiles.
//!
//! # Deployment model
//!
//! **Precompiles** (native code, no EVM bytecode, fixed addresses):
//!   - `NonceManager` (`0x…aa02`)  — 2D nonce reads
//!   - `TxContext`     (`0x…aa03`)  — AA transaction metadata
//!
//! **Deployed contracts** (Solidity, deployed at BASE_V1 activation via
//! `TxDeposit` upgrade transactions — see `base_consensus_upgrades::BaseV1`):
//!   - `AccountConfiguration` — owner registrations, account creation, locks
//!   - `K1Verifier`, `P256Verifier`, `WebAuthnVerifier`, `DelegateVerifier`
//!   - `DefaultAccount` — wallet implementation for EIP-7702 auto-delegation
//!
//! All deployed contract addresses are deterministic: `Deployers::BASE_V1_*.create(0)`.
//! On devnets with BASE_V1 active from genesis, the derivation pipeline injects
//! the upgrade deposit transactions at block 0.

use alloy_primitives::{Address, address};

// ── Precompiles (native, fixed addresses) ─────────────────────────

/// Nonce Manager precompile. Read-only 2D nonce access; writes are
/// protocol-only (handler pre-execution storage writes).
pub const NONCE_MANAGER_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa02");

/// Transaction context precompile. Exposes the current AA transaction's
/// `owner_id`, phase index, and call metadata during execution.
pub const TX_CONTEXT_ADDRESS: Address = address!("0x000000000000000000000000000000000000aa03");

// ── Deployed contracts (TxDeposit at BASE_V1 activation) ─────────
//
// All addresses are deterministic: `Deployers::BASE_V1_*.create(0)`.
// See `crates/consensus/upgrades/src/base_v1.rs` for the deposit
// transactions that deploy these contracts.
//
// On devnets where BASE_V1 is active from genesis, these are deployed
// by the derivation pipeline's upgrade transactions at block 0.

/// Default account (wallet) implementation contract. Bare EOAs that submit
/// AA transactions are auto-delegated to this address via EIP-7702.
pub const DEFAULT_ACCOUNT_ADDRESS: Address =
    address!("0xAb4eE49EE97e49807e180BD5Fb9D9F35783b84F2");

/// Account configuration system contract.
/// Manages owner registrations, account creation, config changes, and locks.
pub const ACCOUNT_CONFIG_ADDRESS: Address =
    address!("0xf946601D5424118A4e4054BB0B13133f216b4FeE");

/// K1 (secp256k1 ECDSA) verifier contract.
pub const K1_VERIFIER_ADDRESS: Address =
    address!("0x5Be482Da3E457aB3b439B184532224EC42c6b8Db");

/// P256 raw ECDSA verifier contract.
pub const P256_RAW_VERIFIER_ADDRESS: Address =
    address!("0x6751c7ED0C58319e75437f8E6Dafa2d7F6b8306F");

/// P256 WebAuthn verifier contract.
pub const P256_WEBAUTHN_VERIFIER_ADDRESS: Address =
    address!("0x3572bb3F611a40DDcA70e5b55Cc797D58357AD44");

/// Delegate verifier contract (1-hop delegation).
pub const DELEGATE_VERIFIER_ADDRESS: Address =
    address!("0xc758A89C53542164aaB7f6439e8c8cAcf628fF62");
