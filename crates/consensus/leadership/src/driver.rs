//! The [`ConsensusDriver`] trait that abstracts the underlying consensus engine.
//!
//! Drivers consume [`DriverRequest`]s and publish [`DriverEvent`]s on a watch channel;
//! everything else (status synthesis, override handling, health-driven step-down) lives
//! in the actor.

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::{ClusterMembership, DriverError, LeadershipConfig, ValidatorEntry, ValidatorId};

/// Events a [`ConsensusDriver`] publishes to the [`LeadershipActor`](crate::LeadershipActor).
///
/// Carried over a [`watch::channel`] so consumers always observe the most recent
/// election state without a queue: under view churn the actor sees the freshest leader
/// rather than the head of a backlog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriverEvent {
    /// The consensus protocol elected `leader` in the given consensus view.
    LeaderElected {
        /// The id of the elected leader.
        leader: ValidatorId,
        /// The consensus view in which the election happened.
        view: u64,
    },
    /// The consensus protocol does not currently have an elected leader (startup, quorum
    /// loss, or a view change in progress).
    NoLeader,
}

/// A request issued by the [`LeadershipActor`](crate::LeadershipActor) to a
/// [`ConsensusDriver`], carrying an `ack` channel so the driver can report success or
/// failure back to the actor synchronously.
#[derive(Debug)]
pub struct DriverRequest {
    /// The operation the actor is requesting.
    pub kind: DriverRequestKind,
    /// Channel on which the driver returns the result of the operation. Drivers must
    /// always send exactly one message; the actor uses a closed channel as evidence of
    /// driver shutdown.
    pub ack: oneshot::Sender<Result<(), DriverError>>,
}

/// The operation requested in a [`DriverRequest`].
///
/// Derived from [`LeadershipCommand`](crate::LeadershipCommand)s after the actor has done
/// its part of the handling (e.g. version-checking, override application).
#[derive(Debug)]
pub enum DriverRequestKind {
    /// Initiate a leadership transfer. `Some(id)` targets a specific validator; `None`
    /// asks the driver to step down so the elector advances to the next leader.
    TransferLeadership(Option<ValidatorId>),
    /// Add a voting validator to the cluster.
    AddVoter(ValidatorEntry),
    /// Remove a voting validator from the cluster.
    RemoveVoter(ValidatorId),
    /// Pause participation in consensus.
    Pause,
    /// Resume participation in consensus.
    Resume,
}

/// The runtime context handed to a [`ConsensusDriver`] at startup.
#[derive(Debug)]
pub struct DriverContext {
    /// Static configuration loaded at node startup.
    pub config: LeadershipConfig,
    /// Initial cluster membership snapshot, derived from `config.validators`.
    pub membership: ClusterMembership,
    /// Watch channel on which the driver publishes the freshest [`DriverEvent`].
    pub events_tx: watch::Sender<DriverEvent>,
    /// Channel on which the driver receives requests from the actor.
    pub requests_rx: mpsc::Receiver<DriverRequest>,
    /// Cancellation token signalling driver shutdown.
    pub cancel: CancellationToken,
}

/// The trait every consensus engine must implement to plug into the
/// [`LeadershipActor`](crate::LeadershipActor).
#[async_trait]
pub trait ConsensusDriver: Send + 'static {
    /// Runs the driver until cancelled or until an unrecoverable error occurs.
    async fn run(self: Box<Self>, ctx: DriverContext) -> Result<(), DriverError>;
}
