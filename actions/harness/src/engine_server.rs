//! In-process Engine API JSON-RPC server for action tests.
//!
//! Wraps [`ActionEngineClient`] behind a real jsonrpsee HTTP server so that the
//! production [`base_consensus_engine::BaseEngineClient`] communicates with it
//! over localhost TCP. This ensures the full JSON-RPC serialisation / deserialisation
//! path and the JWT-authenticated HTTP transport are exercised in every actor test.

use std::{net::SocketAddr, sync::Arc};

use alloy_eips::{eip1898::BlockNumberOrTag, eip7685::Requests};
use alloy_primitives::{B256, BlockHash, U64};
use alloy_rpc_types_engine::{
    ClientVersionV1, ExecutionPayloadBodiesV1, ExecutionPayloadEnvelopeV2, ExecutionPayloadInputV2,
    ExecutionPayloadV3, ForkchoiceState, ForkchoiceUpdated, JwtSecret, PayloadId, PayloadStatus,
};
use alloy_rpc_types_eth::Block;
use async_trait::async_trait;
use base_common_network::Base;
use base_common_provider::BaseEngineApi;
use base_common_rpc_types::Transaction as BaseTransaction;
use base_common_rpc_types_engine::{
    BaseExecutionPayloadEnvelopeV3, BaseExecutionPayloadEnvelopeV4, BaseExecutionPayloadEnvelopeV5,
    BaseExecutionPayloadV4, BasePayloadAttributes,
};
use base_consensus_engine::HyperAuthClient;
use base_execution_payload_builder::BasePayloadTypes;
use base_execution_rpc::engine::{BaseEngineApiServer, OP_ENGINE_CAPABILITIES};
use base_node_core::OpEngineTypes;
use jsonrpsee::{
    core::RpcResult,
    proc_macros::rpc,
    server::{Server, ServerHandle},
    types::ErrorObject,
};
use alloy_transport_http::Http;
use url::Url;

use crate::ActionEngineClient;

/// Implements [`BaseEngineApiServer`] by delegating every call to an
/// in-process [`ActionEngineClient`].
struct HarnessEngineRpc {
    engine: Arc<ActionEngineClient>,
}

fn rpc_err(e: impl std::fmt::Display) -> ErrorObject<'static> {
    ErrorObject::owned(-32603, e.to_string(), None::<()>)
}

#[async_trait]
impl BaseEngineApiServer<OpEngineTypes<BasePayloadTypes>> for HarnessEngineRpc {
    async fn new_payload_v2(&self, payload: ExecutionPayloadInputV2) -> RpcResult<PayloadStatus> {
        <ActionEngineClient as BaseEngineApi<Base, Http<HyperAuthClient>>>::new_payload_v2(
            &self.engine,
            payload,
        )
        .await
        .map_err(rpc_err)
    }

    async fn new_payload_v3(
        &self,
        payload: ExecutionPayloadV3,
        _versioned_hashes: Vec<B256>,
        parent_beacon_block_root: B256,
    ) -> RpcResult<PayloadStatus> {
        <ActionEngineClient as BaseEngineApi<Base, Http<HyperAuthClient>>>::new_payload_v3(
            &self.engine,
            payload,
            parent_beacon_block_root,
        )
        .await
        .map_err(rpc_err)
    }

    async fn new_payload_v4(
        &self,
        payload: BaseExecutionPayloadV4,
        _versioned_hashes: Vec<B256>,
        parent_beacon_block_root: B256,
        _execution_requests: Requests,
    ) -> RpcResult<PayloadStatus> {
        <ActionEngineClient as BaseEngineApi<Base, Http<HyperAuthClient>>>::new_payload_v4(
            &self.engine,
            payload,
            parent_beacon_block_root,
        )
        .await
        .map_err(rpc_err)
    }

    async fn fork_choice_updated_v1(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<BasePayloadAttributes>,
    ) -> RpcResult<ForkchoiceUpdated> {
        <ActionEngineClient as BaseEngineApi<Base, Http<HyperAuthClient>>>::fork_choice_updated_v2(
            &self.engine,
            fork_choice_state,
            payload_attributes,
        )
        .await
        .map_err(rpc_err)
    }

    async fn fork_choice_updated_v2(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<BasePayloadAttributes>,
    ) -> RpcResult<ForkchoiceUpdated> {
        <ActionEngineClient as BaseEngineApi<Base, Http<HyperAuthClient>>>::fork_choice_updated_v2(
            &self.engine,
            fork_choice_state,
            payload_attributes,
        )
        .await
        .map_err(rpc_err)
    }

    async fn fork_choice_updated_v3(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<BasePayloadAttributes>,
    ) -> RpcResult<ForkchoiceUpdated> {
        <ActionEngineClient as BaseEngineApi<Base, Http<HyperAuthClient>>>::fork_choice_updated_v3(
            &self.engine,
            fork_choice_state,
            payload_attributes,
        )
        .await
        .map_err(rpc_err)
    }

    async fn get_payload_v2(&self, payload_id: PayloadId) -> RpcResult<ExecutionPayloadEnvelopeV2> {
        <ActionEngineClient as BaseEngineApi<Base, Http<HyperAuthClient>>>::get_payload_v2(
            &self.engine,
            payload_id,
        )
        .await
        .map_err(rpc_err)
    }

    async fn get_payload_v3(
        &self,
        payload_id: PayloadId,
    ) -> RpcResult<BaseExecutionPayloadEnvelopeV3> {
        <ActionEngineClient as BaseEngineApi<Base, Http<HyperAuthClient>>>::get_payload_v3(
            &self.engine,
            payload_id,
        )
        .await
        .map_err(rpc_err)
    }

    async fn get_payload_v4(
        &self,
        payload_id: PayloadId,
    ) -> RpcResult<BaseExecutionPayloadEnvelopeV4> {
        <ActionEngineClient as BaseEngineApi<Base, Http<HyperAuthClient>>>::get_payload_v4(
            &self.engine,
            payload_id,
        )
        .await
        .map_err(rpc_err)
    }

    async fn get_payload_v5(
        &self,
        payload_id: PayloadId,
    ) -> RpcResult<BaseExecutionPayloadEnvelopeV5> {
        <ActionEngineClient as BaseEngineApi<Base, Http<HyperAuthClient>>>::get_payload_v5(
            &self.engine,
            payload_id,
        )
        .await
        .map_err(rpc_err)
    }

    async fn get_payload_bodies_by_hash_v1(
        &self,
        _block_hashes: Vec<BlockHash>,
    ) -> RpcResult<ExecutionPayloadBodiesV1> {
        Err(ErrorObject::owned(-32601, "not supported in test harness", None::<()>))
    }

    async fn get_payload_bodies_by_range_v1(
        &self,
        _start: U64,
        _count: U64,
    ) -> RpcResult<ExecutionPayloadBodiesV1> {
        Err(ErrorObject::owned(-32601, "not supported in test harness", None::<()>))
    }

    async fn get_client_version_v1(
        &self,
        _client_version: ClientVersionV1,
    ) -> RpcResult<Vec<ClientVersionV1>> {
        Ok(vec![])
    }

    async fn exchange_capabilities(&self, _capabilities: Vec<String>) -> RpcResult<Vec<String>> {
        Ok(OP_ENGINE_CAPABILITIES.iter().map(|s| s.to_string()).collect())
    }
}

/// eth namespace methods served alongside the Engine API.
///
/// The production [`base_consensus_engine::BaseEngineClient::get_l2_block`]
/// issues `eth_getBlockByNumber` / `eth_getBlockByHash` calls to the Engine
/// API endpoint (the same port that serves the `engine_*` methods).
/// These two methods provide those lookups so that `find_starting_forkchoice`
/// and related bootstrap calls can resolve L2 block data over HTTP.
#[rpc(server, namespace = "eth")]
trait HarnessEthL2Api {
    #[method(name = "getBlockByNumber")]
    async fn get_block_by_number(
        &self,
        block: BlockNumberOrTag,
        full: bool,
    ) -> RpcResult<Option<Block<BaseTransaction>>>;

    #[method(name = "getBlockByHash")]
    async fn get_block_by_hash(
        &self,
        hash: B256,
        full: bool,
    ) -> RpcResult<Option<Block<BaseTransaction>>>;
}

struct HarnessEthL2Rpc {
    engine: Arc<ActionEngineClient>,
}

#[async_trait]
impl HarnessEthL2ApiServer for HarnessEthL2Rpc {
    async fn get_block_by_number(
        &self,
        block: BlockNumberOrTag,
        _full: bool,
    ) -> RpcResult<Option<Block<BaseTransaction>>> {
        Ok(self.engine.get_l2_block_by_numtag(block))
    }

    async fn get_block_by_hash(
        &self,
        hash: B256,
        _full: bool,
    ) -> RpcResult<Option<Block<BaseTransaction>>> {
        Ok(self.engine.get_l2_block_by_hash(hash))
    }
}

/// A running in-process Engine API JSON-RPC server.
///
/// The server binds to a random localhost port and serves all Engine API
/// methods by delegating to an [`ActionEngineClient`]. Callers build a
/// production [`base_consensus_engine::BaseEngineClient`] using [`Self::url`]
/// and [`Self::jwt`] via [`base_consensus_engine::EngineClientBuilder`].
///
/// The server is stopped automatically when this struct is dropped.
pub struct HarnessEngineServer {
    /// The URL to pass to [`base_consensus_engine::EngineClientBuilder`] as `l2`.
    pub url: Url,
    /// The JWT secret to pass to [`base_consensus_engine::EngineClientBuilder`] as `l2_jwt`.
    pub jwt: JwtSecret,
    _handle: ServerHandle,
}

impl std::fmt::Debug for HarnessEngineServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessEngineServer").field("url", &self.url).finish_non_exhaustive()
    }
}

impl HarnessEngineServer {
    /// Spawn a new engine API server backed by `engine`.
    ///
    /// Binds to `127.0.0.1:0` (OS-assigned port). The server runs for as long
    /// as the returned [`HarnessEngineServer`] is alive.
    pub async fn spawn(engine: Arc<ActionEngineClient>) -> std::io::Result<Self> {
        let jwt = JwtSecret::random();
        let mut module = HarnessEngineRpc { engine: Arc::clone(&engine) }.into_rpc();
        let eth_module = HarnessEthL2Rpc { engine }.into_rpc();
        module.merge(eth_module).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        let server = Server::builder().build("127.0.0.1:0").await?;
        let addr: SocketAddr = server.local_addr()?;
        let url = Url::parse(&format!("http://127.0.0.1:{}", addr.port()))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let handle = server.start(module);

        Ok(Self { url, jwt, _handle: handle })
    }
}
