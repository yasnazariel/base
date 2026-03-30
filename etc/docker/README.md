# `docker`

This directory contains the Dockerfiles and Compose configuration for the Base node.

## Dockerfiles

`Dockerfile.rust-services` is the shared multi-target Dockerfile for the Debian-based Rust services. It provides `client`, `base`, `builder`, `consensus`, `proposer`, `websocket-proxy`, `ingress-rpc`, `audit-archiver`, and `batcher` targets.

`Dockerfile.devnet` builds a utility image containing genesis generation tools (`eth-genesis-state-generator`, `eth2-val-tools`, `op-deployer`) and setup scripts. This image bootstraps L1 and L2 chain configurations for local development.

`Dockerfile.nitro-enclave` and `Dockerfile.proxyd` remain separate because they have different toolchains and runtime requirements.

## Docker Compose

The `docker-compose.yml` orchestrates a complete local devnet environment with both L1 and L2 chains. It now runs two L2 validator variants side by side:

- An L1 execution client (Reth) and consensus client (Lighthouse) with a validator
- The Base builder on L2
- `base-client` as the current split validator stack (`base-reth-node` + `base-client-cl`)
- `base-client-combined` as the unified `base` single binary
- The `base-batcher` for submitting L2 data to L1

All services read configuration from `devnet-env` in this directory. The devnet stores chain data in `.devnet/` which is created on first run.

## Usage

The easiest way to interact with Docker is through the Justfile recipes:

```bash
just devnet up     # Start fresh devnet (stops existing, clears data, rebuilds)
just devnet down   # Stop devnet and remove data
just devnet logs   # Stream logs from all containers
just devnet status # Check block numbers and sync status
```

To build a specific Rust service image directly:

```bash
just devnet build-image client release
```

Plain `docker build` still works if you prefer it:

```bash
docker build -t base-reth-node -f etc/docker/Dockerfile.rust-services --target client .
```
