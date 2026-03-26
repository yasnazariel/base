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

// ---------------------------------------------------------------------------
// Verifier gas cost table
// ---------------------------------------------------------------------------

/// Configurable gas costs for native signature verification.
///
/// Each verifier type has a fixed gas charge that is deducted from the
/// transaction's gas limit during intrinsic gas calculation. These costs
/// account for the CPU work performed by native (Rust) verifiers that
/// would otherwise be "free" relative to on-chain STATICCALL verification.
///
/// `DELEGATE` acts as a 1-hop indirection: its cost is additive with the
/// cost of the target verifier it resolves to. For example, a delegate
/// wrapping K1 costs `DELEGATE + K1 = 3_000 + 6_000 = 9_000`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifierGasCosts {
    /// secp256k1 ECDSA recovery.
    pub k1: u64,
    /// secp256r1 / P-256 raw ECDSA verification.
    pub p256_raw: u64,
    /// secp256r1 / P-256 WebAuthn assertion verification.
    pub p256_webauthn: u64,
    /// Delegate hop overhead (added to the target verifier's cost).
    pub delegate: u64,
}

impl VerifierGasCosts {
    /// Default gas costs for BASE_V1.
    pub const BASE_V1: Self = Self { k1: 6_000, p256_raw: 9_500, p256_webauthn: 15_000, delegate: 3_000 };

    /// Returns the verification gas for a given verifier type byte.
    ///
    /// - For native types (K1, P256_RAW, P256_WEBAUTHN): returns the flat cost.
    /// - For DELEGATE: returns `delegate + inner_verifier_cost`. The
    ///   `inner_verifier_type` must be provided by the caller after resolving
    ///   the delegation target. If the inner type is unknown or custom, only
    ///   the delegate overhead is returned.
    /// - For CUSTOM (0x00) or unknown types: returns 0 (metered at runtime
    ///   via STATICCALL).
    pub fn gas_for_verifier(&self, verifier_type: u8, inner_verifier_type: Option<u8>) -> u64 {
        match verifier_type {
            VERIFIER_K1 => self.k1,
            VERIFIER_P256_RAW => self.p256_raw,
            VERIFIER_P256_WEBAUTHN => self.p256_webauthn,
            VERIFIER_DELEGATE => {
                let inner_cost = inner_verifier_type
                    .map(|t| self.gas_for_verifier(t, None))
                    .unwrap_or(0);
                self.delegate + inner_cost
            }
            _ => 0,
        }
    }
}

impl Default for VerifierGasCosts {
    fn default() -> Self {
        Self::BASE_V1
    }
}
