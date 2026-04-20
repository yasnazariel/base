//! Leadership status published by the [`LeadershipActor`](crate::LeadershipActor).
//!
//! Delivered over a `tokio::sync::watch` channel so consumers see the most recent value
//! without draining a queue.

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::ValidatorId;

/// The leadership state of the local node, as determined by the consensus driver.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeaderStatus {
    /// The local node is the elected leader and is safe to sequence blocks.
    Leader {
        /// The consensus view (epoch + round) in which this node was elected.
        ///
        /// Operators use this to disambiguate rapid leader churn in logs and metrics.
        view: u64,
        /// `true` when the node is leading because of a manual `OverrideLeader` admin
        /// command rather than a real consensus decision. Operators should treat
        /// `overridden = true` as a disaster-recovery state.
        overridden: bool,
        /// Monotonically increasing override generation, bumped each time the override is
        /// (re-)enabled. `0` when no override has ever been active.
        ///
        /// External observers compare generations to detect concurrent or stale overrides
        /// during a network partition: if a remote node is also reporting
        /// `overridden = true` with a different `override_generation`, at least one of
        /// the two is operating on stale information.
        override_generation: u64,
    },
    /// Some other node is the elected leader. The local node must not sequence blocks.
    Follower {
        /// The id of the validator currently serving as leader.
        leader: ValidatorId,
        /// The consensus view in which `leader` was elected.
        view: u64,
    },
    /// The cluster does not currently have an agreed-upon leader.
    ///
    /// This is the state during startup, after a quorum loss, during a network partition
    /// that isolates the local node, or while a view change is in progress. The local node
    /// must not sequence blocks while in this state.
    #[default]
    Unknown,
}

impl LeaderStatus {
    /// Returns `true` if the local node is the elected leader and may sequence blocks.
    pub const fn is_leader(&self) -> bool {
        matches!(self, Self::Leader { .. })
    }

    /// Returns `true` if a leader is known but it is not the local node.
    pub const fn is_follower(&self) -> bool {
        matches!(self, Self::Follower { .. })
    }

    /// Returns `true` if no leader is currently established.
    pub const fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// Returns `true` if the local node is leading because of a manual override.
    pub const fn is_overridden(&self) -> bool {
        matches!(self, Self::Leader { overridden: true, .. })
    }

    /// Returns the consensus view associated with this status, if any.
    ///
    /// `Unknown` carries no view and returns `None`.
    pub const fn view(&self) -> Option<u64> {
        match self {
            Self::Leader { view, .. } | Self::Follower { view, .. } => Some(*view),
            Self::Unknown => None,
        }
    }

    /// Returns the override generation, or `0` if the local node is not overridden.
    pub const fn override_generation(&self) -> u64 {
        match self {
            Self::Leader { override_generation, .. } => *override_generation,
            _ => 0,
        }
    }
}

/// Producer side of the leader-status channel, owned by the
/// [`LeadershipActor`](crate::LeadershipActor).
pub type LeaderStatusSender = watch::Sender<LeaderStatus>;

/// Consumer side of the leader-status channel, subscribed to by the sequencer and any
/// other observers.
pub type LeaderStatusReceiver = watch::Receiver<LeaderStatus>;

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(LeaderStatus::Leader { view: 7, overridden: false, override_generation: 0 }, true, false, false, Some(7))]
    #[case(LeaderStatus::Leader { view: 9, overridden: true, override_generation: 1 }, true, false, false, Some(9))]
    #[case(
        LeaderStatus::Follower { leader: ValidatorId::new("seq-2"), view: 9 },
        false,
        true,
        false,
        Some(9)
    )]
    #[case(LeaderStatus::Unknown, false, false, true, None)]
    fn predicates_and_view_match_variant(
        #[case] status: LeaderStatus,
        #[case] is_leader: bool,
        #[case] is_follower: bool,
        #[case] is_unknown: bool,
        #[case] view: Option<u64>,
    ) {
        assert_eq!(status.is_leader(), is_leader);
        assert_eq!(status.is_follower(), is_follower);
        assert_eq!(status.is_unknown(), is_unknown);
        assert_eq!(status.view(), view);
    }

    #[test]
    fn is_overridden_only_true_for_overridden_leader() {
        assert!(
            LeaderStatus::Leader { view: 0, overridden: true, override_generation: 1 }
                .is_overridden()
        );
        assert!(
            !LeaderStatus::Leader { view: 0, overridden: false, override_generation: 0 }
                .is_overridden()
        );
        assert!(!LeaderStatus::Follower { leader: ValidatorId::new("x"), view: 0 }.is_overridden());
        assert!(!LeaderStatus::Unknown.is_overridden());
    }

    #[test]
    fn override_generation_returns_zero_when_not_overridden() {
        assert_eq!(LeaderStatus::Unknown.override_generation(), 0);
        assert_eq!(
            LeaderStatus::Follower { leader: ValidatorId::new("x"), view: 5 }.override_generation(),
            0
        );
        assert_eq!(
            LeaderStatus::Leader { view: 0, overridden: false, override_generation: 0 }
                .override_generation(),
            0
        );
        assert_eq!(
            LeaderStatus::Leader { view: 0, overridden: true, override_generation: 7 }
                .override_generation(),
            7
        );
    }

    #[test]
    fn default_status_is_unknown() {
        assert_eq!(LeaderStatus::default(), LeaderStatus::Unknown);
    }
}
