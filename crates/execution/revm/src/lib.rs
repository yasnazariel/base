#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc as std;

mod api;
pub use api::{DefaultOp, DefaultOpEvm, OpBuilder, OpContext, OpContextTr, OpError};

mod constants;
pub use constants::*;

mod evm;
pub use evm::OpEvm;

mod handler;
pub use handler::{IsTxError, OpHandler};

mod l1block;
pub use l1block::L1BlockInfo;

mod precompiles;
pub use precompiles::{
    BasePrecompiles, Eip8130TxContext, NONCE_BASE_SLOT, NONCE_MANAGER_ADDRESS, NONCE_MANAGER_GAS,
    TX_CONTEXT_ADDRESS, TX_CONTEXT_GAS, aa_nonce_slot, base_v1, bls12_381, bn254_pair,
    clear_eip8130_tx_context, encode_address, encode_b256, encode_u256, fjord, granite,
    get_eip8130_tx_context, isthmus, jovian, selector, set_eip8130_tx_context,
};

mod result;
pub use result::OpHaltReason;

mod rollup_config;
pub use rollup_config::RollupConfigExt;

mod spec;
pub use spec::*;

mod transaction;
pub use transaction::{
    Eip8130Call, Eip8130CodePlacement, Eip8130Parts, Eip8130PhaseResult, Eip8130StorageWrite, DEPOSIT_TRANSACTION_TYPE,
    DepositTransactionParts, OpBuildError, OpTransaction, OpTransactionBuilder,
    OpTransactionError, OpTxTr, decode_phase_statuses, encode_phase_statuses,
};

mod compat;
