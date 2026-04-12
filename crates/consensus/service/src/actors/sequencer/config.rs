//! Configuration for the [`SequencerActor`].
//!
//! [`SequencerActor`]: super::SequencerActor

use std::time::Duration;

use url::Url;

/// Configuration for the [`SequencerActor`].
///
/// [`SequencerActor`]: super::SequencerActor
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequencerConfig {
    /// Whether or not the sequencer is enabled at startup.
    pub sequencer_stopped: bool,
    /// Whether or not the sequencer is in recovery mode.
    pub sequencer_recovery_mode: bool,
    /// The [`Url`] for the conductor RPC endpoint. If [`Some`], enables the conductor service.
    pub conductor_rpc_url: Option<Url>,
    /// The confirmation delay for the sequencer.
    pub l1_conf_delay: u64,
    /// WebSocket URL for the leader's flashblocks feed, used for preconfirmation tracking.
    ///
    /// When set alongside [`conductor_rpc_url`], the sequencer subscribes to this feed as a
    /// follower and accumulates preconfirmed transactions. On leadership transfer, the new
    /// leader injects those transactions into its first block to preserve user-visible ordering.
    ///
    /// [`conductor_rpc_url`]: Self::conductor_rpc_url
    pub preconfirmation_ws_url: Option<Url>,
    /// Time-to-live for preconfirmed transaction sets.
    ///
    /// Entries held longer than this duration are discarded on the next
    /// [`take_transactions`] call.
    ///
    /// [`take_transactions`]: super::PreconfirmationTracker::take_transactions
    pub preconfirmation_ttl: Duration,
}

impl Default for SequencerConfig {
    fn default() -> Self {
        Self {
            sequencer_stopped: false,
            sequencer_recovery_mode: false,
            conductor_rpc_url: None,
            l1_conf_delay: 0,
            preconfirmation_ws_url: None,
            preconfirmation_ttl: Duration::from_secs(30),
        }
    }
}
