//! Registrar metrics.

base_metrics::define_metrics_named! {
    RegistrarMetrics, "base_registrar",

    #[describe("Registrar build info")]
    #[label("version", version)]
    info: gauge,

    #[describe("Registrar is running")]
    up: gauge,

    #[describe("Total number of signer registrations submitted")]
    registrations_total: counter,

    #[describe("Total number of signer deregistrations submitted")]
    deregistrations_total: counter,

    #[describe("Total number of successful discovery cycles")]
    discovery_success_total: counter,

    #[describe("Total number of processing errors encountered")]
    processing_errors_total: counter,
}

impl RegistrarMetrics {
    /// Records startup metrics (info gauge with version label, up gauge set to 1).
    pub fn record_startup(version: &str) {
        Self::info(version.to_string()).set(1.0);
        Self::up().set(1.0);
    }

    /// Records shutdown by setting the up gauge to 0.
    pub fn record_shutdown() {
        Self::up().set(0.0);
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    fn record_startup_does_not_panic() {
        RegistrarMetrics::record_startup("0.0.0-test");
    }
}
