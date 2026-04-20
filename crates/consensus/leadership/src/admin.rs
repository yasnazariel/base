//! Admin command surface for the [`LeadershipActor`](crate::LeadershipActor).
//!
//! Each variant carries a oneshot `ack` channel that the actor uses to return its result,
//! giving operators synchronous semantics over an asynchronous transport.

use tokio::sync::{mpsc, oneshot};

use crate::{ClusterMembership, LeaderStatus, LeadershipError, ValidatorEntry, ValidatorId};

/// A single admin command issued to the [`LeadershipActor`](crate::LeadershipActor).
#[derive(Debug)]
pub enum LeadershipCommand {
    /// Returns the current [`LeaderStatus`].
    GetStatus {
        /// Channel on which the actor sends its response.
        ack: oneshot::Sender<LeaderStatus>,
    },
    /// Returns a snapshot of the current [`ClusterMembership`].
    GetMembership {
        /// Channel on which the actor sends its response.
        ack: oneshot::Sender<ClusterMembership>,
    },
    /// Returns the local node's [`ValidatorId`]. Used by external tooling (e.g. basectl)
    /// to map operator-friendly node names back to the consensus-level identifier
    /// without requiring duplicate configuration.
    GetLocalValidatorId {
        /// Channel on which the actor sends its response.
        ack: oneshot::Sender<ValidatorId>,
    },
    /// Initiates a leadership transfer. `Some(id)` targets a specific validator; `None`
    /// lets the elector choose the successor.
    TransferLeadership {
        /// The validator to transfer to, or `None` to let the driver choose.
        to: Option<ValidatorId>,
        /// Channel on which the actor sends its response.
        ack: oneshot::Sender<Result<(), LeadershipError>>,
    },
    /// Adds a voting validator to the cluster. `version` follows op-conductor-style
    /// optimistic concurrency: the command is rejected unless it matches the actor's
    /// current membership version.
    AddVoter {
        /// The validator to add.
        entry: ValidatorEntry,
        /// The cluster-membership version observed by the caller.
        version: u64,
        /// Channel on which the actor sends its response.
        ack: oneshot::Sender<Result<ClusterMembership, LeadershipError>>,
    },
    /// Removes a voting validator from the cluster. `version` is checked as for
    /// [`AddVoter`](Self::AddVoter).
    RemoveVoter {
        /// The id of the validator to remove.
        id: ValidatorId,
        /// The cluster-membership version observed by the caller.
        version: u64,
        /// Channel on which the actor sends its response.
        ack: oneshot::Sender<Result<ClusterMembership, LeadershipError>>,
    },
    /// Forces the local node into the [`LeaderStatus::Leader`] state regardless of
    /// consensus, for disaster recovery when the cluster has lost quorum.
    OverrideLeader {
        /// Whether to enable (`true`) or clear (`false`) the override.
        enabled: bool,
        /// Channel on which the actor sends its response.
        ack: oneshot::Sender<Result<(), LeadershipError>>,
    },
    /// Pauses participation in consensus without leaving the cluster.
    Pause {
        /// Channel on which the actor sends its response.
        ack: oneshot::Sender<Result<(), LeadershipError>>,
    },
    /// Resumes participation in consensus after a [`Pause`](Self::Pause).
    Resume {
        /// Channel on which the actor sends its response.
        ack: oneshot::Sender<Result<(), LeadershipError>>,
    },
}

/// Producer side of the admin-command channel. Held by RPC handlers and CLI tooling.
pub type LeadershipCommandSender = mpsc::Sender<LeadershipCommand>;

/// Consumer side of the admin-command channel. Owned by the
/// [`LeadershipActor`](crate::LeadershipActor).
pub type LeadershipCommandReceiver = mpsc::Receiver<LeadershipCommand>;
