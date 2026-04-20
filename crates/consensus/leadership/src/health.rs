//! Health monitoring for the local sequencer.
//!
//! The [`HealthAggregator`] evaluates a [`HealthSignals`] snapshot against configured
//! thresholds and publishes a [`HealthVerdict`]. When the local node is leading and the
//! verdict turns unhealthy, the [`LeadershipActor`](crate::LeadershipActor) voluntarily
//! steps down rather than waiting for the consensus engine's view timeout.

use std::time::{Duration, Instant};

use tokio::sync::watch;

use crate::HealthThresholds;

/// Snapshot of the inputs that determine the local sequencer's health verdict.
///
/// Missing signals (`None`) are treated as unhealthy so the node is never considered fit
/// to lead before every signal has reported at least once.
#[derive(Clone, Copy, Debug, Default)]
pub struct HealthSignals {
    /// Wall-clock time at which the local node last observed an unsafe head update.
    pub last_unsafe_head_update: Option<Instant>,
    /// Wall-clock time at which the local node last observed an L1 head update.
    pub last_l1_head_update: Option<Instant>,
    /// Whether the execution layer reports itself as in sync.
    pub el_in_sync: Option<bool>,
    /// Number of currently connected gossip peers.
    pub peer_count: Option<usize>,
}

/// The verdict the [`HealthAggregator`] publishes on each evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HealthVerdict {
    /// The local sequencer is fit to lead.
    Healthy,
    /// The local sequencer is not fit to lead. The contained list enumerates each
    /// violated threshold in evaluation order.
    Unhealthy(Vec<HealthFailure>),
}

impl HealthVerdict {
    /// Returns `true` if the verdict is [`HealthVerdict::Healthy`].
    pub const fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy)
    }

    /// Returns `true` if the verdict is [`HealthVerdict::Unhealthy`].
    pub const fn is_unhealthy(&self) -> bool {
        matches!(self, Self::Unhealthy(_))
    }
}

impl Default for HealthVerdict {
    fn default() -> Self {
        // Default to unhealthy so a freshly-spawned actor does not spuriously claim
        // fitness to lead before any signals have been observed.
        Self::Unhealthy(vec![HealthFailure::NoSignalsYet])
    }
}

/// Specific health threshold violations that an [`HealthVerdict::Unhealthy`] verdict
/// enumerates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HealthFailure {
    /// The aggregator has not yet observed any signals; this is the startup state.
    NoSignalsYet,
    /// The unsafe head has not been updated within the configured threshold.
    UnsafeHeadStale {
        /// How long ago the unsafe head was last updated.
        age: Duration,
    },
    /// The L1 head has not been updated within the configured threshold.
    L1HeadStale {
        /// How long ago the L1 head was last updated.
        age: Duration,
    },
    /// The execution layer reports itself as still syncing.
    ExecutionLayerSyncing,
    /// The connected peer count is below the configured floor.
    LowPeerCount {
        /// The observed peer count.
        observed: usize,
        /// The configured minimum peer count.
        required: usize,
    },
}

/// Aggregates local sequencer health signals and publishes a verdict.
///
/// The aggregator is a pure evaluator: it does not own subscriptions to the underlying
/// signals. The [`LeadershipActor`](crate::LeadershipActor) is responsible for collecting
/// the latest [`HealthSignals`] snapshot on each tick and calling [`HealthAggregator::evaluate`].
#[derive(Debug)]
pub struct HealthAggregator {
    thresholds: HealthThresholds,
    verdict_tx: watch::Sender<HealthVerdict>,
}

impl HealthAggregator {
    /// Constructs a new aggregator with the given thresholds.
    ///
    /// Returns the aggregator together with the receiver side of its verdict watch channel.
    pub fn new(thresholds: HealthThresholds) -> (Self, watch::Receiver<HealthVerdict>) {
        let (verdict_tx, verdict_rx) = watch::channel(HealthVerdict::default());
        (Self { thresholds, verdict_tx }, verdict_rx)
    }

    /// Returns the configured polling interval.
    ///
    /// The actor uses this to drive its evaluation cadence.
    pub const fn poll_interval(&self) -> Duration {
        self.thresholds.poll_interval
    }

    /// Evaluates the verdict given the supplied signals and the supplied wall-clock `now`.
    ///
    /// `now` is taken as a parameter rather than read from `Instant::now()` so the
    /// evaluator is fully deterministic and easy to test. The actor passes `Instant::now()`
    /// at call time.
    ///
    /// Publishes the new verdict on the watch channel only if it differs from the
    /// previously-published value, so consumers do not see redundant wake-ups.
    pub fn evaluate(&self, signals: HealthSignals, now: Instant) -> HealthVerdict {
        let mut reasons = Vec::new();

        match signals.last_unsafe_head_update {
            None => reasons.push(HealthFailure::NoSignalsYet),
            Some(t) => {
                let age = now.saturating_duration_since(t);
                if age > self.thresholds.unsafe_head_max_age {
                    reasons.push(HealthFailure::UnsafeHeadStale { age });
                }
            }
        }

        match signals.last_l1_head_update {
            None => {
                if !reasons.contains(&HealthFailure::NoSignalsYet) {
                    reasons.push(HealthFailure::NoSignalsYet);
                }
            }
            Some(t) => {
                let age = now.saturating_duration_since(t);
                if age > self.thresholds.l1_head_max_age {
                    reasons.push(HealthFailure::L1HeadStale { age });
                }
            }
        }

        match signals.el_in_sync {
            None => {
                if !reasons.contains(&HealthFailure::NoSignalsYet) {
                    reasons.push(HealthFailure::NoSignalsYet);
                }
            }
            Some(false) => reasons.push(HealthFailure::ExecutionLayerSyncing),
            Some(true) => {}
        }

        match signals.peer_count {
            None => {
                if !reasons.contains(&HealthFailure::NoSignalsYet) {
                    reasons.push(HealthFailure::NoSignalsYet);
                }
            }
            Some(count) if count < self.thresholds.min_peer_count => {
                reasons.push(HealthFailure::LowPeerCount {
                    observed: count,
                    required: self.thresholds.min_peer_count,
                });
            }
            Some(_) => {}
        }

        let verdict = if reasons.is_empty() {
            HealthVerdict::Healthy
        } else {
            HealthVerdict::Unhealthy(reasons)
        };

        self.verdict_tx.send_if_modified(|prev| {
            if prev == &verdict {
                false
            } else {
                *prev = verdict.clone();
                true
            }
        });

        verdict
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn thresholds() -> HealthThresholds {
        HealthThresholds {
            unsafe_head_max_age: Duration::from_secs(30),
            l1_head_max_age: Duration::from_secs(60),
            min_peer_count: 3,
            poll_interval: Duration::from_secs(5),
        }
    }

    fn fresh_signals(now: Instant) -> HealthSignals {
        HealthSignals {
            last_unsafe_head_update: Some(now),
            last_l1_head_update: Some(now),
            el_in_sync: Some(true),
            peer_count: Some(5),
        }
    }

    #[test]
    fn fresh_signals_evaluate_to_healthy() {
        let (agg, mut rx) = HealthAggregator::new(thresholds());
        let now = Instant::now();
        let verdict = agg.evaluate(fresh_signals(now), now);
        assert!(verdict.is_healthy());
        assert!(rx.has_changed().unwrap());
        assert!(rx.borrow_and_update().is_healthy());
    }

    #[test]
    fn missing_signals_evaluate_to_unhealthy_no_signals_yet() {
        let (agg, _rx) = HealthAggregator::new(thresholds());
        let verdict = agg.evaluate(HealthSignals::default(), Instant::now());
        let HealthVerdict::Unhealthy(reasons) = verdict else {
            panic!("expected unhealthy");
        };
        // NoSignalsYet should appear once even though all four inputs are missing.
        assert_eq!(reasons.iter().filter(|r| matches!(r, HealthFailure::NoSignalsYet)).count(), 1);
    }

    #[rstest]
    #[case::stale_unsafe_head(
        HealthSignals {
            last_unsafe_head_update: Some(Instant::now() - Duration::from_secs(60)),
            last_l1_head_update: Some(Instant::now()),
            el_in_sync: Some(true),
            peer_count: Some(5),
        },
        |reasons: &[HealthFailure]| matches!(reasons, [HealthFailure::UnsafeHeadStale { .. }]),
    )]
    #[case::stale_l1_head(
        HealthSignals {
            last_unsafe_head_update: Some(Instant::now()),
            last_l1_head_update: Some(Instant::now() - Duration::from_secs(120)),
            el_in_sync: Some(true),
            peer_count: Some(5),
        },
        |reasons: &[HealthFailure]| matches!(reasons, [HealthFailure::L1HeadStale { .. }]),
    )]
    #[case::el_syncing(
        HealthSignals {
            last_unsafe_head_update: Some(Instant::now()),
            last_l1_head_update: Some(Instant::now()),
            el_in_sync: Some(false),
            peer_count: Some(5),
        },
        |reasons: &[HealthFailure]| matches!(reasons, [HealthFailure::ExecutionLayerSyncing]),
    )]
    #[case::low_peer_count(
        HealthSignals {
            last_unsafe_head_update: Some(Instant::now()),
            last_l1_head_update: Some(Instant::now()),
            el_in_sync: Some(true),
            peer_count: Some(1),
        },
        |reasons: &[HealthFailure]| matches!(
            reasons,
            [HealthFailure::LowPeerCount { observed: 1, required: 3 }],
        ),
    )]
    fn each_threshold_breach_surfaces_a_specific_failure(
        #[case] signals: HealthSignals,
        #[case] expected: fn(&[HealthFailure]) -> bool,
    ) {
        let (agg, _rx) = HealthAggregator::new(thresholds());
        let verdict = agg.evaluate(signals, Instant::now());
        let HealthVerdict::Unhealthy(reasons) = verdict else {
            panic!("expected unhealthy");
        };
        assert!(expected(&reasons), "unexpected reasons: {reasons:?}");
    }

    #[test]
    fn watch_channel_does_not_re_emit_on_unchanged_verdict() {
        let (agg, mut rx) = HealthAggregator::new(thresholds());
        let now = Instant::now();
        let _ = agg.evaluate(fresh_signals(now), now);
        let _ = rx.borrow_and_update();
        let _ = agg.evaluate(fresh_signals(now), now);
        assert!(!rx.has_changed().unwrap(), "unchanged verdict must not re-notify");
    }
}
