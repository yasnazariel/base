//! Block indexer.
//!
//! Two phases:
//!
//!  1. **Backfill**: from the stored cursor (or `VIBESCAN_START_BLOCK`) up
//!     to the node's current head, process blocks sequentially with a small
//!     parallel window for fetches.
//!  2. **Live**: subscribe to `newHeads` over WS and process each new head.
//!     If the subscription drops, we fall back to polling `eth_blockNumber`
//!     and resume cleanly.
//!
//! Reorgs: vibenet runs a single sequencer so reorgs should not happen. We
//! do assert `block.parentHash == stored_prev.hash` as a cheap correctness
//! check and refuse to proceed on mismatch. That's enough for devnet.

use crate::{
    rpc_proxy::{BaseBlock, BaseReceipt, RpcClient},
    storage::{ActivityRole, ActivityWrite, BlockRow, BlockWrite, Storage, TxRow},
};
use alloy_network_primitives::{ReceiptResponse as _, TransactionResponse as _};
use alloy_primitives::{Address, B256, U256, b256};
use alloy_rpc_types_eth::TransactionTrait as _;
use alloy_provider::{Provider, ProviderBuilder, RootProvider};
use alloy_rpc_types_eth::BlockId;
use base_common_network::Base;
use eyre::{Result, WrapErr, eyre};
use futures::StreamExt;
use std::time::Duration;
use tracing::{debug, info, warn};

/// `keccak256("Transfer(address,address,uint256)")`. Covers ERC-20 and
/// ERC-721 (ERC-721 marks from/to/id as indexed so topics.len() == 4; we
/// treat both identically for activity indexing).
const TRANSFER_TOPIC: B256 =
    b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

pub struct Indexer {
    rpc: RpcClient,
    ws_url: String,
    storage: Storage,
    expected_chain_id: u64,
    start_block: u64,
    backfill_concurrency: usize,
}

impl Indexer {
    pub fn new(
        rpc: RpcClient,
        ws_url: String,
        storage: Storage,
        expected_chain_id: u64,
        start_block: u64,
        backfill_concurrency: usize,
    ) -> Self {
        Self { rpc, ws_url, storage, expected_chain_id, start_block, backfill_concurrency }
    }

    /// Main loop: guard against wrong chain, backfill, then stream live.
    pub async fn run(self, shutdown: tokio::sync::watch::Receiver<bool>) -> Result<()> {
        let actual = self.rpc.chain_id().await?;
        if actual != self.expected_chain_id {
            return Err(eyre!(
                "chain id mismatch: upstream reports {actual}, config expects {}",
                self.expected_chain_id
            ));
        }
        info!(chain_id = actual, "indexer connected to upstream");

        self.ensure_genesis_consistent().await?;
        self.backfill(&shutdown).await?;
        self.stream_live(shutdown).await
    }

    /// Detect a chain-reset where the sqlite volume outlived the upstream
    /// chain. We compare the hash we stored for block 0 against the node's
    /// current block 0. On mismatch the DB is wiped and we re-index from
    /// scratch. This makes `just vibe` (which resets chain state) safe to
    /// run repeatedly without manually clearing the vibescan volume.
    async fn ensure_genesis_consistent(&self) -> Result<()> {
        let Some(stored) = self.storage.block_hash(0).await? else {
            return Ok(()); // fresh DB, nothing to compare
        };
        let upstream = self
            .rpc
            .block_by_number(0)
            .await?
            .ok_or_else(|| eyre!("upstream has no block 0 yet; is the node booted?"))?;
        let upstream_hash = upstream.header.hash;
        if upstream_hash != stored {
            warn!(
                stored = %stored,
                upstream = %upstream_hash,
                "genesis hash mismatch - chain was reset; wiping vibescan DB"
            );
            self.storage.wipe().await?;
        }
        Ok(())
    }

    /// Catch up from the cursor (or start_block) to the current head.
    async fn backfill(&self, shutdown: &tokio::sync::watch::Receiver<bool>) -> Result<()> {
        let mut next = match self.storage.cursor().await? {
            Some((n, _)) => n + 1,
            None => self.start_block,
        };
        let mut head = self.rpc.block_number().await?;

        info!(from = next, head, "backfill start");
        let mut last_log = std::time::Instant::now();

        while next <= head {
            if *shutdown.borrow() {
                info!("backfill interrupted by shutdown");
                return Ok(());
            }

            // Process the next window of blocks in strict order. We fetch
            // blocks + receipts in parallel but insert serially so the
            // cursor advances monotonically.
            let window: Vec<u64> = (next..=head.min(next + self.backfill_concurrency as u64 - 1))
                .collect();
            let window_len = window.len() as u64;

            let fetched = futures::stream::iter(window.iter().copied())
                .map(|n| {
                    let rpc = self.rpc.clone();
                    async move {
                        let block = rpc
                            .block_by_number(n)
                            .await
                            .wrap_err_with(|| format!("fetching block {n}"))?
                            .ok_or_else(|| eyre!("block {n} not found during backfill"))?;
                        let receipts = rpc
                            .block_receipts(BlockId::Number(n.into()))
                            .await
                            .wrap_err_with(|| format!("fetching receipts for block {n}"))?
                            .unwrap_or_default();
                        Ok::<_, eyre::Report>((block, receipts))
                    }
                })
                .buffered(self.backfill_concurrency)
                .collect::<Vec<_>>()
                .await;

            for result in fetched {
                let (block, receipts) = result?;
                let write = build_block_write(&block, &receipts)?;
                self.storage.insert_block(write).await?;
            }

            next += window_len;

            // Refresh head periodically so we exit the loop as soon as the
            // chain stops producing blocks during dev.
            if last_log.elapsed() >= Duration::from_secs(2) {
                head = self.rpc.block_number().await?;
                info!(next, head, "backfill progress");
                last_log = std::time::Instant::now();
            }
        }

        info!(at = next.saturating_sub(1), "backfill complete");
        Ok(())
    }

    /// Subscribe to newHeads; re-fetch each announced block as full (the
    /// subscription payload only carries the header).
    async fn stream_live(
        self,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }

            match self.run_subscription_once(&mut shutdown).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    warn!(%err, "live subscription errored; retrying in 3s");
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                        _ = shutdown.changed() => return Ok(()),
                    }
                }
            }
        }
    }

    async fn run_subscription_once(
        &self,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        // Open a dedicated WS provider. We don't reuse the HTTP one because
        // subscriptions require a persistent connection. Typed against the
        // Base network so the newHeads payloads (which embed OP deposit txs
        // on some chains) deserialize correctly.
        let ws_provider: RootProvider<Base> = ProviderBuilder::new()
            .disable_recommended_fillers()
            .network::<Base>()
            .connect_ws(alloy_provider::WsConnect::new(self.ws_url.clone()))
            .await
            .wrap_err_with(|| format!("connecting ws {}", self.ws_url))?
            .root()
            .clone();

        let sub = ws_provider
            .subscribe_blocks()
            .await
            .wrap_err("subscribing to newHeads")?;
        let mut stream = sub.into_stream();
        info!(ws = %self.ws_url, "live subscription established");

        loop {
            tokio::select! {
                head = stream.next() => {
                    let Some(head) = head else {
                        return Err(eyre!("newHeads stream closed"));
                    };
                    // Before indexing the new head, catch up any gap. This
                    // handles both the race between backfill finishing and
                    // the first subscription message, and any future
                    // reconnect where we missed a few blocks.
                    let expected_next = match self.storage.cursor().await? {
                        Some((n, _)) => n + 1,
                        None => self.start_block,
                    };
                    let announced = head.number;
                    if announced < expected_next {
                        debug!(announced, expected_next, "head below cursor; ignoring");
                        continue;
                    }
                    for n in expected_next..=announced {
                        self.index_one(n).await?;
                    }
                }
                _ = shutdown.changed() => return Ok(()),
            }
        }
    }

    async fn index_one(&self, number: u64) -> Result<()> {
        let block = self
            .rpc
            .block_by_number(number)
            .await?
            .ok_or_else(|| eyre!("block {number} missing during live index"))?;
        let receipts = self
            .rpc
            .block_receipts(BlockId::Number(number.into()))
            .await?
            .unwrap_or_default();

        // Cheap reorg guard: the parent of block N must match what we
        // stored as block N-1. A divergence on a single-sequencer devnet
        // indicates something is broken, so refuse to advance.
        if let Some((prev_num, prev_hash)) = self.storage.cursor().await? {
            if block.header.number > 0 && prev_num + 1 == block.header.number {
                let parent = block.header.parent_hash;
                if parent != prev_hash {
                    return Err(eyre!(
                        "reorg detected: block {number} parent {parent} != cursor hash {prev_hash}"
                    ));
                }
            }
        }

        let write = build_block_write(&block, &receipts)?;
        self.storage.insert_block(write).await?;
        debug!(number, "indexed block");
        Ok(())
    }
}

fn build_block_write(block: &BaseBlock, receipts: &[BaseReceipt]) -> Result<BlockWrite> {
    let header = &block.header;
    let tx_count = block.transactions.len() as u64;

    let block_row = BlockRow {
        number: header.number,
        hash: header.hash,
        timestamp: header.timestamp,
        miner: header.beneficiary,
        tx_count,
        gas_used: header.gas_used,
        gas_limit: header.gas_limit,
        base_fee: header.base_fee_per_gas.map(U256::from),
    };

    let mut tx_rows = Vec::with_capacity(tx_count as usize);
    let mut activity = Vec::with_capacity(tx_count as usize * 2);

    for (idx, tx) in block.transactions.txns().enumerate() {
        let hash = tx.tx_hash();
        let receipt = receipts.iter().find(|r| r.transaction_hash() == hash);
        let status = receipt.map(|r| u8::from(r.status())).unwrap_or(0);
        let to_addr = tx.to();
        let created =
            if to_addr.is_none() { receipt.and_then(|r| r.contract_address()) } else { None };

        let from_addr = tx.from();

        tx_rows.push(TxRow {
            hash,
            block_num: header.number,
            tx_index: idx as u64,
            from_addr,
            to_addr,
            value: tx.value(),
            status,
            created,
        });

        activity.push(ActivityWrite {
            address: from_addr,
            block_num: header.number,
            tx_index: idx as u64,
            log_index: -1,
            tx_hash: hash,
            role: ActivityRole::Sender,
            token: None,
        });

        if let Some(to) = to_addr {
            activity.push(ActivityWrite {
                address: to,
                block_num: header.number,
                tx_index: idx as u64,
                log_index: -1,
                tx_hash: hash,
                role: ActivityRole::Recipient,
                token: None,
            });
        }

        if let Some(created) = created {
            activity.push(ActivityWrite {
                address: created,
                block_num: header.number,
                tx_index: idx as u64,
                log_index: -1,
                tx_hash: hash,
                role: ActivityRole::Creator,
                token: None,
            });
        }

        // ERC-20/721 Transfer log extraction for the activity feed.
        if let Some(rcpt) = receipt {
            for log in rcpt.inner.logs() {
                let topics = log.topics();
                // Must have topic0 + from + to (3 topics min). ERC-721 has
                // id as a third indexed arg, which we ignore.
                if topics.len() < 3 || topics[0] != TRANSFER_TOPIC {
                    continue;
                }
                let token = log.address();
                let from = topic_to_address(&topics[1]);
                let to = topic_to_address(&topics[2]);
                let li = log.log_index.map(|i| i as i64).unwrap_or(-1);

                activity.push(ActivityWrite {
                    address: from,
                    block_num: header.number,
                    tx_index: idx as u64,
                    log_index: li,
                    tx_hash: hash,
                    role: ActivityRole::LogFrom,
                    token: Some(token),
                });
                activity.push(ActivityWrite {
                    address: to,
                    block_num: header.number,
                    tx_index: idx as u64,
                    log_index: li,
                    tx_hash: hash,
                    role: ActivityRole::LogTo,
                    token: Some(token),
                });
            }
        }
    }

    Ok(BlockWrite { block: Some(block_row), txs: tx_rows, activity })
}

fn topic_to_address(topic: &B256) -> Address {
    let bytes = topic.as_slice();
    Address::from_slice(&bytes[12..])
}
