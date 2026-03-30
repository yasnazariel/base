//! Tokio runtime utilities with graceful shutdown handling.
//!
//! Provides [`RuntimeManager`] for creating Tokio runtimes and installing
//! OS signal handlers (SIGINT + SIGTERM on unix, SIGINT on other platforms)
//! that cancel a [`CancellationToken`] for cooperative shutdown.

use std::future::Future;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// A runtime manager.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeManager;

impl RuntimeManager {
    /// Creates a new default tokio multi-thread [Runtime](tokio::runtime::Runtime) with all
    /// features enabled.
    pub fn tokio_runtime() -> Result<tokio::runtime::Runtime, std::io::Error> {
        tokio::runtime::Builder::new_multi_thread().enable_all().build()
    }

    /// Installs SIGTERM + SIGINT handlers that cancel the given token.
    ///
    /// On unix, this listens for both SIGINT and SIGTERM. On other platforms,
    /// only SIGINT (Ctrl-C) is handled. When a signal is received the
    /// [`CancellationToken`] is cancelled, allowing all holders of child tokens
    /// to begin cooperative shutdown.
    pub fn install_signal_handler(cancel: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(async move {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigterm =
                    signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
                tokio::select! {
                    result = tokio::signal::ctrl_c() => {
                        result.expect("failed to listen for SIGINT");
                        info!(signal = "SIGINT", "received shutdown signal");
                    }
                    _ = sigterm.recv() => {
                        info!(signal = "SIGTERM", "received shutdown signal");
                    }
                }
            }

            #[cfg(not(unix))]
            {
                tokio::signal::ctrl_c().await.expect("failed to listen for SIGINT");
                info!(signal = "SIGINT", "received shutdown signal");
            }

            cancel.cancel();
        })
    }

    /// Run a fallible future with a signal-driven [`CancellationToken`].
    ///
    /// This creates a new runtime, installs the standard SIGINT/SIGTERM handler, and passes the
    /// cancellation token into `f` so the future can coordinate a graceful shutdown instead of
    /// being dropped as soon as a signal arrives.
    pub fn run_with_signal_token<F, Fut>(f: F) -> eyre::Result<()>
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = eyre::Result<()>>,
    {
        let rt = Self::tokio_runtime().map_err(|e| eyre::eyre!(e))?;
        rt.block_on(async move {
            let cancellation = CancellationToken::new();
            let _signal_handler = Self::install_signal_handler(cancellation.clone());
            f(cancellation).await
        })
    }

    /// Run a fallible future until ctrl-c is pressed.
    pub fn run_until_ctrl_c<F>(fut: F) -> eyre::Result<()>
    where
        F: Future<Output = eyre::Result<()>>,
    {
        let rt = Self::tokio_runtime().map_err(|e| eyre::eyre!(e))?;
        rt.block_on(async move {
            tokio::select! {
                biased;
                _ = tokio::signal::ctrl_c() => {
                    info!(target: "cli", "Received Ctrl-C, shutting down...");
                    Ok(())
                }
                res = fut => res,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::RuntimeManager;

    #[test]
    fn run_with_signal_token_executes_future() {
        let ran = Arc::new(AtomicBool::new(false));
        let ran_inner = Arc::clone(&ran);

        RuntimeManager::run_with_signal_token(move |cancellation| async move {
            assert!(!cancellation.is_cancelled());
            ran_inner.store(true, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();

        assert!(ran.load(Ordering::SeqCst));
    }
}
