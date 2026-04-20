//! Validator identity and cluster-membership types.
//!
//! These types form the consensus-engine-agnostic boundary that the rest of `base-consensus`
//! sees. They intentionally do not reference any underlying consensus library.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// A unique identifier for a validator in the leadership cluster.
///
/// This is a short human-readable string (e.g. `"sequencer-1"`) chosen at deployment time.
/// It is the stable identity used in admin commands, metrics, and on-disk consensus
/// state — distinct from the validator's signing key, which can be rotated without changing
/// the id.
#[derive(Clone, Debug, Default, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ValidatorId(String);

impl ValidatorId {
    /// Constructs a [`ValidatorId`] from any string-like value.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the id as a borrowed string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ValidatorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ValidatorId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for ValidatorId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl FromStr for ValidatorId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

/// A single voting member of the leadership cluster.
///
/// `addr` is the dial address that the consensus transport will use to reach this
/// validator. It accepts both IP:port (`"1.2.3.4:9050"`) and hostname:port
/// (`"base-builder-cl:9050"`) notation; the driver resolves the hostname to an IP
/// at startup.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ValidatorEntry {
    /// The stable id of this validator.
    pub id: ValidatorId,
    /// The `host:port` address peers should dial to reach this validator's leadership
    /// transport. Accepts DNS hostnames as well as numeric IP addresses.
    pub addr: String,
}

/// A snapshot of the current cluster configuration.
///
/// Returned by [`LeadershipCommand::GetMembership`](crate::LeadershipCommand::GetMembership)
/// to admin RPC callers. The `version` field follows op-conductor's optimistic-concurrency
/// pattern: callers pass it back when issuing membership-change commands so concurrent
/// modifications are detected and rejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterMembership {
    /// The set of voting validators, sorted by [`ValidatorId`] for deterministic output.
    pub voters: Vec<ValidatorEntry>,
    /// A monotonically increasing version that bumps on every membership change.
    pub version: u64,
}

impl ClusterMembership {
    /// Constructs a new [`ClusterMembership`], sorting the voters by id.
    pub fn new(mut voters: Vec<ValidatorEntry>, version: u64) -> Self {
        voters.sort_by(|a, b| a.id.cmp(&b.id));
        Self { voters, version }
    }

    /// Returns the [`ValidatorEntry`] for the given id, or `None` if no such voter exists.
    pub fn get(&self, id: &ValidatorId) -> Option<&ValidatorEntry> {
        self.voters.iter().find(|v| &v.id == id)
    }

    /// Returns the number of voters.
    pub const fn len(&self) -> usize {
        self.voters.len()
    }

    /// Returns `true` if there are no voters.
    pub const fn is_empty(&self) -> bool {
        self.voters.is_empty()
    }

    /// Returns the smallest number of voters that constitute a quorum.
    ///
    /// For a Byzantine-fault-tolerant protocol this is `2f + 1` of `3f + 1` voters; for a
    /// crash-fault-tolerant protocol it is the strict majority. We use the strict-majority
    /// definition here because it is the floor that any liveness-preserving protocol
    /// must meet.
    pub const fn quorum(&self) -> usize {
        self.voters.len() / 2 + 1
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn entry(id: &str) -> ValidatorEntry {
        ValidatorEntry { id: ValidatorId::new(id), addr: "127.0.0.1:50050".to_owned() }
    }

    #[test]
    fn validator_id_round_trips_through_string_and_str() {
        let id: ValidatorId = "seq-1".into();
        assert_eq!(id.as_str(), "seq-1");
        assert_eq!(id.to_string(), "seq-1");
        assert_eq!(ValidatorId::from("seq-1".to_owned()), id);
        assert_eq!(ValidatorId::from_str("seq-1").unwrap(), id);
    }

    #[test]
    fn membership_new_sorts_voters_by_id() {
        let voters = vec![entry("seq-c"), entry("seq-a"), entry("seq-b")];
        let membership = ClusterMembership::new(voters, 7);
        let ids: Vec<_> = membership.voters.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, vec!["seq-a", "seq-b", "seq-c"]);
        assert_eq!(membership.version, 7);
    }

    #[test]
    fn membership_get_finds_known_voters_and_returns_none_for_unknown() {
        let membership = ClusterMembership::new(vec![entry("seq-a"), entry("seq-b")], 0);
        assert!(membership.get(&ValidatorId::new("seq-a")).is_some());
        assert!(membership.get(&ValidatorId::new("missing")).is_none());
    }

    #[rstest]
    #[case(1, 1)]
    #[case(2, 2)]
    #[case(3, 2)]
    #[case(4, 3)]
    #[case(5, 3)]
    #[case(6, 4)]
    #[case(7, 4)]
    fn quorum_is_strict_majority(#[case] voters: usize, #[case] expected: usize) {
        let entries = (0..voters).map(|i| entry(&format!("seq-{i}"))).collect();
        let membership = ClusterMembership::new(entries, 0);
        assert_eq!(membership.quorum(), expected);
    }
}
