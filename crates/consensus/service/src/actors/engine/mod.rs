//! The engine module — re-exports [`EngineHandle`] and supporting types from the engine crate,
//! plus the engine-specific service configuration and RPC processor.

mod config;
pub use config::EngineConfig;

mod error;
pub use error::EngineError;

mod rpc_request_processor;
pub use base_consensus_engine::{
    BootstrapRole, BuildTaskError, ConsolidateInput, DerivationEngineClient, EngineClient,
    EngineEvent, EngineHandle, EngineQueries, EngineState, HandleClientError as EngineClientError,
    HandleClientResult as EngineClientResult, NetworkEngineClient, SealTaskError,
    SequencerEngineClient,
};
#[cfg(test)]
pub use base_consensus_engine::{
    MockDerivationEngineClient, MockNetworkEngineClient, MockSequencerEngineClient,
};
pub use rpc_request_processor::{EngineRpcProcessor, EngineRpcRequestReceiver};
