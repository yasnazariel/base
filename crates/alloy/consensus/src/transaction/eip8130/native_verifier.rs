//! Native Rust verification for known verifier types.
//!
//! Provides a fast-path for mempool validation: known verifier types
//! (K1, P256_RAW) are verified using pure-Rust crypto instead of
//! spinning up an EVM STATICCALL. Custom and WebAuthn verifiers
//! always fall back to the on-chain STATICCALL path.

use alloy_primitives::{Address, B256, Bytes};
use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey};
use p256::ecdsa::{Signature as P256Signature, VerifyingKey as P256VerifyingKey};

use super::constants::{VERIFIER_K1, VERIFIER_P256_RAW};

/// Outcome of a native verification attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeVerifyResult {
    /// Verification succeeded — the returned `B256` is the resolved `ownerId`.
    Verified(B256),
    /// The verifier type is not natively supported; fall back to STATICCALL.
    Unsupported,
    /// Verification was attempted but the signature is invalid.
    Invalid(NativeVerifyError),
}

/// Errors from native signature verification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NativeVerifyError {
    /// K1 auth data has wrong length (expected 65 bytes).
    #[error("invalid K1 signature data (expected 65 bytes, got {0})")]
    K1BadLength(usize),
    /// K1 ecrecover failed.
    #[error("K1 recovery failed: {0}")]
    K1RecoveryFailed(String),
    /// K1 recovered address does not match the expected owner.
    #[error("K1 recovered address does not match expected owner")]
    K1AddressMismatch,
    /// P256 auth data has wrong length (expected 128 bytes).
    #[error("invalid P256 signature data (expected 128 bytes, got {0})")]
    P256BadLength(usize),
    /// P256 signature verification failed.
    #[error("P256 verification failed: {0}")]
    P256VerificationFailed(String),
}

/// Attempts native verification for a known verifier type.
///
/// - `verifier_type`: The type byte from `sender_auth[0]`.
/// - `data`: The auth data after the type byte (`sender_auth[1..]`).
/// - `hash`: The signature hash (sender or payer).
///
/// Returns [`NativeVerifyResult::Unsupported`] for custom (0x00), WebAuthn
/// (0x03), and delegate (0x04) verifiers, signaling the caller to use
/// the EVM STATICCALL path instead.
pub fn try_native_verify(
    verifier_type: u8,
    data: &Bytes,
    hash: B256,
) -> NativeVerifyResult {
    match verifier_type {
        VERIFIER_K1 => verify_k1(data, hash),
        VERIFIER_P256_RAW => verify_p256_raw(data, hash),
        _ => NativeVerifyResult::Unsupported,
    }
}

/// secp256k1 ECDSA verification with address recovery.
///
/// `data` layout: `r(32) || s(32) || v(1)` — standard 65-byte Ethereum signature.
///
/// The `ownerId` is derived as `bytes32(bytes20(ecrecover(hash, v, r, s)))`.
fn verify_k1(data: &Bytes, hash: B256) -> NativeVerifyResult {
    if data.len() != 65 {
        return NativeVerifyResult::Invalid(NativeVerifyError::K1BadLength(data.len()));
    }

    let r_s = &data[..64];
    let v_byte = data[64];

    let recovery_id = match v_byte {
        0 | 27 => 0,
        1 | 28 => 1,
        _ => {
            return NativeVerifyResult::Invalid(NativeVerifyError::K1RecoveryFailed(
                format!("invalid v byte: {v_byte}"),
            ));
        }
    };

    let signature = match K256Signature::from_slice(r_s) {
        Ok(sig) => sig,
        Err(e) => {
            return NativeVerifyResult::Invalid(NativeVerifyError::K1RecoveryFailed(
                e.to_string(),
            ));
        }
    };

    let recid = match RecoveryId::new(recovery_id != 0, false) {
        id => id,
    };

    let recovered_key = match VerifyingKey::recover_from_prehash(hash.as_slice(), &signature, recid)
    {
        Ok(key) => key,
        Err(e) => {
            return NativeVerifyResult::Invalid(NativeVerifyError::K1RecoveryFailed(
                e.to_string(),
            ));
        }
    };

    let public_key_bytes = recovered_key.to_encoded_point(false);
    let address = address_from_pubkey(public_key_bytes.as_bytes());

    let mut owner_id = [0u8; 32];
    owner_id[..20].copy_from_slice(address.as_slice());
    NativeVerifyResult::Verified(B256::from(owner_id))
}

/// Derives an Ethereum address from an uncompressed public key (65 bytes).
fn address_from_pubkey(uncompressed: &[u8]) -> Address {
    debug_assert!(uncompressed.len() == 65 && uncompressed[0] == 0x04);
    let hash = alloy_primitives::keccak256(&uncompressed[1..]);
    Address::from_slice(&hash[12..])
}

/// secp256r1 (P-256) raw ECDSA verification.
///
/// `data` layout: `public_key(64) || r(32) || s(32)` — 128 bytes total.
///
/// The `ownerId` is `keccak256(public_key)`.
fn verify_p256_raw(data: &Bytes, hash: B256) -> NativeVerifyResult {
    if data.len() != 128 {
        return NativeVerifyResult::Invalid(NativeVerifyError::P256BadLength(data.len()));
    }

    let pubkey_bytes = &data[..64];
    let sig_bytes = &data[64..128];

    let mut uncompressed = [0u8; 65];
    uncompressed[0] = 0x04;
    uncompressed[1..].copy_from_slice(pubkey_bytes);

    let verifying_key = match P256VerifyingKey::from_sec1_bytes(&uncompressed) {
        Ok(key) => key,
        Err(e) => {
            return NativeVerifyResult::Invalid(NativeVerifyError::P256VerificationFailed(
                e.to_string(),
            ));
        }
    };

    let signature = match P256Signature::from_slice(sig_bytes) {
        Ok(sig) => sig,
        Err(e) => {
            return NativeVerifyResult::Invalid(NativeVerifyError::P256VerificationFailed(
                e.to_string(),
            ));
        }
    };

    use p256::ecdsa::signature::hazmat::PrehashVerifier;
    match verifying_key.verify_prehash(hash.as_slice(), &signature) {
        Ok(()) => {}
        Err(e) => {
            return NativeVerifyResult::Invalid(NativeVerifyError::P256VerificationFailed(
                e.to_string(),
            ));
        }
    }

    let owner_id = alloy_primitives::keccak256(pubkey_bytes);
    NativeVerifyResult::Verified(owner_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::keccak256;
    use k256::elliptic_curve::rand_core::OsRng;

    #[test]
    fn unsupported_verifier_types_return_unsupported() {
        let data = Bytes::from(vec![0u8; 65]);
        let hash = B256::repeat_byte(0xAA);

        assert_eq!(
            try_native_verify(0x00, &data, hash),
            NativeVerifyResult::Unsupported,
        );
        assert_eq!(
            try_native_verify(0x03, &data, hash),
            NativeVerifyResult::Unsupported,
        );
        assert_eq!(
            try_native_verify(0x04, &data, hash),
            NativeVerifyResult::Unsupported,
        );
        assert_eq!(
            try_native_verify(0xFF, &data, hash),
            NativeVerifyResult::Unsupported,
        );
    }

    #[test]
    fn k1_bad_length_returns_invalid() {
        let data = Bytes::from(vec![0u8; 64]);
        let hash = B256::repeat_byte(0xAA);
        let result = try_native_verify(VERIFIER_K1, &data, hash);
        assert!(matches!(
            result,
            NativeVerifyResult::Invalid(NativeVerifyError::K1BadLength(64))
        ));
    }

    #[test]
    fn p256_bad_length_returns_invalid() {
        let data = Bytes::from(vec![0u8; 100]);
        let hash = B256::repeat_byte(0xBB);
        let result = try_native_verify(VERIFIER_P256_RAW, &data, hash);
        assert!(matches!(
            result,
            NativeVerifyResult::Invalid(NativeVerifyError::P256BadLength(100))
        ));
    }

    #[test]
    fn k1_valid_signature_verifies() {
        use k256::ecdsa::{SigningKey, signature::hazmat::PrehashSigner};

        let signing_key = SigningKey::random(&mut OsRng);
        let verifying_key = VerifyingKey::from(&signing_key);

        let hash = keccak256(b"test message");

        let (signature, recid) = signing_key.sign_prehash(hash.as_slice()).unwrap();
        let mut sig_bytes = Vec::with_capacity(65);
        sig_bytes.extend_from_slice(&signature.to_bytes());
        sig_bytes.push(recid.to_byte());

        let result = try_native_verify(VERIFIER_K1, &Bytes::from(sig_bytes), hash);

        match &result {
            NativeVerifyResult::Verified(owner_id) => {
                let pk_bytes = verifying_key.to_encoded_point(false);
                let expected_addr = address_from_pubkey(pk_bytes.as_bytes());
                let mut expected_owner_id = [0u8; 32];
                expected_owner_id[..20].copy_from_slice(expected_addr.as_slice());
                assert_eq!(*owner_id, B256::from(expected_owner_id));
            }
            other => panic!("expected Verified, got {:?}", other),
        }
    }

    #[test]
    fn p256_valid_signature_verifies() {
        use p256::ecdsa::{SigningKey as P256SigningKey, signature::hazmat::PrehashSigner};

        let signing_key = P256SigningKey::random(&mut OsRng);
        let verifying_key = P256VerifyingKey::from(&signing_key);

        let hash = keccak256(b"p256 test");

        let (signature, _): (P256Signature, _) =
            signing_key.sign_prehash(hash.as_slice()).unwrap();

        let pk_uncompressed = verifying_key.to_encoded_point(false);
        let pk_raw = &pk_uncompressed.as_bytes()[1..];

        let mut data = Vec::with_capacity(128);
        data.extend_from_slice(pk_raw);
        data.extend_from_slice(&signature.to_bytes());

        let result = try_native_verify(VERIFIER_P256_RAW, &Bytes::from(data), hash);

        match &result {
            NativeVerifyResult::Verified(owner_id) => {
                let expected = keccak256(pk_raw);
                assert_eq!(*owner_id, expected);
            }
            other => panic!("expected Verified, got {:?}", other),
        }
    }

    #[test]
    fn k1_wrong_signature_returns_valid_but_different_owner() {
        use k256::ecdsa::{SigningKey, signature::hazmat::PrehashSigner};

        let key_a = SigningKey::random(&mut OsRng);
        let key_b = SigningKey::random(&mut OsRng);

        let hash = keccak256(b"different keys");

        let (sig_a, recid_a) = key_a.sign_prehash(hash.as_slice()).unwrap();
        let mut sig_bytes = Vec::with_capacity(65);
        sig_bytes.extend_from_slice(&sig_a.to_bytes());
        sig_bytes.push(recid_a.to_byte());

        let result = try_native_verify(VERIFIER_K1, &Bytes::from(sig_bytes), hash);
        assert!(matches!(result, NativeVerifyResult::Verified(_)));

        let pk_b = VerifyingKey::from(&key_b).to_encoded_point(false);
        let addr_b = address_from_pubkey(pk_b.as_bytes());
        let mut owner_b = [0u8; 32];
        owner_b[..20].copy_from_slice(addr_b.as_slice());

        if let NativeVerifyResult::Verified(owner_id) = result {
            assert_ne!(owner_id, B256::from(owner_b));
        }
    }
}
