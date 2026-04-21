//! KV-aware L1 header fetch and parent-header prefetching.
//!
//! When the proof program walks backwards through L1 headers (following
//! `parent_hash` pointers), each header triggers a separate
//! `debug_getRawHeader` RPC call. [`L1HeaderPrefetcher`] short-circuits on KV
//! hits and speculatively fetches parent headers in parallel so subsequent
//! oracle requests hit the KV store instead of the RPC.

use std::sync::Arc;

use alloy_consensus::Header;
use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{B256, Bytes, keccak256};
use alloy_provider::{Provider, RootProvider};
use alloy_rlp::Decodable;
use alloy_transport::TransportError;
use base_proof_preimage::PreimageKey;
use dashmap::DashSet;
use tokio::task::JoinSet;
use tracing::{debug, trace};

use crate::{HostError, Metrics, Result, SharedKeyValueStore};

/// Default number of parent headers to speculatively prefetch.
pub const DEFAULT_PREFETCH_DEPTH: usize = 64;

/// KV-aware L1 header fetch + background parent-header prefetch.
pub struct L1HeaderPrefetcher {
    provider: RootProvider,
    kv: SharedKeyValueStore,
    prefetch_depth: usize,
    in_flight: Arc<DashSet<u64>>,
}

impl std::fmt::Debug for L1HeaderPrefetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("L1HeaderPrefetcher")
            .field("prefetch_depth", &self.prefetch_depth)
            .finish_non_exhaustive()
    }
}

impl L1HeaderPrefetcher {
    /// Creates a new [`L1HeaderPrefetcher`].
    pub fn new(provider: RootProvider, kv: SharedKeyValueStore, prefetch_depth: usize) -> Self {
        Self { provider, kv, prefetch_depth, in_flight: Arc::new(DashSet::new()) }
    }

    /// Fetches an L1 header by hash and stores it in the KV store.
    ///
    /// Returns the decoded header so the caller can issue parent-prefetch
    /// without a second decode. If already in the KV store the RPC is skipped.
    pub async fn fetch_and_store_header(&self, hash: B256) -> Result<Header> {
        let key = PreimageKey::new_keccak256(*hash);

        {
            let kv = self.kv.read().await;
            if let Some(raw) = kv.get(key.into()) {
                return Header::decode(&mut raw.as_slice()).map_err(HostError::Rlp);
            }
        }

        let raw = self.fetch_raw_header_by_hash(hash).await?;
        let header = Header::decode(&mut raw.as_ref()).map_err(HostError::Rlp)?;
        self.kv.write().await.set(key.into(), raw.into())?;

        Ok(header)
    }

    /// Spawns a fire-and-forget background task that prefetches up to
    /// `prefetch_depth` parent headers below `header.number`.
    pub fn prefetch_parents(self: &Arc<Self>, header: &Header) {
        if self.prefetch_depth == 0 || header.number <= 1 {
            return;
        }

        let start = header.number - 1;
        let end = start.saturating_sub((self.prefetch_depth - 1) as u64).max(1);

        // Construct guards at the insert site so cancellation between here
        // and the per-block spawn still releases the in_flight entries.
        let guards: Vec<InFlightGuard> = (end..=start)
            .filter(|n| self.in_flight.insert(*n))
            .map(|block| InFlightGuard { set: Arc::clone(&self.in_flight), block })
            .collect();

        if guards.is_empty() {
            trace!(from_block = start, to_block = end, "all blocks already in-flight, skipping");
            return;
        }

        debug!(
            from_block = start,
            to_block = end,
            new_blocks = guards.len(),
            "spawning L1 header prefetch"
        );

        let me = Arc::clone(self);
        tokio::spawn(async move { me.prefetch_range(guards).await });
    }

    async fn fetch_raw_header_by_hash(
        &self,
        hash: B256,
    ) -> std::result::Result<Bytes, TransportError> {
        self.provider.client().request::<[B256; 1], Bytes>("debug_getRawHeader", [hash]).await
    }

    async fn fetch_raw_header_by_number(
        &self,
        block_number: u64,
    ) -> std::result::Result<Bytes, TransportError> {
        self.provider
            .client()
            .request::<[BlockNumberOrTag; 1], Bytes>(
                "debug_getRawHeader",
                [BlockNumberOrTag::Number(block_number)],
            )
            .await
    }

    async fn prefetch_range(self: Arc<Self>, guards: Vec<InFlightGuard>) {
        let mut tasks = JoinSet::new();

        for guard in guards {
            let me = Arc::clone(&self);
            tasks.spawn(async move {
                let block_number = guard.block;

                let raw = match me.fetch_raw_header_by_number(block_number).await {
                    Ok(raw) => raw,
                    Err(e) => {
                        debug!(block_number, error = %e, "L1 header prefetch failed");
                        return;
                    }
                };

                let key = PreimageKey::new_keccak256(*keccak256(&raw));
                // Per-entry write so the hint handler isn't blocked by the
                // whole batch and headers appear as each RPC completes.
                match me.kv.write().await.set(key.into(), raw.into()) {
                    Ok(()) => Metrics::l1_prefetch_stored_total().increment(1),
                    Err(e) => debug!(block_number, error = %e, "L1 prefetch store failed"),
                }
            });
        }

        tasks.join_all().await;
    }
}

/// RAII guard that removes a block number from the prefetcher's `in_flight`
/// set when dropped, so cancellation can't leak entries permanently.
struct InFlightGuard {
    set: Arc<DashSet<u64>>,
    block: u64,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.set.remove(&self.block);
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;
    use alloy_rlp::Encodable;
    use alloy_rpc_client::RpcClient;
    use alloy_transport::mock::Asserter;
    use tokio::{
        sync::RwLock,
        time::{Duration, sleep},
    };

    use super::*;
    use crate::MemoryKeyValueStore;

    fn mock_prefetcher(
        prefetch_depth: usize,
    ) -> (Arc<L1HeaderPrefetcher>, SharedKeyValueStore, Asserter) {
        let asserter = Asserter::new();
        let provider = RootProvider::new(RpcClient::mocked(asserter.clone()));
        let kv: SharedKeyValueStore = Arc::new(RwLock::new(MemoryKeyValueStore::new()));
        let prefetcher =
            Arc::new(L1HeaderPrefetcher::new(provider, Arc::clone(&kv), prefetch_depth));
        (prefetcher, kv, asserter)
    }

    fn in_flight_blocks(prefetcher: &L1HeaderPrefetcher) -> Vec<u64> {
        let mut blocks: Vec<u64> = prefetcher.in_flight.iter().map(|e| *e).collect();
        blocks.sort_unstable();
        blocks
    }

    fn make_header(number: u64) -> Header {
        Header { number, difficulty: U256::from(number), ..Header::default() }
    }

    fn rlp_encode(header: &Header) -> Vec<u8> {
        let mut out = Vec::new();
        header.encode(&mut out);
        out
    }

    async fn wait_until<F: FnMut() -> bool>(mut predicate: F) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !predicate() {
            if std::time::Instant::now() > deadline {
                panic!("predicate did not become true before deadline");
            }
            sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn fetch_and_store_header_short_circuits_on_kv_hit() {
        let (prefetcher, kv, asserter) = mock_prefetcher(0);

        let header = make_header(100);
        let raw = rlp_encode(&header);
        let hash = keccak256(&raw);
        let key = PreimageKey::new_keccak256(*hash);
        kv.write().await.set(key.into(), raw.clone()).expect("kv set");

        let got = prefetcher
            .fetch_and_store_header(hash)
            .await
            .expect("cache hit should succeed without RPC");

        assert_eq!(got.number, 100);
        assert_eq!(got.difficulty, U256::from(100u64));
        assert!(asserter.read_q().is_empty(), "no RPC should have been popped");
    }

    #[tokio::test]
    async fn fetch_and_store_header_falls_back_to_rpc_on_miss() {
        let (prefetcher, kv, asserter) = mock_prefetcher(0);

        let header = make_header(200);
        let raw = rlp_encode(&header);
        let hash = keccak256(&raw);

        asserter.push_success(&alloy_primitives::Bytes::from(raw.clone()));

        let got = prefetcher.fetch_and_store_header(hash).await.expect("rpc fetch should succeed");

        assert_eq!(got.number, 200);
        let cached = kv.read().await.get(PreimageKey::new_keccak256(*hash).into());
        assert_eq!(cached, Some(raw));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prefetch_parents_dedups_overlapping_calls() {
        // current_thread runtime: spawned tasks don't run until we yield, so
        // the synchronous in-flight state is observable here.
        let (prefetcher, _kv, asserter) = mock_prefetcher(4);

        let head = make_header(101);

        prefetcher.prefetch_parents(&head);
        assert_eq!(in_flight_blocks(&prefetcher), vec![97, 98, 99, 100]);

        prefetcher.prefetch_parents(&head);
        assert_eq!(in_flight_blocks(&prefetcher), vec![97, 98, 99, 100]);

        for _ in 0..4 {
            asserter.push_failure_msg("simulated rpc failure");
        }

        wait_until(|| in_flight_blocks(&prefetcher).is_empty()).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prefetch_parents_releases_guards_on_success() {
        let (prefetcher, kv, asserter) = mock_prefetcher(3);

        let headers: Vec<Header> = (97..=99).map(make_header).collect();
        for header in &headers {
            asserter.push_success(&alloy_primitives::Bytes::from(rlp_encode(header)));
        }

        prefetcher.prefetch_parents(&make_header(100));
        assert_eq!(in_flight_blocks(&prefetcher), vec![97, 98, 99]);

        wait_until(|| in_flight_blocks(&prefetcher).is_empty()).await;

        let kv = kv.read().await;
        for header in &headers {
            let raw = rlp_encode(header);
            let key = PreimageKey::new_keccak256(*keccak256(&raw));
            assert_eq!(kv.get(key.into()), Some(raw), "header {} not in kv", header.number);
        }
    }

    #[tokio::test]
    async fn prefetch_parents_no_op_below_genesis() {
        let (prefetcher, _kv, asserter) = mock_prefetcher(8);

        prefetcher.prefetch_parents(&make_header(1));
        assert!(in_flight_blocks(&prefetcher).is_empty());

        prefetcher.prefetch_parents(&make_header(0));
        assert!(in_flight_blocks(&prefetcher).is_empty());

        assert!(asserter.read_q().is_empty(), "no RPCs should have been queued");
    }
}
