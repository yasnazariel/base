//! Metrics for transaction tracing.

base_macros::define_metrics! {
    #[scope("reth_transaction_tracing")]
    pub struct Metrics {
        #[describe("Time taken for a transaction to be included in a block from when it's marked as pending")]
        inclusion_duration: histogram,

        #[describe("Time taken for a transaction to be included in a flashblock from when it's marked as pending")]
        fb_inclusion_duration: histogram,
    }
}
