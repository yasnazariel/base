//! Native Rust verification for known verifier types.
//!
//! Provides a fast-path for mempool validation: known verifier types
//! (K1, P256_RAW, P256_WEBAUTHN, DELEGATE) are verified using pure-Rust
//! crypto instead of spinning up an EVM STATICCALL. Custom verifiers
//! (0x00) fall back to the on-chain STATICCALL path.
//!
//! ## Delegate (0x04)
//!
//! Single-hop delegation: the auth blob is `inner_type || inner_data...`.
//! `try_native_verify` dispatches to the inner verifier. Nested delegation
//! (delegate-within-delegate) is rejected.
//!
//! ## WebAuthn (0x03)
//!
//! Full P256 cryptographic verification over the WebAuthn assertion
//! envelope. The auth data layout (after the 0x03 type byte):
//! `publicKey(64) || authenticatorData(37+) || clientDataJSONLen(4, BE) || clientDataJSON || signature(64)`
//!
//! The signed message is `sha256(authenticatorData || sha256(clientDataJSON))`.

use alloy_primitives::{Address, B256, Bytes};
use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey};
use p256::ecdsa::{Signature as P256Signature, VerifyingKey as P256VerifyingKey};
use sha2::{Digest, Sha256};

use super::constants::{
    VERIFIER_DELEGATE, VERIFIER_K1, VERIFIER_P256_RAW, VERIFIER_P256_WEBAUTHN,
};

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
    /// WebAuthn envelope is too short to contain required fields.
    #[error("WebAuthn envelope too short ({0} bytes)")]
    WebAuthnTooShort(usize),
    /// WebAuthn clientDataJSON length field overflows the envelope.
    #[error("WebAuthn clientDataJSON length ({0}) overflows envelope")]
    WebAuthnClientDataOverflow(usize),
    /// WebAuthn clientDataJSON is not valid UTF-8.
    #[error("WebAuthn clientDataJSON is not valid UTF-8")]
    WebAuthnClientDataNotUtf8,
    /// WebAuthn clientDataJSON does not contain the expected challenge.
    #[error("WebAuthn clientDataJSON missing or mismatched challenge")]
    WebAuthnChallengeMismatch,
    /// WebAuthn P256 signature does not verify.
    #[error("WebAuthn P256 signature verification failed: {0}")]
    WebAuthnSignatureInvalid(String),
    /// Delegate auth blob is too short (needs at least inner_type + inner_data).
    #[error("delegate auth too short ({0} bytes, need at least 2)")]
    DelegateTooShort(usize),
    /// Nested delegation (delegate wrapping delegate) is not allowed.
    #[error("nested delegation is not allowed")]
    DelegateNested,
}

/// Attempts native verification for a known verifier type.
///
/// - `verifier_type`: The type byte from `sender_auth[0]`.
/// - `data`: The auth data after the type byte (`sender_auth[1..]`).
/// - `hash`: The signature hash (sender or payer).
///
/// Returns [`NativeVerifyResult::Unsupported`] only for custom verifiers
/// (0x00) and unknown types, signaling the caller to use the EVM
/// STATICCALL path.
pub fn try_native_verify(
    verifier_type: u8,
    data: &Bytes,
    hash: B256,
) -> NativeVerifyResult {
    match verifier_type {
        VERIFIER_K1 => verify_k1(data, hash),
        VERIFIER_P256_RAW => verify_p256_raw(data, hash),
        VERIFIER_P256_WEBAUTHN => verify_webauthn(data, hash),
        VERIFIER_DELEGATE => verify_delegate(data, hash),
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

    let recid = RecoveryId::new(recovery_id != 0, false);

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

    match verify_p256_signature(pubkey_bytes, hash.as_slice(), sig_bytes) {
        Ok(()) => {
            let owner_id = alloy_primitives::keccak256(pubkey_bytes);
            NativeVerifyResult::Verified(owner_id)
        }
        Err(e) => NativeVerifyResult::Invalid(NativeVerifyError::P256VerificationFailed(e)),
    }
}

/// Shared P256 signature verification: parses the key and signature,
/// then verifies against the given prehash message.
fn verify_p256_signature(pubkey_raw: &[u8], message: &[u8], sig_bytes: &[u8]) -> Result<(), String> {
    let mut uncompressed = [0u8; 65];
    uncompressed[0] = 0x04;
    uncompressed[1..].copy_from_slice(pubkey_raw);

    let verifying_key =
        P256VerifyingKey::from_sec1_bytes(&uncompressed).map_err(|e| e.to_string())?;

    let signature = P256Signature::from_slice(sig_bytes).map_err(|e| e.to_string())?;

    use p256::ecdsa::signature::hazmat::PrehashVerifier;
    verifying_key
        .verify_prehash(message, &signature)
        .map_err(|e| e.to_string())
}

// ── WebAuthn ───────────────────────────────────────────────────────

/// Raw P256 public key length (x || y, no 0x04 prefix).
const P256_PUBKEY_LEN: usize = 64;
/// Minimum authenticator data: 32-byte rpIdHash + 1-byte flags + 4-byte signCount.
const MIN_AUTHENTICATOR_DATA_LEN: usize = 37;
/// P256 signature: r(32) || s(32).
const P256_SIG_LEN: usize = 64;
/// clientDataJSON length prefix (big-endian u32).
const CLIENT_DATA_LEN_PREFIX: usize = 4;

/// Full WebAuthn P256 verification.
///
/// Data layout (after the 0x03 type byte):
/// `publicKey(64) || authenticatorData(37+) || clientDataJSONLength(4, BE) || clientDataJSON || signature(64)`
///
/// Verification steps:
/// 1. Parse public key, authenticator data, clientDataJSON, and signature
/// 2. Validate clientDataJSON contains `base64url(hash)` as the challenge
/// 3. Compute `message = sha256(authenticatorData || sha256(clientDataJSON))`
/// 4. Verify P256 signature over `message` using public key
/// 5. Return `Verified(keccak256(publicKey))`
fn verify_webauthn(data: &Bytes, expected_hash: B256) -> NativeVerifyResult {
    let min_len =
        P256_PUBKEY_LEN + MIN_AUTHENTICATOR_DATA_LEN + CLIENT_DATA_LEN_PREFIX + P256_SIG_LEN;
    if data.len() < min_len {
        return NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnTooShort(data.len()));
    }

    let pubkey_bytes = &data[..P256_PUBKEY_LEN];
    let rest = &data[P256_PUBKEY_LEN..];

    let client_data_len_offset = MIN_AUTHENTICATOR_DATA_LEN;
    if rest.len() < client_data_len_offset + CLIENT_DATA_LEN_PREFIX {
        return NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnTooShort(data.len()));
    }

    let len_bytes: [u8; 4] = rest[client_data_len_offset..client_data_len_offset + 4]
        .try_into()
        .unwrap();
    let client_data_len = u32::from_be_bytes(len_bytes) as usize;

    let client_data_start = client_data_len_offset + CLIENT_DATA_LEN_PREFIX;
    let client_data_end = client_data_start + client_data_len;
    let expected_total = client_data_end + P256_SIG_LEN;

    if rest.len() < expected_total {
        return NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnClientDataOverflow(
            client_data_len,
        ));
    }

    let authenticator_data = &rest[..client_data_len_offset];
    let client_data_bytes = &rest[client_data_start..client_data_end];
    let sig_bytes = &rest[client_data_end..client_data_end + P256_SIG_LEN];

    let client_data_str = match core::str::from_utf8(client_data_bytes) {
        Ok(s) => s,
        Err(_) => {
            return NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnClientDataNotUtf8);
        }
    };

    let expected_challenge = base64_url_encode(expected_hash.as_slice());
    if !client_data_str.contains(&expected_challenge) {
        return NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnChallengeMismatch);
    }

    // message = sha256(authenticatorData || sha256(clientDataJSON))
    let client_data_hash = Sha256::digest(client_data_bytes);
    let mut hasher = Sha256::new();
    hasher.update(authenticator_data);
    hasher.update(client_data_hash);
    let message = hasher.finalize();

    match verify_p256_signature(pubkey_bytes, &message, sig_bytes) {
        Ok(()) => {
            let owner_id = alloy_primitives::keccak256(pubkey_bytes);
            NativeVerifyResult::Verified(owner_id)
        }
        Err(e) => {
            NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnSignatureInvalid(e))
        }
    }
}

/// Base64url-encodes bytes without padding (WebAuthn challenge format).
fn base64_url_encode(bytes: &[u8]) -> alloc::string::String {
    use alloc::string::String;

    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    let mut out = String::with_capacity((bytes.len() * 4 + 2) / 3);
    let chunks = bytes.chunks(3);
    for chunk in chunks {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        }
    }
    out
}

// ── Delegate ───────────────────────────────────────────────────────

/// Delegate (1-hop) verification.
///
/// `data` layout (after the 0x04 type byte):
/// `inner_verifier_type(1) || inner_auth_data(...)`
///
/// Dispatches to the inner verifier's native path. Nested delegation
/// (inner type = 0x04) is rejected.
fn verify_delegate(data: &Bytes, hash: B256) -> NativeVerifyResult {
    if data.len() < 2 {
        return NativeVerifyResult::Invalid(NativeVerifyError::DelegateTooShort(data.len()));
    }

    let inner_type = data[0];

    if inner_type == VERIFIER_DELEGATE {
        return NativeVerifyResult::Invalid(NativeVerifyError::DelegateNested);
    }

    let inner_data = Bytes::copy_from_slice(&data[1..]);
    try_native_verify(inner_type, &inner_data, hash)
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

    // ── WebAuthn tests ─────────────────────────────────────────────

    fn build_webauthn_envelope(
        hash: B256,
        signing_key: &p256::ecdsa::SigningKey,
        tamper_challenge: bool,
    ) -> Bytes {
        let pk_uncompressed = P256VerifyingKey::from(signing_key).to_encoded_point(false);
        let pk_raw = &pk_uncompressed.as_bytes()[1..];

        let authenticator_data = vec![0xAA; MIN_AUTHENTICATOR_DATA_LEN];

        let challenge_b64 = base64_url_encode(hash.as_slice());
        let client_data_json = if tamper_challenge {
            alloc::format!(
                r#"{{"type":"webauthn.get","challenge":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","origin":"https://example.com"}}"#,
            )
        } else {
            alloc::format!(
                r#"{{"type":"webauthn.get","challenge":"{}","origin":"https://example.com"}}"#,
                challenge_b64,
            )
        };

        let cd_bytes = client_data_json.as_bytes();
        let cd_len = (cd_bytes.len() as u32).to_be_bytes();

        // sign: sha256(authenticatorData || sha256(clientDataJSON))
        let client_data_hash = Sha256::digest(cd_bytes);
        let mut hasher = Sha256::new();
        hasher.update(&authenticator_data);
        hasher.update(client_data_hash);
        let message = hasher.finalize();

        use p256::ecdsa::signature::hazmat::PrehashSigner;
        let (sig, _): (P256Signature, _) = signing_key.sign_prehash(&message).unwrap();

        let mut envelope = Vec::new();
        envelope.extend_from_slice(pk_raw);
        envelope.extend_from_slice(&authenticator_data);
        envelope.extend_from_slice(&cd_len);
        envelope.extend_from_slice(cd_bytes);
        envelope.extend_from_slice(&sig.to_bytes());
        Bytes::from(envelope)
    }

    #[test]
    fn webauthn_too_short_returns_invalid() {
        let data = Bytes::from(vec![0u8; 50]);
        let hash = B256::repeat_byte(0xCC);
        assert!(matches!(
            try_native_verify(VERIFIER_P256_WEBAUTHN, &data, hash),
            NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnTooShort(50)),
        ));
    }

    #[test]
    fn webauthn_valid_signature_verifies() {
        use p256::ecdsa::SigningKey as P256SigningKey;

        let signing_key = P256SigningKey::random(&mut OsRng);
        let pk_uncompressed = P256VerifyingKey::from(&signing_key).to_encoded_point(false);
        let pk_raw = &pk_uncompressed.as_bytes()[1..];

        let hash = keccak256(b"webauthn test");
        let data = build_webauthn_envelope(hash, &signing_key, false);

        let result = try_native_verify(VERIFIER_P256_WEBAUTHN, &data, hash);
        match &result {
            NativeVerifyResult::Verified(owner_id) => {
                let expected = keccak256(pk_raw);
                assert_eq!(*owner_id, expected);
            }
            other => panic!("expected Verified, got {:?}", other),
        }
    }

    #[test]
    fn webauthn_wrong_challenge_returns_invalid() {
        use p256::ecdsa::SigningKey as P256SigningKey;

        let signing_key = P256SigningKey::random(&mut OsRng);
        let hash = keccak256(b"webauthn test");
        let data = build_webauthn_envelope(hash, &signing_key, true);
        assert!(matches!(
            try_native_verify(VERIFIER_P256_WEBAUTHN, &data, hash),
            NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnChallengeMismatch),
        ));
    }

    #[test]
    fn webauthn_wrong_key_returns_invalid() {
        use p256::ecdsa::SigningKey as P256SigningKey;

        let key_a = P256SigningKey::random(&mut OsRng);
        let key_b = P256SigningKey::random(&mut OsRng);

        let hash = keccak256(b"webauthn wrong key");
        // Build envelope signed by key_a
        let data = build_webauthn_envelope(hash, &key_a, false);

        // Replace the public key with key_b's, keeping key_a's signature
        let pk_b = P256VerifyingKey::from(&key_b).to_encoded_point(false);
        let pk_b_raw = &pk_b.as_bytes()[1..];
        let mut tampered = pk_b_raw.to_vec();
        tampered.extend_from_slice(&data[P256_PUBKEY_LEN..]);

        let result = try_native_verify(
            VERIFIER_P256_WEBAUTHN,
            &Bytes::from(tampered),
            hash,
        );
        assert!(matches!(
            result,
            NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnSignatureInvalid(_)),
        ));
    }

    #[test]
    fn webauthn_overflow_client_data_returns_invalid() {
        let hash = B256::repeat_byte(0xDD);
        let mut data = vec![0u8; P256_PUBKEY_LEN + MIN_AUTHENTICATOR_DATA_LEN];
        data.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // 4GB length
        data.extend_from_slice(&[0u8; P256_SIG_LEN]);
        assert!(matches!(
            try_native_verify(VERIFIER_P256_WEBAUTHN, &Bytes::from(data), hash),
            NativeVerifyResult::Invalid(NativeVerifyError::WebAuthnClientDataOverflow(_)),
        ));
    }

    // ── Delegate tests ─────────────────────────────────────────────

    #[test]
    fn delegate_k1_verifies_inner_signature() {
        use k256::ecdsa::{SigningKey, signature::hazmat::PrehashSigner};

        let signing_key = SigningKey::random(&mut OsRng);
        let verifying_key = VerifyingKey::from(&signing_key);

        let hash = keccak256(b"delegate k1 test");
        let (signature, recid) = signing_key.sign_prehash(hash.as_slice()).unwrap();

        // delegate data: inner_type(K1=0x01) || k1_sig(65)
        let mut data = Vec::new();
        data.push(VERIFIER_K1);
        data.extend_from_slice(&signature.to_bytes());
        data.push(recid.to_byte());

        let result = try_native_verify(VERIFIER_DELEGATE, &Bytes::from(data), hash);
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
    fn delegate_p256_verifies_inner_signature() {
        use p256::ecdsa::{SigningKey as P256SigningKey, signature::hazmat::PrehashSigner};

        let signing_key = P256SigningKey::random(&mut OsRng);
        let verifying_key = P256VerifyingKey::from(&signing_key);

        let hash = keccak256(b"delegate p256 test");
        let (signature, _): (P256Signature, _) =
            signing_key.sign_prehash(hash.as_slice()).unwrap();

        let pk_uncompressed = verifying_key.to_encoded_point(false);
        let pk_raw = &pk_uncompressed.as_bytes()[1..];

        // delegate data: inner_type(P256=0x02) || pubkey(64) || sig(64)
        let mut data = Vec::new();
        data.push(VERIFIER_P256_RAW);
        data.extend_from_slice(pk_raw);
        data.extend_from_slice(&signature.to_bytes());

        let result = try_native_verify(VERIFIER_DELEGATE, &Bytes::from(data), hash);
        match &result {
            NativeVerifyResult::Verified(owner_id) => {
                let expected = keccak256(pk_raw);
                assert_eq!(*owner_id, expected);
            }
            other => panic!("expected Verified, got {:?}", other),
        }
    }

    #[test]
    fn delegate_webauthn_verifies_inner_signature() {
        use p256::ecdsa::SigningKey as P256SigningKey;

        let signing_key = P256SigningKey::random(&mut OsRng);
        let pk_uncompressed = P256VerifyingKey::from(&signing_key).to_encoded_point(false);
        let pk_raw = &pk_uncompressed.as_bytes()[1..];

        let hash = keccak256(b"delegate webauthn test");
        let webauthn_data = build_webauthn_envelope(hash, &signing_key, false);

        // delegate data: inner_type(WebAuthn=0x03) || webauthn_envelope
        let mut data = Vec::new();
        data.push(VERIFIER_P256_WEBAUTHN);
        data.extend_from_slice(&webauthn_data);

        let result = try_native_verify(VERIFIER_DELEGATE, &Bytes::from(data), hash);
        match &result {
            NativeVerifyResult::Verified(owner_id) => {
                let expected = keccak256(pk_raw);
                assert_eq!(*owner_id, expected);
            }
            other => panic!("expected Verified, got {:?}", other),
        }
    }

    #[test]
    fn delegate_nested_returns_invalid() {
        // inner_type = DELEGATE (0x04)
        let data = Bytes::from(vec![VERIFIER_DELEGATE, VERIFIER_K1, 0xAA]);
        let hash = B256::repeat_byte(0xEE);
        assert!(matches!(
            try_native_verify(VERIFIER_DELEGATE, &data, hash),
            NativeVerifyResult::Invalid(NativeVerifyError::DelegateNested),
        ));
    }

    #[test]
    fn delegate_too_short_returns_invalid() {
        let data = Bytes::from(vec![0x01]);
        let hash = B256::repeat_byte(0xFF);
        assert!(matches!(
            try_native_verify(VERIFIER_DELEGATE, &data, hash),
            NativeVerifyResult::Invalid(NativeVerifyError::DelegateTooShort(1)),
        ));
    }

    #[test]
    fn delegate_custom_inner_returns_unsupported() {
        let mut data = vec![0x00]; // CUSTOM inner type
        data.extend_from_slice(&[0xCC; 20]); // fake address
        let hash = B256::repeat_byte(0xAA);
        assert_eq!(
            try_native_verify(VERIFIER_DELEGATE, &Bytes::from(data), hash),
            NativeVerifyResult::Unsupported,
        );
    }

    #[test]
    fn base64_url_encode_known_value() {
        let input = [0xDE, 0xAD, 0xBE, 0xEF];
        let encoded = base64_url_encode(&input);
        assert_eq!(encoded, "3q2-7w");
    }
}
