//! Error types for block building.

use alloy_rpc_types_engine::PayloadStatusEnum;
use alloy_transport::{RpcError, TransportErrorKind};
use thiserror::Error;
use tokio::sync::mpsc;

use super::{EngineTaskError, EngineTaskErrorSeverity};

/// An error that occurs during payload building within the engine.
#[derive(Debug, Error)]
pub enum EngineBuildError {
    /// The finalized head is ahead of the unsafe head.
    #[error("Finalized head is ahead of unsafe head")]
    FinalizedAheadOfUnsafe(u64, u64),
    /// The forkchoice update call to the engine api failed.
    #[error("Failed to build payload attributes in the engine. Forkchoice RPC error: {0}")]
    AttributesInsertionFailed(#[from] RpcError<TransportErrorKind>),
    /// The inserted payload is invalid.
    #[error("The inserted payload is invalid: {0}")]
    InvalidPayload(String),
    /// The inserted payload status is unexpected.
    #[error("The inserted payload status is unexpected: {0}")]
    UnexpectedPayloadStatus(PayloadStatusEnum),
    /// The payload ID is missing.
    #[error("The inserted payload ID is missing")]
    MissingPayloadId,
    /// The engine is syncing.
    #[error("The engine is syncing")]
    EngineSyncing,
}

/// An error that occurs when building a block.
#[derive(Debug, Error)]
pub enum BuildTaskError {
    /// An error occurred when building the payload attributes in the engine.
    #[error("An error occurred when building the payload attributes to the engine.")]
    EngineBuildError(EngineBuildError),
    /// Error sending the built payload envelope.
    #[error(transparent)]
    MpscSend(#[from] Box<mpsc::error::SendError<alloy_rpc_types_engine::PayloadId>>),
}

impl EngineTaskError for BuildTaskError {
    fn severity(&self) -> EngineTaskErrorSeverity {
        match self {
            Self::EngineBuildError(EngineBuildError::FinalizedAheadOfUnsafe(_, _)) => {
                EngineTaskErrorSeverity::Critical
            }
            Self::EngineBuildError(EngineBuildError::AttributesInsertionFailed(_))
            | Self::EngineBuildError(EngineBuildError::InvalidPayload(_))
            | Self::EngineBuildError(EngineBuildError::UnexpectedPayloadStatus(_))
            | Self::EngineBuildError(EngineBuildError::MissingPayloadId)
            | Self::EngineBuildError(EngineBuildError::EngineSyncing) => {
                EngineTaskErrorSeverity::Temporary
            }
            Self::MpscSend(_) => EngineTaskErrorSeverity::Critical,
        }
    }
}
