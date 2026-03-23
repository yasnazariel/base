//! Metrics for the proof host.
//!
//! All metric names are prefixed with `base_proof_host_`.
//!
//! ## Counters
//!
//! | Name | Labels | Description |
//! |------|--------|-------------|
//! | `base_proof_host_requests_total` | `mode` | Total proof requests received |
//! | `base_proof_host_requests_result_total` | `outcome` | Proof request outcomes (incl. `dropped`) |
//! | `base_proof_host_hint_requests_total` | `hint_type` | Hint requests by type |
//! | `base_proof_host_hint_errors_total` | `hint_type` | Hint errors by type |
//! | `base_proof_host_kv_cold_lookups_total` | | KV lookups that missed the cache (resolved via hint fetch) |
//! | `base_proof_host_preimage_accesses_total` | | Total preimage accesses |
//! | `base_proof_host_offline_misses_total` | | Offline backend key misses |
//!
//! ## Gauges
//!
//! | Name | Labels | Description |
//! |------|--------|-------------|
//! | `base_proof_host_in_flight_proofs` | | Currently in-flight proof requests |
//! | `base_proof_host_preimage_count` | | Preimage count from last witness build |
//!
//! ## Histograms
//!
//! | Name | Labels | Description |
//! |------|--------|-------------|
//! | `base_proof_host_proof_duration_seconds` | | End-to-end proof generation duration |
//! | `base_proof_host_witness_build_duration_seconds` | | Witness build duration |
//! | `base_proof_host_prover_duration_seconds` | | Backend prover duration |
//! | `base_proof_host_hint_duration_seconds` | `hint_type` | Hint processing duration by type |
//! | `base_proof_host_replay_duration_seconds` | | Client replay (prologue+execute+validate) duration |

base_metrics::define_metrics! {
    base_proof_host

    #[describe("Total proof requests received")]
    #[label("mode", mode)]
    requests_total: counter,

    #[describe("Proof request outcomes by result")]
    #[label("outcome", outcome)]
    requests_result_total: counter,

    #[describe("Hint requests by type")]
    #[label("hint_type", hint_type)]
    hint_requests_total: counter,

    #[describe("Hint processing errors by type")]
    #[label("hint_type", hint_type)]
    hint_errors_total: counter,

    #[describe("KV lookups that missed the cache and required hint fetching")]
    kv_cold_lookups_total: counter,

    #[describe("Total preimage accesses through the recording oracle")]
    preimage_accesses_total: counter,

    #[describe("Offline backend key-not-found events")]
    offline_misses_total: counter,

    #[describe("Currently in-flight proof requests")]
    in_flight_proofs: gauge,

    #[describe("Number of preimages captured in the last witness build")]
    preimage_count: gauge,

    #[describe("End-to-end proof generation duration")]
    proof_duration_seconds: histogram,

    #[describe("Witness build duration")]
    witness_build_duration_seconds: histogram,

    #[describe("Backend prover duration")]
    prover_duration_seconds: histogram,

    #[describe("Per-hint-type processing duration")]
    #[label("hint_type", hint_type)]
    hint_duration_seconds: histogram,

    #[describe("Client replay duration")]
    replay_duration_seconds: histogram,
}

impl Metrics {
    /// Online operating mode.
    pub const MODE_ONLINE: &str = "online";

    /// Successful proof outcome.
    pub const OUTCOME_SUCCESS: &str = "success";

    /// Witness generation error outcome.
    pub const OUTCOME_WITNESS_ERROR: &str = "witness_error";

    /// Backend proving error outcome.
    pub const OUTCOME_PROVE_ERROR: &str = "prove_error";

    /// Future was cancelled (dropped) before completion.
    pub const OUTCOME_DROPPED: &str = "dropped";
}

pub(crate) use base_metrics::timed;
