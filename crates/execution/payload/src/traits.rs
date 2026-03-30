use base_alloy_consensus::{OpBlock, OpReceipt};
use reth_payload_primitives::PayloadBuilderAttributes;
use reth_primitives::{OpBlockBody, OpHeader, OpPrimitives, OpTransactionSigned};
use reth_primitives_traits::{NodePrimitives, WithEncoded};

use crate::OpPayloadBuilderAttributes;

/// Helper trait to encapsulate common bounds on [`NodePrimitives`] for OP payload builder.
pub trait OpPayloadPrimitives:
    NodePrimitives<
        Block = OpBlock,
        BlockHeader = OpHeader,
        BlockBody = OpBlockBody,
        SignedTx = OpTransactionSigned,
        Receipt = OpReceipt,
    >
{
}

impl OpPayloadPrimitives for OpPrimitives {}

/// Attributes for the OP payload builder.
pub trait OpAttributes:
    PayloadBuilderAttributes<
        RpcPayloadAttributes = crate::OpPayloadAttributes,
        Error = alloy_rlp::Error,
    >
{
    /// Whether to use the transaction pool for the payload.
    fn no_tx_pool(&self) -> bool;

    /// Sequencer transactions to include in the payload.
    fn sequencer_transactions(&self) -> &[WithEncoded<OpTransactionSigned>];
}

impl OpAttributes for OpPayloadBuilderAttributes {
    fn no_tx_pool(&self) -> bool {
        self.no_tx_pool
    }

    fn sequencer_transactions(&self) -> &[WithEncoded<OpTransactionSigned>] {
        &self.transactions
    }
}
