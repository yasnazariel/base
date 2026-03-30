use alloy_consensus::Header;
use reth_ethereum_primitives::TransactionSigned;
#[cfg(any(test, feature = "arbitrary"))]
pub use reth_primitives_traits::test_utils::{generate_valid_header, valid_header_strategy};

/// Ethereum full block.
///
/// Withdrawals can be optionally included at the end of the RLP encoded message.
pub type Block<T = TransactionSigned, H = Header> = alloy_consensus::Block<T, H>;

/// A response to `GetBlockBodies`, containing bodies if any bodies were found.
///
/// Withdrawals can be optionally included at the end of the RLP encoded message.
pub type BlockBody<T = TransactionSigned, H = Header> = alloy_consensus::BlockBody<T, H>;

/// Ethereum sealed block type
pub type SealedBlock<B = Block> = reth_primitives_traits::block::SealedBlock<B>;

/// Base block type.
pub type OpBlock = base_alloy_consensus::OpBlock;

/// Base block body type.
pub type OpBlockBody =
    alloy_consensus::BlockBody<base_alloy_consensus::OpTxEnvelope, alloy_consensus::Header>;
