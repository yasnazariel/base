//! Revm utils and implementations specific to reth.

#![doc(
    html_logo_url = "https://raw.githubusercontent.com/paradigmxyz/reth/main/assets/reth-docs.png",
    html_favicon_url = "https://avatars0.githubusercontent.com/u/97369466?s=256",
    issue_tracker_base_url = "https://github.com/paradigmxyz/reth/issues/"
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

/// Cache database that reads from an underlying [`DatabaseRef`].
/// Database adapters for payload building.
pub mod cached;

/// A marker that can be used to cancel execution.
pub mod cancelled;

/// Contains glue code for integrating reth database into revm's [Database].
#[cfg(feature = "database")]
pub mod database;

mod op_api;
pub use op_api::{DefaultOp, DefaultOpEvm, OpBuilder, OpContext, OpContextTr, OpError};

mod op_constants;
pub use op_constants::*;

mod op_evm;
pub use op_evm::OpEvm;

mod op_handler;
pub use op_handler::{IsTxError, OpHandler};

mod op_l1block;
pub use op_l1block::L1BlockInfo;

mod op_precompiles;
pub use op_precompiles::{BasePrecompiles, bls12_381, bn254_pair};

mod op_result;
pub use op_result::OpHaltReason;

mod op_rollup_config;
pub use op_rollup_config::RollupConfigExt;

mod op_spec;
pub use op_spec::*;

mod op_transaction;
pub use op_transaction::{
    DEPOSIT_TRANSACTION_TYPE, DepositTransactionParts, OpBuildError, OpTransaction,
    OpTransactionBuilder, OpTransactionError, OpTxTr,
};

mod op_compat;

pub use revm::{database as db, inspector};

/// Common test helpers
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

// Convenience re-exports.
pub use revm::{self, database::State, *};

/// Helper types for execution witness generation.
#[cfg(feature = "witness")]
pub mod witness;
