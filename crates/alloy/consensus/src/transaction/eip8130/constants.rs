//! EIP-8130 constants and verifier type identifiers.

/// The EIP-2718 transaction type byte for AA transactions.
pub const AA_TX_TYPE_ID: u8 = 0x05;

/// Payer signature domain separator byte. Ensures cryptographic domain separation
/// so a valid sender signature cannot be replayed as a payer signature.
pub const AA_PAYER_TYPE: u8 = 0x06;

/// Base intrinsic gas cost for an AA transaction (replaces the standard 21 000).
pub const AA_BASE_COST: u64 = 15_000;

/// Size in bytes of the EVM deployment header prepended to bytecode during CREATE2.
pub const DEPLOYMENT_HEADER_SIZE: usize = 14;

/// Maximum allowed size of `sender_auth` or `payer_auth` to bound DoS surface.
pub const MAX_SIGNATURE_SIZE: usize = 2048;

// ---------------------------------------------------------------------------
// Intrinsic gas sub-components
// ---------------------------------------------------------------------------

/// Gas charged when the `nonce_key` channel has not been used before (cold SSTORE).
pub const NONCE_KEY_COLD_GAS: u64 = 22_100;

/// Gas charged for an existing (warm) `nonce_key` channel.
pub const NONCE_KEY_WARM_GAS: u64 = 5_000;

/// Base gas for a CREATE2 deployment triggered by a create entry.
pub const BYTECODE_BASE_GAS: u64 = 32_000;

/// Per-byte gas for deployed bytecode.
pub const BYTECODE_PER_BYTE_GAS: u64 = 200;

/// Gas for each config operation that is applied (SSTORE).
pub const CONFIG_CHANGE_OP_GAS: u64 = 20_000;

/// Gas for each config change entry that is skipped (wrong chain, SLOAD only).
pub const CONFIG_CHANGE_SKIP_GAS: u64 = 2_100;

/// Cost of a single SLOAD during auth resolution.
pub const SLOAD_GAS: u64 = 2_100;

/// Flat gas cost for EOA (ecrecover) authentication.
pub const EOA_AUTH_GAS: u64 = 6_000;

// ---------------------------------------------------------------------------
// Native verifier type bytes
// ---------------------------------------------------------------------------

/// Custom verifier: `0x00 || address(20) || data`.
pub const VERIFIER_CUSTOM: u8 = 0x00;

/// secp256k1 ECDSA.
pub const VERIFIER_K1: u8 = 0x01;

/// secp256r1 / P-256 raw ECDSA.
pub const VERIFIER_P256_RAW: u8 = 0x02;

/// secp256r1 / P-256 WebAuthn assertion envelope.
pub const VERIFIER_P256_WEBAUTHN: u8 = 0x03;

/// Delegated validation (1-hop only).
pub const VERIFIER_DELEGATE: u8 = 0x04;
