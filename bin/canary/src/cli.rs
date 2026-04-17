//! CLI definition for the canary binary.

use clap::Parser;
use eyre::WrapErr;

/// Base Canary.
#[derive(Parser)]
#[command(author, version)]
#[group(skip)]
pub(crate) struct Cli {
    #[command(flatten)]
    args: base_canary::Cli,
}

impl Cli {
    /// Run the canary service.
    pub(crate) async fn run(self) -> eyre::Result<()> {
        let config = base_canary::CanaryConfig::from_cli(self.args)?;
        config.log.init_tracing_subscriber()?;
        config
            .metrics
            .init_with(|| {
                base_cli_utils::register_version_metrics!();
            })
            .wrap_err("failed to install Prometheus recorder")?;
        base_canary::CanaryService::run(config).await
    }
}
