//! EVM compatibility implementations for base-alloy consensus types.
//!
//! Provides [`FromRecoveredTx`] and [`FromTxWithEncoded`] impls for
//! [`OpTxEnvelope`] and [`TxDeposit`].

use alloy_eips::{Encodable2718, Typed2718};
use alloy_evm::{FromRecoveredTx, FromTxWithEncoded};
use alloy_primitives::{Address, Bytes, U256};
use base_revm::{Eip8130Call, Eip8130Parts, Eip8130StorageWrite, DepositTransactionParts, OpTransaction};
use revm::context::TxEnv;

use crate::{
    AccountChangeEntry, OpTxEnvelope, TxEip8130, TxDeposit, auto_delegation_code,
    config_change_writes, owner_registration_writes,
};

/// Build [`Eip8130Parts`] from a decoded [`TxEip8130`] for use by the handler.
fn build_eip8130_parts(tx: &TxEip8130) -> Eip8130Parts {
    let sender = tx.effective_sender();
    let payer = tx.effective_payer();

    let has_create_entry =
        tx.account_changes.iter().any(|e| matches!(e, AccountChangeEntry::Create(_)));

    let mut pre_writes = Vec::new();
    for entry in &tx.account_changes {
        match entry {
            AccountChangeEntry::Create(create) => {
                for w in owner_registration_writes(sender, create) {
                    pre_writes.push(Eip8130StorageWrite {
                        address: w.address,
                        slot: w.slot,
                        value: w.value,
                    });
                }
            }
            AccountChangeEntry::ConfigChange(cc) => {
                for w in config_change_writes(sender, cc) {
                    pre_writes.push(Eip8130StorageWrite {
                        address: w.address,
                        slot: w.slot,
                        value: w.value,
                    });
                }
            }
        }
    }

    let call_phases: Vec<Vec<Eip8130Call>> = tx
        .calls
        .iter()
        .map(|phase| {
            phase
                .iter()
                .map(|call| Eip8130Call {
                    to: call.to,
                    data: call.data.clone(),
                    value: U256::ZERO,
                })
                .collect()
        })
        .collect();

    Eip8130Parts {
        sender,
        payer,
        nonce_key: tx.nonce_key,
        has_create_entry,
        auto_delegation_code: auto_delegation_code(),
        pre_writes,
        call_phases,
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
                Self {
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
                }
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
                let eip8130 = build_eip8130_parts(tx.inner());
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
