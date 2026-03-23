//! Metrics for the proposer service.

base_macros::define_metrics! {
    base_proposer

    #[describe("Proposer build info")]
    #[label("version", version)]
    info: gauge,

    #[describe("Proposer is running")]
    up: gauge,

    #[describe("Total number of L2 output proposals submitted")]
    l2_output_proposals_total: counter,

    #[describe("Total number of TEE proofs skipped due to invalid signer")]
    tee_signer_invalid_total: counter,

    #[describe("Proposer account balance in wei")]
    account_balance_wei: gauge,
}

/// Records startup metrics (INFO gauge with version label, UP gauge set to 1).
pub fn record_startup_metrics(version: &str) {
    Metrics::info(version).set(1.0);
    Metrics::up().set(1.0);
}
