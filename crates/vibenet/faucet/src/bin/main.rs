//! Entrypoint for the `base-vibenet-faucet` binary.

use base_vibenet_faucet::{FaucetConfig, FaucetServer, FaucetState};
use eyre::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = FaucetConfig::from_env()?;
    info!(
        address = %config.address,
        chain_id = config.chain_id,
        drip_wei = %config.drip_wei,
        "starting vibenet faucet"
    );

    let state = FaucetState::new(config)?;
    let server = FaucetServer::bind(state).await?;

    tokio::select! {
        res = server.serve() => res?,
        _ = shutdown_signal() => {
            info!("shutdown signal received");
        }
    }

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,base_vibenet_faucet=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut stream) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            stream.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
