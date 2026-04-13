//! Snapshot sidecar binary entry point.

use std::path::PathBuf;

use base_snapshot::{SnapshotConfig, SnapshotOrchestrator};
use clap::Parser;
use eyre::Result;

#[derive(Parser, Debug)]
#[command(name = "snapshot", about = "Snapshot sidecar for base reth nodes")]
struct Args {
    /// Path to the snapshot configuration file.
    #[arg(short, long, default_value = "snapshot.toml")]
    config: PathBuf,

    /// Run a single snapshot cycle and exit.
    #[arg(long)]
    once: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let config_str = std::fs::read_to_string(&args.config)
        .map_err(|e| eyre::eyre!("failed to read config {}: {e}", args.config.display()))?;
    let config: SnapshotConfig = serde_json::from_str(&config_str)?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let orchestrator = SnapshotOrchestrator::from_config(config).await?;
            if args.once {
                orchestrator.run_snapshot().await?;
            } else {
                orchestrator.run_loop().await?;
            }
            Ok(())
        })
}
