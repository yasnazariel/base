//! Node launch logic for the unified Base validator binary.

use std::{
    io::Write,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    sync::Arc,
};

use alloy_rpc_types_engine::JwtSecret;
use base_bundle_extension::BundleExtension;
use base_consensus_node::{EngineConfig, L1ConfigBuilder, NodeMode, RollupNodeBuilder};
use base_flashblocks_node::FlashblocksExtension;
use base_metering::MeteringExtension;
use base_node_runner::BaseNodeRunner;
use base_txpool_rpc::{TxPoolRpcConfig, TxPoolRpcExtension};
use base_txpool_tracing::{TxPoolExtension, TxpoolConfig};
use eyre::Context;
use reth_db::init_db;
use reth_node_builder::{NodeBuilder, NodeConfig};
use reth_node_core::{
    args::{DatadirArgs, MetricArgs, NetworkArgs, RpcServerArgs, TxPoolArgs},
    dirs::{DataDirPath, MaybePlatformPath},
};
use reth_rpc_server_types::RpcModuleSelection;
use reth_tasks::{RuntimeBuilder, RuntimeConfig, TokioConfig};
use tempfile::NamedTempFile;
use tracing::info;
use url::Url;

use crate::cli::{Cli, Command, NodeArgs, ResolvedNodeConfig};

const HTTP_RPC_MODULES: &str = "admin,debug,eth,net,rpc,txpool,web3";
const RPC_GAS_CAP: u64 = 600_000_000;
const RPC_ETH_PROOF_WINDOW: u64 = 1_209_600;

pub(crate) struct InMemoryAuthJwt {
    secret: JwtSecret,
    path: PathBuf,
    _file: NamedTempFile,
}

impl InMemoryAuthJwt {
    fn path(&self) -> &Path {
        &self.path
    }

    fn secret(&self) -> &JwtSecret {
        &self.secret
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
        base_cli_utils::RuntimeManager::run_until_ctrl_c(async move { self.run_inner().await })
    }

    async fn run_inner(self) -> eyre::Result<()> {
        let resolved = self.resolve()?;
        let task_executor = RuntimeBuilder::new(
            RuntimeConfig::default()
                .with_tokio(TokioConfig::existing_handle(tokio::runtime::Handle::current())),
        )
        .build()?;

        let mut node_config = self.execution_node_config(&resolved);
        let auth_jwt = Self::create_in_memory_auth_jwt()?;

        node_config.rpc.auth_jwtsecret = Some(auth_jwt.path().to_path_buf());
        node_config.rpc.disable_auth_server = false;
        node_config.rpc.auth_addr = IpAddr::V4(Ipv4Addr::LOCALHOST);
        node_config.rpc.auth_port = 0;

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
        let node_exit_future = node_handle.node_exit_future;

        let auth_http_url =
            Url::parse(&format!("http://{}", reth_node.auth_server_handle().local_addr()))
                .wrap_err("failed to build auth RPC URL")?;
        let rollup_config = resolved.rollup_config.clone();
        let engine_config = EngineConfig {
            config: Arc::new(rollup_config.clone()),
            l2_url: auth_http_url,
            l2_jwt_secret: auth_jwt.secret().clone(),
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
        .build();

        let mut consensus_task = tokio::spawn(async move {
            rollup_node
                .start()
                .await
                .map_err(|e| eyre::eyre!("failed to start rollup node service: {e}"))
        });

        tokio::pin!(node_exit_future);

        tokio::select! {
            res = &mut node_exit_future => {
                consensus_task.abort();
                res.wrap_err("execution layer exited")?;
            }
            res = &mut consensus_task => {
                res.wrap_err("consensus task failed to join")??;
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

        let mut network = NetworkArgs::default().with_unused_ports();
        network.discovery.disable_discovery = true;
        network.disable_tx_gossip = self.sequencer_rpc_url.is_some();
        network.no_persist_peers = true;
        network.trusted_peers = self.trusted_peers.clone();

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

        let mut txpool = TxPoolArgs::default();
        txpool.no_locals = true;

        NodeConfig::new(resolved.chain_spec.clone())
            .with_datadir_args(datadir)
            .with_metrics(metrics)
            .with_network(network)
            .with_rpc(rpc)
            .with_txpool(txpool)
    }

    fn create_in_memory_auth_jwt() -> eyre::Result<InMemoryAuthJwt> {
        let secret = JwtSecret::random();
        let mut file = NamedTempFile::new().wrap_err("failed to create auth JWT temp file")?;
        file.write_all(alloy_primitives::hex::encode(secret.as_bytes()).as_bytes())
            .wrap_err("failed to write auth JWT temp file")?;
        file.flush().wrap_err("failed to flush auth JWT temp file")?;
        let path = file.path().to_path_buf();

        Ok(InMemoryAuthJwt { secret, path, _file: file })
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use reth_rpc_server_types::RethRpcModule;

    use super::*;
    use crate::cli::LogArgs;

    fn test_node_args() -> NodeArgs {
        NodeArgs {
            network: None,
            l2_genesis: None,
            rollup_config: None,
            l1_config: None,
            datadir: None,
            http: None,
            metrics: None,
            op_rpc: None,
            l1_rpc_url: Url::parse("http://localhost:8545").unwrap(),
            l1_beacon_url: Url::parse("http://localhost:5052").unwrap(),
            l1_slot_duration: None,
            sequencer_rpc_url: None,
            flashblocks_url: None,
            unsafe_block_signer: None,
            bootnodes: Vec::new(),
            trusted_peers: Vec::new(),
            p2p_listen_tcp_port: None,
            p2p_listen_udp_port: None,
            p2p_advertise_ip: None,
            p2p_scoring: None,
            logging: LogArgs::default(),
        }
    }

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
}
