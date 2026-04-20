# `base-consensus-leadership`

Embedded leader-election and failover consensus for Base sequencers.

## Overview

This crate replaces the responsibilities of an external `op-conductor` sidecar by running
leader-election consensus *inside* each sequencer process. The output is a
`watch::Sender<LeaderStatus>` that the [`SequencerActor`](../service) gates block production on:
when this node is not the elected leader, it does not seal or publish unsafe blocks.

The crate is built around the [`LeadershipActor`], a `NodeActor` that owns three concerns:

1. **Consensus** — leader election driven by a pluggable [`ConsensusDriver`]. The
   production [`OpenraftDriver`] wraps the [openraft](https://github.com/databendlabs/openraft)
   Raft (CFT) engine, persisting its log + state machine on [sled](https://github.com/spacejam/sled)
   and shipping RPCs over a length-prefixed bincode TCP transport. A deterministic
   `MockDriver` (gated behind the `test-utils` Cargo feature) is available for
   exercising the full `LeadershipActor` ↔ `SequencerActor` integration end-to-end in
   tests; the trait boundary is intentionally narrow so swapping engines is local.
2. **Health** — a [`HealthAggregator`] subscribes to local sequencer signals
   (unsafe head freshness, EL sync state, L1 head, peer count) and produces a verdict.
   When the verdict turns unhealthy on a leader node, the actor voluntarily steps down.
3. **Admin** — an mpsc-backed [`LeadershipCommand`] surface lets operators query status,
   transfer leadership, manage cluster membership, and force overrides for disaster
   recovery.

## Design

The actor's interface is intentionally consensus-engine-agnostic. `LeaderStatus`,
`LeadershipCommand`, and `ClusterMembership` do not leak any underlying consensus library's
types. This means the consensus engine can be swapped without touching the rest of
`base-consensus`.

## Failure model

Raft is crash-fault tolerant: an `n`-voter cluster tolerates `f` simultaneous failures
where `n = 2f + 1` (so 1 of 3, 2 of 5). Beyond that floor, automatic election is
mathematically incompatible with safety; severe degradation requires the operator to
issue an `OverrideLeader` admin command on the surviving node, with downstream fencing
(L1 batcher epoch token) preventing split-brain. This matches op-conductor's
`conductor_overrideLeader` shape.

## Status

Embedded leadership is opt-in via the `--leadership.config-path` and
`--leadership.storage-dir` CLI flags. When unset, the legacy `op-conductor` HTTP path
runs unchanged.
