use std::{path::PathBuf, sync::Arc};

use alloy_provider::RootProvider;
use alloy_rpc_types_engine::JwtSecret;
use alloy_transport::TransportError;
use base_alloy_network::Base;
use base_consensus_engine::{EngineClientBuilder, OpEngineClient};
use base_consensus_genesis::RollupConfig;
use url::Url;

use crate::NodeMode;

/// How to reach the L2 engine API.
#[derive(Debug, Clone)]
pub enum EngineRpcAddress {
    /// Connect to the engine API over authenticated HTTP.
    Http(Url),
    /// Connect to the engine API over auth IPC.
    Ipc(PathBuf),
}

impl EngineRpcAddress {
    /// Returns the HTTP URL if this is an [`EngineRpcAddress::Http`] variant.
    pub fn http_url(&self) -> Option<&Url> {
        match self {
            Self::Http(url) => Some(url),
            Self::Ipc(_) => None,
        }
    }
}

/// Configuration for the Engine Actor.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// The [`RollupConfig`].
    pub config: Arc<RollupConfig>,

    /// How to connect to the L2 engine API.
    pub l2_rpc: EngineRpcAddress,

    /// The engine jwt secret.
    ///
    /// Only used when [`Self::l2_rpc`] is [`EngineRpcAddress::Http`].
    pub l2_jwt_secret: JwtSecret,

    /// The L1 rpc url.
    pub l1_url: Url,

    /// The mode of operation for the node.
    /// When the node is in sequencer mode, the engine actor will receive requests to build blocks
    /// from the sequencer actor.
    pub mode: NodeMode,
}

impl EngineConfig {
    /// Builds and returns the [`OpEngineClient`].
    pub async fn build_engine_client(
        self,
    ) -> Result<OpEngineClient<RootProvider, RootProvider<Base>>, TransportError> {
        let Self { config, l2_rpc, l2_jwt_secret, l1_url, mode: _ } = self;
        let engine = match l2_rpc {
            EngineRpcAddress::Http(url) => {
                OpEngineClient::<RootProvider, RootProvider<Base>>::rpc_client::<Base>(
                    url,
                    l2_jwt_secret,
                )
            }
            EngineRpcAddress::Ipc(path) => {
                let endpoint = path.to_string_lossy().into_owned();
                RootProvider::<Base>::connect(&endpoint).await?
            }
        };

        Ok(EngineClientBuilder::new(l1_url, config).build_with_engine_provider(engine))
    }
}
