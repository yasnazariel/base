#![doc = include_str!("../README.md")]
#![doc(
    html_logo_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    html_favicon_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    issue_tracker_base_url = "https://github.com/base/base/issues/"
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(clippy::useless_let_if_seq)]

extern crate alloc;

pub mod builder;
pub use builder::OpPayloadBuilder;
pub mod error;
pub mod payload;
use base_alloy_consensus::OpBlock;
use base_alloy_rpc_types_engine::OpExecutionData;
pub use payload::{
    OpBuiltPayload, OpPayloadAttributes, OpPayloadBuilderAttributes, payload_id_optimism,
};
mod traits;
use reth_payload_primitives::PayloadTypes;
use reth_primitives_traits::{Block as _, SealedBlock};
pub use traits::*;
pub mod validator;
pub use validator::OpExecutionPayloadValidator;

pub mod config;

/// ZST that aggregates Base [`PayloadTypes`].
#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
#[non_exhaustive]
pub struct OpPayloadTypes;

impl PayloadTypes for OpPayloadTypes {
    type ExecutionData = OpExecutionData;
    type BuiltPayload = OpBuiltPayload;
    type PayloadAttributes = OpPayloadAttributes;
    type PayloadBuilderAttributes = OpPayloadBuilderAttributes;

    fn block_to_payload(block: SealedBlock<OpBlock>) -> Self::ExecutionData {
        OpExecutionData::from_block_unchecked(
            block.hash(),
            &block.into_block().into_ethereum_block(),
        )
    }
}
