//! RPC metrics unique for Base.

use std::time::Instant;

base_metrics::define_metrics_named! {
    SequencerMetrics, "base_rpc.sequencer",

    #[describe("How long it takes to forward a transaction to the sequencer")]
    sequencer_forward_latency: histogram,
}

base_metrics::define_metrics_named! {
    EthApiExtMetrics, "base_rpc.eth_api_ext",

    #[describe("How long it takes to handle a eth_getProof request successfully")]
    get_proof_latency: histogram,
    #[describe("Total number of eth_getProof requests")]
    get_proof_requests: counter,
    #[describe("Total number of successful eth_getProof responses")]
    get_proof_successful_responses: counter,
    #[describe("Total number of failures handling eth_getProof requests")]
    get_proof_failures: counter,
}

/// Types of debug apis
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum DebugApis {
    /// `DebugExecutePayload` Api
    DebugExecutePayload,
    /// `DebugExecutionWitness` Api
    DebugExecutionWitness,
}

impl DebugApis {
    /// Returns the operation as a string for metrics labels.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::DebugExecutePayload => "debug_execute_payload",
            Self::DebugExecutionWitness => "debug_execution_witness",
        }
    }
}

base_metrics::define_metrics_named! {
    DebugApiExtRpcMetrics, "base_rpc.debug_api_ext",

    #[describe("End-to-end time to handle this API call")]
    #[label("api", api)]
    latency: histogram,
    #[describe("Total number of requests for this API")]
    #[label("api", api)]
    requests: counter,
    #[describe("Total number of successful responses for this API")]
    #[label("api", api)]
    successful_responses: counter,
    #[describe("Total number of failures for this API")]
    #[label("api", api)]
    failures: counter,
}

/// Record a Debug API call async (tracks latency, requests, success, failures).
pub async fn record_debug_api_async<F, T, E>(api: DebugApis, f: F) -> Result<T, E>
where
    F: Future<Output = Result<T, E>>,
{
    let start = Instant::now();
    let result = f.await;

    let label = api.as_str();
    DebugApiExtRpcMetrics::latency(label).record(start.elapsed().as_secs_f64());
    DebugApiExtRpcMetrics::requests(label).increment(1);

    if result.is_ok() {
        DebugApiExtRpcMetrics::successful_responses(label).increment(1);
    } else {
        DebugApiExtRpcMetrics::failures(label).increment(1);
    }

    result
}
