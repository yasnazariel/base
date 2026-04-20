# Embedded leadership devnet configs

The `leadership-config-*.json` files in this directory are the per-node embedded
leadership configurations consumed by the `docker-compose.embedded.yml` overlay.
Each file is the on-disk form of `base_consensus_leadership::LeadershipConfig`,
loaded at startup via the `--leadership.config-path` CLI flag.

The embedded driver is now backed by [openraft](https://github.com/databendlabs/openraft)
(Raft CFT consensus); the prior commonware-simplex BFT driver — and its per-node
Ed25519 signing keys — has been removed. There are no key files in this directory
anymore.

The cluster is a 3-node Raft (the builder CL plus two sequencer CLs):

| validator             | container             | dial address              |
|-----------------------|-----------------------|---------------------------|
| `builder-consensus`   | `base-builder-cl`     | `base-builder-cl:9050`    |
| `sequencer-1-consensus` | `base-sequencer-1-cl` | `base-sequencer-1-cl:9051` |
| `sequencer-2-consensus` | `base-sequencer-2-cl` | `base-sequencer-2-cl:9052` |

Tolerates 1 of 3 failures (Raft majority). For more severe degradation, see the
operator runbook on `conductor_overrideLeader` semantics; embedded leadership
exposes an equivalent `OverrideLeader` admin command.
