//! Adapter that lets [`base_consensus_leadership::LeadershipActor`] implement the
//! supervisor's [`NodeActor`] trait without creating a circular dependency.

use std::fmt;

use async_trait::async_trait;
use base_consensus_leadership::{ConsensusDriver, LeadershipActor, LeadershipError};

use crate::NodeActor;

/// Newtype that adapts a [`LeadershipActor`] and its [`ConsensusDriver`] to [`NodeActor`].
pub struct LeadershipNodeActor {
    /// The leadership actor.
    pub actor: LeadershipActor,
    /// The consensus driver implementation to spawn alongside the actor.
    pub driver: Box<dyn ConsensusDriver>,
}

// Manual `Debug` impl because [`ConsensusDriver`] is a trait object without a `Debug`
// bound; adding one would force every driver implementation to carry it.
impl fmt::Debug for LeadershipNodeActor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeadershipNodeActor")
            .field("actor", &self.actor)
            .field("driver", &"<dyn ConsensusDriver>")
            .finish()
    }
}

#[async_trait]
impl NodeActor for LeadershipNodeActor {
    type Error = LeadershipError;
    type StartData = ();

    async fn start(self, _: Self::StartData) -> Result<(), Self::Error> {
        self.actor.start(self.driver).await
    }
}
