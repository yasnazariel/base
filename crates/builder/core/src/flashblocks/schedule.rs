//! Flashblock timing schedule for deadline-based build pacing.
//!
//! [`FlashblockSchedule`] tracks how many flashblocks can still fit before the
//! block deadline and provides budget-division helpers.  All time queries accept
//! an explicit `now` parameter so callers can capture `Instant::now()` once and
//! get consistent answers from both [`should_build_next`](FlashblockSchedule::should_build_next)
//! and [`remaining_count`](FlashblockSchedule::remaining_count).

use core::time::Duration;
use std::time::Instant;

/// Timing schedule for flashblock production within a single block.
///
/// Created once per block from the block deadline and the configured
/// flashblock interval.  Methods that depend on the current time take an
/// explicit `now: Instant` parameter to avoid TOCTOU inconsistencies.
#[derive(Debug, Clone, Copy)]
pub struct FlashblockSchedule {
    deadline: Instant,
    interval: Duration,
}

impl FlashblockSchedule {
    /// Creates a new schedule.
    ///
    /// # Panics
    ///
    /// Panics if `interval` is zero — a zero-interval schedule cannot
    /// produce any flashblocks and indicates a configuration error.
    pub fn new(deadline: Instant, interval: Duration) -> Self {
        assert!(!interval.is_zero(), "flashblock interval must be non-zero");
        Self { deadline, interval }
    }

    /// Returns `true` if there is enough time remaining to build at least
    /// one more flashblock.
    pub fn should_build_next(&self, now: Instant) -> bool {
        now + self.interval <= self.deadline
    }

    /// Returns how many flashblocks can still be produced before the deadline.
    ///
    /// Uses ceiling division so that a partial remaining interval still counts
    /// as one flashblock.  This means `remaining_count` may return `1` when
    /// [`should_build_next`](Self::should_build_next) returns `false` (less
    /// than a full interval remains).  Callers that enter a build loop should
    /// gate on `should_build_next` first; callers that divide budgets use the
    /// `.max(1)` at their call site to avoid division-by-zero.
    pub fn remaining_count(&self, now: Instant) -> u64 {
        let remaining = self.deadline.saturating_duration_since(now);
        remaining.as_millis().div_ceil(self.interval.as_millis()) as u64
    }

    /// The configured flashblock interval.
    pub const fn interval(&self) -> Duration {
        self.interval
    }

    /// The absolute block deadline.
    pub const fn deadline(&self) -> Instant {
        self.deadline
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;
    use std::time::Instant;

    use super::FlashblockSchedule;

    #[test]
    fn test_normal_schedule() {
        let now = Instant::now();
        let schedule =
            FlashblockSchedule::new(now + Duration::from_secs(2), Duration::from_millis(200));

        assert!(schedule.should_build_next(now));
        assert_eq!(schedule.remaining_count(now), 10);
    }

    #[test]
    fn test_late_fcu() {
        let now = Instant::now();
        let schedule =
            FlashblockSchedule::new(now + Duration::from_millis(800), Duration::from_millis(200));

        assert!(schedule.should_build_next(now));
        assert_eq!(schedule.remaining_count(now), 4);
    }

    #[test]
    fn test_very_late_fcu() {
        let now = Instant::now();
        let schedule =
            FlashblockSchedule::new(now + Duration::from_millis(100), Duration::from_millis(200));

        assert!(!schedule.should_build_next(now));
        assert_eq!(schedule.remaining_count(now), 1);
    }

    #[test]
    #[should_panic(expected = "flashblock interval must be non-zero")]
    fn test_zero_interval_panics() {
        FlashblockSchedule::new(Instant::now() + Duration::from_millis(500), Duration::ZERO);
    }

    #[test]
    fn test_expired_deadline() {
        let now = Instant::now();
        let schedule =
            FlashblockSchedule::new(now - Duration::from_millis(1), Duration::from_millis(200));

        assert!(!schedule.should_build_next(now));
        assert_eq!(schedule.remaining_count(now), 0);
    }

    #[test]
    fn test_remaining_count_and_should_build_next_consistent() {
        let now = Instant::now();
        let schedule =
            FlashblockSchedule::new(now + Duration::from_millis(250), Duration::from_millis(200));

        // 250ms remaining, 200ms interval → one full interval fits, partial rounds up
        assert!(schedule.should_build_next(now));
        assert_eq!(schedule.remaining_count(now), 2);

        // After 100ms, 150ms remaining → should_build_next is false but div_ceil yields 1
        let later = now + Duration::from_millis(100);
        assert!(!schedule.should_build_next(later));
        assert_eq!(schedule.remaining_count(later), 1);
    }
}
