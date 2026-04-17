//! Canary action trait and outcome types.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

/// Outcome of a canary action execution.
#[derive(Debug, Clone)]
pub struct ActionOutcome {
    /// Whether the action succeeded.
    pub succeeded: bool,
    /// How long the action took.
    pub duration: Duration,
    /// Human-readable summary.
    pub message: String,
}

impl ActionOutcome {
    /// Constructs a failed [`ActionOutcome`] with elapsed time since `start`.
    pub fn failed(message: impl Into<String>, start: Instant) -> Self {
        Self { succeeded: false, duration: start.elapsed(), message: message.into() }
    }

    /// Constructs a successful [`ActionOutcome`] with elapsed time since `start`.
    pub fn success(message: impl Into<String>, start: Instant) -> Self {
        Self { succeeded: true, duration: start.elapsed(), message: message.into() }
    }
}

/// Trait for pluggable canary actions.
///
/// Each action represents a discrete check or test that the canary performs on
/// each scheduled cycle.
#[async_trait]
pub trait CanaryAction: Send + Sync + std::fmt::Debug {
    /// Returns the action name (used for metrics labels and log fields).
    fn name(&self) -> &'static str;

    /// Executes the action, respecting the cancellation token for graceful
    /// shutdown.
    async fn execute(&self, cancel: CancellationToken) -> ActionOutcome;
}
