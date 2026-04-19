//! In-process L1 JSON-RPC server for action tests.
//!
//! Exposes `eth_getBlockByHash` and `eth_getBlockByNumber` backed by a
//! [`SharedL1Chain`], giving the production
//! [`base_consensus_engine::BaseEngineClient`] an HTTP endpoint to call when
//! it needs L1 block data (e.g. during `find_starting_forkchoice`).

use std::net::SocketAddr;

use alloy_consensus::Sealed;
use alloy_eips::BlockNumberOrTag;
use alloy_primitives::B256;
use alloy_rpc_types_eth::{Block, BlockTransactions, Transaction as EthTransaction};
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
    #[method(name = "getBlockByNumber")]
    async fn get_block_by_number(
        &self,
        block: BlockNumberOrTag,
        full: bool,
    ) -> RpcResult<Option<Block<EthTransaction>>>;

    #[method(name = "getBlockByHash")]
    async fn get_block_by_hash(
        &self,
        hash: B256,
        full: bool,
    ) -> RpcResult<Option<Block<EthTransaction>>>;
}

struct HarnessL1Rpc {
    chain: SharedL1Chain,
}

fn l1_block_to_rpc(l1: &L1Block) -> Block<EthTransaction> {
    let sealed = Sealed::new_unchecked(l1.header.clone(), l1.hash());
    let rpc_header = alloy_rpc_types_eth::Header::from_sealed(sealed);
    Block {
        header: rpc_header,
        uncles: vec![],
        transactions: BlockTransactions::Hashes(vec![]),
        withdrawals: None,
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
            BlockNumberOrTag::Latest | BlockNumberOrTag::Pending => {
                match self.chain.tip() {
                    Some(b) => b.number(),
                    None => return Ok(None),
                }
            }
            BlockNumberOrTag::Earliest => 0,
            _ => return Ok(None),
        };
        Ok(self.chain.get_block(number).as_ref().map(l1_block_to_rpc))
    }

    async fn get_block_by_hash(
        &self,
        hash: B256,
        _full: bool,
    ) -> RpcResult<Option<Block<EthTransaction>>> {
        Ok(self.chain.block_by_hash(hash).as_ref().map(l1_block_to_rpc))
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
