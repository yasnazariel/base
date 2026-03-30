//! Reth RPC interface definitions
//!
//! Provides all RPC interfaces.
//!
//! ## Feature Flags
//!
//! - `client`: Enables JSON-RPC client support.

#![doc(
    html_logo_url = "https://raw.githubusercontent.com/paradigmxyz/reth/main/assets/reth-docs.png",
    html_favicon_url = "https://avatars0.githubusercontent.com/u/97369466?s=256",
    issue_tracker_base_url = "https://github.com/paradigmxyz/reth/issues/"
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod admin;
mod debug;
mod engine;
mod miner;
mod net;
mod reth;
mod rpc;
mod testing;
mod trace;
mod txpool;
mod web3;

/// re-export of all server traits
pub use servers::*;
pub use testing::{TESTING_BUILD_BLOCK_V1, TestingBuildBlockRequestV1};

/// Aggregates all server traits.
pub mod servers {
    pub use reth_rpc_eth_api::{
        self as eth, EthApiServer, EthFilterApiServer, EthPubSubApiServer, L2EthApiExtServer,
    };

    pub use crate::{
        admin::AdminApiServer,
        debug::{DebugApiServer, DebugExecutionWitnessApiServer},
        engine::{EngineApiServer, EngineEthApiServer, IntoEngineApiRpcModule},
        miner::MinerApiServer,
        net::NetApiServer,
        reth::RethApiServer,
        rpc::RpcApiServer,
        testing::TestingApiServer,
        trace::TraceApiServer,
        txpool::TxPoolApiServer,
        web3::Web3ApiServer,
    };
}

/// re-export of all client traits
#[cfg(feature = "client")]
pub use clients::*;

/// Aggregates all client traits.
#[cfg(feature = "client")]
pub mod clients {
    pub use reth_rpc_eth_api::{EthApiClient, EthFilterApiClient, L2EthApiExtServer};

    pub use crate::{
        admin::AdminApiClient,
        debug::{DebugApiClient, DebugExecutionWitnessApiClient},
        engine::{EngineApiClient, EngineEthApiClient},
        miner::MinerApiClient,
        net::NetApiClient,
        reth::RethApiClient,
        rpc::RpcApiServer,
        testing::TestingApiClient,
        trace::TraceApiClient,
        txpool::TxPoolApiClient,
        web3::Web3ApiClient,
    };
}
