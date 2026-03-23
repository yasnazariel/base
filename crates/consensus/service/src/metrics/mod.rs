//! Metrics for the node service

base_macros::define_metrics! {
    base_node

    #[describe("L1 reorg count")]
    l1_reorg_count: counter,

    #[describe("Derivation pipeline L1 origin")]
    derivation_l1_origin: counter,

    #[describe("Critical errors in the derivation pipeline")]
    derivation_critical_errors: counter,

    #[describe("Duration of the sequencer attributes builder")]
    sequencer_attributes_build_duration: gauge,

    #[describe("Duration of the sequencer block building start task")]
    sequencer_block_building_start_task_duration: gauge,

    #[describe("Duration of the sequencer block building seal task")]
    sequencer_block_building_seal_task_duration: gauge,

    #[describe("Duration of the sequencer conductor commitment")]
    sequencer_conductor_commitment_duration: gauge,

    #[describe("Total count of sequenced transactions")]
    sequencer_total_transactions_sequenced: counter,

    #[describe("Sequencer seal step retries by step")]
    #[label("step", step)]
    sequencer_seal_step_retries_total: counter,

    #[describe("Sequencer seal step duration by step")]
    #[label("step", step)]
    sequencer_seal_step_duration: gauge,

    #[describe("Seal errors by fatality")]
    #[label("fatal", fatal)]
    sequencer_seal_errors_total: counter,

    #[describe("Sequencer start rejections by reason")]
    #[label("reason", reason)]
    sequencer_start_rejected_total: counter,

    #[describe("Deferred stop_sequencer responses due to in-flight seal pipeline")]
    sequencer_stop_deferred_total: counter,

    #[describe("Blocks sequenced in recovery mode")]
    sequencer_recovery_mode_blocks_total: counter,

    #[describe("Empty blocks produced due to sequencer drift threshold")]
    sequencer_drift_empty_blocks_total: counter,
}

impl Metrics {
    /// Identifier for the counter that tracks sequencer state flags.
    pub const SEQUENCER_STATE: &str = "base_node_sequencer_state";
}
