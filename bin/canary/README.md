# base-canary

Long-lived canary service that periodically performs health checks, balance
monitoring, and load tests against a target L2 network.

Scheduling supports two modes, selectable via `--schedule-mode`:

* **deterministic** — fixed interval between canary runs.
* **random** — interval with a random jitter component
  (`interval + rand(0..jitter)`).

## Usage

```sh
base-canary \
  --l2-rpc-url http://localhost:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
  --schedule-mode deterministic \
  --schedule-interval 60s \
  --load-test-duration 30s
```

All flags can be set via environment variables prefixed with `BASE_CANARY_`
(e.g. `BASE_CANARY_L2_RPC_URL`).
