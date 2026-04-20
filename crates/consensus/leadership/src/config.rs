//! Configuration for the [`LeadershipActor`](crate::LeadershipActor).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{ClusterMembership, LeadershipError, ValidatorEntry, ValidatorId};

/// Top-level CLI choice between embedded leadership and the legacy external `op-conductor`
/// HTTP path.
///
/// The default is [`LeadershipMode::External`] so that adding the leadership crate to a
/// deployment does not change behaviour until an operator explicitly opts in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeadershipMode {
    /// Use the legacy [`op-conductor`](https://github.com/ethereum-optimism/optimism/tree/develop/op-conductor)
    /// HTTP-RPC sidecar for leader election. The [`LeadershipActor`](crate::LeadershipActor)
    /// is not spawned in this mode.
    #[default]
    External,
    /// Use embedded leadership consensus. The [`LeadershipActor`](crate::LeadershipActor)
    /// is spawned and the legacy `op-conductor` HTTP path is bypassed.
    Embedded,
}

impl LeadershipMode {
    /// Returns `true` if this mode requires the [`LeadershipActor`](crate::LeadershipActor)
    /// to be spawned.
    pub const fn is_embedded(&self) -> bool {
        matches!(self, Self::Embedded)
    }
}

/// The transport configuration for the leadership consensus protocol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportConfig {
    /// The local socket address the leadership listener will bind to.
    pub listen_addr: std::net::SocketAddr,
}

/// Health thresholds that trigger voluntary leader step-down when crossed on a leader node.
///
/// These mirror the equivalent settings in `op-conductor`'s `HealthCheck` configuration
/// (`UnsafeInterval`, `MinPeerCount`, etc.) so operators can lift their existing tuning
/// over verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthThresholds {
    /// Maximum age of the local unsafe head before the local node is considered unhealthy.
    pub unsafe_head_max_age: Duration,
    /// Maximum age of the local L1 head before the local node is considered unhealthy.
    pub l1_head_max_age: Duration,
    /// Minimum number of connected gossip peers below which the local node is considered
    /// unhealthy.
    pub min_peer_count: usize,
    /// How often the [`HealthAggregator`](crate::HealthAggregator) re-evaluates the verdict.
    pub poll_interval: Duration,
}

impl Default for HealthThresholds {
    fn default() -> Self {
        // op-conductor defaults: UnsafeInterval=30s, MinPeerCount typically 3, poll cadence ~5s.
        // Operators are expected to tune these per chain via the leadership config block.
        Self {
            unsafe_head_max_age: Duration::from_secs(30),
            l1_head_max_age: Duration::from_secs(60),
            min_peer_count: 3,
            poll_interval: Duration::from_secs(5),
        }
    }
}

/// Static configuration for the [`LeadershipActor`](crate::LeadershipActor).
///
/// Loaded once at startup. Runtime mutations (membership changes, leader override) are
/// issued via [`LeadershipCommand`](crate::LeadershipCommand) instead of by mutating this
/// struct.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadershipConfig {
    /// The id of the local validator. Must be present in `validators`.
    pub local_id: ValidatorId,
    /// The initial set of voting validators in the cluster.
    pub validators: Vec<ValidatorEntry>,
    /// The transport on which to run the leadership protocol.
    pub transport: TransportConfig,
    /// Health thresholds that trigger voluntary step-down.
    pub health: HealthThresholds,
    /// Per-engine timeouts that operators may need to tune per chain.
    pub timeouts: RaftTimeouts,
}

/// Tunable Raft timeouts for the embedded leadership engine, exposed so operators can
/// adapt the engine to per-chain network conditions without recompiling.
///
/// Defaults are tuned for a low-latency LAN/datacenter deployment (heartbeat well below
/// election timeout) and match the values the Base devnet runs with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaftTimeouts {
    /// Lower bound of the randomized election timeout. Followers wait at least this long
    /// without hearing from a leader before starting an election.
    pub election_timeout_min: Duration,
    /// Upper bound of the randomized election timeout. The actual timeout is sampled
    /// uniformly from `[election_timeout_min, election_timeout_max]`.
    ///
    /// Must be strictly greater than `election_timeout_min` to give Raft enough jitter
    /// to break leader-election ties.
    pub election_timeout_max: Duration,
    /// How often the leader sends heartbeats to followers. Should be well below
    /// `election_timeout_min` so a healthy leader never triggers spurious elections —
    /// a 5–10x ratio is typical.
    pub heartbeat_interval: Duration,
    /// Per-RPC deadline for `install_snapshot`. Sized for the largest snapshot the
    /// state machine could reasonably ship; the leadership SM is empty so this is
    /// usually irrelevant in practice.
    pub install_snapshot_timeout: Duration,
    /// Maximum number of log entries the leader will pack into a single `append_entries`
    /// RPC. Caps both the on-wire message size and the per-RPC apply latency.
    pub max_payload_entries: u64,
}

impl Default for RaftTimeouts {
    fn default() -> Self {
        Self {
            election_timeout_min: Duration::from_millis(150),
            election_timeout_max: Duration::from_millis(300),
            heartbeat_interval: Duration::from_millis(50),
            install_snapshot_timeout: Duration::from_secs(1),
            max_payload_entries: 300,
        }
    }
}

impl LeadershipConfig {
    /// Constructs a degenerate single-voter [`LeadershipConfig`], in which the local
    /// node is the only validator.
    ///
    /// In a 1-voter cluster the local node is always the elected leader and there is no
    /// failover possible. This is therefore not useful in production, but it is the
    /// simplest way to exercise the [`LeadershipActor`](crate::LeadershipActor) end-to-end
    /// in single-sequencer development environments where the rest of the wiring (admin
    /// RPC, status watch, sequencer gate) needs to be smoke-tested.
    pub fn single_node(local_id: ValidatorId, listen_addr: std::net::SocketAddr) -> Self {
        let validators =
            vec![ValidatorEntry { id: local_id.clone(), addr: listen_addr.to_string() }];
        Self {
            local_id,
            validators,
            transport: TransportConfig { listen_addr },
            health: HealthThresholds::default(),
            timeouts: RaftTimeouts::default(),
        }
    }

    /// Validates the configuration and returns an initial [`ClusterMembership`] snapshot.
    ///
    /// Validation checks that:
    /// - the local validator id is present in the validator set,
    /// - no two validators share the same id,
    /// - the cluster is non-empty.
    ///
    /// The `version` of the returned membership is `0`. Subsequent membership-change
    /// commands bump it monotonically.
    pub fn validate(&self) -> Result<ClusterMembership, LeadershipError> {
        if self.validators.is_empty() {
            return Err(LeadershipError::InsufficientQuorum { actual: 0, required: 1 });
        }

        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.validators {
            if !seen.insert(entry.id.clone()) {
                return Err(LeadershipError::DuplicateValidator(entry.id.clone()));
            }
        }

        if !seen.contains(&self.local_id) {
            return Err(LeadershipError::LocalValidatorMissing(self.local_id.clone()));
        }

        Ok(ClusterMembership::new(self.validators.clone(), 0))
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use super::*;

    fn entry(id: &str) -> ValidatorEntry {
        ValidatorEntry { id: ValidatorId::new(id), addr: "127.0.0.1:50050".to_owned() }
    }

    fn cfg(local: &str, validators: Vec<ValidatorEntry>) -> LeadershipConfig {
        LeadershipConfig {
            local_id: ValidatorId::new(local),
            validators,
            transport: TransportConfig {
                listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50050),
            },
            health: HealthThresholds::default(),
            timeouts: RaftTimeouts::default(),
        }
    }

    #[test]
    fn validate_rejects_empty_validator_set() {
        let err = cfg("seq-1", vec![]).validate().unwrap_err();
        assert!(matches!(err, LeadershipError::InsufficientQuorum { actual: 0, .. }));
    }

    #[test]
    fn validate_rejects_duplicate_validator_ids() {
        let err = cfg("seq-1", vec![entry("seq-1"), entry("seq-1")]).validate().unwrap_err();
        assert!(matches!(err, LeadershipError::DuplicateValidator(_)));
    }

    #[test]
    fn validate_rejects_missing_local_validator() {
        let err = cfg("seq-9", vec![entry("seq-1"), entry("seq-2")]).validate().unwrap_err();
        assert!(matches!(err, LeadershipError::LocalValidatorMissing(_)));
    }

    #[test]
    fn validate_returns_membership_sorted_with_initial_version() {
        let membership = cfg("seq-1", vec![entry("seq-2"), entry("seq-1")]).validate().unwrap();
        assert_eq!(membership.version, 0);
        let ids: Vec<_> = membership.voters.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, vec!["seq-1", "seq-2"]);
    }

    #[test]
    fn single_node_constructs_a_one_voter_cluster_with_local_as_leader() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50050);
        let cfg = LeadershipConfig::single_node(ValidatorId::new("solo"), addr);
        let membership = cfg.validate().expect("single-node config validates");
        assert_eq!(membership.len(), 1);
        assert_eq!(membership.voters[0].id, ValidatorId::new("solo"));
        assert_eq!(membership.voters[0].addr, addr.to_string());
        assert_eq!(membership.quorum(), 1);
    }

    #[test]
    fn external_mode_is_default_and_does_not_require_actor() {
        assert_eq!(LeadershipMode::default(), LeadershipMode::External);
        assert!(!LeadershipMode::default().is_embedded());
        assert!(LeadershipMode::Embedded.is_embedded());
    }

    #[test]
    fn raft_timeouts_default_satisfies_heartbeat_lt_election_min() {
        let t = RaftTimeouts::default();
        assert!(t.heartbeat_interval < t.election_timeout_min);
        assert!(t.election_timeout_min < t.election_timeout_max);
    }
}
