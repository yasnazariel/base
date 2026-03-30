//! Curated CLI for the unified Base validator binary.

use std::{
    fs::{self, File},
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    path::{Path, PathBuf},
    sync::Arc,
};

use alloy_genesis::Genesis;
use alloy_primitives::Address;
use base_alloy_chains::BaseChainConfig;
use base_cli_utils::LogConfig;
use base_client_cli::P2PArgs;
use base_consensus_genesis::{L1ChainConfig, Roles, RollupConfig};
use base_consensus_peers::PeerScoreLevel;
use base_consensus_registry::Registry;
use base_consensus_rpc::RpcBuilder;
use base_execution_chainspec::{BASE_MAINNET, BASE_SEPOLIA, OpChainSpec};
use base_flashblocks::FlashblocksConfig;
use base_metering::{MeteringConfig, MeteringResourceLimits};
use base_node_core::args::RollupArgs;
use clap::{Parser, ValueEnum};
use eyre::{Context, OptionExt};
use reth_network_peers::TrustedPeer;
use reth_tracing_otlp::OtlpProtocol;
use tracing::warn;
use tracing_subscriber::Layer as _;
use url::Url;

pub(crate) const DEFAULT_METERING_GAS_LIMIT: u64 = 30_000_000;
pub(crate) const DEFAULT_METERING_EXECUTION_TIME_US: u64 = 2_000_000;
pub(crate) const DEFAULT_METERING_STATE_ROOT_TIME_US: u64 = 200_000;
pub(crate) const DEFAULT_METERING_DA_BYTES: u64 = 786_430;
pub(crate) const DEFAULT_METERING_TARGET_FLASHBLOCKS_PER_BLOCK: usize = 10;
pub(crate) const DEFAULT_FLASHBLOCKS_PENDING_DEPTH: u64 = 5;

base_cli_utils::define_log_args!("BASE");

/// Top-level CLI for the curated `base` binary.
#[derive(Debug, Parser)]
#[command(
    version = env!("CARGO_PKG_VERSION"),
    propagate_version = true,
    disable_help_subcommand = true,
    subcommand_required = true,
    arg_required_else_help = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

/// Supported subcommands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum Command {
    /// Run a unified Base validator node.
    Node(NodeArgs),
}

/// Curated named networks supported by the unified binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub(crate) enum NamedNetwork {
    /// Base Mainnet.
    #[default]
    Base,
    /// Base Sepolia.
    #[value(name = "base-sepolia")]
    BaseSepolia,
}

impl NamedNetwork {
    pub(crate) const fn chain_config(self) -> &'static BaseChainConfig {
        match self {
            Self::Base => BaseChainConfig::mainnet(),
            Self::BaseSepolia => BaseChainConfig::sepolia(),
        }
    }

    pub(crate) fn chain_spec(self) -> Arc<OpChainSpec> {
        match self {
            Self::Base => BASE_MAINNET.clone(),
            Self::BaseSepolia => BASE_SEPOLIA.clone(),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::BaseSepolia => "base-sepolia",
        }
    }
}

impl core::fmt::Display for NamedNetwork {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where the execution and rollup configuration came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NetworkSource {
    Named(NamedNetwork),
    Files,
}

impl core::fmt::Display for NetworkSource {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Named(network) => network.fmt(f),
            Self::Files => f.write_str("files"),
        }
    }
}

/// Fully resolved configuration for launching the unified validator.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedNodeConfig {
    pub(crate) chain_spec: Arc<OpChainSpec>,
    pub(crate) rollup_config: RollupConfig,
    pub(crate) l1_config: L1ChainConfig,
    pub(crate) consensus_bootnodes: Vec<String>,
    pub(crate) unsafe_block_signer: Option<Address>,
    pub(crate) source: NetworkSource,
}

/// Validator-only node arguments for the unified binary.
#[derive(Debug, Clone, clap::Args)]
pub(crate) struct NodeArgs {
    /// Named Base network preset. Omit this and provide explicit config files instead.
    #[arg(long, value_enum)]
    pub(crate) network: Option<NamedNetwork>,

    /// Explicit L2 genesis file for file-based deployments.
    #[arg(long, value_name = "PATH", conflicts_with = "network")]
    pub(crate) l2_genesis: Option<PathBuf>,

    /// Explicit rollup config file for file-based deployments.
    #[arg(long, value_name = "PATH", conflicts_with = "network")]
    pub(crate) rollup_config: Option<PathBuf>,

    /// Explicit L1 chain config file for file-based deployments.
    #[arg(long, value_name = "PATH", conflicts_with = "network")]
    pub(crate) l1_config: Option<PathBuf>,

    /// Base data directory. The binary still stores data in the normal chain-specific subdir.
    #[arg(long, value_name = "PATH")]
    pub(crate) datadir: Option<PathBuf>,

    /// Public HTTP RPC listener. Disabled when omitted.
    #[arg(long, value_name = "ADDR")]
    pub(crate) http: Option<SocketAddr>,

    /// Prometheus metrics listener. Disabled when omitted.
    #[arg(long, value_name = "ADDR")]
    pub(crate) metrics: Option<SocketAddr>,

    /// Rollup RPC listener for `optimism_*` endpoints. Disabled when omitted.
    #[arg(long, value_name = "ADDR")]
    pub(crate) op_rpc: Option<SocketAddr>,

    /// L1 execution RPC URL.
    #[arg(long, value_name = "URL")]
    pub(crate) l1_rpc_url: Url,

    /// L1 beacon API URL.
    #[arg(long, value_name = "URL")]
    pub(crate) l1_beacon_url: Url,

    /// Optional fixed L1 slot duration override, in seconds.
    #[arg(long, value_name = "SECONDS")]
    pub(crate) l1_slot_duration: Option<u64>,

    /// Optional upstream sequencer HTTP RPC URL.
    #[arg(long, value_name = "URL")]
    pub(crate) sequencer_rpc_url: Option<Url>,

    /// Optional flashblocks websocket URL.
    #[arg(long, value_name = "URL")]
    pub(crate) flashblocks_url: Option<Url>,

    /// Optional unsafe block signer override for consensus gossip validation.
    #[arg(long, value_name = "ADDRESS")]
    pub(crate) unsafe_block_signer: Option<Address>,

    /// Consensus bootnodes for file-based deployments.
    #[arg(long, value_name = "ENODE")]
    pub(crate) bootnodes: Vec<String>,

    /// Trusted execution-layer peers for the embedded reth node.
    #[arg(
        long = "trusted-peers",
        env = "BASE_TRUSTED_PEERS",
        value_delimiter = ',',
        value_name = "ENODE"
    )]
    pub(crate) trusted_peers: Vec<TrustedPeer>,

    /// Optional fixed TCP port for the embedded execution P2P stack.
    #[arg(long = "el-p2p.listen.tcp", env = "BASE_EL_P2P_LISTEN_TCP_PORT", value_name = "PORT")]
    pub(crate) el_p2p_listen_tcp_port: Option<u16>,

    /// Optional fixed UDP port for the embedded execution discovery stack.
    #[arg(long = "el-p2p.listen.udp", env = "BASE_EL_P2P_LISTEN_UDP_PORT", value_name = "PORT")]
    pub(crate) el_p2p_listen_udp_port: Option<u16>,

    /// Optional fixed TCP port for the embedded consensus P2P stack.
    #[arg(long = "p2p.listen.tcp", env = "BASE_P2P_LISTEN_TCP_PORT", value_name = "PORT")]
    pub(crate) p2p_listen_tcp_port: Option<u16>,

    /// Optional fixed UDP port for the embedded consensus discovery stack.
    #[arg(long = "p2p.listen.udp", env = "BASE_P2P_LISTEN_UDP_PORT", value_name = "PORT")]
    pub(crate) p2p_listen_udp_port: Option<u16>,

    /// Optional IP address or hostname to advertise for the embedded consensus P2P stack.
    #[arg(long = "p2p.advertise.ip", env = "BASE_P2P_ADVERTISE_IP", value_name = "IP_OR_HOST")]
    pub(crate) p2p_advertise_ip: Option<String>,

    /// Optional peer scoring level for the embedded consensus P2P stack.
    #[arg(long = "p2p.scoring", env = "BASE_P2P_SCORING", value_name = "LEVEL")]
    pub(crate) p2p_scoring: Option<PeerScoreLevel>,

    /// Logging configuration.
    #[command(flatten)]
    pub(crate) logging: LogArgs,

    /// OTLP tracing endpoint URL (e.g. `http://localhost:4318/v1/traces`).
    #[arg(
        long = "tracing-otlp",
        env = "BASE_OTLP_ENDPOINT",
        global = true,
        value_name = "URL",
        help_heading = "Tracing"
    )]
    pub(crate) otlp_endpoint: Option<Url>,

    /// Filter directive for the OTLP tracer (same syntax as `RUST_LOG`).
    #[arg(
        long = "tracing-otlp.filter",
        env = "BASE_OTLP_FILTER",
        global = true,
        value_name = "FILTER",
        default_value = "debug",
        help_heading = "Tracing"
    )]
    pub(crate) otlp_filter: tracing_subscriber::EnvFilter,

    /// OTLP transport protocol.
    #[arg(
        long = "tracing-otlp-protocol",
        env = "BASE_OTLP_PROTOCOL",
        global = true,
        value_name = "PROTOCOL",
        default_value = "http",
        help_heading = "Tracing"
    )]
    pub(crate) otlp_protocol: OtlpProtocol,

    /// OTLP trace sampling ratio (0.0 to 1.0).
    #[arg(
        long = "tracing-otlp.sample-ratio",
        env = "BASE_OTLP_SAMPLE_RATIO",
        global = true,
        value_name = "RATIO",
        help_heading = "Tracing"
    )]
    pub(crate) otlp_sample_ratio: Option<f64>,
}

impl NodeArgs {
    pub(crate) fn init_logging(&self) -> eyre::Result<()> {
        let log_config = LogConfig::from(self.logging.clone());

        let filter = tracing_subscriber::EnvFilter::builder()
            .with_default_directive(log_config.global_level.into())
            .from_env_lossy()
            .add_directive("discv5=error".parse().expect("valid directive"));

        let extra = self.build_otlp_layer();

        log_config.init_tracing_subscriber_with_layers(filter, extra)?;

        Ok(())
    }

    fn build_otlp_layer(
        &self,
    ) -> Vec<Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>> {
        let mut layers: Vec<
            Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>,
        > = Vec::new();

        if let Some(endpoint) = &self.otlp_endpoint {
            let service_name =
                std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "base".to_string());

            let config = reth_tracing_otlp::OtlpConfig::new(
                service_name,
                endpoint.clone(),
                self.otlp_protocol,
                self.otlp_sample_ratio,
            );
            match config.and_then(|c| {
                let ep = c.endpoint().clone();
                reth_tracing_otlp::span_layer(c)
                    .map(|layer| (layer, ep))
                    .map_err(|e| eyre::eyre!(e))
            }) {
                Ok((layer, ep)) => {
                    let filtered = layer.with_filter(self.otlp_filter.clone()).boxed();
                    eprintln!("Started OTLP tracing export to {ep}");
                    layers.push(filtered);
                }
                Err(e) => {
                    eprintln!("Failed to initialize OTLP tracing: {e}");
                }
            }
        }

        layers
    }

    pub(crate) fn resolve(&self) -> eyre::Result<ResolvedNodeConfig> {
        if self.file_mode_requested() {
            return self.resolve_from_files();
        }
        self.resolve_named_network(self.network.unwrap_or_default())
    }

    pub(crate) fn consensus_p2p_args(
        &self,
        resolved: &ResolvedNodeConfig,
        data_dir: &Path,
    ) -> eyre::Result<P2PArgs> {
        let defaults = P2PArgs::default();
        let mut p2p = P2PArgs {
            priv_path: Some(data_dir.join("consensus-p2p.key")),
            listen_tcp_port: self.p2p_listen_tcp_port.unwrap_or(defaults.listen_tcp_port),
            listen_udp_port: self
                .p2p_listen_udp_port
                .or(self.p2p_listen_tcp_port)
                .unwrap_or(defaults.listen_udp_port),
            bootnodes: resolved.consensus_bootnodes.clone(),
            unsafe_block_signer: resolved.unsafe_block_signer,
            ..defaults
        };
        if let Some(advertise_ip) = self.p2p_advertise_ip.as_deref() {
            p2p.advertise_ip = Some(resolve_host(advertise_ip)?);
        }
        if let Some(scoring) = self.p2p_scoring {
            p2p.scoring = scoring;
        }
        Ok(p2p)
    }

    pub(crate) fn rollup_args(&self) -> RollupArgs {
        RollupArgs {
            sequencer: self.sequencer_rpc_url.as_ref().map(ToString::to_string),
            disable_txpool_gossip: self.sequencer_rpc_url.is_some(),
            ..RollupArgs::default()
        }
    }

    pub(crate) fn flashblocks_config(&self) -> Option<FlashblocksConfig> {
        self.flashblocks_url.clone().map(|url| {
            let mut config = FlashblocksConfig::new(url, DEFAULT_FLASHBLOCKS_PENDING_DEPTH);
            config.cached_execution = false;
            config
        })
    }

    pub(crate) fn rollup_rpc_config(&self) -> Option<RpcBuilder> {
        self.op_rpc.map(|socket| RpcBuilder {
            no_restart: false,
            socket,
            enable_admin: false,
            admin_persistence: None,
            ws_enabled: false,
            dev_enabled: false,
        })
    }

    pub(crate) fn metering_config(
        &self,
        flashblocks_config: Option<FlashblocksConfig>,
    ) -> MeteringConfig {
        let resource_limits = MeteringResourceLimits {
            gas_limit: Some(DEFAULT_METERING_GAS_LIMIT),
            execution_time_us: Some(DEFAULT_METERING_EXECUTION_TIME_US),
            state_root_time_us: Some(DEFAULT_METERING_STATE_ROOT_TIME_US),
            da_bytes: Some(DEFAULT_METERING_DA_BYTES),
        };

        flashblocks_config
            .map_or_else(MeteringConfig::enabled, MeteringConfig::with_flashblocks)
            .with_resource_limits(resource_limits)
            .with_target_flashblocks_per_block(DEFAULT_METERING_TARGET_FLASHBLOCKS_PER_BLOCK)
    }

    fn resolve_named_network(&self, network: NamedNetwork) -> eyre::Result<ResolvedNodeConfig> {
        if !self.bootnodes.is_empty() {
            eyre::bail!("--bootnodes cannot be combined with --network");
        }

        let chain_cfg = network.chain_config();
        let rollup_config = Registry::rollup_config(chain_cfg.chain_id)
            .cloned()
            .ok_or_eyre(format!("missing rollup config for {network}"))?;
        let l1_config = Registry::l1_config(chain_cfg.l1_chain_id)
            .cloned()
            .ok_or_eyre(format!("missing L1 config for {network}"))?;

        Ok(ResolvedNodeConfig {
            chain_spec: network.chain_spec(),
            rollup_config,
            l1_config,
            consensus_bootnodes: chain_cfg.bootnodes.iter().map(ToString::to_string).collect(),
            unsafe_block_signer: resolve_unsafe_block_signer(
                self.unsafe_block_signer,
                None,
                chain_cfg.chain_id,
            ),
            source: NetworkSource::Named(network),
        })
    }

    fn resolve_from_files(&self) -> eyre::Result<ResolvedNodeConfig> {
        if self.network.is_some() {
            eyre::bail!(
                "--network cannot be combined with --l2-genesis, --rollup-config, or --l1-config"
            );
        }

        let mut missing = Vec::new();
        if self.l2_genesis.is_none() {
            missing.push("--l2-genesis");
        }
        if self.rollup_config.is_none() {
            missing.push("--rollup-config");
        }
        if self.l1_config.is_none() {
            missing.push("--l1-config");
        }
        if !missing.is_empty() {
            eyre::bail!(
                "file-based mode requires explicit config files: missing {}",
                missing.join(", ")
            );
        }

        let l2_genesis_path = self.l2_genesis.as_ref().ok_or_eyre("missing --l2-genesis")?;
        let rollup_config_path =
            self.rollup_config.as_ref().ok_or_eyre("missing --rollup-config")?;
        let l1_config_path = self.l1_config.as_ref().ok_or_eyre("missing --l1-config")?;

        let chain_spec = Arc::new(OpChainSpec::from_genesis(read_genesis(l2_genesis_path)?));
        let rollup_config = read_rollup_config(rollup_config_path)?;
        let (l1_config, file_unsafe_block_signer) = read_l1_config(l1_config_path)?;

        eyre::ensure!(
            rollup_config.l2_chain_id.id() == chain_spec.inner.chain.id(),
            "execution chain ID {} does not match rollup config chain ID {}",
            chain_spec.inner.chain.id(),
            rollup_config.l2_chain_id.id()
        );
        eyre::ensure!(
            l1_config.chain_id == rollup_config.l1_chain_id,
            "L1 chain ID {} does not match rollup config L1 chain ID {}",
            l1_config.chain_id,
            rollup_config.l1_chain_id
        );
        let unsafe_block_signer = resolve_unsafe_block_signer(
            self.unsafe_block_signer,
            file_unsafe_block_signer,
            rollup_config.l2_chain_id.id(),
        );

        Ok(ResolvedNodeConfig {
            unsafe_block_signer,
            consensus_bootnodes: self.bootnodes.clone(),
            chain_spec,
            rollup_config,
            l1_config,
            source: NetworkSource::Files,
        })
    }

    const fn file_mode_requested(&self) -> bool {
        self.l2_genesis.is_some() || self.rollup_config.is_some() || self.l1_config.is_some()
    }
}

fn resolve_unsafe_block_signer(
    cli_unsafe_block_signer: Option<Address>,
    file_unsafe_block_signer: Option<Address>,
    l2_chain_id: u64,
) -> Option<Address> {
    cli_unsafe_block_signer
        .or(file_unsafe_block_signer)
        .or_else(|| Registry::unsafe_block_signer(l2_chain_id))
}

fn resolve_host(host: &str) -> eyre::Result<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }

    let socket_addr = format!("{host}:0");
    socket_addr
        .to_socket_addrs()
        .wrap_err_with(|| format!("failed to resolve {host}"))?
        .next()
        .map(|addr| addr.ip())
        .ok_or_else(|| eyre::eyre!("DNS resolution for {host} returned no addresses"))
}

#[derive(Debug, serde::Deserialize)]
struct L1ConfigRolesEnvelope {
    #[serde(rename = "Roles", alias = "roles")]
    roles: Option<Roles>,
}

fn read_l1_config(path: &Path) -> eyre::Result<(L1ChainConfig, Option<Address>)> {
    let bytes =
        fs::read(path).wrap_err_with(|| format!("failed to read L1 config {}", path.display()))?;
    let l1_config = serde_json::from_slice(&bytes)
        .wrap_err_with(|| format!("failed to parse L1 config {}", path.display()))?;

    let file_unsafe_block_signer = match serde_json::from_slice::<L1ConfigRolesEnvelope>(&bytes) {
        Ok(config) => config.roles.and_then(|roles| roles.unsafe_block_signer),
        Err(error) => {
            warn!(
                path = %path.display(),
                error = %error,
                "L1 config roles did not parse; unsafe_block_signer not extracted"
            );
            None
        }
    };

    Ok((l1_config, file_unsafe_block_signer))
}

fn read_genesis(path: &Path) -> eyre::Result<Genesis> {
    let file = File::open(path)
        .wrap_err_with(|| format!("failed to open L2 genesis {}", path.display()))?;
    serde_json::from_reader(file)
        .wrap_err_with(|| format!("failed to parse L2 genesis {}", path.display()))
}

fn read_rollup_config(path: &Path) -> eyre::Result<RollupConfig> {
    let file = File::open(path)
        .wrap_err_with(|| format!("failed to open rollup config {}", path.display()))?;
    serde_json::from_reader(file)
        .wrap_err_with(|| format!("failed to parse rollup config {}", path.display()))
}

#[cfg(test)]
pub(crate) mod tests {
    use clap::Parser;
    use tempfile::NamedTempFile;

    use super::*;

    pub(crate) fn test_node_args() -> NodeArgs {
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
            el_p2p_listen_tcp_port: None,
            el_p2p_listen_udp_port: None,
            p2p_listen_tcp_port: None,
            p2p_listen_udp_port: None,
            p2p_advertise_ip: None,
            p2p_scoring: None,
            logging: LogArgs::default(),
            otlp_endpoint: None,
            otlp_filter: tracing_subscriber::EnvFilter::from_default_env(),
            otlp_protocol: OtlpProtocol::Http,
            otlp_sample_ratio: None,
        }
    }

    #[test]
    fn parses_base_node_command() {
        let cli = Cli::parse_from([
            "base",
            "node",
            "--l1-rpc-url",
            "http://localhost:8545",
            "--l1-beacon-url",
            "http://localhost:5052",
        ]);

        let Command::Node(args) = cli.command;
        assert_eq!(args.network, None);
        assert_eq!(args.http, None);
        assert_eq!(args.metrics, None);
        assert_eq!(args.op_rpc, None);
        assert_eq!(args.el_p2p_listen_tcp_port, None);
        assert_eq!(args.p2p_listen_tcp_port, None);
        assert_eq!(args.p2p_advertise_ip, None);
        assert!(args.trusted_peers.is_empty());
    }

    #[test]
    fn defaults_to_base_named_network() {
        let args = test_node_args();

        let resolved = args.resolve().unwrap();
        assert_eq!(resolved.source, NetworkSource::Named(NamedNetwork::Base));
        assert_eq!(resolved.rollup_config.l2_chain_id.id(), 8453);
    }

    #[test]
    fn rejects_partial_file_mode() {
        let args =
            NodeArgs { l2_genesis: Some(PathBuf::from("/tmp/genesis.json")), ..test_node_args() };

        let err = args.resolve().unwrap_err().to_string();
        assert!(err.contains("--rollup-config"));
        assert!(err.contains("--l1-config"));
    }

    #[test]
    fn rejects_named_network_with_file_mode() {
        let args = NodeArgs {
            network: Some(NamedNetwork::Base),
            l2_genesis: Some(PathBuf::from("/tmp/genesis.json")),
            rollup_config: Some(PathBuf::from("/tmp/rollup.json")),
            l1_config: Some(PathBuf::from("/tmp/l1.json")),
            ..test_node_args()
        };

        let err = args.resolve().unwrap_err().to_string();
        assert!(err.contains("--network cannot be combined"));
    }

    #[test]
    fn rejects_bootnodes_with_named_network() {
        let args = NodeArgs {
            network: Some(NamedNetwork::Base),
            bootnodes: vec!["enode://node@example:9000".to_string()],
            ..test_node_args()
        };

        let err = args.resolve().unwrap_err().to_string();
        assert!(err.contains("--bootnodes cannot be combined with --network"));
    }

    #[test]
    fn parses_optional_rollup_rpc_listener() {
        let cli = Cli::parse_from([
            "base",
            "node",
            "--op-rpc",
            "127.0.0.1:9549",
            "--l1-rpc-url",
            "http://localhost:8545",
            "--l1-beacon-url",
            "http://localhost:5052",
        ]);

        let Command::Node(args) = cli.command;
        assert_eq!(args.op_rpc, Some("127.0.0.1:9549".parse().unwrap()));
    }

    #[test]
    fn parses_execution_trusted_peers() {
        let cli = Cli::parse_from([
            "base",
            "node",
            "--trusted-peers",
            "enode://20b871f3ced029e14472ec4ebc3c0448164942b123aa6af91a3386c1c403e0ebd3b4a5752a2b6c49e574619e6aa0549eb9ccd036b9bbc507e1f7f9712a236092@base-builder:7303",
            "--l1-rpc-url",
            "http://localhost:8545",
            "--l1-beacon-url",
            "http://localhost:5052",
        ]);

        let Command::Node(args) = cli.command;
        assert_eq!(args.trusted_peers.len(), 1);
        assert_eq!(
            args.trusted_peers[0].to_string(),
            "enode://20b871f3ced029e14472ec4ebc3c0448164942b123aa6af91a3386c1c403e0ebd3b4a5752a2b6c49e574619e6aa0549eb9ccd036b9bbc507e1f7f9712a236092@base-builder:7303"
        );
    }

    #[test]
    fn parses_execution_p2p_ports() {
        let cli = Cli::parse_from([
            "base",
            "node",
            "--el-p2p.listen.tcp",
            "7303",
            "--el-p2p.listen.udp",
            "7304",
            "--l1-rpc-url",
            "http://localhost:8545",
            "--l1-beacon-url",
            "http://localhost:5052",
        ]);

        let Command::Node(args) = cli.command;
        assert_eq!(args.el_p2p_listen_tcp_port, Some(7303));
        assert_eq!(args.el_p2p_listen_udp_port, Some(7304));
    }

    #[test]
    fn consensus_p2p_args_apply_explicit_overrides() {
        let args = NodeArgs {
            p2p_listen_tcp_port: Some(8003),
            p2p_listen_udp_port: Some(8003),
            p2p_advertise_ip: Some("127.0.0.1".to_string()),
            p2p_scoring: Some(PeerScoreLevel::Off),
            ..test_node_args()
        };
        let resolved = ResolvedNodeConfig {
            chain_spec: BASE_MAINNET.clone(),
            rollup_config: Registry::rollup_config(8453).unwrap().clone(),
            l1_config: Registry::l1_config(1).unwrap().clone(),
            consensus_bootnodes: vec!["enode://node@example:9000".to_string()],
            unsafe_block_signer: Registry::unsafe_block_signer(8453),
            source: NetworkSource::Files,
        };

        let p2p = args.consensus_p2p_args(&resolved, Path::new("/tmp")).unwrap();
        assert_eq!(p2p.listen_tcp_port, 8003);
        assert_eq!(p2p.listen_udp_port, 8003);
        assert_eq!(p2p.advertise_ip, Some("127.0.0.1".parse().unwrap()));
        assert_eq!(p2p.scoring, PeerScoreLevel::Off);
        assert_eq!(p2p.bootnodes, resolved.consensus_bootnodes);
    }

    #[test]
    fn consensus_p2p_args_preserve_default_ports_when_not_specified() {
        let args = NodeArgs { p2p_advertise_ip: Some("127.0.0.1".to_string()), ..test_node_args() };
        let resolved = ResolvedNodeConfig {
            chain_spec: BASE_MAINNET.clone(),
            rollup_config: Registry::rollup_config(8453).unwrap().clone(),
            l1_config: Registry::l1_config(1).unwrap().clone(),
            consensus_bootnodes: vec![],
            unsafe_block_signer: Registry::unsafe_block_signer(8453),
            source: NetworkSource::Files,
        };

        let p2p = args.consensus_p2p_args(&resolved, Path::new("/tmp")).unwrap();
        let defaults = P2PArgs::default();
        assert_eq!(p2p.listen_tcp_port, defaults.listen_tcp_port);
        assert_eq!(p2p.listen_udp_port, defaults.listen_udp_port);
        assert_eq!(p2p.advertise_ip, Some("127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn resolve_unsafe_block_signer_prefers_cli_over_file_and_registry() {
        let file_signer: Address = "0xa95B83e39AA78B00F12fe431865B563793D97AF5".parse().unwrap();
        let cli_signer: Address = "0x19CC7073150D9f5888f09E0e9016d2a39667df14".parse().unwrap();
        let registry_signer = BaseChainConfig::mainnet().unsafe_block_signer.unwrap();

        assert_eq!(
            resolve_unsafe_block_signer(Some(cli_signer), Some(file_signer), 8453),
            Some(cli_signer)
        );
        assert_ne!(cli_signer, registry_signer);
    }

    #[test]
    fn resolve_unsafe_block_signer_prefers_file_over_registry() {
        let file_signer: Address = "0xa95B83e39AA78B00F12fe431865B563793D97AF5".parse().unwrap();

        assert_eq!(resolve_unsafe_block_signer(None, Some(file_signer), 8453), Some(file_signer));
    }

    #[test]
    fn rollup_args_disable_txpool_gossip_when_sequencer_rpc_is_configured() {
        let args = NodeArgs {
            sequencer_rpc_url: Some(Url::parse("http://localhost:9545").unwrap()),
            ..test_node_args()
        };

        let rollup_args = args.rollup_args();

        assert!(rollup_args.disable_txpool_gossip);
        assert_eq!(rollup_args.sequencer, Some("http://localhost:9545/".to_string()));
    }

    #[test]
    fn read_l1_config_extracts_unsafe_block_signer_from_same_bytes() {
        let signer: Address = "0xa95B83e39AA78B00F12fe431865B563793D97AF5".parse().unwrap();
        let config = L1ChainConfig { chain_id: 1, ..L1ChainConfig::default() };

        let mut json = serde_json::to_value(&config).unwrap();
        json.as_object_mut().unwrap().insert(
            "Roles".to_string(),
            serde_json::json!({
                "UnsafeBlockSigner": format!("{signer:#x}")
            }),
        );

        let file = NamedTempFile::new().unwrap();
        fs::write(file.path(), serde_json::to_vec(&json).unwrap()).unwrap();

        let (parsed, extracted) = read_l1_config(file.path()).unwrap();

        assert_eq!(parsed.chain_id, config.chain_id);
        assert_eq!(extracted, Some(signer));
    }

    #[test]
    fn read_l1_config_returns_none_without_roles() {
        let config = L1ChainConfig { chain_id: 11_155_111, ..L1ChainConfig::default() };

        let file = NamedTempFile::new().unwrap();
        fs::write(file.path(), serde_json::to_vec(&config).unwrap()).unwrap();

        let (parsed, extracted) = read_l1_config(file.path()).unwrap();

        assert_eq!(parsed.chain_id, config.chain_id);
        assert_eq!(extracted, None);
    }
}
