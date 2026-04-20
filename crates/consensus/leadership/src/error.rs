//! Error types for the leadership crate.

use thiserror::Error;

use crate::ValidatorId;

/// Errors raised by the [`LeadershipActor`](crate::LeadershipActor).
#[derive(Debug, Error)]
pub enum LeadershipError {
    /// The configured local validator id was not present in the validator set.
    #[error("local validator {0} is not a member of the configured validator set")]
    LocalValidatorMissing(ValidatorId),
    /// Two entries in the configured validator set shared the same id.
    #[error("duplicate validator id in cluster configuration: {0}")]
    DuplicateValidator(ValidatorId),
    /// The configured cluster has fewer voting members than required for a quorum.
    #[error("cluster has {actual} voters, but at least {required} are required")]
    InsufficientQuorum {
        /// The number of voters supplied.
        actual: usize,
        /// The minimum number of voters required.
        required: usize,
    },
    /// An admin command was issued that referenced an unknown validator.
    #[error("validator {0} is not a member of the cluster")]
    UnknownValidator(ValidatorId),
    /// `remove_voter` was called with the local node's id. Removing self would partition
    /// the local node from its own cluster view; operators should perform the demotion
    /// from a remote node instead.
    #[error("cannot remove the local validator {0} via remove_voter; demote from a peer")]
    CannotRemoveLocal(ValidatorId),
    /// A membership-changing command was issued with a stale version.
    #[error("membership version mismatch: caller observed {observed}, current is {current}")]
    VersionMismatch {
        /// The version supplied by the caller.
        observed: u64,
        /// The actor's current membership version.
        current: u64,
    },
    /// The underlying consensus driver returned an error.
    #[error(transparent)]
    Driver(#[from] DriverError),
}

/// Errors raised by a [`ConsensusDriver`](crate::ConsensusDriver) implementation.
#[derive(Debug, Error)]
pub enum DriverError {
    /// The driver failed to start.
    #[error("driver failed to start: {0}")]
    Startup(String),
    /// The driver lost contact with a quorum of peers.
    #[error("driver lost quorum")]
    LostQuorum,
    /// The driver exited unexpectedly.
    #[error("driver exited: {0}")]
    Exited(String),
    /// The driver was asked to perform an operation it does not support.
    #[error("driver does not support operation: {0}")]
    Unsupported(&'static str),
}
