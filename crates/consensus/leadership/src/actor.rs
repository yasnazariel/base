//! The [`LeadershipActor`].
//!
//! Owns the channels connecting the consensus driver, health aggregator, and admin command
//! surface, and publishes [`LeaderStatus`] by combining the latest [`DriverEvent`], the
//! leader override flag, and the local validator id.

use std::time::Instant;

use tokio::{
    select,
    sync::{mpsc, oneshot, watch},
    time::interval,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::{
    ClusterMembership, ConsensusDriver, DriverContext, DriverError, DriverEvent, DriverRequest,
    DriverRequestKind, HealthAggregator, HealthFailure, HealthSignals, HealthVerdict, LeaderStatus,
    LeaderStatusReceiver, LeaderStatusSender, LeadershipCommand, LeadershipCommandReceiver,
    LeadershipCommandSender, LeadershipConfig, LeadershipError, ValidatorEntry, ValidatorId,
};

/// Channel size for inter-actor command and request channels.
pub const CHANNEL_CAPACITY: usize = 256;

/// External signal updates that drive the [`HealthAggregator`].
///
/// The actor mutates its internal [`HealthSignals`] snapshot on each inbound update, then
/// evaluates against the configured thresholds on a fixed cadence.
#[derive(Clone, Copy, Debug)]
pub enum HealthSignalUpdate {
    /// The local node observed an unsafe head at the given wall-clock time.
    UnsafeHeadObserved(Instant),
    /// The local node observed an L1 head at the given wall-clock time.
    L1HeadObserved(Instant),
    /// The execution layer reported its sync state.
    ElSyncState(bool),
    /// The network layer reported its current peer count.
    PeerCount(usize),
}

/// External handles produced by [`LeadershipActor::build`].
#[derive(Debug)]
pub struct LeadershipHandles {
    /// Producer side of the admin command channel.
    pub commands_tx: LeadershipCommandSender,
    /// Consumer side of the leader status channel.
    pub leader_status_rx: LeaderStatusReceiver,
    /// Consumer side of the health verdict channel, exposed so observers can react to
    /// health transitions without polling the actor.
    pub health_verdict_rx: watch::Receiver<HealthVerdict>,
    /// Producer side of the health signals channel, held by external signal sources.
    pub health_signals_tx: mpsc::Sender<HealthSignalUpdate>,
}

/// The leadership actor.
///
/// Construct an actor and its public handles via [`LeadershipActor::build`], then drive
/// it with the [`crate::ConsensusDriver`] of choice via [`LeadershipActor::start`].
///
/// All fields are owned by the actor's main loop and never escape — callers mutate the
/// actor only through [`LeadershipCommand`] so invariants (override-generation bumps
/// with override toggles, membership-version bumps with membership changes, status
/// republished after every state mutation) live in one place.
#[derive(Debug)]
pub struct LeadershipActor {
    /// The id of the local validator.
    pub local_id: ValidatorId,
    /// The current cluster membership snapshot.
    pub membership: ClusterMembership,
    /// Whether the leader override is currently active.
    pub override_active: bool,
    /// Monotonically increasing override generation, bumped each time the override is
    /// (re-)enabled. Surfaced on [`LeaderStatus::Leader`] for partition-time fencing.
    pub override_generation: u64,
    /// The most recently observed [`DriverEvent`].
    pub last_event: DriverEvent,
    /// Health-verdict evaluator.
    pub health: HealthAggregator,
    /// Latest snapshot of inbound health signals.
    pub health_signals: HealthSignals,
    /// Producer side of the [`LeaderStatus`] watch channel.
    pub leader_status_tx: LeaderStatusSender,
    /// Producer side of the driver request channel.
    pub driver_requests_tx: mpsc::Sender<DriverRequest>,
    /// Consumer side of the admin command channel.
    pub commands_rx: LeadershipCommandReceiver,
    /// Consumer side of the driver event watch channel.
    pub driver_events_rx: watch::Receiver<DriverEvent>,
    /// Consumer side of the health signals channel.
    pub health_signals_rx: mpsc::Receiver<HealthSignalUpdate>,
    /// Static configuration handed to the driver at start time.
    pub config: LeadershipConfig,
    /// Producer side of the driver event channel, handed to the driver at start time.
    pub driver_events_tx: watch::Sender<DriverEvent>,
    /// Consumer side of the driver request channel, handed to the driver at start time.
    pub driver_requests_rx: mpsc::Receiver<DriverRequest>,
    /// Cancellation token shared with the spawned driver.
    pub cancel: CancellationToken,
}

impl LeadershipActor {
    /// Constructs a new [`LeadershipActor`] from the supplied configuration.
    ///
    /// Returns the actor together with the [`LeadershipHandles`] external code uses to
    /// communicate with it. The actor is not yet running; call [`LeadershipActor::start`]
    /// to drive it (the [`crate::ConsensusDriver`] is supplied at start time so callers
    /// can choose between mock and real drivers without re-validating configuration).
    pub fn build(
        config: LeadershipConfig,
        cancel: CancellationToken,
    ) -> Result<(Self, LeadershipHandles), LeadershipError> {
        let membership = config.validate()?;
        let local_id = config.local_id.clone();

        let (leader_status_tx, leader_status_rx) = watch::channel(LeaderStatus::Unknown);
        let (commands_tx, commands_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (driver_events_tx, driver_events_rx) = watch::channel(DriverEvent::NoLeader);
        let (driver_requests_tx, driver_requests_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (health_signals_tx, health_signals_rx) = mpsc::channel(CHANNEL_CAPACITY);

        let (health, health_verdict_rx) = HealthAggregator::new(config.health.clone());

        let actor = Self {
            local_id,
            membership,
            override_active: false,
            override_generation: 0,
            last_event: DriverEvent::NoLeader,
            health,
            health_signals: HealthSignals::default(),
            leader_status_tx,
            driver_requests_tx,
            commands_rx,
            driver_events_rx,
            health_signals_rx,
            config,
            driver_events_tx,
            driver_requests_rx,
            cancel,
        };

        let handles = LeadershipHandles {
            commands_tx,
            leader_status_rx,
            health_verdict_rx,
            health_signals_tx,
        };

        Ok((actor, handles))
    }

    /// Runs the actor until cancelled.
    ///
    /// The supplied [`ConsensusDriver`] is spawned on the current tokio runtime and runs
    /// concurrently with the actor's main loop. When the cancellation token fires, both
    /// the actor and the driver shut down.
    pub async fn start(self, driver: Box<dyn ConsensusDriver>) -> Result<(), LeadershipError> {
        // Destructure once so the inbound receivers stay as stack locals — that way the
        // `select!` loop can mutably borrow them in the same arm where it mutably
        // borrows the running-actor state, avoiding a split-borrow conflict.
        let Self {
            local_id,
            membership,
            override_active,
            override_generation,
            last_event,
            health,
            health_signals,
            leader_status_tx,
            driver_requests_tx,
            mut commands_rx,
            mut driver_events_rx,
            mut health_signals_rx,
            config,
            driver_events_tx,
            driver_requests_rx,
            cancel,
        } = self;

        let driver_ctx = DriverContext {
            config,
            membership: membership.clone(),
            events_tx: driver_events_tx,
            requests_rx: driver_requests_rx,
            cancel: cancel.clone(),
        };
        let cancel_for_driver = cancel.clone();
        let driver_handle = tokio::spawn(async move {
            // Cancel the shared token whether the driver exits with Err *or* Ok. Without
            // this, a clean Ok(()) return (e.g. graceful internal shutdown, request channel
            // closed) would leave the actor's `select!` loop running with a permanently-
            // pending `driver_events_rx.changed()` arm — silent wedge, no cancellation
            // signal observed. Cancelling unconditionally routes both exits through the
            // actor's `cancel.cancelled()` arm so shutdown is one well-defined path.
            match driver.run(driver_ctx).await {
                Ok(()) => info!(target: "leadership", "consensus driver exited"),
                Err(error) => error!(
                    target: "leadership",
                    error = %error,
                    "consensus driver exited with error",
                ),
            }
            cancel_for_driver.cancel();
        });

        let mut state = RunningActor {
            local_id,
            membership,
            override_active,
            override_generation,
            last_event,
            leader_since: None,
            health,
            health_signals,
            leader_status_tx,
            driver_requests_tx,
            step_down_in_flight: false,
        };
        let mut health_ticker = interval(state.health.poll_interval());

        loop {
            select! {
                biased;
                _ = cancel.cancelled() => {
                    info!(target: "leadership", "cancellation received, shutting down");
                    let _ = driver_handle.await;
                    return Ok(());
                }
                Some(command) = commands_rx.recv() => {
                    state.handle_command(command).await;
                }
                Ok(()) = driver_events_rx.changed() => {
                    let event = driver_events_rx.borrow_and_update().clone();
                    state.observe_driver_event(event);
                }
                Some(update) = health_signals_rx.recv() => {
                    state.apply_health_update(update);
                }
                _ = health_ticker.tick() => {
                    state.evaluate_health().await;
                }
            }
        }
    }
}

/// Owned scratch state used by [`LeadershipActor::start`]'s main loop.
///
/// Holds the mutable actor fields plus the outbound channel handles. Constructed only
/// by the actor's `start` after it consumes the [`LeadershipActor`], so external
/// callers cannot reach in to mutate state directly — the only path is via
/// [`LeadershipCommand`] over `commands_tx`.
#[derive(Debug)]
pub struct RunningActor {
    /// The id of the local validator.
    pub local_id: ValidatorId,
    /// The current cluster membership snapshot.
    pub membership: ClusterMembership,
    /// Whether the leader override is currently active.
    pub override_active: bool,
    /// Monotonically increasing override generation, bumped on every override toggle on.
    pub override_generation: u64,
    /// The most recently observed [`DriverEvent`].
    pub last_event: DriverEvent,
    /// Instant of the most recent local-leader transition, or [`None`] when not leading.
    /// Maintained by [`Self::observe_driver_event`]; consumed by [`Self::is_active`].
    pub leader_since: Option<Instant>,
    /// Health-verdict evaluator.
    pub health: HealthAggregator,
    /// Latest snapshot of inbound health signals.
    pub health_signals: HealthSignals,
    /// Producer side of the [`LeaderStatus`] watch channel.
    pub leader_status_tx: LeaderStatusSender,
    /// Producer side of the driver request channel.
    pub driver_requests_tx: mpsc::Sender<DriverRequest>,
    /// Set after [`Self::evaluate_health`] dispatches a voluntary `TransferLeadership(None)`,
    /// cleared when the local leadership state changes (lost leadership or new term).
    /// Prevents the periodic health tick from re-dispatching step-down on every
    /// `poll_interval` while a previous step-down is still propagating through the driver.
    pub step_down_in_flight: bool,
}

impl RunningActor {
    /// Handles a single admin command.
    pub async fn handle_command(&mut self, command: LeadershipCommand) {
        match command {
            LeadershipCommand::GetStatus { ack } => {
                let _ = ack.send(self.current_status());
            }
            LeadershipCommand::GetMembership { ack } => {
                let _ = ack.send(self.membership.clone());
            }
            LeadershipCommand::GetLocalValidatorId { ack } => {
                let _ = ack.send(self.local_id.clone());
            }
            LeadershipCommand::TransferLeadership { to, ack } => {
                let _ = ack.send(self.dispatch(DriverRequestKind::TransferLeadership(to)).await);
            }
            LeadershipCommand::AddVoter { entry, version, ack } => {
                let _ = ack.send(self.add_voter(entry, version).await);
            }
            LeadershipCommand::RemoveVoter { id, version, ack } => {
                let _ = ack.send(self.remove_voter(id, version).await);
            }
            LeadershipCommand::OverrideLeader { enabled, ack } => {
                if self.override_active != enabled {
                    if enabled {
                        self.override_generation = self.override_generation.saturating_add(1);
                    }
                    warn!(
                        target: "leadership",
                        enabled,
                        generation = self.override_generation,
                        "leader override toggled — bypassing consensus until cleared",
                    );
                    self.override_active = enabled;
                    self.publish_status();
                }
                let _ = ack.send(Ok(()));
            }
            LeadershipCommand::Pause { ack } => {
                let _ = ack.send(self.dispatch(DriverRequestKind::Pause).await);
            }
            LeadershipCommand::Resume { ack } => {
                let _ = ack.send(self.dispatch(DriverRequestKind::Resume).await);
            }
        }
    }

    /// Records a driver event, updates [`Self::leader_since`] on local-leader
    /// transitions, and republishes the leader status. Override-driven leadership does
    /// not set `leader_since` — `evaluate_health` short-circuits on `override_active`.
    pub fn observe_driver_event(&mut self, event: DriverEvent) {
        debug!(target: "leadership", event = ?event, "driver event observed");
        let was_leader = self.local_is_elected_leader();
        self.last_event = event;
        let is_leader = self.local_is_elected_leader();
        match (was_leader, is_leader) {
            (false, true) => {
                info!(target: "leadership", "local node became leader");
                self.leader_since = Some(Instant::now());
                // Fresh term — any prior step-down dispatch is no longer in flight.
                self.step_down_in_flight = false;
            }
            (true, false) => {
                debug!(target: "leadership", "local node lost leadership");
                self.leader_since = None;
                // Step-down (if one was in flight) has been observed by the driver and
                // taken effect; clear the debounce so a future re-election starts clean.
                self.step_down_in_flight = false;
            }
            _ => {}
        }
        self.publish_status();
    }

    /// Returns `true` iff the latest driver event elects the local node — distinct from
    /// [`LeaderStatus::is_leader`], which also returns `true` for override-driven leaders.
    pub fn local_is_elected_leader(&self) -> bool {
        matches!(&self.last_event, DriverEvent::LeaderElected { leader, .. } if leader == &self.local_id)
    }

    /// Returns `true` iff the local node has seen an unsafe-head update strictly after
    /// becoming leader — the embedded analogue of `op-conductor`'s `seqActive` flag.
    pub fn is_active(&self) -> bool {
        Self::is_active_predicate(self.leader_since, self.health_signals.last_unsafe_head_update)
    }

    /// Pure predicate behind [`Self::is_active`]. Strict `>` is the conservative choice:
    /// an equal-instant tie (paused virtual time, coarse OS clock) yields `false`,
    /// deferring the active flag by one health interval rather than risking a
    /// permanent-per-term false positive on a follower-promoted-to-leader whose
    /// pre-election gossip-driven head observation collides with the election instant.
    pub fn is_active_predicate(
        leader_since: Option<Instant>,
        last_unsafe_head_update: Option<Instant>,
    ) -> bool {
        let Some(since) = leader_since else { return false };
        last_unsafe_head_update.is_some_and(|last| last > since)
    }

    /// Folds a single health signal update into the current snapshot.
    pub const fn apply_health_update(&mut self, update: HealthSignalUpdate) {
        match update {
            HealthSignalUpdate::UnsafeHeadObserved(at) => {
                self.health_signals.last_unsafe_head_update = Some(at);
            }
            HealthSignalUpdate::L1HeadObserved(at) => {
                self.health_signals.last_l1_head_update = Some(at);
            }
            HealthSignalUpdate::ElSyncState(in_sync) => {
                self.health_signals.el_in_sync = Some(in_sync);
            }
            HealthSignalUpdate::PeerCount(count) => {
                self.health_signals.peer_count = Some(count);
            }
        }
    }

    /// Re-evaluates health and, if the local node is leading and unhealthy for a real
    /// reason, asks the driver to transfer leadership rather than waiting for the
    /// engine's view timeout. Mirrors `op-conductor`'s `(leader, healthy, active)`
    /// state machine: only an actively-sequencing leader is eligible for transfer.
    /// Two cold-start cases are suppressed: no upstream signals observed yet, and a
    /// freshly-elected leader that hasn't yet produced its first block under this term.
    pub async fn evaluate_health(&mut self) {
        let verdict = self.health.evaluate(self.health_signals, Instant::now());
        let HealthVerdict::Unhealthy(ref reasons) = verdict else { return };
        if !self.current_status().is_leader() || self.override_active {
            return;
        }
        // Cold-start: no signals have flowed yet. Anything else mixed in (e.g. stale
        // head) drops out of this branch and steps down normally.
        if reasons.iter().all(|r| matches!(r, HealthFailure::NoSignalsYet)) {
            debug!(target: "leadership", reasons = ?reasons, "suppressing step-down: no signals yet");
            return;
        }
        // Active gate: a freshly-elected leader that hasn't produced a block under this
        // term inherits the genesis unsafe-head timestamp and would trip `UnsafeHeadStale`
        // within `unsafe_head_max_age` of election. Transferring would just rotate the
        // same cold-start condition through the next voter.
        if !self.is_active() {
            debug!(
                target: "leadership",
                reasons = ?reasons,
                leader_since = ?self.leader_since,
                "suppressing step-down: sequencer hasn't produced under current term",
            );
            return;
        }
        // Debounce: a previous step-down dispatch is still in flight. Re-issuing on every
        // health tick (default 5s) would queue redundant `TransferLeadership(None)` requests
        // — each of which is a destructive membership change in the openraft driver — while
        // the cluster is still applying the first one. The flag is cleared in
        // `observe_driver_event` on any local-leader transition.
        if self.step_down_in_flight {
            debug!(
                target: "leadership",
                reasons = ?reasons,
                "suppressing step-down: previous step-down still in flight",
            );
            return;
        }
        warn!(
            target: "leadership",
            verdict = ?verdict,
            "local node is leading but unhealthy; initiating voluntary step-down",
        );
        self.step_down_in_flight = true;
        if let Err(e) = self.dispatch(DriverRequestKind::TransferLeadership(None)).await {
            warn!(target: "leadership", error = %e, "voluntary step-down failed");
            // Dispatch failed — no request will land. Clear the flag so the next health
            // tick can retry instead of being permanently suppressed.
            self.step_down_in_flight = false;
        }
    }

    /// Adds a voting validator after checking the supplied membership version.
    pub async fn add_voter(
        &mut self,
        entry: ValidatorEntry,
        version: u64,
    ) -> Result<ClusterMembership, LeadershipError> {
        self.check_version(version)?;
        if self.membership.get(&entry.id).is_some() {
            return Err(LeadershipError::DuplicateValidator(entry.id));
        }
        self.dispatch(DriverRequestKind::AddVoter(entry.clone())).await?;
        let mut voters = self.membership.voters.clone();
        voters.push(entry);
        self.membership = ClusterMembership::new(voters, self.membership.version.saturating_add(1));
        Ok(self.membership.clone())
    }

    /// Removes a voting validator after checking the supplied membership version.
    pub async fn remove_voter(
        &mut self,
        id: ValidatorId,
        version: u64,
    ) -> Result<ClusterMembership, LeadershipError> {
        self.check_version(version)?;
        if id == self.local_id {
            return Err(LeadershipError::CannotRemoveLocal(id));
        }
        if self.membership.get(&id).is_none() {
            return Err(LeadershipError::UnknownValidator(id));
        }
        self.dispatch(DriverRequestKind::RemoveVoter(id.clone())).await?;
        let voters = self.membership.voters.iter().filter(|v| v.id != id).cloned().collect();
        self.membership = ClusterMembership::new(voters, self.membership.version.saturating_add(1));
        Ok(self.membership.clone())
    }

    /// Returns an error if the supplied membership version does not match the current one.
    pub const fn check_version(&self, version: u64) -> Result<(), LeadershipError> {
        if version == self.membership.version {
            Ok(())
        } else {
            Err(LeadershipError::VersionMismatch {
                observed: version,
                current: self.membership.version,
            })
        }
    }

    /// Forwards a request to the driver and awaits the driver's ack.
    ///
    /// A closed request channel maps to [`DriverError::Exited`]; an Err returned via the
    /// ack is propagated. The actor uses the ack to detect both transport-level failure
    /// (channel closed) and driver-level rejection (e.g.
    /// [`DriverError::Unsupported`](crate::DriverError::Unsupported)).
    pub async fn dispatch(&self, kind: DriverRequestKind) -> Result<(), LeadershipError> {
        let (ack, rx) = oneshot::channel();
        self.driver_requests_tx.send(DriverRequest { kind, ack }).await.map_err(|_| {
            LeadershipError::Driver(DriverError::Exited("driver request channel closed".into()))
        })?;
        rx.await
            .map_err(|_| {
                LeadershipError::Driver(DriverError::Exited("driver dropped ack channel".into()))
            })?
            .map_err(LeadershipError::Driver)
    }

    /// Computes the current [`LeaderStatus`] from the latest driver event and override flag.
    pub fn current_status(&self) -> LeaderStatus {
        if self.override_active {
            let view = match &self.last_event {
                DriverEvent::LeaderElected { view, .. } => *view,
                DriverEvent::NoLeader => 0,
            };
            return LeaderStatus::Leader {
                view,
                overridden: true,
                override_generation: self.override_generation,
            };
        }
        match &self.last_event {
            DriverEvent::LeaderElected { leader, view } => {
                if leader == &self.local_id {
                    LeaderStatus::Leader { view: *view, overridden: false, override_generation: 0 }
                } else {
                    LeaderStatus::Follower { leader: leader.clone(), view: *view }
                }
            }
            DriverEvent::NoLeader => LeaderStatus::Unknown,
        }
    }

    /// Publishes the current [`LeaderStatus`] on the watch channel, suppressing redundant
    /// notifications.
    pub fn publish_status(&self) {
        let next = self.current_status();
        self.leader_status_tx.send_if_modified(|prev| {
            if prev == &next {
                false
            } else {
                debug!(target: "leadership", from = ?prev, to = ?next, "leader status changed");
                *prev = next;
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr}, // SocketAddr still needed for TransportConfig
        time::Duration,
    };

    use rstest::rstest;
    use tokio::sync::oneshot;

    use super::*;
    use crate::{HealthThresholds, MockDriver, RaftTimeouts, TransportConfig};

    /// Boundary check for [`RunningActor::is_active_predicate`]. Locks in the strict-`>`
    /// semantics: an `UnsafeHeadObserved` whose instant equals `leader_since` is *not*
    /// treated as post-leadership, guarding against a follower-promoted-to-leader whose
    /// pre-election gossip-driven head observation collides with the election instant.
    #[rstest]
    #[case::no_leader_since(None, Some(Duration::ZERO), false)]
    #[case::no_observation(Some(Duration::ZERO), None, false)]
    #[case::observation_strictly_before(
        Some(Duration::from_millis(10)),
        Some(Duration::ZERO),
        false
    )]
    #[case::observation_equal(Some(Duration::ZERO), Some(Duration::ZERO), false)]
    #[case::observation_strictly_after(Some(Duration::ZERO), Some(Duration::from_millis(10)), true)]
    fn is_active_predicate_uses_strict_greater_than(
        #[case] leader_since_offset: Option<Duration>,
        #[case] last_unsafe_offset: Option<Duration>,
        #[case] expected: bool,
    ) {
        let base = Instant::now();
        let leader_since = leader_since_offset.map(|d| base + d);
        let last_unsafe = last_unsafe_offset.map(|d| base + d);
        assert_eq!(RunningActor::is_active_predicate(leader_since, last_unsafe), expected,);
    }

    fn entry(id: &str) -> ValidatorEntry {
        ValidatorEntry { id: ValidatorId::new(id), addr: "127.0.0.1:50050".to_owned() }
    }

    /// Builds a config with the given local id and validator set, and a fast-tick health
    /// poll so transitions surface promptly in tests.
    ///
    /// Staleness thresholds are intentionally generous (60s) so the only health failure
    /// these tests can trigger is the explicit `PeerCount(0)` push below — staleness can
    /// never fire opportunistically on a slow CI runner and mask the peer-count test.
    fn config_with(local_id: &str, voters: &[&str]) -> LeadershipConfig {
        LeadershipConfig {
            local_id: ValidatorId::new(local_id),
            validators: voters.iter().map(|id| entry(id)).collect(),
            transport: TransportConfig {
                listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50050),
            },
            health: HealthThresholds {
                unsafe_head_max_age: Duration::from_secs(60),
                l1_head_max_age: Duration::from_secs(60),
                min_peer_count: 1,
                poll_interval: Duration::from_millis(20),
            },
            timeouts: RaftTimeouts::default(),
        }
    }

    /// Spawns the actor with a [`MockDriver`] and returns its handles, the join handle,
    /// and a cancellation token to tear it down.
    fn spawn_actor(
        cfg: LeadershipConfig,
    ) -> (LeadershipHandles, tokio::task::JoinHandle<Result<(), LeadershipError>>, CancellationToken)
    {
        let cancel = CancellationToken::new();
        let (actor, handles) = LeadershipActor::build(cfg, cancel.clone()).unwrap();
        let handle = tokio::spawn(actor.start(Box::new(MockDriver::new())));
        (handles, handle, cancel)
    }

    /// Awaits until the watch receiver yields a value satisfying `predicate`, with a
    /// 2-second timeout. Used by tests that need to synchronise on a state transition
    /// without sleeping for a guess-the-cadence wall-clock duration.
    async fn await_watch<T: Clone + std::fmt::Debug>(
        rx: &mut watch::Receiver<T>,
        predicate: impl Fn(&T) -> bool,
    ) -> T {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if predicate(&rx.borrow()) {
                return rx.borrow().clone();
            }
            tokio::select! {
                _ = rx.changed() => continue,
                _ = tokio::time::sleep_until(deadline) => {
                    panic!("timed out waiting for watch value; current = {:?}", rx.borrow());
                }
            }
        }
    }

    #[tokio::test]
    async fn local_node_becomes_leader_when_elected() {
        let (mut handles, task, cancel) = spawn_actor(config_with("seq-1", &["seq-1", "seq-2"]));

        let status = await_watch(&mut handles.leader_status_rx, |s| s.is_leader()).await;
        assert!(status.is_leader());
        assert!(!status.is_overridden());

        cancel.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn transfer_leadership_makes_local_node_a_follower() {
        let (mut handles, task, cancel) = spawn_actor(config_with("seq-1", &["seq-1", "seq-2"]));
        await_watch(&mut handles.leader_status_rx, |s| s.is_leader()).await;

        let (ack_tx, ack_rx) = oneshot::channel();
        handles
            .commands_tx
            .send(LeadershipCommand::TransferLeadership { to: None, ack: ack_tx })
            .await
            .unwrap();
        ack_rx.await.unwrap().unwrap();

        let status = await_watch(&mut handles.leader_status_rx, |s| s.is_follower()).await;
        match status {
            LeaderStatus::Follower { leader, .. } => assert_eq!(leader, ValidatorId::new("seq-2")),
            other => panic!("expected follower, got {other:?}"),
        }

        cancel.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn override_leader_forces_leader_status_and_bumps_generation() {
        // Local node is "seq-2" — would normally start as follower of "seq-1".
        let (mut handles, task, cancel) = spawn_actor(config_with("seq-2", &["seq-1", "seq-2"]));
        await_watch(&mut handles.leader_status_rx, |s| s.is_follower()).await;

        let (ack_tx, ack_rx) = oneshot::channel();
        handles
            .commands_tx
            .send(LeadershipCommand::OverrideLeader { enabled: true, ack: ack_tx })
            .await
            .unwrap();
        ack_rx.await.unwrap().unwrap();

        let status = await_watch(&mut handles.leader_status_rx, |s| s.is_overridden()).await;
        assert!(status.is_leader());
        assert_eq!(status.override_generation(), 1);

        cancel.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn unhealthy_leader_voluntarily_steps_down() {
        let (mut handles, task, cancel) =
            spawn_actor(config_with("seq-1", &["seq-1", "seq-2", "seq-3"]));
        await_watch(&mut handles.leader_status_rx, |s| s.is_leader()).await;

        let now = Instant::now();
        for update in [
            HealthSignalUpdate::UnsafeHeadObserved(now),
            HealthSignalUpdate::L1HeadObserved(now),
            HealthSignalUpdate::ElSyncState(true),
            HealthSignalUpdate::PeerCount(5),
        ] {
            handles.health_signals_tx.send(update).await.unwrap();
        }

        // Degrade peer count below the floor: the next health tick should observe an
        // unhealthy verdict and the leader-side path should issue a step-down.
        handles.health_signals_tx.send(HealthSignalUpdate::PeerCount(0)).await.unwrap();

        let status = await_watch(&mut handles.leader_status_rx, |s| !s.is_leader()).await;
        assert!(!status.is_leader());

        cancel.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn unhealthy_leader_does_not_step_down_before_sequencer_produces_a_block() {
        // Captured before actor startup so it pre-dates `leader_since`.
        let pre_leadership = Instant::now();

        let (mut handles, task, cancel) =
            spawn_actor(config_with("seq-1", &["seq-1", "seq-2", "seq-3"]));
        await_watch(&mut handles.leader_status_rx, |s| s.is_leader()).await;

        for update in [
            HealthSignalUpdate::UnsafeHeadObserved(pre_leadership),
            HealthSignalUpdate::L1HeadObserved(pre_leadership),
            HealthSignalUpdate::ElSyncState(true),
            HealthSignalUpdate::PeerCount(0),
        ] {
            handles.health_signals_tx.send(update).await.unwrap();
        }

        // Wait for the health aggregator to ingest the signals and turn `Unhealthy` —
        // this is the deterministic precondition that the active gate is now the only
        // thing standing between us and a step-down. Avoids relying on a wall-clock
        // sleep timing out the right number of `poll_interval` ticks on a slow runner.
        await_watch(&mut handles.health_verdict_rx, HealthVerdict::is_unhealthy).await;
        assert!(
            handles.leader_status_rx.borrow().is_leader(),
            "active gate must suppress step-down while sequencer hasn't produced",
        );

        // Post-leadership unsafe-head observation flips `is_active` true; the existing
        // `PeerCount = 0` failure is no longer suppressed and step-down fires.
        handles
            .health_signals_tx
            .send(HealthSignalUpdate::UnsafeHeadObserved(Instant::now()))
            .await
            .unwrap();
        let status = await_watch(&mut handles.leader_status_rx, |s| !s.is_leader()).await;
        assert!(!status.is_leader());

        cancel.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn add_voter_with_stale_version_returns_version_mismatch() {
        let (handles, task, cancel) = spawn_actor(config_with("seq-1", &["seq-1", "seq-2"]));

        let (ack_tx, ack_rx) = oneshot::channel();
        handles
            .commands_tx
            .send(LeadershipCommand::AddVoter { entry: entry("seq-3"), version: 99, ack: ack_tx })
            .await
            .unwrap();

        let err = ack_rx.await.unwrap().expect_err("expected version mismatch");
        assert!(matches!(err, LeadershipError::VersionMismatch { observed: 99, current: 0 }));

        cancel.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn cancellation_terminates_the_actor() {
        let (_handles, task, cancel) = spawn_actor(config_with("seq-1", &["seq-1", "seq-2"]));
        cancel.cancel();
        // Bound the wait so a future regression that fails to propagate cancellation to
        // the driver fails the test loudly with a timeout rather than hanging the suite.
        let outcome = tokio::time::timeout(Duration::from_secs(2), task).await.expect(
            "actor failed to terminate within 2s of cancel — driver did not observe shutdown",
        );
        outcome.expect("actor task panicked").expect("actor returned an error");
    }

    #[tokio::test]
    async fn remove_voter_rejects_local_id() {
        let (handles, task, cancel) = spawn_actor(config_with("seq-1", &["seq-1", "seq-2"]));

        let (ack_tx, ack_rx) = oneshot::channel();
        handles
            .commands_tx
            .send(LeadershipCommand::RemoveVoter {
                id: ValidatorId::new("seq-1"),
                version: 0,
                ack: ack_tx,
            })
            .await
            .unwrap();
        let err = ack_rx.await.unwrap().expect_err("expected CannotRemoveLocal");
        assert!(matches!(err, LeadershipError::CannotRemoveLocal(id) if id.as_str() == "seq-1"));

        cancel.cancel();
        let _ = task.await;
    }
}
