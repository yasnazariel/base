//! Configuration types and validation for the canary.

use std::{net::SocketAddr, time::Duration};

use alloy_primitives::U256;
use alloy_signer_local::PrivateKeySigner;
use base_cli_utils::{LogConfig, MetricsConfig};
use thiserror::Error;
use url::Url;

use crate::{
    ScheduleMode,
    cli::{Cli, ScheduleModeArg},
};

/// Errors that can occur during configuration validation.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Invalid URL format.
    #[error("invalid {field} URL: missing host")]
    InvalidUrl {
        /// The field name containing the invalid URL.
        field: &'static str,
    },
    /// A field value is out of the allowed range.
    #[error("{field} must be {constraint}, got {value}")]
    OutOfRange {
        /// The field name that is out of range.
        field: &'static str,
        /// Description of the allowed range.
        constraint: &'static str,
        /// The actual value supplied.
        value: String,
    },
}

/// Validated canary configuration.
#[derive(Debug)]
pub struct CanaryConfig {
    /// L2 HTTP RPC endpoint.
    pub l2_rpc_url: Url,
    /// L2 WebSocket RPC endpoint (optional).
    pub l2_ws_url: Option<Url>,
    /// Signer for the canary wallet.
    pub private_key: PrivateKeySigner,
    /// Schedule mode.
    pub schedule_mode: ScheduleMode,
    /// Base interval between canary runs.
    pub schedule_interval: Duration,
    /// Maximum random jitter added in random mode.
    pub schedule_jitter: Duration,
    /// Chain ID (`None` → auto-detect from RPC).
    pub chain_id: Option<u64>,
    /// Target gas per second for load tests.
    pub load_test_gps: u64,
    /// Duration of each load test run.
    pub load_test_duration: Duration,
    /// Number of sender accounts for load tests.
    pub load_test_accounts: usize,
    /// Seed for deterministic load test account derivation.
    pub load_test_seed: u64,
    /// Funding amount per account in wei.
    pub funding_amount_wei: U256,
    /// Whether the load test action is enabled.
    pub enable_load_test: bool,
    /// Whether the health check action is enabled.
    pub enable_health_check: bool,
    /// Whether the balance check action is enabled.
    pub enable_balance_check: bool,
    /// Minimum acceptable wallet balance in wei.
    pub min_balance_wei: U256,
    /// Maximum acceptable block age.
    pub max_block_age: Duration,
    /// Whether the gossip spam action is enabled.
    pub enable_gossip_spam: bool,
    /// Whether the invalid batch action is enabled.
    pub enable_invalid_batch: bool,
    /// Consensus-layer RPC URL (required for gossip-spam and invalid-batch).
    pub cl_rpc_url: Option<Url>,
    /// L1 RPC URL (required for invalid-batch).
    pub l1_rpc_url: Option<Url>,
    /// Number of spam messages per gossip-spam cycle.
    pub gossip_spam_count: u32,
    /// Interval between gossip spam messages.
    pub gossip_spam_interval: Duration,
    /// Logging configuration.
    pub log: LogConfig,
    /// Metrics server configuration.
    pub metrics: MetricsConfig,
    /// Health server socket address.
    pub health_addr: SocketAddr,
}

impl CanaryConfig {
    /// Creates a validated [`CanaryConfig`] from parsed CLI arguments.
    pub fn from_cli(cli: Cli) -> Result<Self, ConfigError> {
        let Cli { canary, logging, metrics, health } = cli;

        validate_url(&canary.l2_rpc_url, "l2-rpc-url")?;
        if let Some(ws) = &canary.l2_ws_url {
            validate_url(ws, "l2-ws-url")?;
        }

        if canary.schedule_interval.is_zero() {
            return Err(ConfigError::OutOfRange {
                field: "schedule-interval",
                constraint: "greater than 0",
                value: "0".to_string(),
            });
        }

        if canary.enable_load_test {
            if canary.load_test_accounts == 0 {
                return Err(ConfigError::OutOfRange {
                    field: "load-test-accounts",
                    constraint: "at least 1",
                    value: "0".to_string(),
                });
            }
            if canary.load_test_gps == 0 {
                return Err(ConfigError::OutOfRange {
                    field: "load-test-gps",
                    constraint: "greater than 0",
                    value: "0".to_string(),
                });
            }
            if canary.load_test_duration.is_zero() {
                return Err(ConfigError::OutOfRange {
                    field: "load-test-duration",
                    constraint: "greater than 0",
                    value: "0".to_string(),
                });
            }
            if canary.funding_amount_wei == U256::ZERO {
                return Err(ConfigError::OutOfRange {
                    field: "funding-amount-eth",
                    constraint: "greater than 0",
                    value: "0".to_string(),
                });
            }
        }

        if canary.enable_health_check && canary.max_block_age_secs == 0 {
            return Err(ConfigError::OutOfRange {
                field: "max-block-age-secs",
                constraint: "greater than 0",
                value: "0".to_string(),
            });
        }

        if (canary.enable_gossip_spam || canary.enable_invalid_batch) && canary.cl_rpc_url.is_none()
        {
            return Err(ConfigError::InvalidUrl { field: "cl-rpc-url" });
        }

        if let Some(cl) = &canary.cl_rpc_url {
            validate_url(cl, "cl-rpc-url")?;
        }

        if canary.enable_invalid_batch && canary.l1_rpc_url.is_none() {
            return Err(ConfigError::InvalidUrl { field: "l1-rpc-url" });
        }

        if let Some(l1) = &canary.l1_rpc_url {
            validate_url(l1, "l1-rpc-url")?;
        }

        if canary.enable_gossip_spam && canary.gossip_spam_count == 0 {
            return Err(ConfigError::OutOfRange {
                field: "gossip-spam-count",
                constraint: "greater than 0",
                value: "0".to_string(),
            });
        }

        if metrics.enabled && metrics.port == 0 {
            return Err(ConfigError::OutOfRange {
                field: "metrics-port",
                constraint: "non-zero when metrics are enabled",
                value: "0".to_string(),
            });
        }

        Ok(Self {
            l2_rpc_url: canary.l2_rpc_url,
            l2_ws_url: canary.l2_ws_url,
            private_key: canary.private_key,
            schedule_mode: ScheduleMode::from(canary.schedule_mode),
            schedule_interval: canary.schedule_interval,
            schedule_jitter: canary.schedule_jitter,
            chain_id: canary.chain_id,
            load_test_gps: canary.load_test_gps,
            load_test_duration: canary.load_test_duration,
            load_test_accounts: canary.load_test_accounts,
            load_test_seed: canary.load_test_seed,
            funding_amount_wei: canary.funding_amount_wei,
            enable_load_test: canary.enable_load_test,
            enable_health_check: canary.enable_health_check,
            enable_balance_check: canary.enable_balance_check,
            min_balance_wei: canary.min_balance_wei,
            max_block_age: Duration::from_secs(canary.max_block_age_secs),
            enable_gossip_spam: canary.enable_gossip_spam,
            enable_invalid_batch: canary.enable_invalid_batch,
            cl_rpc_url: canary.cl_rpc_url,
            l1_rpc_url: canary.l1_rpc_url,
            gossip_spam_count: canary.gossip_spam_count,
            gossip_spam_interval: Duration::from_millis(canary.gossip_spam_interval_ms),
            log: LogConfig::from(logging),
            metrics: metrics.into(),
            health_addr: health.socket_addr(),
        })
    }
}

impl From<ScheduleModeArg> for ScheduleMode {
    fn from(arg: ScheduleModeArg) -> Self {
        match arg {
            ScheduleModeArg::Deterministic => Self::Deterministic,
            ScheduleModeArg::Random => Self::Random,
        }
    }
}

/// Validates that a URL has a host component.
fn validate_url(url: &Url, field: &'static str) -> Result<(), ConfigError> {
    if url.host().is_none() {
        return Err(ConfigError::InvalidUrl { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use alloy_primitives::U256;
    use alloy_signer_local::PrivateKeySigner;
    use rstest::rstest;

    use super::*;
    use crate::cli::{CanaryArgs, HealthArgs, LogArgs, MetricsArgs, ScheduleModeArg};

    fn minimal_cli() -> Cli {
        Cli {
            canary: CanaryArgs {
                l2_rpc_url: Url::parse("http://localhost:8545").unwrap(),
                l2_ws_url: None,
                private_key: "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
                    .parse::<PrivateKeySigner>()
                    .unwrap(),
                schedule_mode: ScheduleModeArg::Deterministic,
                schedule_interval: Duration::from_secs(60),
                schedule_jitter: Duration::from_secs(30),
                chain_id: None,
                load_test_gps: 210_000,
                load_test_duration: Duration::from_secs(30),
                load_test_accounts: 5,
                load_test_seed: 1,
                funding_amount_wei: U256::from(100_000_000_000_000_000u128),
                enable_load_test: true,
                enable_health_check: true,
                enable_balance_check: true,
                min_balance_wei: U256::from(10_000_000_000_000_000u128),
                max_block_age_secs: 30,
                enable_gossip_spam: false,
                enable_invalid_batch: false,
                cl_rpc_url: None,
                l1_rpc_url: None,
                gossip_spam_count: 1000,
                gossip_spam_interval_ms: 0,
            },
            logging: LogArgs { level: 3, stdout_quiet: false, ..Default::default() },
            metrics: MetricsArgs { enabled: false, ..Default::default() },
            health: HealthArgs::default(),
        }
    }

    #[test]
    fn test_valid_config() {
        let config = CanaryConfig::from_cli(minimal_cli()).unwrap();
        assert_eq!(config.schedule_mode, ScheduleMode::Deterministic);
        assert_eq!(config.schedule_interval, Duration::from_secs(60));
        assert_eq!(config.load_test_accounts, 5);
        assert_eq!(config.load_test_seed, 1);
        assert!(config.enable_load_test);
        assert_eq!(config.funding_amount_wei, U256::from(100_000_000_000_000_000u128));
        assert_eq!(config.min_balance_wei, U256::from(10_000_000_000_000_000u128));
    }

    #[rstest]
    #[case::schedule_interval(|c: &mut CanaryArgs| c.schedule_interval = Duration::ZERO, "schedule-interval")]
    #[case::load_test_accounts(|c: &mut CanaryArgs| c.load_test_accounts = 0, "load-test-accounts")]
    #[case::load_test_gps(|c: &mut CanaryArgs| c.load_test_gps = 0, "load-test-gps")]
    #[case::funding_amount(|c: &mut CanaryArgs| c.funding_amount_wei = U256::ZERO, "funding-amount-eth")]
    #[case::max_block_age(|c: &mut CanaryArgs| c.max_block_age_secs = 0, "max-block-age-secs")]
    fn test_out_of_range_rejected(
        #[case] mutate: fn(&mut CanaryArgs),
        #[case] field: &'static str,
    ) {
        let mut cli = minimal_cli();
        mutate(&mut cli.canary);
        let result = CanaryConfig::from_cli(cli);
        assert!(matches!(result, Err(ConfigError::OutOfRange { field: f, .. }) if f == field));
    }
}
