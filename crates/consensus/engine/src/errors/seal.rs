//! Error types for block sealing.

use alloy_transport::{RpcError, TransportErrorKind};
use base_protocol::FromBlockError;
use thiserror::Error;

use super::{EngineTaskError, EngineTaskErrorSeverity, InsertTaskError, SynchronizeTaskError};

/// An error that occurs when sealing a block.
#[derive(Debug, Error)]
pub enum SealTaskError {
    /// Impossible to insert the payload into the engine.
    #[error(transparent)]
    PayloadInsertionFailed(#[from] Box<InsertTaskError>),
    /// The get payload call to the engine api failed.
    #[error(transparent)]
    GetPayloadFailed(RpcError<TransportErrorKind>),
    /// A deposit-only payload failed to import.
    #[error("Deposit-only payload failed to import")]
    DepositOnlyPayloadFailed,
    /// Failed to re-attempt payload import with deposit-only payload.
    #[error("Failed to re-attempt payload import with deposit-only payload")]
    DepositOnlyPayloadReattemptFailed,
    /// The payload is invalid, and the derivation pipeline must be flushed post-holocene.
    #[error("Invalid payload, must flush post-holocene")]
    HoloceneInvalidFlush,
    /// Failed to convert a payload to `L2BlockInfo`.
    #[error(transparent)]
    FromBlock(#[from] FromBlockError),
    /// The clock went backwards.
    #[error("The clock went backwards")]
    ClockWentBackwards,
    /// Unsafe head changed between build and seal.
    #[error("Unsafe head changed between build and seal")]
    UnsafeHeadChangedSinceBuild,
}

impl SealTaskError {
    /// Whether this error is fatal from the sequencer's perspective.
    pub fn is_fatal(&self) -> bool {
        match self {
            Self::PayloadInsertionFailed(insert_err) => match &**insert_err {
                InsertTaskError::ForkchoiceUpdateFailed(synchronize_error) => {
                    match synchronize_error {
                        SynchronizeTaskError::FinalizedAheadOfUnsafe(_, _) => true,
                        SynchronizeTaskError::ForkchoiceUpdateFailed(_)
                        | SynchronizeTaskError::InvalidForkchoiceState
                        | SynchronizeTaskError::UnexpectedPayloadStatus(_) => false,
                    }
                }
                InsertTaskError::FromBlockError(_)
                | InsertTaskError::L2BlockInfoConstruction(_) => true,
                InsertTaskError::InsertFailed(_) | InsertTaskError::UnexpectedPayloadStatus(_) => {
                    false
                }
            },
            Self::GetPayloadFailed(_)
            | Self::HoloceneInvalidFlush
            | Self::UnsafeHeadChangedSinceBuild => false,
            Self::DepositOnlyPayloadFailed
            | Self::DepositOnlyPayloadReattemptFailed
            | Self::FromBlock(_)
            | Self::ClockWentBackwards => true,
        }
    }
}

impl EngineTaskError for SealTaskError {
    fn severity(&self) -> EngineTaskErrorSeverity {
        match self {
            Self::PayloadInsertionFailed(inner) => inner.severity(),
            Self::GetPayloadFailed(_) => EngineTaskErrorSeverity::Temporary,
            Self::HoloceneInvalidFlush => EngineTaskErrorSeverity::Flush,
            Self::UnsafeHeadChangedSinceBuild => EngineTaskErrorSeverity::Reset,
            Self::DepositOnlyPayloadReattemptFailed
            | Self::DepositOnlyPayloadFailed
            | Self::FromBlock(_)
            | Self::ClockWentBackwards => EngineTaskErrorSeverity::Critical,
        }
    }
}
