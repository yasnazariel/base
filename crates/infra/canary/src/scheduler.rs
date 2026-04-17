//! Scheduling logic for canary action dispatch.

use std::time::Duration;

use rand::Rng;

/// Schedule mode for canary runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleMode {
    /// Fixed interval between runs.
    Deterministic,
    /// Random interval within `[interval, interval + jitter]`.
    Random,
}

/// Computes the next sleep duration between canary runs.
#[derive(Debug, Clone)]
pub struct Scheduler {
    mode: ScheduleMode,
    interval: Duration,
    jitter: Duration,
}

impl Scheduler {
    /// Creates a new [`Scheduler`].
    pub const fn new(mode: ScheduleMode, interval: Duration, jitter: Duration) -> Self {
        Self { mode, interval, jitter }
    }

    /// Returns the next delay before the next canary cycle.
    pub fn next_delay(&self) -> Duration {
        match self.mode {
            ScheduleMode::Deterministic => self.interval,
            ScheduleMode::Random => {
                let jitter_millis = if self.jitter.is_zero() {
                    0
                } else {
                    rand::rng().random_range(0..=self.jitter.as_millis() as u64)
                };
                self.interval + Duration::from_millis(jitter_millis)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deterministic_returns_exact_interval() {
        let scheduler =
            Scheduler::new(ScheduleMode::Deterministic, Duration::from_secs(60), Duration::ZERO);

        for _ in 0..10 {
            assert_eq!(scheduler.next_delay(), Duration::from_secs(60));
        }
    }

    #[test]
    fn test_random_within_bounds() {
        let interval = Duration::from_secs(60);
        let jitter = Duration::from_secs(30);
        let scheduler = Scheduler::new(ScheduleMode::Random, interval, jitter);

        for _ in 0..100 {
            let delay = scheduler.next_delay();
            assert!(delay >= interval, "delay {delay:?} should be >= interval {interval:?}");
            assert!(
                delay <= interval + jitter,
                "delay {delay:?} should be <= interval + jitter {:?}",
                interval + jitter
            );
        }
    }

    #[test]
    fn test_random_zero_jitter() {
        let scheduler =
            Scheduler::new(ScheduleMode::Random, Duration::from_secs(10), Duration::ZERO);

        for _ in 0..10 {
            assert_eq!(scheduler.next_delay(), Duration::from_secs(10));
        }
    }
}
