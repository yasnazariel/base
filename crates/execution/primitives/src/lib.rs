#![doc = include_str!("../README.md")]
#![doc(
    html_logo_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    html_favicon_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    issue_tracker_base_url = "https://github.com/base/base/issues/"
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(not(feature = "std"), no_std)]

// Used in submodule transaction::signed and receipt.
use alloy_primitives as _;
use base_alloy_consensus::{OpBlock, OpReceipt};
use reth_primitives_traits::{RecoveredBlock, SealedBlock, SealedHeader};

pub mod transaction;
pub use transaction::*;

mod receipt;

pub use receipt::DepositReceipt;

/// Base-specific header type.
pub type OpHeader = alloy_consensus::Header;

/// Base-specific block body type.
pub type OpBlockBody = <OpBlock as reth_primitives_traits::Block>::Body;

/// Base-specific sealed header type.
pub type OpSealedHeader = SealedHeader<OpHeader>;

/// Base-specific sealed block type.
pub type OpSealedBlock = SealedBlock<OpBlock>;

/// Base-specific recovered block type.
pub type OpRecoveredBlock = RecoveredBlock<OpBlock>;

/// Primitive types for Base Node.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OpPrimitives;

impl reth_primitives_traits::NodePrimitives for OpPrimitives {
    type Block = OpBlock;
    type BlockHeader = OpHeader;
    type BlockBody = OpBlockBody;
    type SignedTx = OpTransactionSigned;
    type Receipt = OpReceipt;
}

/// Bincode-compatible serde implementations.
#[cfg(feature = "serde-bincode-compat")]
pub mod serde_bincode_compat {
    pub use super::receipt::serde_bincode_compat::OpReceipt as LocalOpReceipt;
}
