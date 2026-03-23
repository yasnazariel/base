//! Admin command channel for runtime control of the batch driver.

use std::sync::Arc;

/// Type-erased setter for the global log level.
///
/// The setter receives the raw level string from the JSON-RPC caller and is
/// responsible for parsing it and applying it to the global subscriber. An
/// error from the setter is surfaced as [`AdminError::SetLogLevel`].
pub type LogSetter = Arc<dyn Fn(&str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> + Send + Sync>;

use tokio::sync::{mpsc, oneshot};

use crate::{ThrottleConfig, ThrottleInfo, ThrottleStrategy};

/// Capacity of the admin command channel.
///
/// 32 is generous for an infrequently-used admin API; commands are processed
/// in the main driver loop on every iteration so the channel rarely fills.
pub const ADMIN_CHANNEL_CAPACITY: usize = 32;

/// Runtime state snapshot returned by [`AdminCommand::GetStatus`].
///
/// Serialised directly as the `admin_getBatcherStatus` JSON-RPC response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BatcherStatus {
    /// Whether block ingestion is currently paused via the admin API.
    pub paused: bool,
    /// Number of L1 transactions submitted but not yet confirmed.
    pub in_flight: usize,
    /// Estimated unsubmitted DA backlog in bytes.
    pub da_backlog_bytes: u64,
}

/// Errors produced by admin operations.
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    /// The driver task has exited and the command channel is closed.
    #[error("admin channel closed: driver has shut down")]
    ChannelClosed,
    /// The requested operation is not yet supported.
    #[error("not yet supported: {0}")]
    NotSupported(&'static str),
    /// The log level setter failed (e.g. unknown level string or subscriber error).
    #[error("failed to set log level: {0}")]
    SetLogLevel(String),
}

/// Result type alias for admin operations.
pub type AdminResult<T> = Result<T, AdminError>;

/// Commands the admin HTTP server can send to the running driver task.
#[derive(derive_more::Debug)]
pub enum AdminCommand {
    /// Resume block ingestion after a [`Pause`](Self::Pause).
    Resume,
    /// Pause block ingestion without stopping the driver task.
    Pause,
    /// Force-close the current encoding channel (equivalent to a flush event).
    Flush,
    /// Replace the throttle strategy and configuration.
    SetThrottle {
        /// The new throttle strategy to apply.
        strategy: ThrottleStrategy,
        /// The new throttle configuration to apply.
        #[debug(skip)]
        config: ThrottleConfig,
    },
    /// Clear the throttle dedup cache so limits are re-applied unconditionally.
    ResetThrottle,
    /// Read current throttle state; reply sent via the embedded oneshot sender.
    GetThrottleInfo {
        /// Channel to send the throttle info snapshot back on.
        #[debug(skip)]
        reply: oneshot::Sender<ThrottleInfo>,
    },
    /// Read current driver runtime state; reply sent via the embedded oneshot sender.
    GetStatus {
        /// Channel to send the batcher status back on.
        #[debug(skip)]
        reply: oneshot::Sender<BatcherStatus>,
    },
}

/// Cloneable handle to the driver's admin command channel.
///
/// Create with [`AdminHandle::channel`]; wire the returned
/// [`mpsc::Receiver`] into the driver via [`BatchDriver::with_admin_rx`].
/// Optionally attach a log-level setter via [`AdminHandle::with_log_setter`]
/// to enable `admin_setLogLevel`.
#[derive(Clone, derive_more::Debug)]
pub struct AdminHandle {
    tx: mpsc::Sender<AdminCommand>,
    #[debug(skip)]
    log_setter: Option<LogSetter>,
}

impl AdminHandle {
    /// Create a matched `(AdminHandle, Receiver)` pair.
    pub fn channel() -> (Self, mpsc::Receiver<AdminCommand>) {
        let (tx, rx) = mpsc::channel(ADMIN_CHANNEL_CAPACITY);
        (Self { tx, log_setter: None }, rx)
    }

    /// Attach a runtime log-level setter.
    ///
    /// The setter is called by [`set_log_level`](Self::set_log_level) with the
    /// raw level string from the JSON-RPC caller. It should parse the level and
    /// apply it to the global subscriber.
    pub fn with_log_setter(self, setter: LogSetter) -> Self {
        Self { log_setter: Some(setter), ..self }
    }

    /// Resume block ingestion if currently paused.
    pub async fn resume(&self) -> AdminResult<()> {
        self.send(AdminCommand::Resume).await
    }

    /// Pause block ingestion without stopping the driver task.
    ///
    /// In-flight submissions continue to resolve; no new blocks are ingested
    /// until [`resume`](Self::resume) is called.
    pub async fn pause(&self) -> AdminResult<()> {
        self.send(AdminCommand::Pause).await
    }

    /// Force-close the current encoding channel, submitting any buffered frames.
    pub async fn flush(&self) -> AdminResult<()> {
        self.send(AdminCommand::Flush).await
    }

    /// Replace the throttle strategy and configuration.
    ///
    /// The full [`ThrottleConfig`] is required — partial updates are not
    /// supported. Callers that want to change only one field should call
    /// [`get_throttle_info`](Self::get_throttle_info) first to read the
    /// current config, adjust the desired field, and pass the result here.
    pub async fn set_throttle(
        &self,
        strategy: ThrottleStrategy,
        config: ThrottleConfig,
    ) -> AdminResult<()> {
        self.send(AdminCommand::SetThrottle { strategy, config }).await
    }

    /// Clear the throttle dedup cache so limits are re-applied unconditionally
    /// on the next driver iteration.
    pub async fn reset_throttle(&self) -> AdminResult<()> {
        self.send(AdminCommand::ResetThrottle).await
    }

    /// Read the current throttle controller state.
    pub async fn get_throttle_info(&self) -> AdminResult<ThrottleInfo> {
        let (tx, rx) = oneshot::channel();
        self.send(AdminCommand::GetThrottleInfo { reply: tx }).await?;
        rx.await.map_err(|_| AdminError::ChannelClosed)
    }

    /// Read the current driver runtime state.
    pub async fn get_status(&self) -> AdminResult<BatcherStatus> {
        let (tx, rx) = oneshot::channel();
        self.send(AdminCommand::GetStatus { reply: tx }).await?;
        rx.await.map_err(|_| AdminError::ChannelClosed)
    }

    /// Change the global log level at runtime.
    ///
    /// Requires a setter to have been attached via [`with_log_setter`](Self::with_log_setter).
    /// Returns [`AdminError::NotSupported`] when no setter is configured and
    /// [`AdminError::SetLogLevel`] when the setter itself fails (e.g. unknown
    /// level string).
    pub fn set_log_level(&self, level: String) -> AdminResult<()> {
        let setter = self.log_setter.as_ref().ok_or(AdminError::NotSupported("set_log_level"))?;
        setter(&level).map_err(|e| AdminError::SetLogLevel(e.to_string()))
    }

    async fn send(&self, cmd: AdminCommand) -> AdminResult<()> {
        self.tx.send(cmd).await.map_err(|_| AdminError::ChannelClosed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resume_returns_channel_closed_when_rx_dropped() {
        let (handle, rx) = AdminHandle::channel();
        drop(rx);
        let err = handle.resume().await.unwrap_err();
        assert!(matches!(err, AdminError::ChannelClosed));
    }

    #[tokio::test]
    async fn get_status_returns_channel_closed_when_rx_dropped() {
        let (handle, rx) = AdminHandle::channel();
        drop(rx);
        let err = handle.get_status().await.unwrap_err();
        assert!(matches!(err, AdminError::ChannelClosed));
    }

    #[test]
    fn set_log_level_returns_not_supported_without_setter() {
        let (handle, _rx) = AdminHandle::channel();
        let err = handle.set_log_level("debug".to_string()).unwrap_err();
        assert!(matches!(err, AdminError::NotSupported(_)));
    }

    #[test]
    fn set_log_level_calls_setter_when_configured() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = Arc::clone(&called);
        let setter: Arc<dyn Fn(&str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> + Send + Sync> =
            Arc::new(move |_level: &str| {
                called_clone.store(true, Ordering::Relaxed);
                Ok(())
            });
        let (handle, _rx) = AdminHandle::channel();
        let handle = handle.with_log_setter(setter);
        handle.set_log_level("debug".to_string()).unwrap();
        assert!(called.load(Ordering::Relaxed));
    }

    #[test]
    fn set_log_level_returns_set_log_level_error_on_setter_failure() {
        let setter: Arc<dyn Fn(&str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> + Send + Sync> =
            Arc::new(|_: &str| Err("bad level".into()));
        let (handle, _rx) = AdminHandle::channel();
        let handle = handle.with_log_setter(setter);
        let err = handle.set_log_level("notavalidlevel".to_string()).unwrap_err();
        assert!(matches!(err, AdminError::SetLogLevel(_)));
    }
}
