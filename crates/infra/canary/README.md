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

## Adding a new action

Each action is a struct that implements the `CanaryAction` trait. The trait has
two methods: `name`, which returns a static string used as a metrics label and
log field, and `execute`, which receives a `CancellationToken` and returns an
`ActionOutcome`.

Start by creating a file in `src/actions/`. A minimal action looks like this:

```rust
// src/actions/peer_count.rs

use std::time::Instant;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{ActionOutcome, CanaryAction};

#[derive(Debug)]
pub struct PeerCountAction {
    cl_rpc_url: Url,
}

impl PeerCountAction {
    pub const fn new(cl_rpc_url: Url) -> Self {
        Self { cl_rpc_url }
    }
}

#[async_trait]
impl CanaryAction for PeerCountAction {
    fn name(&self) -> &'static str {
        "peer_count"
    }

    async fn execute(&self, cancel: CancellationToken) -> ActionOutcome {
        let start = Instant::now();

        if cancel.is_cancelled() {
            return ActionOutcome::failed("cancelled", start);
        }

        // ... perform the check ...

        ActionOutcome::success("peer count within expected range", start)
    }
}
```

`ActionOutcome::success` and `ActionOutcome::failed` both accept any
`impl Into<String>` as the message and record elapsed time automatically from
the `start` instant you pass in. If the operation can be cancelled mid-flight,
check `cancel.is_cancelled()` at natural yield points or use `tokio::select!`
to race against `cancel.cancelled()`.

Once the file exists, export it from `src/actions/mod.rs`:

```rust
mod peer_count;
pub use peer_count::PeerCountAction;
```

Then add it to the `pub use actions::{...}` list in `src/lib.rs` so consumers
can import it directly from the crate root.

If the action records its own metrics, add the metric definitions to
`src/metrics.rs` using the `define_metrics!` macro and call them from within
`execute`. The `action_runs_total` and `action_duration_seconds` histograms are
recorded automatically by `CanaryService` for every action, so you only need
additional entries for action-specific observations. If you add a new action
name, add it to the `default` label list on those two metrics so Prometheus
pre-populates the time series.

To enable the action in the running service, add it inside `build_actions` in
`src/service.rs`:

```rust
if config.enable_peer_count {
    if let Some(cl_rpc_url) = &config.cl_rpc_url {
        actions.push(Box::new(PeerCountAction::new(cl_rpc_url.clone())));
    }
}
```

If the action needs configuration, add the corresponding fields to
`CanaryArgs` in `src/cli.rs` and to `CanaryConfig` in `src/config.rs`,
following the same pattern as the existing `enable_*` and URL fields. The
`from_cli` method on `CanaryConfig` is where validation lives; add any
required checks there before constructing the config struct.

## Scheduling

The `Scheduler` supports two modes:

* **Deterministic** — sleeps for a fixed interval between cycles.
* **Random** — sleeps for `interval + rand(0..jitter)`, producing a
  semi-randomized cadence.
