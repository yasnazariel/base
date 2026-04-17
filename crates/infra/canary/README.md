# base-canary

Core library for the Base canary service. Provides scheduled execution of
pluggable canary actions — load tests, block-production health checks, and
wallet balance monitoring — with Prometheus metrics and structured tracing.

## Actions

| Action | Description |
|--------|-------------|
| `LoadTestAction` | Wraps `base-load-tests` `LoadRunner` to submit transactions at a target gas rate and report latency / throughput metrics. |
| `HealthCheckAction` | Fetches the latest L2 block and checks whether its age exceeds a configurable threshold. |
| `BalanceCheckAction` | Monitors the canary wallet balance and warns when it drops below a minimum. |

## Scheduling

The `Scheduler` supports two modes:

* **Deterministic** — sleeps for a fixed interval between cycles.
* **Random** — sleeps for `interval + rand(0..jitter)`, producing a
  semi-randomized cadence.
