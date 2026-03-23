//! Metrics for the proposer service.

base_metrics::define_metrics! {
    base_proposer

    #[describe("Total number of L2 output proposals submitted")]
    l2_output_proposals_total: counter,

    #[describe("Total number of TEE proofs skipped due to invalid signer")]
    tee_signer_invalid_total: counter,

    #[describe("Proposer account balance in wei")]
    account_balance_wei: gauge,
}
