//! JSON-RPC server implementation for the leadership namespace.
//!
//! Bridges operator-facing RPC calls to [`LeadershipCommand`]s on the
//! [`LeadershipActor`](base_consensus_leadership::LeadershipActor).
//! Each method dispatches a command and awaits the ack, giving operators
//! synchronous semantics over an async internal channel.

use async_trait::async_trait;
use base_consensus_leadership::{
    ClusterMembership, LeaderStatus, LeadershipCommand, LeadershipCommandSender, LeadershipError,
    ValidatorEntry, ValidatorId,
};
use jsonrpsee::{
    core::RpcResult,
    types::{ErrorCode, ErrorObject},
};
use tokio::sync::oneshot;

use crate::LeadershipApiServer;

/// The leadership rpc server.
#[derive(Debug)]
pub struct LeadershipRpc {
    /// Sender used to dispatch [`LeadershipCommand`]s to the
    /// [`LeadershipActor`](base_consensus_leadership::LeadershipActor). `None` on nodes
    /// where the actor is not spawned (e.g. External mode).
    pub command_sender: Option<LeadershipCommandSender>,
}

impl LeadershipRpc {
    /// Constructs a new [`LeadershipRpc`] given an optional [`LeadershipCommandSender`].
    pub const fn new(command_sender: Option<LeadershipCommandSender>) -> Self {
        Self { command_sender }
    }

    /// Returns the inner sender, or the standard "leadership not enabled" error.
    fn sender(&self) -> Result<&LeadershipCommandSender, ErrorObject<'static>> {
        self.command_sender.as_ref().ok_or_else(LeadershipErrorCode::unavailable)
    }

    /// Sends `command` and awaits its ack, mapping a closed channel to an internal error.
    async fn dispatch<T>(
        sender: &LeadershipCommandSender,
        command: LeadershipCommand,
        rx: oneshot::Receiver<T>,
    ) -> RpcResult<T> {
        sender.send(command).await.map_err(|_| ErrorObject::from(ErrorCode::InternalError))?;
        rx.await.map_err(|_| ErrorObject::from(ErrorCode::InternalError))
    }

    /// Like [`Self::dispatch`], but unwraps the inner `Result<T, LeadershipError>`.
    async fn dispatch_fallible<T>(
        sender: &LeadershipCommandSender,
        command: LeadershipCommand,
        rx: oneshot::Receiver<Result<T, LeadershipError>>,
    ) -> RpcResult<T> {
        let result = Self::dispatch(sender, command, rx).await?;
        result.map_err(LeadershipErrorCode::from_command_failure)
    }
}

/// Application-specific JSON-RPC error codes for the `leadership` namespace.
///
/// Codes follow the JSON-RPC 2.0 reservation: server-defined errors live in the
/// `[-32099, -32000]` range. Downstream tooling can match on the integer to
/// programmatically distinguish leadership-disabled from command-failed. Marked
/// `#[non_exhaustive]` so adding a new variant is not a breaking change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
#[non_exhaustive]
pub enum LeadershipErrorCode {
    /// The leadership actor is not running on this node (External mode, or sequencer-only
    /// nodes where the embedded path is disabled).
    NotEnabled = -32001,
    /// A leadership command was dispatched but failed inside the actor (driver error,
    /// version mismatch, unknown validator, etc.). The accompanying message carries the
    /// underlying [`LeadershipError`] description.
    CommandFailed = -32002,
}

impl LeadershipErrorCode {
    /// Returns the JSON-RPC integer code.
    pub const fn code(self) -> i32 {
        self as i32
    }

    /// Builds the standard "leadership not enabled" RPC error object.
    pub fn unavailable() -> ErrorObject<'static> {
        ErrorObject::owned(
            Self::NotEnabled.code(),
            "leadership not enabled on this node",
            None::<()>,
        )
    }

    /// Maps a [`LeadershipError`] into a JSON-RPC error object, preserving the message so
    /// operators can distinguish failure modes without inspecting logs.
    ///
    /// Client-induced failures (stale `VersionMismatch`, `DuplicateValidator`,
    /// `UnknownValidator`, `CannotRemoveLocal`, `LocalValidatorMissing`,
    /// `InsufficientQuorum`) are logged at `warn`. Driver-side faults are escalated to
    /// `error` because they indicate the actor or transport is in a bad state.
    pub fn from_command_failure(err: LeadershipError) -> ErrorObject<'static> {
        if matches!(
            err,
            LeadershipError::VersionMismatch { .. }
                | LeadershipError::DuplicateValidator(_)
                | LeadershipError::UnknownValidator(_)
                | LeadershipError::CannotRemoveLocal(_)
                | LeadershipError::LocalValidatorMissing(_)
                | LeadershipError::InsufficientQuorum { .. }
        ) {
            warn!(error = %err, "leadership command rejected");
        } else {
            error!(error = %err, "leadership command failed");
        }
        ErrorObject::owned(Self::CommandFailed.code(), err.to_string(), None::<()>)
    }
}

#[async_trait]
impl LeadershipApiServer for LeadershipRpc {
    async fn leadership_status(&self) -> RpcResult<LeaderStatus> {
        let sender = self.sender()?;
        let (ack, rx) = oneshot::channel();
        Self::dispatch(sender, LeadershipCommand::GetStatus { ack }, rx).await
    }

    async fn leadership_membership(&self) -> RpcResult<ClusterMembership> {
        let sender = self.sender()?;
        let (ack, rx) = oneshot::channel();
        Self::dispatch(sender, LeadershipCommand::GetMembership { ack }, rx).await
    }

    async fn leadership_validator_id(&self) -> RpcResult<ValidatorId> {
        let sender = self.sender()?;
        let (ack, rx) = oneshot::channel();
        Self::dispatch(sender, LeadershipCommand::GetLocalValidatorId { ack }, rx).await
    }

    async fn leadership_transfer_leadership(&self, to: Option<ValidatorId>) -> RpcResult<()> {
        let sender = self.sender()?;
        let (ack, rx) = oneshot::channel();
        Self::dispatch_fallible(sender, LeadershipCommand::TransferLeadership { to, ack }, rx).await
    }

    async fn leadership_add_voter(
        &self,
        entry: ValidatorEntry,
        version: u64,
    ) -> RpcResult<ClusterMembership> {
        let sender = self.sender()?;
        let (ack, rx) = oneshot::channel();
        Self::dispatch_fallible(sender, LeadershipCommand::AddVoter { entry, version, ack }, rx)
            .await
    }

    async fn leadership_remove_voter(
        &self,
        id: ValidatorId,
        version: u64,
    ) -> RpcResult<ClusterMembership> {
        let sender = self.sender()?;
        let (ack, rx) = oneshot::channel();
        Self::dispatch_fallible(sender, LeadershipCommand::RemoveVoter { id, version, ack }, rx)
            .await
    }

    async fn leadership_override_leader(&self, enabled: bool) -> RpcResult<()> {
        let sender = self.sender()?;
        let (ack, rx) = oneshot::channel();
        Self::dispatch_fallible(sender, LeadershipCommand::OverrideLeader { enabled, ack }, rx)
            .await
    }

    async fn leadership_pause(&self) -> RpcResult<()> {
        let sender = self.sender()?;
        let (ack, rx) = oneshot::channel();
        Self::dispatch_fallible(sender, LeadershipCommand::Pause { ack }, rx).await
    }

    async fn leadership_resume(&self) -> RpcResult<()> {
        let sender = self.sender()?;
        let (ack, rx) = oneshot::channel();
        Self::dispatch_fallible(sender, LeadershipCommand::Resume { ack }, rx).await
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use jsonrpsee::types::ErrorCode;
    use rstest::rstest;
    use tokio::sync::mpsc;

    use super::*;

    fn entry(id: &str) -> ValidatorEntry {
        ValidatorEntry {
            id: ValidatorId::new(id),
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50050).to_string(),
        }
    }

    /// Spawns a fake actor task that drains commands from `rx` and replies via `responder`.
    ///
    /// The responder owns full control over how each command is acked, so individual tests
    /// can simulate happy-path replies, error replies, or a closed actor (dropped receiver).
    fn spawn_fake_actor<F>(mut rx: mpsc::Receiver<LeadershipCommand>, mut responder: F)
    where
        F: FnMut(LeadershipCommand) + Send + 'static,
    {
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                responder(cmd);
            }
        });
    }

    #[tokio::test]
    async fn status_passes_actor_response_through() {
        let (tx, rx) = mpsc::channel(8);
        spawn_fake_actor(rx, |cmd| match cmd {
            LeadershipCommand::GetStatus { ack } => {
                let _ = ack.send(LeaderStatus::Leader {
                    view: 42,
                    overridden: false,
                    override_generation: 0,
                });
            }
            other => panic!("unexpected command: {other:?}"),
        });

        let rpc = LeadershipRpc::new(Some(tx));
        let status = rpc.leadership_status().await.expect("status");
        assert_eq!(
            status,
            LeaderStatus::Leader { view: 42, overridden: false, override_generation: 0 }
        );
    }

    #[tokio::test]
    async fn membership_passes_actor_response_through() {
        let (tx, rx) = mpsc::channel(8);
        let snapshot = ClusterMembership::new(vec![entry("seq-a"), entry("seq-b")], 3);
        let snapshot_clone = snapshot.clone();
        spawn_fake_actor(rx, move |cmd| match cmd {
            LeadershipCommand::GetMembership { ack } => {
                let _ = ack.send(snapshot_clone.clone());
            }
            other => panic!("unexpected command: {other:?}"),
        });

        let rpc = LeadershipRpc::new(Some(tx));
        let got = rpc.leadership_membership().await.expect("membership");
        assert_eq!(got, snapshot);
    }

    #[tokio::test]
    async fn add_voter_returns_updated_membership() {
        let (tx, rx) = mpsc::channel(8);
        let updated = ClusterMembership::new(vec![entry("seq-a"), entry("seq-b")], 8);
        let updated_clone = updated.clone();
        spawn_fake_actor(rx, move |cmd| match cmd {
            LeadershipCommand::AddVoter { entry: e, version, ack } => {
                assert_eq!(e.id, ValidatorId::new("seq-b"));
                assert_eq!(version, 7);
                let _ = ack.send(Ok(updated_clone.clone()));
            }
            other => panic!("unexpected command: {other:?}"),
        });

        let rpc = LeadershipRpc::new(Some(tx));
        let got = rpc.leadership_add_voter(entry("seq-b"), 7).await.expect("add voter");
        assert_eq!(got, updated);
    }

    #[tokio::test]
    async fn add_voter_propagates_leadership_error_as_rpc_error() {
        let (tx, rx) = mpsc::channel(8);
        spawn_fake_actor(rx, |cmd| match cmd {
            LeadershipCommand::AddVoter { ack, .. } => {
                let _ =
                    ack.send(Err(LeadershipError::DuplicateValidator(ValidatorId::new("seq-b"))));
            }
            other => panic!("unexpected command: {other:?}"),
        });

        let rpc = LeadershipRpc::new(Some(tx));
        let err =
            rpc.leadership_add_voter(entry("seq-b"), 4).await.expect_err("expected rpc error");
        assert_eq!(err.code(), LeadershipErrorCode::CommandFailed.code());
        assert!(err.message().contains("seq-b"));
    }

    #[rstest]
    #[tokio::test]
    async fn every_method_returns_unavailable_when_sender_is_none() {
        let rpc = LeadershipRpc::new(None);

        let err = rpc.leadership_status().await.expect_err("status");
        assert_eq!(err.code(), LeadershipErrorCode::NotEnabled.code());

        let err = rpc.leadership_membership().await.expect_err("membership");
        assert_eq!(err.code(), LeadershipErrorCode::NotEnabled.code());

        let err = rpc.leadership_validator_id().await.expect_err("validator_id");
        assert_eq!(err.code(), LeadershipErrorCode::NotEnabled.code());

        let err = rpc.leadership_transfer_leadership(None).await.expect_err("transfer");
        assert_eq!(err.code(), LeadershipErrorCode::NotEnabled.code());

        let err = rpc.leadership_add_voter(entry("seq-a"), 0).await.expect_err("add");
        assert_eq!(err.code(), LeadershipErrorCode::NotEnabled.code());

        let err =
            rpc.leadership_remove_voter(ValidatorId::new("seq-a"), 0).await.expect_err("remove");
        assert_eq!(err.code(), LeadershipErrorCode::NotEnabled.code());

        let err = rpc.leadership_override_leader(true).await.expect_err("override");
        assert_eq!(err.code(), LeadershipErrorCode::NotEnabled.code());

        let err = rpc.leadership_pause().await.expect_err("pause");
        assert_eq!(err.code(), LeadershipErrorCode::NotEnabled.code());

        let err = rpc.leadership_resume().await.expect_err("resume");
        assert_eq!(err.code(), LeadershipErrorCode::NotEnabled.code());
    }

    #[tokio::test]
    async fn dropped_actor_ack_yields_internal_error() {
        let (tx, rx) = mpsc::channel(8);
        // Responder drops the ack without sending, simulating an actor that died mid-handle.
        spawn_fake_actor(rx, |cmd| {
            drop(cmd);
        });

        let rpc = LeadershipRpc::new(Some(tx));
        let err = rpc.leadership_status().await.expect_err("expected internal error");
        assert_eq!(err.code(), ErrorCode::InternalError.code());
    }
}
