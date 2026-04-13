//! Engine task error severity.

use derive_more::Display;

/// The severity of an engine task error.
#[derive(Debug, PartialEq, Eq, Display, Clone, Copy)]
pub enum EngineTaskErrorSeverity {
    /// The error is temporary and the task should be retried.
    #[display("temporary")]
    Temporary,
    /// The error is critical and is propagated to the caller.
    #[display("critical")]
    Critical,
    /// The error indicates that the engine should be reset.
    #[display("reset")]
    Reset,
    /// The error indicates that the engine should be flushed.
    #[display("flush")]
    Flush,
}

impl EngineTaskErrorSeverity {
    /// Returns a static string label for use in metrics.
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Temporary => "temporary",
            Self::Critical => "critical",
            Self::Reset => "reset",
            Self::Flush => "flush",
        }
    }
}

/// The interface for an engine task error.
pub trait EngineTaskError {
    /// The severity of the error.
    fn severity(&self) -> EngineTaskErrorSeverity;
}
