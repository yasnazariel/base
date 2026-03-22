//! Prometheus metrics for the tips ingress RPC service.

/// Records an RPC latency histogram sample for the given method name.
pub fn record_histogram(rpc_latency: std::time::Duration, rpc: String) {
    #[cfg(feature = "metrics")]
    metrics::histogram!("tips_ingress_rpc_rpc_latency", "rpc" => rpc)
        .record(rpc_latency.as_secs_f64());
}

base_macros::define_metrics! {
    #[scope("tips_ingress_rpc")]
    pub struct Metrics {
        #[describe("Number of valid transactions received")]
        transactions_received: counter,

        #[describe("Number of valid bundles parsed")]
        bundles_parsed: counter,

        #[describe("Number of bundles simulated")]
        successful_simulations: counter,

        #[describe("Number of bundles that failed simulation")]
        failed_simulations: counter,

        #[describe("Number of bundles sent to kafka")]
        sent_to_kafka: counter,

        #[describe("Number of transactions sent to mempool")]
        sent_to_mempool: counter,

        #[describe("Duration of validate_tx")]
        validate_tx_duration: histogram,

        #[describe("Duration of validate_bundle")]
        validate_bundle_duration: histogram,

        #[describe("Duration of meter_bundle")]
        meter_bundle_duration: histogram,

        #[describe("Duration of send_raw_transaction")]
        send_raw_transaction_duration: histogram,

        #[describe("Total raw transactions forwarded to additional endpoint")]
        raw_tx_forwards_total: counter,

        #[describe("Number of bundles that exceeded the metering time")]
        bundles_exceeded_metering_time: counter,

        #[describe("Size of buffered meter bundle responses")]
        buffered_meter_bundle_responses_size: gauge,
    }
}
