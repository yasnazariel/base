# `base`

Unified Base execution + consensus validator binary.

The curated `base node` command starts:

- `base-reth-node` execution services
- `base-consensus` rollup node services

The internal EL/CL link uses Reth auth IPC instead of Engine API HTTP. The public RPC surface is
HTTP-only when `--http` is provided; the embedded consensus service and auth IPC remain internal.
Prometheus metrics are exposed only when `--metrics` is provided.
The rollup RPC for `optimism_*` methods is exposed only when `--op-rpc` is provided.
For file-based chains not present in the registry, `--unsafe-block-signer` can be used to provide
the initial consensus signer explicitly.

Use a named preset for stable public networks:

```text
base node --network base-sepolia --l1-rpc-url http://... --l1-beacon-url http://...
```

Use explicit files for file-based deployments:

```text
base node \
  --http 0.0.0.0:8545 \
  --op-rpc 0.0.0.0:9549 \
  --metrics 0.0.0.0:9090 \
  --l2-genesis /path/to/genesis.json \
  --rollup-config /path/to/rollup.json \
  --l1-config /path/to/l1.json \
  --unsafe-block-signer 0x... \
  --bootnodes enode://...@bootnode.example.org:9000 \
  --l1-rpc-url http://... \
  --l1-beacon-url http://...
```

Consensus metrics are exported through the execution layer Prometheus endpoint. The standalone
`base-reth-node` and `base-consensus` binaries remain unchanged.
