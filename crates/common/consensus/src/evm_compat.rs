//! EVM compatibility implementations for base-alloy consensus types.
//!
//! Provides [`FromRecoveredTx`] and [`FromTxWithEncoded`] impls for
//! [`OpTxEnvelope`] and [`TxDeposit`].

use alloy_eips::{Encodable2718, Typed2718};
use alloy_evm::{FromRecoveredTx, FromTxWithEncoded};
use alloy_primitives::{Address, B256, Bytes, U256};
use base_revm::{
    DepositTransactionParts, Eip8130Call, Eip8130CodePlacement, Eip8130Parts,
    Eip8130SequenceUpdate, Eip8130StorageWrite, Eip8130VerifyCall, OpTransaction,
};
use revm::context::TxEnv;

use crate::{
    ACCOUNT_CONFIG_ADDRESS, AccountChangeEntry, OpTxEnvelope, OwnerScope, TxDeposit, TxEip8130,
    VERIFIER_CUSTOM, VerifierGasCosts, auto_delegation_code, config_change_sequence,
    config_change_writes, delegate_inner_verifier_type, derive_account_address,
    encode_verify_call, intrinsic_gas_with_costs, owner_registration_writes,
    payer_signature_hash, sender_signature_hash, total_verification_gas,
};
#[cfg(feature = "native-verifier")]
use crate::{
    NativeVerifyResult, ParsedSenderAuth, VERIFIER_K1, parse_sender_auth, try_native_verify,
};

#[cfg(feature = "native-verifier")]
fn derive_sender_owner_id(tx: &TxEip8130) -> B256 {
    let parsed = match parse_sender_auth(tx) {
        Ok(parsed) => parsed,
        Err(_) => return B256::ZERO,
    };
    let sig_hash = sender_signature_hash(tx);

    match parsed {
        ParsedSenderAuth::Eoa { signature } => {
            let signature = Bytes::copy_from_slice(&signature);
            match try_native_verify(VERIFIER_K1, &signature, sig_hash) {
                NativeVerifyResult::Verified(owner_id) => owner_id,
                NativeVerifyResult::Invalid(_) | NativeVerifyResult::Unsupported => B256::ZERO,
            }
        }
        ParsedSenderAuth::Configured { verifier_type, data } => {
            match try_native_verify(verifier_type, &data, sig_hash) {
                NativeVerifyResult::Verified(owner_id) => owner_id,
                NativeVerifyResult::Invalid(_) | NativeVerifyResult::Unsupported => B256::ZERO,
            }
        }
    }
}

#[cfg(not(feature = "native-verifier"))]
fn derive_sender_owner_id(_tx: &TxEip8130) -> B256 {
    B256::ZERO
}

/// Verifies the payer's signature and returns the payer `owner_id`.
///
/// For self-pay transactions returns `B256::ZERO`. For sponsored transactions,
/// the first byte of `payer_auth` identifies the verifier type; the remaining
/// bytes are passed to the native verifier.
#[cfg(feature = "native-verifier")]
fn derive_payer_owner_id(tx: &TxEip8130) -> B256 {
    if tx.is_self_pay() || tx.payer_auth.is_empty() {
        return B256::ZERO;
    }

    let verifier_type = tx.payer_auth[0];
    let data = Bytes::copy_from_slice(&tx.payer_auth[1..]);
    let sig_hash = payer_signature_hash(tx);

    match try_native_verify(verifier_type, &data, sig_hash) {
        NativeVerifyResult::Verified(owner_id) => owner_id,
        NativeVerifyResult::Invalid(_) | NativeVerifyResult::Unsupported => B256::ZERO,
    }
}

#[cfg(not(feature = "native-verifier"))]
fn derive_payer_owner_id(_tx: &TxEip8130) -> B256 {
    B256::ZERO
}

/// Builds a [`Eip8130VerifyCall`] for a custom verifier auth blob.
///
/// Returns `None` for native verifiers (type != 0x00) or empty blobs.
/// For custom verifiers: extracts the verifier address and remaining data,
/// then ABI-encodes `IVerifier.verify(hash, data)`.
fn build_verify_call(
    auth: &[u8],
    sig_hash: B256,
    account: Address,
    required_scope: u8,
) -> Option<Eip8130VerifyCall> {
    if auth.is_empty() || auth[0] != VERIFIER_CUSTOM {
        return None;
    }
    if auth.len() < 21 {
        return None;
    }
    let verifier = Address::from_slice(&auth[1..21]);
    let data = Bytes::copy_from_slice(&auth[21..]);
    let calldata = encode_verify_call(sig_hash, &data);
    Some(Eip8130VerifyCall { verifier, calldata, account, required_scope })
}

/// Build [`Eip8130Parts`] from a decoded [`TxEip8130`] for use by the handler.
///
/// `recovered_caller` is the ecrecovered sender address. For EOA mode
/// (`from == Address::ZERO`) this is the address derived from ecrecover;
/// for configured mode it equals `tx.from`.
///
/// Uses [`VerifierGasCosts::BASE_V1`] for verification gas computation.
/// Call [`build_eip8130_parts_with_costs`] to supply custom gas costs.
pub fn build_eip8130_parts(tx: &TxEip8130, recovered_caller: Address) -> Eip8130Parts {
    build_eip8130_parts_with_costs(tx, recovered_caller, &VerifierGasCosts::BASE_V1)
}

/// Build [`Eip8130Parts`] from a decoded [`TxEip8130`] with explicit gas costs.
///
/// `recovered_caller` is the ecrecovered sender address. For EOA mode
/// it overrides `tx.from` (which is `Address::ZERO`). For self-pay
/// transactions, the payer is also set to `recovered_caller`.
pub fn build_eip8130_parts_with_costs(
    tx: &TxEip8130,
    recovered_caller: Address,
    costs: &VerifierGasCosts,
) -> Eip8130Parts {
    let sender = recovered_caller;
    let payer = if tx.is_self_pay() { recovered_caller } else { tx.payer };
    let owner_id = derive_sender_owner_id(tx);
    let payer_owner_id = derive_payer_owner_id(tx);

    let sender_inner = delegate_inner_verifier_type(&tx.sender_auth);
    let payer_inner = delegate_inner_verifier_type(&tx.payer_auth);
    let verification_gas = total_verification_gas(tx, costs, sender_inner, payer_inner);

    let has_create_entry =
        tx.account_changes.iter().any(|e| matches!(e, AccountChangeEntry::Create(_)));

    let mut pre_writes = Vec::new();
    let mut sequence_updates = Vec::new();
    let mut code_placements = Vec::new();
    for entry in &tx.account_changes {
        match entry {
            AccountChangeEntry::Create(create) => {
                let account = derive_account_address(
                    ACCOUNT_CONFIG_ADDRESS,
                    create.user_salt,
                    &create.bytecode,
                    &create.initial_owners,
                );
                for w in owner_registration_writes(account, create) {
                    pre_writes.push(Eip8130StorageWrite {
                        address: w.address,
                        slot: w.slot,
                        value: w.value,
                    });
                }
                code_placements.push(Eip8130CodePlacement {
                    address: account,
                    code: create.bytecode.clone(),
                });
            }
            AccountChangeEntry::ConfigChange(cc) => {
                for w in config_change_writes(sender, cc) {
                    pre_writes.push(Eip8130StorageWrite {
                        address: w.address,
                        slot: w.slot,
                        value: w.value,
                    });
                }
                let seq = config_change_sequence(sender, cc);
                sequence_updates.push(Eip8130SequenceUpdate {
                    slot: seq.slot,
                    is_multichain: seq.is_multichain,
                    new_value: seq.new_value,
                });
            }
        }
    }

    let call_phases: Vec<Vec<Eip8130Call>> = tx
        .calls
        .iter()
        .map(|phase| {
            phase
                .iter()
                .map(|call| Eip8130Call { to: call.to, data: call.data.clone(), value: U256::ZERO })
                .collect()
        })
        .collect();

    let sender_verify_call = if !tx.is_eoa() {
        let sig_hash = sender_signature_hash(tx);
        build_verify_call(&tx.sender_auth, sig_hash, sender, OwnerScope::SENDER)
    } else {
        None
    };

    let payer_verify_call = if !tx.is_self_pay() && !tx.payer_auth.is_empty() {
        let sig_hash = payer_signature_hash(tx);
        build_verify_call(&tx.payer_auth, sig_hash, payer, OwnerScope::PAYER)
    } else {
        None
    };

    let aa_intrinsic_gas =
        intrinsic_gas_with_costs(tx, false /* cold nonce — worst case */, tx.chain_id, costs);

    let sender_auth_empty = !tx.is_eoa() && tx.sender_auth.is_empty();
    let payer_auth_empty = !tx.is_self_pay() && tx.payer_auth.is_empty();

    Eip8130Parts {
        sender,
        payer,
        owner_id,
        payer_owner_id,
        nonce_key: tx.nonce_key,
        has_create_entry,
        verification_gas,
        aa_intrinsic_gas,
        auto_delegation_code: auto_delegation_code(),
        pre_writes,
        sequence_updates,
        code_placements,
        call_phases,
        sender_verify_call,
        payer_verify_call,
        sender_auth_empty,
        payer_auth_empty,
    }
}

// ---------------------------------------------------------------------------
// FromRecoveredTx / FromTxWithEncoded – OpTxEnvelope -> TxEnv
// ---------------------------------------------------------------------------

impl FromRecoveredTx<OpTxEnvelope> for TxEnv {
    fn from_recovered_tx(tx: &OpTxEnvelope, caller: Address) -> Self {
        match tx {
            OpTxEnvelope::Legacy(tx) => Self::from_recovered_tx(tx.tx(), caller),
            OpTxEnvelope::Eip1559(tx) => Self::from_recovered_tx(tx.tx(), caller),
            OpTxEnvelope::Eip2930(tx) => Self::from_recovered_tx(tx.tx(), caller),
            OpTxEnvelope::Eip7702(tx) => Self::from_recovered_tx(tx.tx(), caller),
            OpTxEnvelope::Eip8130(tx) => {
                let inner = tx.inner();
                let mut env = Self {
                    tx_type: inner.ty(),
                    caller,
                    gas_limit: inner.gas_limit,
                    nonce: inner.nonce_sequence,
                    kind: revm::primitives::TxKind::Call(caller),
                    value: alloy_primitives::U256::ZERO,
                    data: alloy_primitives::Bytes::default(),
                    gas_price: inner.max_fee_per_gas,
                    gas_priority_fee: Some(inner.max_priority_fee_per_gas),
                    ..Default::default()
                };
                if !inner.authorization_list.is_empty() {
                    env.set_signed_authorization(inner.authorization_list.clone());
                }
                env
            }
            OpTxEnvelope::Deposit(tx) => Self::from_recovered_tx(tx.inner(), caller),
        }
    }
}

impl FromRecoveredTx<TxDeposit> for TxEnv {
    fn from_recovered_tx(tx: &TxDeposit, caller: Address) -> Self {
        let TxDeposit {
            to,
            value,
            gas_limit,
            input,
            source_hash: _,
            from: _,
            mint: _,
            is_system_transaction: _,
        } = tx;
        Self {
            tx_type: tx.ty(),
            caller,
            gas_limit: *gas_limit,
            kind: *to,
            value: *value,
            data: input.clone(),
            ..Default::default()
        }
    }
}

impl FromTxWithEncoded<OpTxEnvelope> for TxEnv {
    fn from_encoded_tx(tx: &OpTxEnvelope, caller: Address, _encoded: Bytes) -> Self {
        Self::from_recovered_tx(tx, caller)
    }
}

// ---------------------------------------------------------------------------
// FromRecoveredTx / FromTxWithEncoded – OpTxEnvelope -> OpTransaction<TxEnv>
// ---------------------------------------------------------------------------

impl FromRecoveredTx<OpTxEnvelope> for OpTransaction<TxEnv> {
    fn from_recovered_tx(tx: &OpTxEnvelope, sender: Address) -> Self {
        let encoded = tx.encoded_2718();
        Self::from_encoded_tx(tx, sender, encoded.into())
    }
}

impl FromTxWithEncoded<OpTxEnvelope> for OpTransaction<TxEnv> {
    fn from_encoded_tx(tx: &OpTxEnvelope, caller: Address, encoded: Bytes) -> Self {
        match tx {
            OpTxEnvelope::Legacy(tx) => Self {
                base: TxEnv::from_recovered_tx(tx.tx(), caller),
                enveloped_tx: Some(encoded),
                deposit: Default::default(),
                eip8130: Default::default(),
            },
            OpTxEnvelope::Eip1559(tx) => Self {
                base: TxEnv::from_recovered_tx(tx.tx(), caller),
                enveloped_tx: Some(encoded),
                deposit: Default::default(),
                eip8130: Default::default(),
            },
            OpTxEnvelope::Eip2930(tx) => Self {
                base: TxEnv::from_recovered_tx(tx.tx(), caller),
                enveloped_tx: Some(encoded),
                deposit: Default::default(),
                eip8130: Default::default(),
            },
            OpTxEnvelope::Eip7702(tx) => Self {
                base: TxEnv::from_recovered_tx(tx.tx(), caller),
                enveloped_tx: Some(encoded),
                deposit: Default::default(),
                eip8130: Default::default(),
            },
            OpTxEnvelope::Eip8130(tx) => {
                let eip8130 = build_eip8130_parts(tx.inner(), caller);
                Self {
                    base: TxEnv::from_recovered_tx(&OpTxEnvelope::Eip8130(tx.clone()), caller),
                    enveloped_tx: Some(encoded),
                    deposit: Default::default(),
                    eip8130,
                }
            }
            OpTxEnvelope::Deposit(tx) => Self::from_encoded_tx(tx.inner(), caller, encoded),
        }
    }
}

// ---------------------------------------------------------------------------
// TxDeposit -> OpTransaction<TxEnv>
// ---------------------------------------------------------------------------

impl FromRecoveredTx<TxDeposit> for OpTransaction<TxEnv> {
    fn from_recovered_tx(tx: &TxDeposit, sender: Address) -> Self {
        let encoded = tx.encoded_2718();
        Self::from_encoded_tx(tx, sender, encoded.into())
    }
}

impl FromTxWithEncoded<TxDeposit> for OpTransaction<TxEnv> {
    fn from_encoded_tx(tx: &TxDeposit, caller: Address, encoded: Bytes) -> Self {
        let base = TxEnv::from_recovered_tx(tx, caller);
        let deposit = DepositTransactionParts {
            source_hash: tx.source_hash,
            mint: Some(tx.mint),
            is_system_transaction: tx.is_system_transaction,
        };
        Self { base, enveloped_tx: Some(encoded), deposit, eip8130: Default::default() }
    }
}

#[cfg(all(test, feature = "native-verifier"))]
mod tests {
    use alloy_primitives::keccak256;
    use k256::{
        ecdsa::{SigningKey, signature::hazmat::PrehashSigner},
        elliptic_curve::rand_core::OsRng,
    };

    use super::*;

    fn address_from_signing_key(signing_key: &SigningKey) -> Address {
        let pubkey = signing_key.verifying_key().to_encoded_point(false);
        let hash = keccak256(&pubkey.as_bytes()[1..]);
        Address::from_slice(&hash[12..])
    }

    #[test]
    fn derive_owner_id_for_configured_k1_sender() {
        let signing_key = SigningKey::random(&mut OsRng);
        let sender = address_from_signing_key(&signing_key);

        let mut tx = TxEip8130 {
            chain_id: 8453,
            from: sender,
            nonce_key: U256::ZERO,
            nonce_sequence: 7,
            max_priority_fee_per_gas: 1,
            max_fee_per_gas: 1,
            gas_limit: 21_000,
            ..Default::default()
        };

        let sig_hash = sender_signature_hash(&tx);
        let (signature, recovery_id) = signing_key.sign_prehash(sig_hash.as_slice()).unwrap();
        let mut auth = Vec::with_capacity(66);
        auth.push(VERIFIER_K1);
        auth.extend_from_slice(&signature.to_bytes());
        auth.push(recovery_id.to_byte());
        tx.sender_auth = Bytes::from(auth);

        let owner_id = derive_sender_owner_id(&tx);
        let mut expected = [0u8; 32];
        expected[..20].copy_from_slice(sender.as_slice());
        assert_eq!(owner_id, B256::from(expected));
    }

    #[test]
    fn derive_owner_id_unsupported_verifier_returns_zero() {
        let tx = TxEip8130 {
            from: Address::repeat_byte(0x11),
            sender_auth: Bytes::from(
                [
                    0x00u8, // custom verifier
                    0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
                    0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
                ]
                .to_vec(),
            ),
            ..Default::default()
        };

        assert_eq!(derive_sender_owner_id(&tx), B256::ZERO);
    }
}
