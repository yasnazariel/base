//! Preconfirmation tracking for sequencer leadership transfer.
//!
//! When a node is a follower in a conductor-based HA setup, it subscribes to the
//! current leader's flashblocks WebSocket feed and accumulates the transactions that
//! have been preconfirmed. On leadership transfer, the new leader calls
//! [`PreconfirmationTracker::take_transactions`] when building its first block so
//! those transactions are injected in-order, preserving user-visible transaction
//! ordering across the handoff.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use alloy_primitives::{B256, Bytes};
use alloy_rpc_types_engine::PayloadId;
use base_alloy_flashblocks::Flashblock;
use futures::StreamExt as _;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;

#[derive(Debug)]
struct PreconfirmationInner {
    /// Maps `payload_id` to `parent_hash`, populated when the first flashblock
    /// (index == 0) for a payload is received.
    payload_to_parent: HashMap<PayloadId, B256>,
    /// Maps `parent_hash` to the accumulated ordered transaction list together
    /// with the instant the entry was first created.
    transactions: HashMap<B256, (Vec<Bytes>, Instant)>,
}

/// Thread-safe accumulator of preconfirmed transactions keyed by parent block hash.
///
/// Written to by a [`PreconfirmationSubscriber`] task and read by the sequencer's
/// [`PayloadBuilder`](super::PayloadBuilder) when constructing the first block after
/// a leadership transfer.
#[derive(Debug)]
pub struct PreconfirmationTracker {
    inner: Mutex<PreconfirmationInner>,
    ttl: Duration,
}

impl PreconfirmationTracker {
    /// Creates a new tracker. Entries are evicted by [`take_transactions`] once they
    /// have been held for longer than `ttl`.
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(PreconfirmationInner {
                payload_to_parent: HashMap::new(),
                transactions: HashMap::new(),
            }),
            ttl,
        }
    }

    /// Records transactions from a received flashblock.
    ///
    /// For the first flashblock of a payload (`index == 0`, `base` is `Some`), the
    /// `parent_hash` is registered so subsequent differential flashblocks can be
    /// associated with the same parent. All transactions in `diff.transactions` are
    /// appended to the accumulated list for that parent hash.
    pub fn on_flashblock(&self, flashblock: &Flashblock) {
        let mut inner = self.inner.lock().expect("preconfirmation tracker lock poisoned");

        if flashblock.index == 0 {
            if let Some(base) = &flashblock.base {
                inner.payload_to_parent.insert(flashblock.payload_id, base.parent_hash);
            }
        }

        let Some(&parent_hash) = inner.payload_to_parent.get(&flashblock.payload_id) else {
            trace!(
                target: "sequencer::preconfirmation",
                payload_id = ?flashblock.payload_id,
                index = flashblock.index,
                "Received flashblock for unknown payload, skipping"
            );
            return;
        };

        let tx_count = flashblock.diff.transactions.len();
        let entry = inner.transactions.entry(parent_hash).or_insert_with(|| (Vec::new(), Instant::now()));
        entry.0.extend(flashblock.diff.transactions.iter().cloned());

        trace!(
            target: "sequencer::preconfirmation",
            payload_id = ?flashblock.payload_id,
            index = flashblock.index,
            parent_hash = %parent_hash,
            tx_count,
            "Accumulated preconfirmed transactions"
        );
    }

    /// Returns and removes all preconfirmed transactions for the given `parent_hash`.
    ///
    /// Returns `None` if no transactions are tracked for that hash or if the entry
    /// has exceeded the TTL. Consume-once: the entry is drained on read.
    pub fn take_transactions(&self, parent_hash: B256) -> Option<Vec<Bytes>> {
        let mut inner = self.inner.lock().expect("preconfirmation tracker lock poisoned");

        let (_, created_at) = inner.transactions.get(&parent_hash)?;
        if created_at.elapsed() > self.ttl {
            inner.transactions.remove(&parent_hash);
            inner.payload_to_parent.retain(|_, ph| *ph != parent_hash);
            return None;
        }

        let (txs, _) = inner.transactions.remove(&parent_hash)?;
        inner.payload_to_parent.retain(|_, ph| *ph != parent_hash);

        debug!(
            target: "sequencer::preconfirmation",
            parent_hash = %parent_hash,
            tx_count = txs.len(),
            "Taking preconfirmed transactions for injection"
        );

        Some(txs)
    }
}

/// Subscribes to the leader's flashblocks WebSocket feed and forwards flashblocks
/// to a [`PreconfirmationTracker`].
///
/// Reconnects with exponential backoff capped at 32 seconds. The subscription
/// should be started before the node can become leader so that preconfirmed
/// transactions are accumulated in time for the first block after a transfer.
#[derive(Debug)]
pub struct PreconfirmationSubscriber {
    ws_url: Url,
    tracker: Arc<PreconfirmationTracker>,
}

impl PreconfirmationSubscriber {
    /// Creates a new subscriber.
    pub const fn new(ws_url: Url, tracker: Arc<PreconfirmationTracker>) -> Self {
        Self { ws_url, tracker }
    }

    /// Spawns the background WebSocket subscription task.
    pub fn start(self) {
        tokio::spawn(self.run());
    }

    async fn run(self) {
        let mut backoff = Duration::from_secs(1);

        loop {
            match connect_async(self.ws_url.as_str()).await {
                Ok((ws_stream, _)) => {
                    backoff = Duration::from_secs(1);
                    info!(
                        target: "sequencer::preconfirmation",
                        url = %self.ws_url,
                        "WebSocket connected to flashblocks feed"
                    );

                    let (_, mut read) = ws_stream.split();
                    loop {
                        match read.next().await {
                            Some(Ok(msg @ (Message::Binary(_) | Message::Text(_)))) => {
                                match Flashblock::try_decode_message(msg.into_data()) {
                                    Ok(fb) => self.tracker.on_flashblock(&fb),
                                    Err(err) => {
                                        warn!(
                                            target: "sequencer::preconfirmation",
                                            error = %err,
                                            "Failed to decode flashblock"
                                        );
                                    }
                                }
                            }
                            Some(Ok(Message::Close(_))) | None => {
                                info!(
                                    target: "sequencer::preconfirmation",
                                    "WebSocket connection closed, reconnecting"
                                );
                                break;
                            }
                            Some(Err(err)) => {
                                warn!(
                                    target: "sequencer::preconfirmation",
                                    error = %err,
                                    "WebSocket error, reconnecting"
                                );
                                break;
                            }
                            Some(Ok(_)) => {}
                        }
                    }
                }
                Err(err) => {
                    warn!(
                        target: "sequencer::preconfirmation",
                        backoff = ?backoff,
                        error = %err,
                        "WebSocket connection failed, retrying"
                    );
                }
            }

            tokio::time::sleep(backoff).await;
            backoff = std::cmp::min(backoff * 2, Duration::from_secs(32));
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Bloom, Bytes, U256};
    use alloy_rpc_types_engine::PayloadId;
    use base_alloy_flashblocks::{
        ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, Flashblock, Metadata,
    };

    use super::*;

    fn make_flashblock(
        payload_id: PayloadId,
        index: u64,
        parent_hash: Option<B256>,
        transactions: Vec<Bytes>,
    ) -> Flashblock {
        let base = parent_hash.map(|ph| ExecutionPayloadBaseV1 {
            parent_hash: ph,
            parent_beacon_block_root: B256::ZERO,
            fee_recipient: Default::default(),
            prev_randao: B256::ZERO,
            block_number: 1,
            gas_limit: 30_000_000,
            timestamp: 1_700_000_000,
            extra_data: Bytes::new(),
            base_fee_per_gas: U256::from(1u64),
        });
        Flashblock {
            payload_id,
            index,
            base,
            diff: ExecutionPayloadFlashblockDeltaV1 {
                state_root: B256::ZERO,
                receipts_root: B256::ZERO,
                logs_bloom: Bloom::default(),
                gas_used: 0,
                block_hash: B256::ZERO,
                transactions,
                withdrawals: vec![],
                withdrawals_root: B256::ZERO,
                blob_gas_used: None,
            },
            metadata: Metadata { block_number: 1 },
        }
    }

    #[test]
    fn on_flashblock_index_zero_records_parent_hash() {
        let tracker = PreconfirmationTracker::new(Duration::from_secs(30));
        let parent = B256::from([1u8; 32]);
        let payload_id = PayloadId::default();

        tracker.on_flashblock(&make_flashblock(payload_id, 0, Some(parent), vec![]));

        let inner = tracker.inner.lock().unwrap();
        assert_eq!(inner.payload_to_parent.get(&payload_id), Some(&parent));
    }

    #[test]
    fn on_flashblock_accumulates_transactions_across_indices() {
        let tracker = PreconfirmationTracker::new(Duration::from_secs(30));
        let parent = B256::from([2u8; 32]);
        let payload_id = PayloadId::default();
        let tx1 = Bytes::from_static(b"tx1");
        let tx2 = Bytes::from_static(b"tx2");

        tracker.on_flashblock(&make_flashblock(payload_id, 0, Some(parent), vec![tx1.clone()]));
        tracker.on_flashblock(&make_flashblock(payload_id, 1, None, vec![tx2.clone()]));

        let txs = tracker.take_transactions(parent).expect("should have transactions");
        assert_eq!(txs, vec![tx1, tx2]);
    }

    #[test]
    fn take_transactions_is_consume_once() {
        let tracker = PreconfirmationTracker::new(Duration::from_secs(30));
        let parent = B256::from([3u8; 32]);
        let payload_id = PayloadId::default();

        tracker.on_flashblock(&make_flashblock(
            payload_id,
            0,
            Some(parent),
            vec![Bytes::from_static(b"tx")],
        ));

        assert!(tracker.take_transactions(parent).is_some());
        assert!(tracker.take_transactions(parent).is_none());
    }

    #[test]
    fn take_transactions_returns_none_for_expired_entry() {
        let tracker = PreconfirmationTracker::new(Duration::ZERO);
        let parent = B256::from([4u8; 32]);
        let payload_id = PayloadId::default();

        tracker.on_flashblock(&make_flashblock(
            payload_id,
            0,
            Some(parent),
            vec![Bytes::from_static(b"tx")],
        ));

        // TTL is zero so any elapsed time exceeds it.
        assert!(tracker.take_transactions(parent).is_none());
    }

    #[test]
    fn on_flashblock_nonzero_index_without_prior_zero_is_ignored() {
        let tracker = PreconfirmationTracker::new(Duration::from_secs(30));
        let parent = B256::from([5u8; 32]);
        let payload_id = PayloadId::default();

        // index=1 arrives without a prior index=0 registration.
        tracker.on_flashblock(&make_flashblock(payload_id, 1, None, vec![Bytes::from_static(b"tx")]));

        // Nothing should be accumulated.
        assert!(tracker.take_transactions(parent).is_none());
    }
}
