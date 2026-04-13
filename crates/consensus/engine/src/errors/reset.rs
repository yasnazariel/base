//! Error types for engine reset.

use base_protocol::BaseBlockConversionError;
use thiserror::Error;

use super::SynchronizeTaskError;
use crate::SyncStartError;

/// An error occurred while attempting to reset the engine.
#[derive(Debug, Error)]
pub enum EngineResetError {
    /// An error that occurred while updating the forkchoice state.
    #[error(transparent)]
    Forkchoice(#[from] SynchronizeTaskError),
    /// An error occurred while traversing the L1 for the sync starting point.
    #[error(transparent)]
    SyncStart(#[from] SyncStartError),
    /// An error occurred while constructing the `SystemConfig` for the new safe head.
    #[error(transparent)]
    SystemConfigConversion(#[from] BaseBlockConversionError),
    /// The EL is still syncing; the reset cannot proceed yet.
    #[error("EL sync in progress; reset deferred")]
    ELSyncing,
}
