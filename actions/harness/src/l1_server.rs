//! In-process L1 JSON-RPC server for action tests.
//!
//! Exposes `eth_getBlockByHash`, `eth_getBlockByNumber`, and `eth_getLogs`
//! backed by a [`SharedL1Chain`], giving the production
//! [`base_consensus_engine::BaseEngineClient`] and [`L1WatcherActor`] an HTTP
//! endpoint to call when they need L1 block data or system-config logs.
//!
//! [`L1WatcherActor`]: base_consensus_node::L1WatcherActor

use std::net::SocketAddr;

use alloy_consensus::Sealed;
use alloy_eips::BlockNumberOrTag;
use alloy_primitives::B256;
use alloy_rpc_types_eth::{Block, BlockTransactions, Filter, Log, Transaction as EthTransaction};
use async_trait::async_trait;
use jsonrpsee::{
    core::RpcResult,
    proc_macros::rpc,
    server::{Server, ServerHandle},
};
use url::Url;

use crate::{L1Block, SharedL1Chain};

#[rpc(server, namespace = "eth")]
pub trait HarnessEthApi {
    /// Look up an L1 block by number or tag.  Transactions are always
    /// returned as hashes only (`full` is ignored).  Returns `None` for
    /// `Finalized` and `Safe` tags so the production [`BlockStream`] silently
    /// skips ticks rather than surfacing a deserialization error.
    ///
    /// [`BlockStream`]: base_consensus_node::BlockStream
    #[method(name = "getBlockByNumber")]
    async fn get_block_by_number(
        &self,
        block: BlockNumberOrTag,
        full: bool,
    ) -> RpcResult<Option<Block<EthTransaction>>>;

    /// Look up an L1 block by hash.  Transactions are always returned as
    /// hashes only (`full` is ignored).
    #[method(name = "getBlockByHash")]
    async fn get_block_by_hash(
        &self,
        hash: B256,
        full: bool,
    ) -> RpcResult<Option<Block<EthTransaction>>>;

    /// Returns an empty log set.
    ///
    /// Test chains never emit system-config updates, so `eth_getLogs` always
    /// returns `[]`.  This prevents [`L1WatcherActor`]'s log-retrier from
    /// exhausting its retry budget and killing the watcher task.
    ///
    /// [`L1WatcherActor`]: base_consensus_node::L1WatcherActor
    #[method(name = "getLogs")]
    async fn get_logs(&self, filter: Filter) -> RpcResult<Vec<Log>>;
}

/// JSON-RPC server adapter that resolves L1 `eth_*` reads against a
/// [`SharedL1Chain`] snapshot.
#[derive(Debug)]
pub struct HarnessL1Rpc {
    chain: SharedL1Chain,
}

impl HarnessL1Rpc {
    /// Convert an [`L1Block`] into an `eth`-namespace [`Block<EthTransaction>`]
    /// suitable for [`HarnessEthApi`] responses (transactions are returned as
    /// hashes only).
    pub fn l1_block_to_eth_rpc(l1: &L1Block) -> Block<EthTransaction> {
        let sealed = Sealed::new_unchecked(l1.header.clone(), l1.hash());
        let rpc_header = alloy_rpc_types_eth::Header::from_sealed(sealed);
        Block {
            header: rpc_header,
            uncles: vec![],
            transactions: BlockTransactions::Hashes(vec![]),
            withdrawals: None,
        }
    }
}

#[async_trait]
impl HarnessEthApiServer for HarnessL1Rpc {
    async fn get_block_by_number(
        &self,
        block: BlockNumberOrTag,
        _full: bool,
    ) -> RpcResult<Option<Block<EthTransaction>>> {
        let number = match block {
            BlockNumberOrTag::Number(n) => n,
            BlockNumberOrTag::Latest | BlockNumberOrTag::Pending => match self.chain.tip() {
                Some(b) => b.number(),
                None => return Ok(None),
            },
            BlockNumberOrTag::Earliest => 0,
            // Tests have no external finality signal; return null so BlockStream
            // silently skips the tick rather than surfacing a deserialization error.
            BlockNumberOrTag::Finalized | BlockNumberOrTag::Safe => return Ok(None),
        };
        Ok(self.chain.get_block(number).as_ref().map(Self::l1_block_to_eth_rpc))
    }

    async fn get_block_by_hash(
        &self,
        hash: B256,
        _full: bool,
    ) -> RpcResult<Option<Block<EthTransaction>>> {
        Ok(self.chain.block_by_hash(hash).as_ref().map(Self::l1_block_to_eth_rpc))
    }

    async fn get_logs(&self, _filter: Filter) -> RpcResult<Vec<Log>> {
        Ok(vec![])
    }
}

/// A running in-process L1 JSON-RPC server.
///
/// Serves `eth_getBlockByHash` and `eth_getBlockByNumber` from a
/// [`SharedL1Chain`] snapshot. Used so the production
/// [`base_consensus_engine::BaseEngineClient`] can fetch L1 block headers
/// during `find_starting_forkchoice` without a live L1 node.
///
/// The server is stopped automatically when this struct is dropped.
pub struct HarnessL1Server {
    /// The URL to pass to [`base_consensus_engine::EngineClientBuilder`] as `l1_rpc`.
    pub url: Url,
    _handle: ServerHandle,
}

impl std::fmt::Debug for HarnessL1Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessL1Server").field("url", &self.url).finish_non_exhaustive()
    }
}

impl HarnessL1Server {
    /// Spawn a new L1 server backed by `chain`.
    ///
    /// Binds to `127.0.0.1:0` (OS-assigned port). The server runs for as long
    /// as the returned [`HarnessL1Server`] is alive.
    pub async fn spawn(chain: SharedL1Chain) -> std::io::Result<Self> {
        let rpc = HarnessL1Rpc { chain };
        let module = rpc.into_rpc();

        let server = Server::builder().build("127.0.0.1:0").await?;
        let addr: SocketAddr = server.local_addr()?;
        let url = Url::parse(&format!("http://127.0.0.1:{}", addr.port()))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let handle = server.start(module);

        Ok(Self { url, _handle: handle })
    }
}
