use base_consensus_derive::PipelineErrorKind;
use base_consensus_engine::BuildTaskError;

use crate::{
    ConductorError, L1OriginSelectorError, UnsafePayloadGossipClientError,
    actors::engine::EngineClientError,
};

/// An error produced by the [`crate::SequencerActor`].
#[derive(Debug, thiserror::Error)]
pub enum SequencerActorError {
    /// An error occurred while building payload attributes.
    #[error(transparent)]
    AttributesBuilder(#[from] PipelineErrorKind),
    /// A channel was unexpectedly closed.
    #[error("Channel closed unexpectedly")]
    ChannelClosed,
    /// An error occurred while selecting the next L1 origin.
    #[error(transparent)]
    L1OriginSelector(#[from] L1OriginSelectorError),
    /// An error occurred communicating with the engine.
    #[error(transparent)]
    EngineError(#[from] EngineClientError),
    /// An error occurred while attempting to build a payload.
    #[error(transparent)]
    BuildError(#[from] BuildTaskError),
    /// An error occurred while attempting to schedule unsafe payload gossip.
    #[error("An error occurred while attempting to schedule unsafe payload gossip: {0}")]
    PayloadGossip(#[from] UnsafePayloadGossipClientError),
    /// Conductor commit failed (non-fatal, retry with backoff).
    #[error("Conductor commit failed: {0}")]
    ConductorCommitFailed(ConductorError),
}

impl SequencerActorError {
    /// Returns `true` for errors that should terminate the sequencer.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::EngineError(EngineClientError::SealError(err)) if err.is_fatal()
        )
    }
}
