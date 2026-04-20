//! Deterministic in-process [`ConsensusDriver`] used for tests and the spike harness.
//!
//! Elects leaders by walking the validator set in id order, advancing on each
//! [`DriverRequest::TransferLeadership`]. Does not model partitions, message loss, or
//! Byzantine behaviour — those concerns belong to a real consensus engine.

use async_trait::async_trait;
use tokio::sync::watch;
use tracing::debug;

use crate::{
    ClusterMembership, ConsensusDriver, DriverContext, DriverError, DriverEvent, DriverRequest,
    DriverRequestKind, ValidatorEntry, ValidatorId,
};

/// A deterministic, in-process [`ConsensusDriver`] for testing.
#[derive(Clone, Debug, Default)]
pub struct MockDriver;

impl MockDriver {
    /// Constructs a new [`MockDriver`].
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ConsensusDriver for MockDriver {
    async fn run(self: Box<Self>, mut ctx: DriverContext) -> Result<(), DriverError> {
        let mut state = MockState::new(ctx.membership.clone());
        state.publish(&ctx.events_tx);

        loop {
            tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => {
                    debug!(target: "leadership::mock", "cancellation received");
                    return Ok(());
                }
                Some(request) = ctx.requests_rx.recv() => {
                    let DriverRequest { kind, ack } = request;
                    let result = state.apply(kind);
                    let _ = ack.send(result);
                    state.publish(&ctx.events_tx);
                }
                else => {
                    debug!(target: "leadership::mock", "request channel closed");
                    return Ok(());
                }
            }
        }
    }
}

/// Internal state for the mock driver.
#[derive(Clone, Debug)]
pub struct MockState {
    membership: ClusterMembership,
    /// Index into `membership.voters` of the current leader, or `None` if the cluster is
    /// empty (in which case `NoLeader` is published).
    leader_idx: Option<usize>,
    /// Monotonically increasing view number, bumped on every leader change.
    view: u64,
    /// Whether the driver is currently paused.
    paused: bool,
}

impl MockState {
    fn new(membership: ClusterMembership) -> Self {
        let leader_idx = (!membership.is_empty()).then_some(0);
        Self { membership, leader_idx, view: 0, paused: false }
    }

    fn apply(&mut self, kind: DriverRequestKind) -> Result<(), DriverError> {
        match kind {
            DriverRequestKind::TransferLeadership(to) => self.transfer_leadership(to),
            DriverRequestKind::AddVoter(entry) => self.add_voter(entry),
            DriverRequestKind::RemoveVoter(id) => self.remove_voter(&id),
            DriverRequestKind::Pause => self.pause(),
            DriverRequestKind::Resume => self.resume(),
        }
    }

    fn transfer_leadership(&mut self, to: Option<ValidatorId>) -> Result<(), DriverError> {
        if self.membership.is_empty() {
            return Err(DriverError::Unsupported("transfer_leadership on empty cluster"));
        }
        let next_idx = match to {
            Some(target) => self
                .membership
                .voters
                .iter()
                .position(|v| v.id == target)
                .ok_or(DriverError::Unsupported("transfer_leadership target not in cluster"))?,
            None => {
                // Round-robin: advance to the next voter in id order, wrapping.
                let current = self.leader_idx.unwrap_or(0);
                (current + 1) % self.membership.voters.len()
            }
        };
        self.leader_idx = Some(next_idx);
        self.view = self.view.saturating_add(1);
        Ok(())
    }

    fn add_voter(&mut self, entry: ValidatorEntry) -> Result<(), DriverError> {
        if self.membership.voters.iter().any(|v| v.id == entry.id) {
            return Err(DriverError::Unsupported("add_voter for already-present validator"));
        }
        let mut voters = self.membership.voters.clone();
        voters.push(entry);
        let new_version = self.membership.version.saturating_add(1);
        self.membership = ClusterMembership::new(voters, new_version);
        if self.leader_idx.is_none() {
            self.leader_idx = Some(0);
            self.view = self.view.saturating_add(1);
        }
        Ok(())
    }

    fn remove_voter(&mut self, id: &ValidatorId) -> Result<(), DriverError> {
        let pos = self
            .membership
            .voters
            .iter()
            .position(|v| &v.id == id)
            .ok_or(DriverError::Unsupported("remove_voter for unknown validator"))?;

        let mut voters = self.membership.voters.clone();
        voters.remove(pos);
        let new_version = self.membership.version.saturating_add(1);
        self.membership = ClusterMembership::new(voters, new_version);

        match self.leader_idx {
            Some(current) if current == pos => {
                self.leader_idx =
                    (!self.membership.is_empty()).then(|| current % self.membership.voters.len());
                self.view = self.view.saturating_add(1);
            }
            Some(current) if current > pos => {
                self.leader_idx = Some(current - 1);
            }
            _ => {}
        }
        if self.membership.is_empty() {
            self.leader_idx = None;
        }
        Ok(())
    }

    const fn pause(&mut self) -> Result<(), DriverError> {
        if !self.paused {
            self.paused = true;
            self.view = self.view.saturating_add(1);
        }
        Ok(())
    }

    const fn resume(&mut self) -> Result<(), DriverError> {
        self.paused = false;
        Ok(())
    }

    fn current_event(&self) -> DriverEvent {
        self.leader_idx.map_or(DriverEvent::NoLeader, |i| DriverEvent::LeaderElected {
            leader: self.membership.voters[i].id.clone(),
            view: self.view,
        })
    }

    fn publish(&self, tx: &watch::Sender<DriverEvent>) {
        let _ = tx.send(self.current_event());
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr}; // SocketAddr still needed for TransportConfig

    use tokio::sync::{mpsc, oneshot};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{HealthThresholds, LeadershipConfig, RaftTimeouts, TransportConfig};

    fn entry(id: &str) -> ValidatorEntry {
        ValidatorEntry { id: ValidatorId::new(id), addr: "127.0.0.1:50050".to_owned() }
    }

    fn config(local: &str, voters: Vec<ValidatorEntry>) -> LeadershipConfig {
        LeadershipConfig {
            local_id: ValidatorId::new(local),
            validators: voters,
            transport: TransportConfig {
                listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50050),
            },
            health: HealthThresholds::default(),
            timeouts: RaftTimeouts::default(),
        }
    }

    fn ctx(
        config: LeadershipConfig,
    ) -> (DriverContext, mpsc::Sender<DriverRequest>, watch::Receiver<DriverEvent>, CancellationToken)
    {
        let membership = config.validate().unwrap();
        let (events_tx, events_rx) = watch::channel(DriverEvent::NoLeader);
        let (requests_tx, requests_rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        (
            DriverContext { config, membership, events_tx, requests_rx, cancel: cancel.clone() },
            requests_tx,
            events_rx,
            cancel,
        )
    }

    async fn send_and_wait(
        requests_tx: &mpsc::Sender<DriverRequest>,
        kind: DriverRequestKind,
    ) -> Result<(), DriverError> {
        let (ack, rx) = oneshot::channel();
        requests_tx.send(DriverRequest { kind, ack }).await.unwrap();
        rx.await.unwrap()
    }

    async fn await_leader(rx: &mut watch::Receiver<DriverEvent>, expected: ValidatorId, view: u64) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if matches!(&*rx.borrow(), DriverEvent::LeaderElected { leader, view: v } if leader == &expected && *v == view)
            {
                return;
            }
            tokio::select! {
                _ = rx.changed() => continue,
                _ = tokio::time::sleep_until(deadline) => {
                    panic!("timed out waiting for leader {expected} view={view}; current = {:?}", rx.borrow());
                }
            }
        }
    }

    #[tokio::test]
    async fn emits_initial_leader_on_startup() {
        let cfg = config("seq-1", vec![entry("seq-1"), entry("seq-2"), entry("seq-3")]);
        let (driver_ctx, _requests_tx, mut events_rx, cancel) = ctx(cfg);

        let driver = Box::new(MockDriver::new());
        let handle = tokio::spawn(driver.run(driver_ctx));

        await_leader(&mut events_rx, ValidatorId::new("seq-1"), 0).await;

        cancel.cancel();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn round_robin_transfer_advances_to_next_voter() {
        let cfg = config("seq-1", vec![entry("seq-1"), entry("seq-2"), entry("seq-3")]);
        let (driver_ctx, requests_tx, mut events_rx, cancel) = ctx(cfg);

        let driver = Box::new(MockDriver::new());
        let handle = tokio::spawn(driver.run(driver_ctx));

        await_leader(&mut events_rx, ValidatorId::new("seq-1"), 0).await;
        send_and_wait(&requests_tx, DriverRequestKind::TransferLeadership(None)).await.unwrap();
        await_leader(&mut events_rx, ValidatorId::new("seq-2"), 1).await;
        send_and_wait(&requests_tx, DriverRequestKind::TransferLeadership(None)).await.unwrap();
        await_leader(&mut events_rx, ValidatorId::new("seq-3"), 2).await;
        // Wraps back to the first voter.
        send_and_wait(&requests_tx, DriverRequestKind::TransferLeadership(None)).await.unwrap();
        await_leader(&mut events_rx, ValidatorId::new("seq-1"), 3).await;

        cancel.cancel();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn explicit_transfer_target_jumps_to_named_validator() {
        let cfg = config("seq-1", vec![entry("seq-1"), entry("seq-2"), entry("seq-3")]);
        let (driver_ctx, requests_tx, mut events_rx, cancel) = ctx(cfg);

        let driver = Box::new(MockDriver::new());
        let handle = tokio::spawn(driver.run(driver_ctx));

        await_leader(&mut events_rx, ValidatorId::new("seq-1"), 0).await;
        send_and_wait(
            &requests_tx,
            DriverRequestKind::TransferLeadership(Some(ValidatorId::new("seq-3"))),
        )
        .await
        .unwrap();
        await_leader(&mut events_rx, ValidatorId::new("seq-3"), 1).await;

        cancel.cancel();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cancellation_returns_promptly() {
        let cfg = config("seq-1", vec![entry("seq-1")]);
        let (driver_ctx, _requests_tx, _events_rx, cancel) = ctx(cfg);

        let driver = Box::new(MockDriver::new());
        let handle = tokio::spawn(driver.run(driver_ctx));

        cancel.cancel();
        handle.await.unwrap().unwrap();
    }
}
