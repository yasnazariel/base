//! EIP-8130 signature hash computation and auth parsing.

use alloc::vec::Vec;

use alloy_primitives::{Address, B256, Bytes, keccak256};

use super::{
    TxEip8130,
    constants::{
        VERIFIER_CUSTOM, VERIFIER_DELEGATE, VERIFIER_K1, VERIFIER_P256_RAW, VERIFIER_P256_WEBAUTHN,
    },
    types::ConfigChangeEntry,
};

/// Computes the EIP-712 config change authorization digest.
///
/// Matches the JS reference in `send-aa-tx.mjs::configChangeDigest()`.
/// The authorizer (an owner with CONFIG scope) signs this digest to
/// authorize the operations in a [`ConfigChangeEntry`].
pub fn config_change_digest(account: Address, change: &ConfigChangeEntry) -> B256 {
    let typehash = keccak256(
        "ConfigChange(address account,uint64 chainId,uint64 sequence,\
         ConfigOperation[] operations)\
         ConfigOperation(uint8 opType,address verifier,bytes32 ownerId,uint8 scope)",
    );

    let mut op_hashes = Vec::with_capacity(change.operations.len() * 32);
    for op in &change.operations {
        let mut buf = [0u8; 128]; // 4 * 32 bytes
        buf[31] = op.op_type;
        buf[44..64].copy_from_slice(op.verifier.as_slice());
        buf[64..96].copy_from_slice(op.owner_id.as_slice());
        buf[127] = op.scope;
        op_hashes.extend_from_slice(keccak256(buf).as_slice());
    }
    let operations_hash = keccak256(&op_hashes);

    let mut buf = [0u8; 160]; // 5 * 32 bytes
    buf[0..32].copy_from_slice(typehash.as_slice());
    buf[44..64].copy_from_slice(account.as_slice());
    buf[88..96].copy_from_slice(&change.chain_id.to_be_bytes());
    buf[120..128].copy_from_slice(&change.sequence.to_be_bytes());
    buf[128..160].copy_from_slice(operations_hash.as_slice());
    keccak256(buf)
}

/// Computed sender signature hash.
pub fn sender_signature_hash(tx: &TxEip8130) -> B256 {
    let mut buf = Vec::with_capacity(512);
    tx.encode_for_sender_signing(&mut buf);
    keccak256(&buf)
}

/// Computed payer signature hash.
pub fn payer_signature_hash(tx: &TxEip8130) -> B256 {
    let mut buf = Vec::with_capacity(512);
    tx.encode_for_payer_signing(&mut buf);
    keccak256(&buf)
}

/// Parsed sender authentication data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedSenderAuth {
    /// EOA mode: raw 65-byte ECDSA signature `(r || s || v)`.
    Eoa {
        /// The raw 65-byte ECDSA signature.
        signature: [u8; 65],
    },
    /// Configured owner mode: verifier type byte + verifier-specific data.
    Configured {
        /// The verifier type byte.
        verifier_type: u8,
        /// For native verifiers (0x01-0x04): the data after the type byte.
        /// For custom (0x00): the verifier address (20 bytes) + remaining data.
        data: Bytes,
    },
}

/// Identifies the verifier for a configured owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierTarget {
    /// One of the native verifiers (K1, P256_RAW, P256_WEBAUTHN, DELEGATE).
    Native {
        /// The verifier type byte (0x01-0x04).
        verifier_type: u8,
        /// Verifier-specific authentication data.
        data: Bytes,
    },
    /// A custom verifier contract.
    Custom {
        /// The address of the custom verifier contract.
        verifier_address: Address,
        /// Verifier-specific authentication data.
        data: Bytes,
    },
}

/// Parse `sender_auth` based on the transaction's `from` field.
///
/// - If `from == Address::ZERO` (EOA mode): expect exactly 65 bytes (raw ECDSA).
/// - Otherwise (configured owner): first byte is the verifier type.
pub fn parse_sender_auth(tx: &TxEip8130) -> Result<ParsedSenderAuth, &'static str> {
    if tx.is_eoa() {
        if tx.sender_auth.len() != 65 {
            return Err("EOA sender_auth must be exactly 65 bytes");
        }
        let mut sig = [0u8; 65];
        sig.copy_from_slice(&tx.sender_auth);
        return Ok(ParsedSenderAuth::Eoa { signature: sig });
    }

    if tx.sender_auth.is_empty() {
        return Err("configured sender_auth must not be empty");
    }

    let verifier_type = tx.sender_auth[0];
    let data = Bytes::copy_from_slice(&tx.sender_auth[1..]);
    Ok(ParsedSenderAuth::Configured { verifier_type, data })
}

/// Resolve a configured auth's verifier type + data into a concrete target.
pub fn resolve_verifier(verifier_type: u8, data: &Bytes) -> Result<VerifierTarget, &'static str> {
    match verifier_type {
        VERIFIER_K1 | VERIFIER_P256_RAW | VERIFIER_P256_WEBAUTHN | VERIFIER_DELEGATE => {
            Ok(VerifierTarget::Native { verifier_type, data: data.clone() })
        }
        VERIFIER_CUSTOM => {
            if data.len() < 20 {
                return Err("custom verifier auth must contain at least 20-byte address");
            }
            let verifier_address = Address::from_slice(&data[..20]);
            let remaining = Bytes::copy_from_slice(&data[20..]);
            Ok(VerifierTarget::Custom { verifier_address, data: remaining })
        }
        _ => Err("unknown verifier type byte"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TxEip8130;

    #[test]
    fn parse_eoa_auth() {
        let tx = TxEip8130 {
            from: Address::ZERO,
            sender_auth: Bytes::from([0xABu8; 65].as_slice()),
            ..Default::default()
        };
        let parsed = parse_sender_auth(&tx).unwrap();
        assert!(matches!(parsed, ParsedSenderAuth::Eoa { .. }));
    }

    #[test]
    fn parse_eoa_wrong_length() {
        let tx = TxEip8130 {
            from: Address::ZERO,
            sender_auth: Bytes::from_static(&[0x01; 64]),
            ..Default::default()
        };
        assert!(parse_sender_auth(&tx).is_err());
    }

    #[test]
    fn parse_configured_k1() {
        let mut auth = vec![VERIFIER_K1];
        auth.extend_from_slice(&[0xAB; 65]);
        let tx = TxEip8130 {
            from: Address::repeat_byte(0x01),
            sender_auth: Bytes::from(auth),
            ..Default::default()
        };
        let parsed = parse_sender_auth(&tx).unwrap();
        match parsed {
            ParsedSenderAuth::Configured { verifier_type, data } => {
                assert_eq!(verifier_type, VERIFIER_K1);
                assert_eq!(data.len(), 65);
            }
            _ => panic!("expected Configured"),
        }
    }

    #[test]
    fn parse_configured_custom() {
        let mut auth = vec![VERIFIER_CUSTOM];
        auth.extend_from_slice(&[0xCC; 20]); // verifier address
        auth.extend_from_slice(&[0xDD; 32]); // data
        let tx = TxEip8130 {
            from: Address::repeat_byte(0x01),
            sender_auth: Bytes::from(auth),
            ..Default::default()
        };
        let parsed = parse_sender_auth(&tx).unwrap();
        match parsed {
            ParsedSenderAuth::Configured { verifier_type, data } => {
                assert_eq!(verifier_type, VERIFIER_CUSTOM);
                let target = resolve_verifier(verifier_type, &data).unwrap();
                match target {
                    VerifierTarget::Custom { verifier_address, data } => {
                        assert_eq!(verifier_address, Address::repeat_byte(0xCC));
                        assert_eq!(data.len(), 32);
                    }
                    _ => panic!("expected Custom"),
                }
            }
            _ => panic!("expected Configured"),
        }
    }

    #[test]
    fn sender_payer_hashes_are_deterministic() {
        let tx = TxEip8130 {
            chain_id: 1,
            from: Address::repeat_byte(0x01),
            nonce_key: alloy_primitives::U256::ZERO,
            nonce_sequence: 1,
            ..Default::default()
        };
        let h1 = sender_signature_hash(&tx);
        let h2 = sender_signature_hash(&tx);
        assert_eq!(h1, h2);

        let p1 = payer_signature_hash(&tx);
        let p2 = payer_signature_hash(&tx);
        assert_eq!(p1, p2);

        assert_ne!(h1, p1, "sender and payer hashes must differ");
    }
}
