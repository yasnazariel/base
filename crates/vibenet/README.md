# vibenet

Rust services backing **vibenet**, the public-facing devnet product surface. These
crates are intentionally devnet-only: they are not used by production Base node
binaries and can be excluded from release pipelines by path.

## Crates

| Path | Crate | Role |
| --- | --- | --- |
| [`faucet/`](./faucet) | `base-vibenet-faucet` | HTTP faucet that drips native ETH and USDV to requested addresses. Rate-limited per client IP and per destination. |
| [`explorer/`](./explorer) | `vibescan` | Lightweight block explorer (Axum + SQLite + Askama) that indexes address activity and renders server-side HTML. |

Both produce standalone binaries and talk to an upstream L2 JSON-RPC node. They
do not share a library surface with the core node crates.

## Where the rest of vibenet lives

The Rust services in this directory are only one part of the product. The full
vibenet stack is assembled elsewhere:

- [`etc/vibenet/`](../../etc/vibenet) — nginx gateway, docker-compose overlay,
  setup scripts, and host bootstrap for bare-metal deployments.
- [`apps/vibenet-ui/`](../../apps/vibenet-ui) — static landing page and faucet UI.
- [`etc/docker/Justfile`](../../etc/docker/Justfile) — `just vibe` / `just vibe-down`
  entry points for local and remote lifecycle.

See [`etc/vibenet/README.md`](../../etc/vibenet/README.md) for the end-to-end
overview, including RPC endpoints, rate limits, and deployment.

## Conventions

- Devnet-only. Chain state is ephemeral; restarts wipe everything on purpose.
- Configuration is environment-driven (see each crate's README for variables).
- Secrets are read from env only — never logged, never surfaced via status
  endpoints, never committed to this branch.
