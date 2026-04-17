//! CLI argument definitions for the canary.
//!
//! All flags use the `BASE_CANARY_` environment-variable prefix
//! (e.g. `BASE_CANARY_L2_RPC_URL`). The default metrics port is **7400**.

use std::time::Duration;

use alloy_primitives::U256;
use alloy_signer_local::PrivateKeySigner;
use base_cli_utils::CliStyles;
use clap::{Parser, ValueEnum};
use url::Url;

base_cli_utils::define_cli_env!("BASE_CANARY");
base_cli_utils::define_log_args!("BASE_CANARY");
base_cli_utils::define_metrics_args!("BASE_CANARY", 7400);
base_cli_utils::define_health_args!("BASE_CANARY", 8080);

/// Parses an ETH amount string (e.g. `"0.1"`) into wei.
fn parse_eth_amount(s: &str) -> Result<U256, String> {
    alloy_primitives::utils::parse_ether(s).map_err(|e| e.to_string())
}

/// Schedule mode for canary action dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScheduleModeArg {
    /// Fixed interval between runs.
    Deterministic,
    /// Random interval within `[interval, interval + jitter]`.
    Random,
}

/// Canary — scheduled network health monitoring for Base.
#[derive(Debug, Parser)]
#[command(name = "canary")]
#[command(version, about, long_about = None)]
#[command(styles = CliStyles::init())]
pub struct Cli {
    /// Canary configuration arguments.
    #[command(flatten)]
    pub canary: CanaryArgs,

    /// Logging configuration arguments.
    #[command(flatten)]
    pub logging: LogArgs,

    /// Metrics configuration arguments.
    #[command(flatten)]
    pub metrics: MetricsArgs,

    /// Health server configuration arguments.
    #[command(flatten)]
    pub health: HealthArgs,
}

/// Core canary configuration arguments.
#[derive(Debug, Parser)]
#[command(next_help_heading = "Canary")]
pub struct CanaryArgs {
    /// URL of the L2 HTTP RPC endpoint.
    #[arg(long = "l2-rpc-url", env = cli_env!("L2_RPC_URL"))]
    pub l2_rpc_url: Url,

    /// URL of the L2 WebSocket RPC endpoint (optional; used for block subscriptions during load tests).
    #[arg(long = "l2-ws-url", env = cli_env!("L2_WS_URL"))]
    pub l2_ws_url: Option<Url>,

    /// Private key for the canary wallet (hex-encoded, `0x`-prefixed).
    #[arg(long = "private-key", env = cli_env!("PRIVATE_KEY"))]
    pub private_key: PrivateKeySigner,

    /// Schedule mode: deterministic (fixed interval) or random (interval + jitter).
    #[arg(
        long = "schedule-mode",
        default_value = "deterministic",
        env = cli_env!("SCHEDULE_MODE")
    )]
    pub schedule_mode: ScheduleModeArg,

    /// Base interval between canary runs (e.g. `"60s"`, `"5m"`).
    #[arg(
        long = "schedule-interval",
        default_value = "60s",
        env = cli_env!("SCHEDULE_INTERVAL"),
        value_parser = humantime::parse_duration
    )]
    pub schedule_interval: Duration,

    /// Maximum random jitter added to the interval in random mode (e.g. `"30s"`).
    #[arg(
        long = "schedule-jitter",
        default_value = "30s",
        env = cli_env!("SCHEDULE_JITTER"),
        value_parser = humantime::parse_duration
    )]
    pub schedule_jitter: Duration,

    /// Chain ID. Auto-detected from the L2 RPC if omitted.
    #[arg(long = "chain-id", env = cli_env!("CHAIN_ID"))]
    pub chain_id: Option<u64>,

    /// Target gas per second for load test transactions.
    #[arg(long = "load-test-gps", default_value = "210000", env = cli_env!("LOAD_TEST_GPS"))]
    pub load_test_gps: u64,

    /// Duration of each load test run (e.g. `"30s"`, `"2m"`).
    #[arg(
        long = "load-test-duration",
        default_value = "30s",
        env = cli_env!("LOAD_TEST_DURATION"),
        value_parser = humantime::parse_duration
    )]
    pub load_test_duration: Duration,

    /// Number of sender accounts for the load test.
    #[arg(
        long = "load-test-accounts",
        default_value = "5",
        env = cli_env!("LOAD_TEST_ACCOUNTS")
    )]
    pub load_test_accounts: usize,

    /// Seed for deterministic load test account generation.
    ///
    /// Each canary instance should use a unique seed to avoid nonce collisions
    /// when multiple instances run against the same network.
    #[arg(long = "load-test-seed", default_value = "1", env = cli_env!("LOAD_TEST_SEED"))]
    pub load_test_seed: u64,

    /// Funding amount per load test account (e.g. `"0.1"` ETH).
    #[arg(
        long = "funding-amount-eth",
        default_value = "0.1",
        env = cli_env!("FUNDING_AMOUNT_ETH"),
        value_parser = parse_eth_amount
    )]
    pub funding_amount_wei: U256,

    /// Enable the load test action.
    #[arg(
        long = "enable-load-test",
        default_value = "true",
        action = clap::ArgAction::Set,
        env = cli_env!("ENABLE_LOAD_TEST")
    )]
    pub enable_load_test: bool,

    /// Enable the health check action.
    #[arg(
        long = "enable-health-check",
        default_value = "true",
        action = clap::ArgAction::Set,
        env = cli_env!("ENABLE_HEALTH_CHECK")
    )]
    pub enable_health_check: bool,

    /// Enable the balance check action.
    #[arg(
        long = "enable-balance-check",
        default_value = "true",
        action = clap::ArgAction::Set,
        env = cli_env!("ENABLE_BALANCE_CHECK")
    )]
    pub enable_balance_check: bool,

    /// Minimum wallet balance before the balance check warns (e.g. `"0.01"` ETH).
    #[arg(
        long = "min-balance-eth",
        default_value = "0.01",
        env = cli_env!("MIN_BALANCE_ETH"),
        value_parser = parse_eth_amount
    )]
    pub min_balance_wei: U256,

    /// Maximum acceptable block age in seconds before the health check fails.
    #[arg(
        long = "max-block-age-secs",
        default_value = "30",
        env = cli_env!("MAX_BLOCK_AGE_SECS")
    )]
    pub max_block_age_secs: u64,

    /// Enable the gossip spam action.
    #[arg(
        long = "enable-gossip-spam",
        default_value = "false",
        action = clap::ArgAction::Set,
        env = cli_env!("ENABLE_GOSSIP_SPAM")
    )]
    pub enable_gossip_spam: bool,

    /// Enable the invalid batch action.
    #[arg(
        long = "enable-invalid-batch",
        default_value = "false",
        action = clap::ArgAction::Set,
        env = cli_env!("ENABLE_INVALID_BATCH")
    )]
    pub enable_invalid_batch: bool,

    /// URL of the consensus-layer RPC endpoint (required for gossip-spam and invalid-batch).
    #[arg(long = "cl-rpc-url", env = cli_env!("CL_RPC_URL"))]
    pub cl_rpc_url: Option<Url>,

    /// URL of the L1 HTTP RPC endpoint (required for invalid-batch).
    #[arg(long = "l1-rpc-url", env = cli_env!("L1_RPC_URL"))]
    pub l1_rpc_url: Option<Url>,

    /// Number of spam messages to publish per gossip-spam cycle.
    #[arg(
        long = "gossip-spam-count",
        default_value = "1000",
        env = cli_env!("GOSSIP_SPAM_COUNT")
    )]
    pub gossip_spam_count: u32,

    /// Interval between gossip spam messages in milliseconds (0 = flood as fast as possible).
    #[arg(
        long = "gossip-spam-interval-ms",
        default_value = "0",
        env = cli_env!("GOSSIP_SPAM_INTERVAL_MS")
    )]
    pub gossip_spam_interval_ms: u64,
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const TEST_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    fn base_args() -> Vec<&'static str> {
        vec!["canary", "--l2-rpc-url", "http://localhost:8545", "--private-key", TEST_KEY]
    }

    #[test]
    fn test_cli_defaults() {
        let cli = Cli::try_parse_from(base_args()).unwrap();
        assert_eq!(cli.canary.schedule_mode, ScheduleModeArg::Deterministic);
        assert_eq!(cli.canary.schedule_interval, Duration::from_secs(60));
        assert_eq!(cli.canary.schedule_jitter, Duration::from_secs(30));
        assert_eq!(cli.canary.load_test_gps, 210_000);
        assert_eq!(cli.canary.load_test_duration, Duration::from_secs(30));
        assert_eq!(cli.canary.load_test_accounts, 5);
        assert_eq!(cli.canary.load_test_seed, 1);
        assert!(cli.canary.enable_load_test);
        assert!(cli.canary.enable_health_check);
        assert!(cli.canary.enable_balance_check);
        assert!(cli.canary.chain_id.is_none());
    }

    #[test]
    fn test_cli_eth_amounts_parsed_to_wei() {
        let cli = Cli::try_parse_from(base_args()).unwrap();
        assert_eq!(cli.canary.funding_amount_wei, U256::from(100_000_000_000_000_000u128));
        assert_eq!(cli.canary.min_balance_wei, U256::from(10_000_000_000_000_000u128));
    }

    #[test]
    fn test_cli_missing_required() {
        assert!(Cli::try_parse_from(["canary"]).is_err());
    }

    #[rstest]
    #[case::invalid_private_key(vec!["--private-key", "not-a-key"])]
    #[case::invalid_eth_amount(vec!["--funding-amount-eth", "not-a-number"])]
    fn test_cli_invalid_arg_rejected(#[case] overrides: Vec<&'static str>) {
        let mut args = base_args();
        args.extend_from_slice(&overrides);
        assert!(Cli::try_parse_from(args).is_err());
    }

    #[test]
    fn test_cli_random_mode() {
        let mut args = base_args();
        args.extend_from_slice(&["--schedule-mode", "random", "--schedule-jitter", "45s"]);
        let cli = Cli::try_parse_from(args).unwrap();
        assert_eq!(cli.canary.schedule_mode, ScheduleModeArg::Random);
        assert_eq!(cli.canary.schedule_jitter, Duration::from_secs(45));
    }

    #[test]
    fn test_cli_disable_action() {
        let mut args = base_args();
        args.extend_from_slice(&["--enable-load-test", "false"]);
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(!cli.canary.enable_load_test);
        assert!(cli.canary.enable_health_check);
    }
}
