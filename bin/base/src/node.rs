//! Node launch logic for the unified Base validator binary.

use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use alloy_provider::RootProvider;
use alloy_rpc_types_engine::JwtSecret;
use base_alloy_network::Base;
use base_bundle_extension::BundleExtension;
use base_consensus_node::{
    EngineConfig, EngineRpcAddress, L1ConfigBuilder, NodeMode, RollupNodeBuilder,
};
use base_flashblocks_node::FlashblocksExtension;
use base_metering::MeteringExtension;
use base_node_runner::BaseNodeRunner;
use base_txpool_rpc::{TxPoolRpcConfig, TxPoolRpcExtension};
use base_txpool_tracing::{TxPoolExtension, TxpoolConfig};
use eyre::{Context, OptionExt};
use reth_db::init_db;
use reth_node_builder::{NodeBuilder, NodeConfig, rpc::EngineShutdown};
use reth_node_core::{
    args::{DatadirArgs, MetricArgs, NetworkArgs, RpcServerArgs, TxPoolArgs},
    dirs::{DataDirPath, MaybePlatformPath},
    exit::NodeExitFuture,
};
use reth_rpc_server_types::RpcModuleSelection;
use reth_tasks::{RuntimeBuilder, RuntimeConfig, TaskExecutor, TokioConfig};
use tempfile::NamedTempFile;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::cli::{Cli, Command, NodeArgs, ResolvedNodeConfig};

const HTTP_RPC_MODULES: &str = "admin,debug,eth,net,rpc,txpool,web3";
const RPC_GAS_CAP: u64 = 600_000_000;
const RPC_ETH_PROOF_WINDOW: u64 = 1_209_600;

pub(crate) struct InMemoryAuthJwt {
    secret: JwtSecret,
    path: PathBuf,
    // Keep the temp file alive for the full process lifetime. Dropping it deletes the on-disk JWT
    // file that the embedded EL auth server advertises to the CL.
    _file: NamedTempFile,
}

impl InMemoryAuthJwt {
    fn path(&self) -> &Path {
        &self.path
    }

    const fn secret(&self) -> JwtSecret {
        self.secret
    }
}

impl Cli {
    pub(crate) fn run(self) -> eyre::Result<()> {
        match self.command {
            Command::Node(args) => args.run(),
        }
    }
}

impl NodeArgs {
    pub(crate) fn run(self) -> eyre::Result<()> {
        self.init_logging()?;
        base_cli_utils::RuntimeManager::run_with_signal_token(move |shutdown| async move {
            self.run_inner(shutdown).await
        })
    }

    async fn run_inner(self, shutdown: CancellationToken) -> eyre::Result<()> {
        let resolved = self.resolve()?;
        let task_executor = RuntimeBuilder::new(
            RuntimeConfig::default()
                .with_tokio(TokioConfig::existing_handle(tokio::runtime::Handle::current())),
        )
        .build()?;

        let mut node_config = self.execution_node_config(&resolved);
        // Keep the JWT temp file alive until both embedded nodes have fully shut down.
        let auth_jwt = Self::create_in_memory_auth_jwt()?;

        Self::configure_execution_auth_ipc(&mut node_config, &auth_jwt);

        let p2p_flags = self.consensus_p2p_args(&resolved, node_config.datadir().data_dir())?;
        let p2p_config = p2p_flags
            .config(
                &resolved.rollup_config,
                resolved.rollup_config.l2_chain_id.id(),
                Some(self.l1_rpc_url.clone()),
                resolved.unsafe_block_signer,
            )
            .await?;

        let db_path = node_config.datadir().db();
        info!(target: "reth::cli", path = ?db_path, "Opening database");
        let database = init_db(db_path, node_config.db.database_args())?.with_metrics();

        info!(
            target: "base",
            chain_id = resolved.rollup_config.l2_chain_id.id(),
            source = %resolved.source,
            mode = %NodeMode::Validator,
            "Starting unified Base validator"
        );

        let builder = NodeBuilder::new(node_config)
            .with_database(database)
            .with_launch_context(task_executor);

        let mut runner = BaseNodeRunner::new(self.rollup_args());
        let flashblocks_config = self.flashblocks_config();
        runner.install_ext::<TxPoolRpcExtension>(TxPoolRpcConfig {
            sequencer_rpc: self.sequencer_rpc_url.as_ref().map(ToString::to_string),
        });
        runner.install_ext::<TxPoolExtension>(TxpoolConfig {
            tracing_enabled: false,
            tracing_logs_enabled: false,
            flashblocks_config: flashblocks_config.clone(),
        });
        runner.install_ext::<MeteringExtension>(self.metering_config(flashblocks_config.clone()));
        runner.install_ext::<BundleExtension>(());
        runner.install_ext::<FlashblocksExtension>(flashblocks_config);

        let node_handle = runner.launch(builder).await?;
        let reth_node = node_handle.node;
        let mut node_exit_future = Box::pin(node_handle.node_exit_future);
        let execution_shutdown = reth_node.add_ons_handle.engine_shutdown.clone();
        let execution_task_executor = reth_node.task_executor.clone();
        let execution_exit_is_expected = reth_node.config.debug.terminate;

        let auth_ipc_path = reth_node
            .auth_server_handle()
            .ipc_endpoint()
            .map(PathBuf::from)
            .ok_or_eyre("auth IPC endpoint should be configured before launch returns")?;
        let auth_ipc_endpoint = auth_ipc_path.to_string_lossy().into_owned();
        let l2_provider = RootProvider::<Base>::connect(&auth_ipc_endpoint)
            .await
            .wrap_err("failed to connect to embedded execution auth IPC")?;
        let rollup_config = resolved.rollup_config;
        let engine_config = EngineConfig {
            config: Arc::new(rollup_config.clone()),
            l2_rpc: EngineRpcAddress::Ipc(auth_ipc_path),
            l2_jwt_secret: auth_jwt.secret(),
            l1_url: self.l1_rpc_url.clone(),
            mode: NodeMode::Validator,
        };

        let rollup_node = RollupNodeBuilder::new(
            rollup_config,
            L1ConfigBuilder {
                chain_config: resolved.l1_config,
                trust_rpc: true,
                beacon: self.l1_beacon_url.clone(),
                rpc_url: self.l1_rpc_url.clone(),
                slot_duration_override: self.l1_slot_duration,
            },
            true,
            engine_config,
            p2p_config,
            self.rollup_rpc_config(),
        )
        .with_l2_provider(l2_provider)
        .build();

        let consensus_shutdown = shutdown.child_token();
        let mut consensus_task = tokio::spawn({
            let consensus_shutdown = consensus_shutdown.clone();
            async move {
                rollup_node
                    .start_with_shutdown(consensus_shutdown)
                    .await
                    .map_err(|e| eyre::eyre!("failed to start rollup node service: {e}"))
            }
        });

        enum ExitReason {
            Signal,
            Execution(eyre::Result<()>),
            Consensus(Result<eyre::Result<()>, tokio::task::JoinError>),
        }

        let exit_reason = tokio::select! {
            _ = shutdown.cancelled() => {
                ExitReason::Signal
            }
            res = &mut node_exit_future => {
                ExitReason::Execution(res)
            }
            res = &mut consensus_task => {
                ExitReason::Consensus(res)
            }
        };

        match exit_reason {
            ExitReason::Signal => {
                info!(target: "base", "Received shutdown signal, stopping unified Base validator");
                consensus_shutdown.cancel();
                let execution_result =
                    request_execution_shutdown(node_exit_future.as_mut(), execution_shutdown)
                        .await?;
                finish_consensus_task(
                    consensus_task.await,
                    "consensus task failed to join during shutdown",
                )?;
                wait_for_execution_tasks(execution_task_executor).await?;
                if let Err(ref e) = execution_result {
                    warn!(target: "base", error = %e, "Execution layer exited with error during shutdown");
                }
                execution_result?;
            }
            ExitReason::Execution(res) => {
                let execution_result = res.wrap_err("execution layer exited");
                let execution_exited_cleanly = execution_result.is_ok();
                consensus_shutdown.cancel();
                finish_consensus_task(
                    consensus_task.await,
                    "consensus task failed to join after execution layer exit",
                )?;
                wait_for_execution_tasks(execution_task_executor).await?;
                if execution_exited_cleanly && !execution_exit_is_expected {
                    warn!(
                        target: "base",
                        "Execution layer exited cleanly without terminate=true; treating it as unexpected validator shutdown"
                    );
                    eyre::bail!("execution layer exited unexpectedly");
                }
                execution_result?;
            }
            ExitReason::Consensus(res) => {
                let consensus_result = finish_consensus_task(res, "consensus task failed to join");
                if let Err(ref e) = consensus_result {
                    warn!(target: "base", error = %e, "Consensus layer exited with error, shutting down execution layer");
                }
                match request_execution_shutdown(node_exit_future.as_mut(), execution_shutdown)
                    .await
                {
                    Ok(el_exit) => {
                        if let Err(ref e) = el_exit {
                            warn!(target: "base", error = %e, "Execution layer exited with error during shutdown");
                        }
                    }
                    Err(e) => {
                        warn!(target: "base", error = %e, "Execution layer shutdown mechanism failed");
                    }
                }
                wait_for_execution_tasks(execution_task_executor).await?;
                if consensus_exit_is_unexpected(&consensus_result, &shutdown) {
                    warn!(
                        target: "base",
                        "Consensus layer exited cleanly before shutdown request; treating it as unexpected validator shutdown"
                    );
                    eyre::bail!("consensus layer exited unexpectedly");
                }
                consensus_result?;
            }
        }

        Ok(())
    }

    fn execution_node_config(
        &self,
        resolved: &ResolvedNodeConfig,
    ) -> NodeConfig<base_execution_chainspec::OpChainSpec> {
        let datadir = self.datadir.clone().map_or_else(DatadirArgs::default, |path| DatadirArgs {
            datadir: MaybePlatformPath::<DataDirPath>::from(path),
            ..DatadirArgs::default()
        });
        let metrics = MetricArgs { prometheus: self.metrics, ..MetricArgs::default() };

        let mut network = NetworkArgs::default();
        network.disable_tx_gossip = self.sequencer_rpc_url.is_some();
        network.trusted_peers = self.trusted_peers.clone();
        if let Some(port) = self.el_p2p_listen_tcp_port {
            network.port = port;
        }
        if let Some(port) = self.el_p2p_listen_udp_port.or(self.el_p2p_listen_tcp_port) {
            network.discovery.port = port;
            network.discovery.discv5_port = port;
        }

        let mut rpc = RpcServerArgs::default();
        if let Some(http) = self.http {
            rpc = rpc.with_http();
            rpc.http_addr = http.ip();
            rpc.http_port = http.port();
            rpc.http_api = Some(
                HTTP_RPC_MODULES
                    .parse::<RpcModuleSelection>()
                    .expect("static HTTP RPC module list should parse"),
            );
            rpc.http_corsdomain = Some("*".to_string());
        }
        rpc.ws = false;
        rpc.ws_api = None;
        rpc.ipcdisable = true;
        rpc.rpc_tx_fee_cap = 0;
        rpc.rpc_gas_cap = RPC_GAS_CAP;
        rpc.rpc_eth_proof_window = RPC_ETH_PROOF_WINDOW;

        let txpool = TxPoolArgs { no_locals: true, ..TxPoolArgs::default() };

        NodeConfig::new(Arc::clone(&resolved.chain_spec))
            .with_datadir_args(datadir)
            .with_metrics(metrics)
            .with_network(network)
            .with_rpc(rpc)
            .with_txpool(txpool)
    }

    fn create_in_memory_auth_jwt() -> eyre::Result<InMemoryAuthJwt> {
        let secret = JwtSecret::random();
        let mut file = NamedTempFile::new().wrap_err("failed to create auth JWT temp file")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = file
                .as_file()
                .metadata()
                .wrap_err("failed to stat auth JWT temp file")?
                .permissions();
            permissions.set_mode(0o600);
            file.as_file()
                .set_permissions(permissions)
                .wrap_err("failed to set auth JWT temp file permissions")?;
        }
        file.write_all(alloy_primitives::hex::encode(secret.as_bytes()).as_bytes())
            .wrap_err("failed to write auth JWT temp file")?;
        file.flush().wrap_err("failed to flush auth JWT temp file")?;
        let path = file.path().to_path_buf();

        Ok(InMemoryAuthJwt { secret, path, _file: file })
    }

    fn configure_execution_auth_ipc(
        node_config: &mut NodeConfig<base_execution_chainspec::OpChainSpec>,
        auth_jwt: &InMemoryAuthJwt,
    ) {
        node_config.rpc.auth_jwtsecret = Some(auth_jwt.path().to_path_buf());
        node_config.rpc.auth_ipc = true;
        node_config.rpc.auth_ipc_path =
            Self::execution_auth_ipc_path(node_config.datadir().data_dir()).display().to_string();
        node_config.rpc.disable_auth_server = false;
    }

    fn execution_auth_ipc_path(data_dir: &Path) -> PathBuf {
        data_dir.join("auth-engine.ipc")
    }
}

/// Request a graceful execution layer shutdown and wait for it to complete.
///
/// Returns `Err` if the shutdown mechanism itself failed (e.g. the completion channel was
/// dropped). Returns `Ok(Err)` if the execution layer exited with an error during shutdown.
/// Returns `Ok(Ok(()))` when the execution layer shut down cleanly.
async fn request_execution_shutdown(
    mut node_exit_future: std::pin::Pin<&mut NodeExitFuture>,
    engine_shutdown: EngineShutdown,
) -> eyre::Result<eyre::Result<()>> {
    if let Some(done_rx) = engine_shutdown.shutdown() {
        let result = tokio::select! {
            biased;
            res = &mut node_exit_future => {
                res.wrap_err("execution layer exited while shutting down")
            }
            res = done_rx => {
                res.wrap_err("execution shutdown completion channel dropped before completion")?;
                Ok(())
            }
        };
        return Ok(result);
    }

    Ok(Ok(()))
}

async fn wait_for_execution_tasks(task_executor: TaskExecutor) -> eyre::Result<()> {
    tokio::task::spawn_blocking(move || task_executor.graceful_shutdown())
        .await
        .wrap_err("execution task runtime failed to join during graceful shutdown")?;

    Ok(())
}

fn finish_consensus_task(
    result: Result<eyre::Result<()>, tokio::task::JoinError>,
    context: &'static str,
) -> eyre::Result<()> {
    match result {
        Ok(inner) => inner,
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => Err(eyre::eyre!(e).wrap_err(context)),
    }
}

fn consensus_exit_is_unexpected(
    consensus_result: &eyre::Result<()>,
    shutdown: &CancellationToken,
) -> bool {
    consensus_result.is_ok() && !shutdown.is_cancelled()
}

#[cfg(test)]
mod tests {
    use std::{fs, net::SocketAddr};

    use reth_network_peers::TrustedPeer;
    use reth_rpc_server_types::RethRpcModule;
    use tokio_util::sync::CancellationToken;
    use url::Url;

    use super::*;
    use crate::cli::tests::test_node_args;

    fn http_config(args: NodeArgs) -> NodeConfig<base_execution_chainspec::OpChainSpec> {
        let resolved = args.resolve().unwrap();
        args.execution_node_config(&resolved)
    }

    #[test]
    fn default_http_modules_exclude_base_and_miner() {
        let args = NodeArgs {
            http: Some("127.0.0.1:8545".parse::<SocketAddr>().unwrap()),
            ..test_node_args()
        };

        let http_api = http_config(args).rpc.http_api.unwrap();

        assert!(http_api.contains(&RethRpcModule::Admin));
        assert!(http_api.contains(&RethRpcModule::Debug));
        assert!(http_api.contains(&RethRpcModule::Eth));
        assert!(http_api.contains(&RethRpcModule::Net));
        assert!(http_api.contains(&RethRpcModule::Rpc));
        assert!(http_api.contains(&RethRpcModule::Txpool));
        assert!(http_api.contains(&RethRpcModule::Web3));
        assert!(!http_api.contains(&RethRpcModule::Other("base".into())));
        assert!(!http_api.contains(&RethRpcModule::Other("miner".into())));
    }

    #[test]
    fn keeps_tx_gossip_enabled_without_sequencer_rpc() {
        let args = test_node_args();

        let config = http_config(args);

        assert!(!config.network.disable_tx_gossip);
    }

    #[test]
    fn disables_tx_gossip_when_sequencer_rpc_is_configured() {
        let args = NodeArgs {
            sequencer_rpc_url: Some(Url::parse("http://localhost:9545").unwrap()),
            ..test_node_args()
        };

        let config = http_config(args);

        assert!(config.network.disable_tx_gossip);
    }

    #[test]
    fn enables_execution_discovery_by_default() {
        let args = test_node_args();

        let config = http_config(args);

        assert!(!config.network.discovery.disable_discovery);
    }

    #[test]
    fn persists_execution_peers_by_default() {
        let args = test_node_args();

        let config = http_config(args);

        assert!(!config.network.no_persist_peers);
    }

    #[test]
    fn keeps_configured_trusted_execution_peers() {
        let trusted_peer: TrustedPeer = "enode://20b871f3ced029e14472ec4ebc3c0448164942b123aa6af91a3386c1c403e0ebd3b4a5752a2b6c49e574619e6aa0549eb9ccd036b9bbc507e1f7f9712a236092@base-builder:7303"
            .parse()
            .unwrap();
        let args = NodeArgs { trusted_peers: vec![trusted_peer.clone()], ..test_node_args() };

        let config = http_config(args);

        assert_eq!(config.network.trusted_peers, vec![trusted_peer]);
    }

    #[test]
    fn keeps_stable_execution_ports_by_default() {
        let args = test_node_args();

        let config = http_config(args);
        let defaults = NetworkArgs::default();

        assert_eq!(config.network.port, defaults.port);
        assert_eq!(config.network.discovery.port, defaults.discovery.port);
        assert_eq!(config.network.discovery.discv5_port, defaults.discovery.discv5_port);
    }

    #[test]
    fn applies_explicit_execution_ports() {
        let args = NodeArgs {
            el_p2p_listen_tcp_port: Some(7303),
            el_p2p_listen_udp_port: Some(7304),
            ..test_node_args()
        };

        let config = http_config(args);

        assert_eq!(config.network.port, 7303);
        assert_eq!(config.network.discovery.port, 7304);
        assert_eq!(config.network.discovery.discv5_port, 7304);
    }

    #[test]
    fn execution_udp_port_defaults_to_execution_tcp_port() {
        let args = NodeArgs { el_p2p_listen_tcp_port: Some(7303), ..test_node_args() };

        let config = http_config(args);

        assert_eq!(config.network.port, 7303);
        assert_eq!(config.network.discovery.port, 7303);
        assert_eq!(config.network.discovery.discv5_port, 7303);
    }

    #[test]
    fn clean_consensus_exit_is_unexpected_without_shutdown_signal() {
        let shutdown = CancellationToken::new();

        assert!(consensus_exit_is_unexpected(&Ok(()), &shutdown));
    }

    #[test]
    fn clean_consensus_exit_is_allowed_during_shutdown() {
        let shutdown = CancellationToken::new();
        shutdown.cancel();

        assert!(!consensus_exit_is_unexpected(&Ok(()), &shutdown));
    }

    #[test]
    fn failed_consensus_exit_is_not_classified_as_clean_unexpected_exit() {
        let shutdown = CancellationToken::new();

        assert!(!consensus_exit_is_unexpected(&Err(eyre::eyre!("consensus failed")), &shutdown));
    }

    #[test]
    fn in_memory_auth_jwt_keeps_temp_file_alive_until_drop() {
        let auth_jwt = NodeArgs::create_in_memory_auth_jwt().unwrap();
        let path = auth_jwt.path().to_path_buf();

        assert!(path.is_file());

        drop(auth_jwt);

        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn in_memory_auth_jwt_uses_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let auth_jwt = NodeArgs::create_in_memory_auth_jwt().unwrap();
        let mode = fs::metadata(auth_jwt.path()).unwrap().permissions().mode() & 0o777;

        assert_eq!(mode, 0o600);
    }

    #[test]
    fn configures_execution_auth_ipc_under_datadir() {
        let mut config = http_config(test_node_args());
        let auth_jwt = NodeArgs::create_in_memory_auth_jwt().unwrap();
        let expected_ipc_path = config.datadir().data_dir().join("auth-engine.ipc");

        NodeArgs::configure_execution_auth_ipc(&mut config, &auth_jwt);

        assert!(config.rpc.auth_ipc);
        assert_eq!(PathBuf::from(&config.rpc.auth_ipc_path), expected_ipc_path);
        assert_eq!(config.rpc.auth_jwtsecret, Some(auth_jwt.path().to_path_buf()));
    }
}
