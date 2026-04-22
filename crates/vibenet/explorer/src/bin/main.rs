//! Entrypoint: reads config, opens storage, starts indexer + HTTP server,
//! and waits for SIGINT/SIGTERM for a clean shutdown.

use base_vibenet_explorer::{
    ExplorerConfig,
    indexer::Indexer,
    rpc_proxy::RpcClient,
    server::Explorer,
    storage::Storage,
};
use eyre::{Result, WrapErr};
use tokio::signal;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    color_eyre_or_anyhow_noop();

    let config = ExplorerConfig::from_env().wrap_err("loading config from env")?;
    info!(?config, "vibescan starting");

    let storage = Storage::open(&config.db_path).await.wrap_err("opening storage")?;
    let rpc = RpcClient::connect(&config.rpc_http_url).await.wrap_err("connecting http rpc")?;

    let (tx_shutdown, rx_shutdown) = tokio::sync::watch::channel(false);

    let indexer = Indexer::new(
        rpc.clone(),
        config.rpc_ws_url.clone(),
        storage.clone(),
        config.expected_chain_id,
        config.start_block,
        config.backfill_concurrency,
    );
    let indexer_task = tokio::spawn({
        let rx = rx_shutdown.clone();
        async move {
            if let Err(err) = indexer.run(rx).await {
                tracing::error!(%err, "indexer exited with error");
                return Err(err);
            }
            Ok(())
        }
    });

    let server = Explorer::new(&config, storage, rpc);
    let server_task = tokio::spawn({
        let rx = rx_shutdown.clone();
        let addr = config.bind;
        async move { server.serve(addr, rx).await }
    });

    wait_for_shutdown().await;
    info!("shutdown signal received");
    let _ = tx_shutdown.send(true);

    // Best-effort: give both tasks a chance to drain.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let _ = tokio::join!(indexer_task, server_task);
    })
    .await;

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("VIBESCAN_LOG")
        .or_else(|_| EnvFilter::try_new("info,hyper=warn,sqlx=warn"))
        .unwrap();
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).init();
}

/// No-op placeholder; color-eyre is not a workspace dep so we rely on plain
/// eyre reports. Keeps the main fn readable for future hook points.
fn color_eyre_or_anyhow_noop() {}

async fn wait_for_shutdown() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("install ctrl_c handler");
    };
    #[cfg(unix)]
    let term = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}
