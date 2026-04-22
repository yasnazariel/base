//! Batcher service startup and wiring.

use std::{collections::HashSet, sync::Arc};

use alloy_provider::{Provider, ProviderBuilder, RootProvider, network::Ethereum};
use alloy_rpc_types_eth::BlockNumberOrTag;
use base_batcher_admin::AdminServer;
use base_batcher_core::{
    AdminHandle, BatchDriver, DaThrottle, NoopThrottleClient, ThrottleClient, ThrottleConfig,
    ThrottleController, ThrottleStrategy,
};
use base_batcher_encoder::BatchEncoder;
use base_batcher_source::{BlockSubscription, HybridBlockSource, HybridL1HeadSource, SourceError};
use base_common_consensus::BaseBlock;
use base_common_network::Base;
use base_consensus_rpc::RollupNodeApiClient;
use base_runtime::TokioRuntime;
use base_tx_manager::{BaseTxMetrics, SignerConfig, SimpleTxManager, TxManagerConfig};
use futures::{StreamExt, future::BoxFuture, stream::BoxStream};
use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use tokio::sync::watch;
use tracing::{info, warn};
use url::Url;

use crate::{
    BatcherConfig, EndpointPool, EndpointRole, HealthMonitor, L1EndpointPool, L2EndpointPool,
    MAX_CHECK_RECENT_TXS_DEPTH, NullL1HeadSubscription, NullSubscription, Probe, RecentTxScanner,
    RollupEndpointPool, RpcL1HeadPollingSource, RpcPollingSource, RpcThrottleClient,
    SafeHeadPoller, WsBlockSubscription, WsL1HeadSubscription,
};

/// Service-internal throttle client variant: either a no-op or an RPC client.
///
/// Using a concrete enum avoids heap allocation while still allowing
/// `start` to return either branch based on config.
enum ServiceThrottle {
    Noop(NoopThrottleClient),
    Rpc(RpcThrottleClient),
}

impl ThrottleClient for ServiceThrottle {
    fn set_max_da_size(
        &self,
        max_tx_size: u64,
        max_block_size: u64,
    ) -> BoxFuture<'_, Result<(), Box<dyn std::error::Error + Send + Sync>>> {
        match self {
            Self::Noop(n) => n.set_max_da_size(max_tx_size, max_block_size),
            Self::Rpc(r) => r.set_max_da_size(max_tx_size, max_block_size),
        }
    }
}

/// Batcher-internal L2 subscription variant: either a live WS subscription or a no-op.
///
/// Using a concrete enum avoids heap allocation while still allowing
/// `build_subscription` to return either branch to `start`.
enum Subscription {
    Ws(WsBlockSubscription),
    Null(NullSubscription),
}

impl BlockSubscription for Subscription {
    fn take_stream(&mut self) -> BoxStream<'static, Result<BaseBlock, SourceError>> {
        match self {
            Self::Ws(ws) => ws.take_stream(),
            Self::Null(null) => null.take_stream(),
        }
    }
}

/// Batcher-internal L1 subscription variant: either a live WS subscription or a no-op.
enum L1Subscription {
    Ws(WsL1HeadSubscription),
    Null(NullL1HeadSubscription),
}

impl base_batcher_source::L1HeadSubscription for L1Subscription {
    fn take_stream(&mut self) -> BoxStream<'static, Result<u64, SourceError>> {
        match self {
            Self::Ws(ws) => ws.take_stream(),
            Self::Null(null) => null.take_stream(),
        }
    }
}

/// Concrete driver type produced by [`BatcherService::setup`].
///
/// Private — callers interact only through [`ReadyBatcher`].
type ServiceDriver = BatchDriver<
    TokioRuntime,
    BatchEncoder,
    HybridBlockSource<Subscription, RpcPollingSource, TokioRuntime>,
    SimpleTxManager<RootProvider>,
    ServiceThrottle,
    HybridL1HeadSource<L1Subscription, RpcL1HeadPollingSource>,
>;

/// A fully-initialised batcher ready to run the submission loop.
///
/// Created by [`BatcherService::setup`]. All connections are live and the
/// rollup config has been fetched. Call [`run`](Self::run) to enter the
/// main driver loop, or spawn it in a background task for in-process use.
#[derive(derive_more::Debug)]
pub struct ReadyBatcher {
    #[debug(skip)]
    driver: ServiceDriver,
    #[debug(skip)]
    admin_server: Option<AdminServer>,
}

impl ReadyBatcher {
    /// Run the batch submission loop until the runtime is cancelled.
    pub async fn run(self) -> eyre::Result<()> {
        info!("batcher driver running");
        match self.admin_server {
            Some(admin) => {
                let driver_run = self.driver.run();
                tokio::pin!(driver_run);
                tokio::select! {
                    r = &mut driver_run => { r?; }
                    () = admin.stopped() => {
                        warn!("admin server stopped unexpectedly; batcher continues without admin API");
                        driver_run.await?;
                    }
                }
            }
            None => self.driver.run().await?,
        }
        info!("batcher service shutting down");
        Ok(())
    }
}

/// The batcher service.
///
/// Wires the encoder, block source, L1 head source, transaction manager, and driver.
/// Call [`setup`](Self::setup) to initialise all components, then call
/// [`ReadyBatcher::run`] to enter the submission loop.
#[derive(Debug)]
pub struct BatcherService {
    /// Full batcher configuration.
    config: BatcherConfig,
}

impl BatcherService {
    /// Create a new [`BatcherService`] from the given configuration.
    pub const fn new(config: BatcherConfig) -> Self {
        Self { config }
    }

    /// Build a block subscription for the given optional L2 WebSocket URL.
    ///
    /// When `url` is `Some`, connects a dedicated WS provider, subscribes to
    /// new block headers, and builds a stream that fetches the full block for
    /// each header. The provider is wrapped in a [`WsBlockSubscription`] so its
    /// lifetime is tied to the returned subscription — and therefore to the
    /// [`HybridBlockSource`] that consumes it — rather than to this function's
    /// stack frame.
    ///
    /// When `url` is `None`, or if the WS connection fails, returns a
    /// [`NullSubscription`] so that [`HybridBlockSource`] falls back entirely
    /// to polling.
    ///
    /// [`HybridBlockSource`]: base_batcher_source::HybridBlockSource
    async fn build_l2_subscription(
        url: Option<&Url>,
        fetch_pool: Arc<L2EndpointPool>,
    ) -> Subscription {
        let Some(url) = url else {
            return Subscription::Null(NullSubscription);
        };

        let ws_provider = match ProviderBuilder::new().connect(url.as_str()).await {
            Ok(p) => Arc::new(p),
            Err(e) => {
                warn!(error = %e, l2_rpc = %url, "failed to connect L2 WS provider; falling back to polling");
                return Subscription::Null(NullSubscription);
            }
        };

        let sub = match ws_provider.subscribe_blocks().await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "failed to subscribe to new L2 blocks; falling back to polling");
                return Subscription::Null(NullSubscription);
            }
        };

        let stream = sub
            .into_stream()
            .then(move |header| {
                // Resolve the active provider per-header so the fetch follows
                // the L2 pool's failover decisions instead of pinning to the
                // initial active endpoint for the lifetime of the subscription.
                let provider = fetch_pool.active();
                async move {
                    let rpc_block = provider
                        .get_block_by_number(BlockNumberOrTag::Number(header.number))
                        .full()
                        .await
                        .map_err(|e| SourceError::Provider(e.to_string()))?
                        .ok_or_else(|| {
                            SourceError::Provider(format!("block {} not found", header.number))
                        })?;
                    let block =
                        rpc_block.into_consensus().map_transactions(|t| t.inner.into_inner());
                    Ok(block)
                }
            })
            .boxed();

        Subscription::Ws(WsBlockSubscription::new(ws_provider, stream))
    }

    /// Build an L1 head subscription for the given optional L1 WebSocket URL.
    ///
    /// When `url` is `Some`, connects a dedicated WS provider, subscribes to
    /// new L1 block headers, and streams their block numbers. The provider is
    /// wrapped in a [`WsL1HeadSubscription`] to keep the connection alive.
    ///
    /// When `url` is `None`, or if the WS connection fails, returns a
    /// [`NullL1HeadSubscription`] so that [`HybridL1HeadSource`] falls back
    /// entirely to polling.
    ///
    /// [`HybridL1HeadSource`]: base_batcher_source::HybridL1HeadSource
    async fn build_l1_subscription(url: Option<&Url>) -> L1Subscription {
        let Some(url) = url else {
            return L1Subscription::Null(NullL1HeadSubscription);
        };

        let ws_provider = match ProviderBuilder::new().connect(url.as_str()).await {
            Ok(p) => Arc::new(p),
            Err(e) => {
                warn!(error = %e, l1_ws = %url, "failed to connect L1 WS provider; falling back to polling");
                return L1Subscription::Null(NullL1HeadSubscription);
            }
        };

        let sub = match ws_provider.subscribe_blocks().await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "failed to subscribe to new L1 blocks; falling back to polling");
                return L1Subscription::Null(NullL1HeadSubscription);
            }
        };

        let stream = sub.into_stream().map(|header| Ok(header.number)).boxed();
        L1Subscription::Ws(WsL1HeadSubscription::new(ws_provider, stream))
    }

    /// Try each URL in order, returning the first that connects.
    ///
    /// Logs each failed attempt with the endpoint that produced it so operators
    /// can tell whether failover occurred. Returns an error containing the last
    /// failure if every endpoint fails. The list must be non-empty.
    async fn connect_first<T, F, Fut, E>(
        urls: &[Url],
        label: &'static str,
        mut build: F,
    ) -> eyre::Result<T>
    where
        F: FnMut(&Url) -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        let mut last_err: Option<String> = None;
        for url in urls {
            match build(url).await {
                Ok(t) => {
                    info!(endpoint = %label, url = %url, "connected to endpoint");
                    return Ok(t);
                }
                Err(e) => {
                    warn!(endpoint = %label, url = %url, error = %e, "endpoint connection failed, trying next");
                    last_err = Some(e.to_string());
                }
            }
        }
        Err(eyre::eyre!(
            "failed to connect to any {label} endpoint ({} candidate(s)): {}",
            urls.len(),
            last_err.unwrap_or_else(|| "no candidates".to_string()),
        ))
    }

    /// Try every URL and return all successful (url, provider) pairs.
    ///
    /// Returns an error only if every URL fails to connect. URLs that fail at
    /// startup are logged and excluded from the resulting pool — they cannot
    /// participate in runtime failover until the operator restarts the batcher.
    /// The list must be non-empty.
    async fn connect_each<T, F, Fut, E>(
        urls: &[Url],
        label: &'static str,
        mut build: F,
    ) -> eyre::Result<Vec<(Url, T)>>
    where
        F: FnMut(&Url) -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        let mut entries = Vec::new();
        let mut errors = Vec::new();
        for url in urls {
            match build(url).await {
                Ok(t) => {
                    info!(endpoint = %label, url = %url, "connected to endpoint");
                    entries.push((url.clone(), t));
                }
                Err(e) => {
                    warn!(endpoint = %label, url = %url, error = %e, "endpoint connection failed");
                    errors.push(format!("{url}: {e}"));
                }
            }
        }
        if entries.is_empty() {
            return Err(eyre::eyre!(
                "failed to connect to any {label} endpoint ({} candidate(s)): {}",
                urls.len(),
                errors.join("; "),
            ));
        }
        Ok(entries)
    }

    /// Block until the rollup node reports a non-zero sync status, or until
    /// `timeout` elapses.
    ///
    /// Polls `optimism_syncStatus` on `poll_interval` against the pool's
    /// active endpoint and returns once both `current_l1.number` and
    /// `unsafe_l2.block_info.number` are non-zero. RPC errors are logged and
    /// retried with exponential backoff (capped at 30 seconds) so a
    /// permanently-broken endpoint is not hammered at the poll cadence.
    /// Returns an error when `timeout` is exceeded so operators see an
    /// explicit failure rather than a silent hang.
    async fn wait_for_node_sync(
        rollup_pool: &Arc<RollupEndpointPool>,
        poll_interval: std::time::Duration,
        timeout: std::time::Duration,
    ) -> eyre::Result<()> {
        // Cap RPC-error backoff so a broken endpoint backs off but eventually
        // recovers within a reasonable window.
        const MAX_ERROR_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);

        info!(
            timeout_secs = %timeout.as_secs(),
            "waiting for rollup node to report a non-zero sync status"
        );
        let deadline = std::time::Instant::now() + timeout;
        let mut error_backoff = poll_interval;
        loop {
            match rollup_pool.active().sync_status().await {
                Ok(status)
                    if status.current_l1.number > 0 && status.unsafe_l2.block_info.number > 0 =>
                {
                    info!(
                        current_l1 = %status.current_l1.number,
                        unsafe_l2 = %status.unsafe_l2.block_info.number,
                        safe_l2 = %status.safe_l2.block_info.number,
                        "rollup node reports sync, proceeding with batcher startup"
                    );
                    return Ok(());
                }
                Ok(status) => {
                    // Reset error backoff: the RPC is responsive, the node
                    // just hasn't produced/derived blocks yet.
                    error_backoff = poll_interval;
                    info!(
                        current_l1 = %status.current_l1.number,
                        unsafe_l2 = %status.unsafe_l2.block_info.number,
                        "rollup node not yet synced, waiting"
                    );
                    Self::sleep_or_timeout(poll_interval, deadline).await?;
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        backoff_secs = %error_backoff.as_secs(),
                        "optimism_syncStatus RPC failed during wait, backing off"
                    );
                    Self::sleep_or_timeout(error_backoff, deadline).await?;
                    error_backoff = (error_backoff * 2).min(MAX_ERROR_BACKOFF);
                }
            }
        }
    }

    /// Sleep for `dur` or until `deadline`, whichever is sooner.
    ///
    /// Returns `Err` if the deadline is reached before or during the sleep so
    /// callers surface a single timeout error rather than silently looping
    /// past the deadline.
    async fn sleep_or_timeout(
        dur: std::time::Duration,
        deadline: std::time::Instant,
    ) -> eyre::Result<()> {
        let now = std::time::Instant::now();
        if now >= deadline {
            return Err(eyre::eyre!(
                "wait_for_node_sync timed out before the rollup node reported a non-zero sync status"
            ));
        }
        let remaining = deadline - now;
        tokio::time::sleep(dur.min(remaining)).await;
        if std::time::Instant::now() >= deadline {
            return Err(eyre::eyre!(
                "wait_for_node_sync timed out before the rollup node reported a non-zero sync status"
            ));
        }
        Ok(())
    }

    /// Initialise all batcher components and return a [`ReadyBatcher`].
    ///
    /// Connects to the L2 and L1 RPC endpoints, fetches the rollup config,
    /// validates the private key, and constructs the driver. Returns an error
    /// if any of those steps fail — the caller sees the failure immediately,
    /// before any background work is spawned.
    ///
    /// The runtime's cancellation token is forwarded to the safe-head poller
    /// spawned here so it stops cleanly when the batcher shuts down.
    pub async fn setup(self, runtime: TokioRuntime) -> eyre::Result<ReadyBatcher> {
        self.config.encoder_config.validate()?;

        if self.config.stopped && self.config.admin_addr.is_none() {
            eyre::bail!(
                "--stopped requires --admin-port: the batcher would start stopped with no way to \
                 resume because the admin JSON-RPC server is not enabled"
            );
        }
        if self.config.l1_rpc_url.is_empty() {
            eyre::bail!("at least one L1 RPC endpoint is required");
        }
        if self.config.l2_rpc_url.is_empty() {
            eyre::bail!("at least one L2 RPC endpoint is required");
        }
        if self.config.rollup_rpc_url.is_empty() {
            eyre::bail!("at least one rollup RPC endpoint is required");
        }

        info!(
            l1_rpc_count = self.config.l1_rpc_url.len(),
            l2_rpc_count = self.config.l2_rpc_url.len(),
            rollup_rpc_count = self.config.rollup_rpc_url.len(),
            l2_ws = self.config.l2_ws_url.as_ref().map(|u| u.as_str()),
            l1_ws = self.config.l1_ws_url.as_ref().map(|u| u.as_str()),
            "starting batcher service"
        );

        // Connect to every L2 RPC endpoint and build a pool. The active
        // endpoint is the first one that connected; the health monitor
        // (spawned below) probes it periodically and rotates the active
        // selection when the current endpoint stops responding.
        let l2_entries: Vec<(Url, Arc<dyn Provider<Base> + Send + Sync>)> =
            Self::connect_each(&self.config.l2_rpc_url, "l2-rpc", |url| {
                let url = url.clone();
                async move {
                    ProviderBuilder::new()
                        .disable_recommended_fillers()
                        .network::<Base>()
                        .connect(url.as_str())
                        .await
                        .map(|p| Arc::new(p) as Arc<dyn Provider<Base> + Send + Sync>)
                }
            })
            .await?;
        let l2_pool: Arc<L2EndpointPool> = Arc::new(EndpointPool::new(l2_entries)?);

        // Build the L2 block subscription. When l2_ws_url is configured the
        // subscription owns its WS provider Arc so the connection stays live
        // for the full driver run. The per-header `fetch_pool` resolves
        // through the L2 pool on every block, so when the pool fails over
        // the subscription's full-block fetches follow the active endpoint.
        let l2_subscription =
            Self::build_l2_subscription(self.config.l2_ws_url.as_ref(), Arc::clone(&l2_pool)).await;

        // Connect to every rollup-node RPC endpoint and build a pool. Each
        // URL is probed via `optimism_rollupConfig` during connection so that
        // unresponsive endpoints are excluded from the pool at startup. The
        // health monitor (spawned below) keeps the pool's active selection
        // fresh by probing on a fixed interval.
        let rollup_entries: Vec<(Url, Arc<HttpClient>)> =
            Self::connect_each(&self.config.rollup_rpc_url, "rollup-rpc", |url| {
                let url = url.clone();
                async move {
                    let client = HttpClientBuilder::default()
                        .build(url.as_str())
                        .map_err(|e| eyre::eyre!("failed to build rollup RPC client: {e}"))?;
                    // Cheap probe call so a non-responsive endpoint is dropped
                    // from the pool rather than left to fail on first real use.
                    client
                        .rollup_config()
                        .await
                        .map_err(|e| eyre::eyre!("optimism_rollupConfig probe failed: {e}"))?;
                    eyre::Ok(Arc::new(client))
                }
            })
            .await?;
        let rollup_pool: Arc<RollupEndpointPool> = Arc::new(EndpointPool::new(rollup_entries)?);
        let rollup_config = Arc::new(
            rollup_pool
                .active()
                .rollup_config()
                .await
                .map_err(|e| eyre::eyre!("optimism_rollupConfig RPC failed: {e}"))?,
        );
        info!(
            inbox = %rollup_config.batch_inbox_address,
            "rollup config loaded"
        );

        // Optionally block startup until the rollup node reports a non-zero
        // sync status. Mirrors op-batcher's `--wait-node-sync`.
        if self.config.wait_node_sync {
            Self::wait_for_node_sync(
                &rollup_pool,
                self.config.poll_interval,
                self.config.wait_node_sync_timeout,
            )
            .await?;
        }

        // Fetch sync status to determine the safe L2 head for startup backfill.
        let sync_status = rollup_pool
            .active()
            .sync_status()
            .await
            .map_err(|e| eyre::eyre!("optimism_syncStatus RPC failed: {e}"))?;
        let safe_l2_number = sync_status.safe_l2.block_info.number;
        let next_l2_timestamp =
            sync_status.safe_l2.block_info.timestamp.saturating_add(rollup_config.block_time);
        self.config.encoder_config.validate_for_rollup_config(&rollup_config, next_l2_timestamp)?;
        info!(safe_l2 = %safe_l2_number, "fetched safe L2 head");

        // Validate the recent-tx scan depth against the maximum. Do this early so
        // the error surfaces before any network I/O for the scan.
        if self.config.check_recent_txs_depth > MAX_CHECK_RECENT_TXS_DEPTH {
            return Err(eyre::eyre!(
                "check_recent_txs_depth {} exceeds maximum of {}",
                self.config.check_recent_txs_depth,
                MAX_CHECK_RECENT_TXS_DEPTH,
            ));
        }

        // Connect to every L1 RPC endpoint and build a pool used by the L1
        // head polling source. The health monitor (spawned below) probes the
        // active endpoint and rotates on failure.
        let l1_entries: Vec<(Url, Arc<dyn Provider + Send + Sync>)> =
            Self::connect_each(&self.config.l1_rpc_url, "l1-rpc", |url| {
                let url = url.clone();
                async move {
                    ProviderBuilder::new()
                        .disable_recommended_fillers()
                        .connect(url.as_str())
                        .await
                        .map(|p: RootProvider| Arc::new(p) as Arc<dyn Provider + Send + Sync>)
                }
            })
            .await?;
        let l1_pool: Arc<L1EndpointPool> = Arc::new(EndpointPool::new(l1_entries)?);

        // Build a separate concrete `RootProvider` for the recent-tx scanner
        // and the tx manager. These callers need a typed `RootProvider`
        // (the tx manager is parameterised over the concrete provider type),
        // and they currently lack runtime failover — connecting once at
        // startup matches the prior behaviour. Rotating these on a dead
        // endpoint is a follow-up that requires plumbing the pool into the
        // tx manager.
        let l1_provider: RootProvider =
            Self::connect_first(&self.config.l1_rpc_url, "l1-rpc-tx", |url| {
                let url = url.clone();
                async move {
                    ProviderBuilder::new().disable_recommended_fillers().connect(url.as_str()).await
                }
            })
            .await?;

        // Optionally scan recent L1 blocks to find the highest L2 block already
        // submitted but not yet reflected in the safe head, preventing re-submissions
        // after an unclean restart. Peek at the batcher address from the private key
        // (without consuming it) only when the scan is requested.
        let scanned_highest = if self.config.check_recent_txs_depth > 0 {
            let batcher_address = self
                .config
                .batcher_private_key
                .as_ref()
                .ok_or_else(|| eyre::eyre!("batcher_private_key must be set before starting"))?
                .address();
            RecentTxScanner::highest_submitted_l2_block(
                &l1_provider,
                batcher_address,
                rollup_config.batch_inbox_address,
                self.config.check_recent_txs_depth,
                &rollup_config,
            )
            .await?
        } else {
            None
        };

        // Get the current L2 latest block to decide whether historical backfill is needed.
        let latest_l2 = l2_pool
            .active()
            .get_block_number()
            .await
            .map_err(|e| eyre::eyre!("failed to fetch L2 latest block number: {e}"))?;

        // Advance the cursor past any L2 blocks that are already on L1 but not yet safe.
        // Use the higher of the safe head and the scan result as the backfill start.
        let cursor_start = safe_l2_number.max(scanned_highest.unwrap_or(0));

        // Build the L2 polling source. If blocks between cursor_start+1 and latest
        // were not yet submitted, use sequential catchup mode to avoid skipping them.
        // The poller resolves its provider through the L2 pool on every call so
        // runtime failover takes effect immediately.
        let poller = if cursor_start < latest_l2 {
            info!(
                safe_l2 = %safe_l2_number,
                cursor_start = %cursor_start,
                latest_l2 = %latest_l2,
                "starting sequential backfill from cursor"
            );
            RpcPollingSource::new_from(Arc::clone(&l2_pool), cursor_start + 1)
        } else {
            RpcPollingSource::new(Arc::clone(&l2_pool))
        };

        // Assemble the hybrid L2 block source.
        let source = HybridBlockSource::new(
            TokioRuntime::new(),
            l2_subscription,
            poller,
            self.config.poll_interval,
        );
        let encoder =
            BatchEncoder::new(Arc::clone(&rollup_config), self.config.encoder_config.clone());

        // Build the throttle controller and the appropriate client. The throttle
        // RPC uses the L2 endpoint(s) plus any `throttle_additional_endpoints`
        // (e.g. rollup-boost builders); `RpcThrottleClient` fans `miner_setMaxDASize`
        // out to every endpoint in parallel so the throttle signal reaches
        // both the sequencer and every builder at once.
        let throttle_client = match &self.config.throttle {
            None => ServiceThrottle::Noop(NoopThrottleClient),
            Some(_) => {
                // Dedup endpoints: an operator may legitimately list the same
                // URL in both `--l2-rpc-url` and `--throttle-additional-endpoints`
                // (e.g. when their builder is also a sequencer endpoint).
                // Without dedup we'd double-fan to that node every tick. The
                // `--l2-rpc-url` entries are always tagged Sequencer; on a
                // collision we keep the Sequencer role rather than demoting
                // to Builder.
                let mut seen = HashSet::new();
                let endpoints: Vec<(String, EndpointRole)> = self
                    .config
                    .l2_rpc_url
                    .iter()
                    .map(|u| (u.as_str().to_string(), EndpointRole::Sequencer))
                    .chain(
                        self.config
                            .throttle_additional_endpoints
                            .iter()
                            .map(|u| (u.as_str().to_string(), EndpointRole::Builder)),
                    )
                    .filter(|(s, _)| seen.insert(s.clone()))
                    .collect();
                let client = RpcThrottleClient::new(&endpoints)?;
                info!(
                    endpoints = client.endpoint_count(),
                    sequencers = client.sequencer_count(),
                    additional = self.config.throttle_additional_endpoints.len(),
                    "throttle client fan-out configured"
                );
                ServiceThrottle::Rpc(client)
            }
        };
        let (throttle_config, throttle_strategy) = self.config.throttle.clone().map_or_else(
            || (ThrottleConfig::default(), ThrottleStrategy::Off),
            |cfg| (cfg, ThrottleStrategy::Linear),
        );
        let throttle = ThrottleController::new(throttle_config, throttle_strategy);

        // Build the L1 head source: a hybrid of optional WS subscription + polling.
        // The polling path resolves through the L1 pool on every call.
        let l1_head_subscription =
            Self::build_l1_subscription(self.config.l1_ws_url.as_ref()).await;
        let l1_head_poller = RpcL1HeadPollingSource::new(Arc::clone(&l1_pool));
        let l1_head_source = HybridL1HeadSource::new(
            l1_head_subscription,
            l1_head_poller,
            self.config.poll_interval,
        );

        // Build the signer config from the configured private key.
        let signer_config = SignerConfig::local(
            self.config
                .batcher_private_key
                .ok_or_else(|| eyre::eyre!("batcher_private_key must be set before starting"))?,
        );

        // Fetch L1 chain ID and construct the tx manager.
        let l1_chain_id = l1_provider
            .get_chain_id()
            .await
            .map_err(|e| eyre::eyre!("failed to fetch L1 chain ID: {e}"))?;
        let tx_manager_config = TxManagerConfig {
            resubmission_timeout: self.config.resubmission_timeout,
            num_confirmations: self.config.num_confirmations as u64,
            ..TxManagerConfig::default()
        };
        let tx_manager = SimpleTxManager::new(
            l1_provider,
            signer_config,
            tx_manager_config,
            l1_chain_id,
            Arc::new(BaseTxMetrics::new("batcher")),
        )
        .await
        .map_err(|e| eyre::eyre!("failed to create tx manager: {e}"))?;

        // Create a safe-head watch channel for runtime pruning of confirmed blocks.
        let (safe_head_tx, safe_head_rx) = watch::channel::<u64>(safe_l2_number);

        // Spawn the safe-head poller. It polls `optimism_syncStatus` at the
        // configured interval and advances the watch when the safe L2 head
        // moves forward, allowing the encoder to prune confirmed blocks.
        // The pool wrapper resolves the active rollup-rpc endpoint per call,
        // so the poller follows the rollup pool's failover decisions.
        SafeHeadPoller::new(Arc::clone(&rollup_pool), self.config.poll_interval, safe_head_tx)
            .spawn(runtime.token().clone());

        // Spawn the L1 and L2 endpoint health monitors. Each probes its pool's
        // active endpoint on `active_endpoint_check_interval` and rotates the
        // active selection on failure. Single-endpoint pools short-circuit
        // inside `HealthMonitor::run` so this is a no-op when the operator
        // configured exactly one URL.
        let interval = self.config.active_endpoint_check_interval;
        let token = runtime.token().clone();
        // L1 pool: simple liveness probe — we don't care about leader status
        // on the L1 read path, only that the endpoint responds.
        HealthMonitor::new(
            Arc::clone(&l1_pool),
            interval,
            "l1-rpc",
            Probe::block_number::<_, Ethereum>(),
        )
        .spawn(token.clone());
        // L2 pool: head-advancement probe so a passive sequencer (or paused
        // replica) that responds OK without producing blocks is detected and
        // we fail over to a node that's actually keeping up.
        HealthMonitor::new(
            Arc::clone(&l2_pool),
            interval,
            "l2-rpc",
            Probe::head_advancement::<_, Base>(self.config.head_advancement_max_stalls),
        )
        .spawn(token.clone());

        // Rollup-rpc probe: `optimism_syncStatus` both checks liveness and
        // proves the endpoint is actually serving the consensus API (a node
        // with the rollup namespace disabled would respond OK to a generic
        // RPC but fail this call).
        HealthMonitor::new(
            Arc::clone(&rollup_pool),
            interval,
            "rollup-rpc",
            |_idx, client: Arc<HttpClient>| {
                Box::pin(async move {
                    client.sync_status().await.map(|_| ()).map_err(|e| e.to_string())
                })
            },
        )
        .spawn(token);

        // Build the driver — all fallible setup is complete at this point.
        let mut driver = BatchDriver::new(
            runtime,
            encoder,
            source,
            tx_manager,
            base_batcher_core::BatchDriverConfig {
                inbox: rollup_config.batch_inbox_address,
                max_pending_transactions: self.config.max_pending_transactions,
                drain_timeout: self.config.resubmission_timeout * 2,
                force_blobs_when_throttling: self.config.force_blobs_when_throttling,
            },
            DaThrottle::new(throttle, throttle_client),
            l1_head_source,
        )
        .with_safe_head_rx(safe_head_rx)
        .with_stopped(self.config.stopped);

        let admin_server = match self.config.admin_addr {
            Some(addr) => {
                let (admin_handle, admin_rx) = AdminHandle::channel();
                driver = driver.with_admin_rx(admin_rx);
                Some(AdminServer::spawn(addr, admin_handle).await?)
            }
            None => None,
        };

        info!("batcher service components initialized");
        Ok(ReadyBatcher { driver, admin_server })
    }
}
