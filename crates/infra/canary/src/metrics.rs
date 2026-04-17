//! Canary Prometheus metrics.

base_metrics::define_metrics! {
    base_canary,
    struct = Metrics,

    #[describe("Canary service is running")]
    up: gauge,

    #[describe("Total canary action executions")]
    #[label(
        name = "action",
        default = ["load_test", "health_check", "balance_check", "gossip_spam", "invalid_batch"]
    )]
    #[label(name = "outcome", default = ["success", "failure"])]
    action_runs_total: counter,

    #[describe("Action execution duration in seconds")]
    #[label(
        name = "action",
        default = ["load_test", "health_check", "balance_check", "gossip_spam", "invalid_batch"]
    )]
    action_duration_seconds: histogram,

    #[describe("Canary wallet balance in wei")]
    wallet_balance_wei: gauge,

    #[describe("Last observed load test transactions per second")]
    load_test_tps: gauge,

    #[describe("Last observed load test p50 block latency in milliseconds")]
    load_test_p50_latency_ms: gauge,

    #[describe("Last observed load test p99 block latency in milliseconds")]
    load_test_p99_latency_ms: gauge,

    #[describe("Last observed load test success rate percentage")]
    load_test_success_rate: gauge,

    #[describe("Last observed block age in milliseconds")]
    health_check_block_age_ms: gauge,

    #[describe("Seconds until next scheduled canary run")]
    schedule_next_run_seconds: gauge,

    #[describe("Total gossip spam messages published")]
    gossip_spam_msgs_total: counter,

    #[describe("Number of gossip peers connected during last spam cycle")]
    gossip_spam_connected_peers: gauge,

    #[describe("Total invalid batch transactions submitted to L1")]
    #[label(name = "outcome", default = ["success", "failure"])]
    invalid_batch_l1_txs_total: counter,
}
