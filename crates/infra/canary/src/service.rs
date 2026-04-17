//! Canary service lifecycle.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use alloy_provider::{Provider, ProviderBuilder};
use base_cli_utils::RuntimeManager;
use base_health::HealthServer;
use eyre::Result;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::{
    BalanceCheckAction, CanaryAction, CanaryConfig, GossipSpamAction, HealthCheckAction,
    InvalidBatchAction, LoadTestAction, LoadTestConfig, Metrics, Scheduler,
};

/// Timeout for the chain ID RPC call during startup.
const CHAIN_ID_TIMEOUT: Duration = Duration::from_secs(30);

/// Top-level canary service.
#[derive(Debug)]
pub struct CanaryService;

impl CanaryService {
    /// Runs the full canary service lifecycle.
    pub async fn run(config: CanaryConfig) -> Result<()> {
        info!(version = env!("CARGO_PKG_VERSION"), "Canary starting");

        let cancel = CancellationToken::new();
        let _signal_handle = RuntimeManager::install_signal_handler(cancel.clone());

        // Auto-detect chain ID if not provided.
        let chain_id = match config.chain_id {
            Some(id) => id,
            None => {
                let provider = ProviderBuilder::new().connect_http(config.l2_rpc_url.clone());
                let id =
                    timeout(CHAIN_ID_TIMEOUT, provider.get_chain_id()).await.map_err(|_| {
                        eyre::eyre!(
                            "chain ID auto-detection timed out after {}s",
                            CHAIN_ID_TIMEOUT.as_secs()
                        )
                    })??;
                info!(chain_id = id, "auto-detected chain ID");
                id
            }
        };

        // Build enabled actions.
        let actions = Self::build_actions(&config, chain_id);
        if actions.is_empty() {
            return Err(eyre::eyre!("no canary actions enabled"));
        }

        // Start health server.
        let ready = Arc::new(AtomicBool::new(false));
        let health_cancel = cancel.clone();
        let health_ready = Arc::clone(&ready);
        let health_addr = config.health_addr;
        let health_handle = tokio::spawn(async move {
            if let Err(e) = HealthServer::serve(health_addr, health_ready, health_cancel).await {
                error!(error = %e, "health server failed");
            }
        });

        let scheduler =
            Scheduler::new(config.schedule_mode, config.schedule_interval, config.schedule_jitter);

        ready.store(true, Ordering::SeqCst);
        Metrics::up().set(1.0);

        info!(
            schedule_mode = ?config.schedule_mode,
            schedule_interval = ?config.schedule_interval,
            schedule_jitter = ?config.schedule_jitter,
            enabled_actions = actions.len(),
            health_addr = %config.health_addr,
            chain_id = chain_id,
            "canary service ready"
        );

        // Main loop.
        loop {
            for action in &actions {
                if cancel.is_cancelled() {
                    break;
                }

                let action_cancel = cancel.child_token();
                info!(action = action.name(), "executing canary action");

                let outcome = action.execute(action_cancel).await;

                let outcome_label = if outcome.succeeded { "success" } else { "failure" };
                Metrics::action_runs_total(action.name(), outcome_label).increment(1);
                Metrics::action_duration_seconds(action.name())
                    .record(outcome.duration.as_secs_f64());

                if outcome.succeeded {
                    info!(
                        action = action.name(),
                        duration_ms = outcome.duration.as_millis() as u64,
                        message = %outcome.message,
                        "canary action succeeded"
                    );
                } else {
                    error!(
                        action = action.name(),
                        duration_ms = outcome.duration.as_millis() as u64,
                        message = %outcome.message,
                        "canary action failed"
                    );
                }
            }

            // Compute and sleep until next cycle.
            let delay = scheduler.next_delay();
            Metrics::schedule_next_run_seconds().set(delay.as_secs_f64());
            info!(next_run_secs = delay.as_secs(), "waiting for next scheduled run");

            tokio::select! {
                () = cancel.cancelled() => break,
                () = tokio::time::sleep(delay) => {}
            }
        }

        // Shutdown.
        Metrics::up().set(0.0);
        ready.store(false, Ordering::SeqCst);
        info!("canary stopped");

        health_handle.abort();
        match health_handle.await {
            Ok(()) => {}
            Err(e) if e.is_cancelled() => {}
            Err(e) => warn!(error = %e, "health server task error during shutdown"),
        }

        Ok(())
    }

    fn build_actions(config: &CanaryConfig, chain_id: u64) -> Vec<Box<dyn CanaryAction>> {
        let mut actions: Vec<Box<dyn CanaryAction>> = Vec::new();

        if config.enable_balance_check {
            let address = config.private_key.address();
            actions.push(Box::new(BalanceCheckAction::new(
                config.l2_rpc_url.clone(),
                address,
                config.min_balance_wei,
            )));
        }

        if config.enable_health_check {
            actions.push(Box::new(HealthCheckAction::new(
                config.l2_rpc_url.clone(),
                config.max_block_age,
            )));
        }

        if config.enable_load_test {
            actions.push(Box::new(LoadTestAction::new(LoadTestConfig {
                l2_rpc_url: config.l2_rpc_url.clone(),
                l2_ws_url: config.l2_ws_url.clone(),
                chain_id,
                funding_key: config.private_key.clone(),
                funding_amount_wei: config.funding_amount_wei,
                target_gps: config.load_test_gps,
                duration: config.load_test_duration,
                account_count: config.load_test_accounts,
                seed: config.load_test_seed,
            })));
        }

        if config.enable_gossip_spam
            && let Some(cl_rpc_url) = &config.cl_rpc_url
        {
            actions.push(Box::new(GossipSpamAction::new(
                cl_rpc_url.clone(),
                config.gossip_spam_count,
                config.gossip_spam_interval,
            )));
        }

        if config.enable_invalid_batch
            && let (Some(l1), Some(cl)) = (&config.l1_rpc_url, &config.cl_rpc_url)
        {
            actions.push(Box::new(InvalidBatchAction::new(
                l1.clone(),
                cl.clone(),
                config.private_key.clone(),
            )));
        }

        actions
    }
}
