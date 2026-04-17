//! Load test canary action.

use std::time::{Duration, Instant};

use alloy_primitives::U256;
use alloy_signer_local::PrivateKeySigner;
use async_trait::async_trait;
use base_load_tests::{
    DEFAULT_MAX_GAS_PRICE, DisplaySnapshot, LoadConfig, LoadRunner, TxConfig, TxType,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use url::Url;

use crate::{ActionOutcome, CanaryAction, Metrics};

/// Configuration for [`LoadTestAction`].
#[derive(Debug, Clone)]
pub struct LoadTestConfig {
    /// L2 HTTP RPC endpoint.
    pub l2_rpc_url: Url,
    /// L2 WebSocket RPC endpoint (optional, for block latency tracking).
    pub l2_ws_url: Option<Url>,
    /// Chain ID.
    pub chain_id: u64,
    /// Private key used to fund and drain test accounts.
    pub funding_key: PrivateKeySigner,
    /// Amount to fund each test account, in wei.
    pub funding_amount_wei: U256,
    /// Target gas per second for transaction generation.
    pub target_gps: u64,
    /// How long to run the load test per cycle.
    pub duration: Duration,
    /// Number of sender accounts to create.
    pub account_count: usize,
    /// Seed for deterministic account derivation.
    ///
    /// Each canary instance should use a unique seed to avoid nonce collisions
    /// when multiple instances run against the same network.
    pub seed: u64,
}

/// Wraps [`LoadRunner`] from `base-load-tests` as a canary action.
///
/// Each execution creates a fresh `LoadRunner`, funds test accounts, runs
/// the load test for the configured duration, drains accounts, and reports
/// metrics.
#[derive(Debug)]
pub struct LoadTestAction {
    funding_key: PrivateKeySigner,
    funding_amount_wei: U256,
    load_config: LoadConfig,
}

impl LoadTestAction {
    /// Creates a new [`LoadTestAction`] from the given configuration.
    pub fn new(config: LoadTestConfig) -> Self {
        let load_config = LoadConfig {
            rpc_http_url: config.l2_rpc_url,
            chain_id: config.chain_id,
            account_count: config.account_count,
            seed: config.seed,
            mnemonic: None,
            sender_offset: 0,
            transactions: vec![TxConfig { weight: 100, tx_type: TxType::Transfer }],
            target_gps: config.target_gps,
            duration: Some(config.duration),
            max_in_flight_per_sender: 50,
            batch_size: 5,
            batch_timeout: Duration::from_millis(50),
            max_gas_price: DEFAULT_MAX_GAS_PRICE,
            rpc_ws_url: config.l2_ws_url,
            flashblocks_ws_url: None,
        };

        Self {
            funding_key: config.funding_key,
            funding_amount_wei: config.funding_amount_wei,
            load_config,
        }
    }
}

#[async_trait]
impl CanaryAction for LoadTestAction {
    fn name(&self) -> &'static str {
        "load_test"
    }

    async fn execute(&self, cancel: CancellationToken) -> ActionOutcome {
        let start = Instant::now();

        // Reset metrics at the start of each cycle so stale values don't persist on failure.
        Metrics::load_test_tps().set(0.0);
        Metrics::load_test_p50_latency_ms().set(0.0);
        Metrics::load_test_p99_latency_ms().set(0.0);
        Metrics::load_test_success_rate().set(0.0);

        // Create a fresh LoadRunner for each cycle.
        let mut runner = match LoadRunner::new(self.load_config.clone()) {
            Ok(r) => r,
            Err(e) => {
                return ActionOutcome::failed(format!("failed to create load runner: {e}"), start);
            }
        };

        // Suppress indicatif progress bars so they don't bleed into the TUI.
        // set_snapshot_tx causes progress_bar() to return ProgressBar::hidden().
        let (snapshot_tx, _snapshot_rx) = tokio::sync::watch::channel(DisplaySnapshot::default());
        runner.set_snapshot_tx(snapshot_tx);

        // Set the stop flag so we can abort on cancellation.
        let stop_flag = runner.stop_flag();
        let cancel_guard = cancel.clone();
        let stop_handle = tokio::spawn(async move {
            cancel_guard.cancelled().await;
            stop_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        // Fund test accounts, aborting early if cancelled.
        info!(
            accounts = self.load_config.account_count,
            funding_wei = %self.funding_amount_wei,
            "funding load test accounts"
        );
        let fund_result = tokio::select! {
            () = cancel.cancelled() => {
                stop_handle.abort();
                return ActionOutcome::failed("cancelled during account funding", start);
            }
            result = runner.fund_accounts(self.funding_key.clone(), self.funding_amount_wei) => result
        };
        if let Err(e) = fund_result {
            stop_handle.abort();
            return ActionOutcome::failed(format!("failed to fund accounts: {e}"), start);
        }

        // Run the load test.
        debug!(
            target_gps = self.load_config.target_gps,
            duration = ?self.load_config.duration,
            "starting load test"
        );
        let summary = match runner.run().await {
            Ok(s) => s,
            Err(e) => {
                stop_handle.abort();
                // Best-effort drain even on failure.
                if let Err(drain_err) = runner.drain_accounts(self.funding_key.clone()).await {
                    warn!(error = %drain_err, "failed to drain accounts after load test failure");
                }
                return ActionOutcome::failed(format!("load test failed: {e}"), start);
            }
        };

        stop_handle.abort();

        // Best-effort drain.
        if let Err(e) = runner.drain_accounts(self.funding_key.clone()).await {
            warn!(error = %e, "failed to drain load test accounts");
        }

        // Record metrics.
        let tps = summary.throughput.tps;
        let p50_ms = summary.block_latency.p50.as_millis() as f64;
        let p99_ms = summary.block_latency.p99.as_millis() as f64;
        let success_rate = summary.throughput.success_rate();

        Metrics::load_test_tps().set(tps);
        Metrics::load_test_p50_latency_ms().set(p50_ms);
        Metrics::load_test_p99_latency_ms().set(p99_ms);
        Metrics::load_test_success_rate().set(success_rate);

        let succeeded = summary.throughput.total_confirmed > 0;
        let message = format!(
            "tps={tps:.1} p50={p50_ms:.0}ms p99={p99_ms:.0}ms confirmed={}/{} rate={success_rate:.1}%",
            summary.throughput.total_confirmed, summary.throughput.total_submitted,
        );

        ActionOutcome { succeeded, duration: start.elapsed(), message }
    }
}
