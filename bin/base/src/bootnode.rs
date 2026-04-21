use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use base_bootnode::{
    Bootnode, ClBootnode, ClBootnodeConfig, DEFAULT_CL_BOOTNODE_PORT, DEFAULT_EL_BOOTNODE_PORT,
    ElBootnode, ElBootnodeConfig,
};
use base_cli_utils::RuntimeManager;
use clap::Args;
use reth_net_nat::NatResolver;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::config::ResolvedChainConfig;

/// Arguments for `base bootnode`.
#[derive(Args, Clone, Debug)]
#[command(next_help_heading = "Bootnode")]
pub struct BootnodeArgs {
    /// Skip starting the execution-layer (reth) bootnode.
    #[arg(long = "no-el", env = "BASE_BOOTNODE_NO_EL", global = false)]
    pub no_el: bool,

    /// Skip starting the consensus-layer (base-consensus) bootnode.
    #[arg(long = "no-cl", env = "BASE_BOOTNODE_NO_CL", global = false)]
    pub no_cl: bool,

    #[command(flatten)]
    pub el: ElArgs,

    #[command(flatten)]
    pub cl: ClArgs,
}

/// Execution-layer bootnode arguments.
#[derive(Args, Clone, Debug)]
#[command(next_help_heading = "Bootnode (Execution Layer)")]
pub struct ElArgs {
    /// Combined UDP/TCP listen address for the EL discovery service.
    #[arg(
        id = "el_addr",
        long = "el.addr",
        value_name = "ADDR",
        env = "BASE_BOOTNODE_EL_ADDR",
        default_value_t = ElArgs::default_addr(),
    )]
    pub addr: SocketAddr,

    /// Path to a hex-encoded secp256k1 secret key for the EL ENR. Generated
    /// and persisted at this path if the file does not exist.
    #[arg(
        id = "el_secret_key",
        long = "el.secret-key",
        value_name = "PATH",
        env = "BASE_BOOTNODE_EL_SECRET_KEY"
    )]
    pub secret_key: Option<PathBuf>,

    /// Strategy for resolving the externally-advertised EL IP.
    #[arg(
        id = "el_nat",
        long = "el.nat",
        value_name = "RESOLVER",
        env = "BASE_BOOTNODE_EL_NAT",
        default_value = "any"
    )]
    pub nat: NatResolver,

    /// Disable the EL discv5 service (discv4 still runs).
    #[arg(id = "el_no_discv5", long = "el.no-discv5", env = "BASE_BOOTNODE_EL_NO_DISCV5")]
    pub no_discv5: bool,
}

/// Consensus-layer bootnode arguments.
#[derive(Args, Clone, Debug)]
#[command(next_help_heading = "Bootnode (Consensus Layer)")]
pub struct ClArgs {
    /// IP to bind the CL discv5 socket to.
    #[arg(
        id = "cl_listen_ip",
        long = "cl.listen-ip",
        value_name = "IP",
        env = "BASE_BOOTNODE_CL_LISTEN_IP",
        default_value_t = IpAddr::V4(Ipv4Addr::UNSPECIFIED),
    )]
    pub listen_ip: IpAddr,

    /// UDP port to bind the CL discv5 socket to.
    #[arg(
        id = "cl_listen_port",
        long = "cl.listen-port",
        value_name = "PORT",
        env = "BASE_BOOTNODE_CL_LISTEN_PORT",
        default_value_t = DEFAULT_CL_BOOTNODE_PORT,
    )]
    pub listen_port: u16,

    /// IP to advertise in the local CL ENR. Defaults to `--cl.listen-ip`.
    #[arg(
        id = "cl_advertise_ip",
        long = "cl.advertise-ip",
        value_name = "IP",
        env = "BASE_BOOTNODE_CL_ADVERTISE_IP"
    )]
    pub advertise_ip: Option<IpAddr>,

    /// TCP port to advertise in the local CL ENR. Defaults to `--cl.listen-port`.
    #[arg(
        id = "cl_advertise_tcp",
        long = "cl.advertise-tcp",
        value_name = "PORT",
        env = "BASE_BOOTNODE_CL_ADVERTISE_TCP"
    )]
    pub advertise_tcp: Option<u16>,

    /// UDP port to advertise in the local CL ENR. Defaults to `--cl.listen-port`.
    #[arg(
        id = "cl_advertise_udp",
        long = "cl.advertise-udp",
        value_name = "PORT",
        env = "BASE_BOOTNODE_CL_ADVERTISE_UDP"
    )]
    pub advertise_udp: Option<u16>,

    /// Path to a hex-encoded secp256k1 secret key for the CL ENR. Generated
    /// and persisted at this path if the file does not exist.
    #[arg(
        id = "cl_secret_key",
        long = "cl.secret-key",
        value_name = "PATH",
        env = "BASE_BOOTNODE_CL_SECRET_KEY"
    )]
    pub secret_key: Option<PathBuf>,

    /// Override the on-disk bootstore path. Defaults to
    /// `~/.base/<chain_id>/bootstore.json`.
    #[arg(
        id = "cl_bootstore",
        long = "cl.bootstore",
        value_name = "PATH",
        env = "BASE_BOOTNODE_CL_BOOTSTORE"
    )]
    pub bootstore: Option<PathBuf>,

    /// User-supplied bootnodes (`enr:...` or `enode://...`). When provided,
    /// these replace the chain default list. Repeat the flag for multiple.
    #[arg(
        id = "cl_bootnodes",
        long = "cl.bootnode",
        value_name = "BOOTNODE",
        env = "BASE_BOOTNODE_CL_BOOTNODE"
    )]
    pub bootnodes: Vec<String>,

    /// Disable ENR auto-update so the advertised CL IP is static.
    #[arg(id = "cl_static_ip", long = "cl.static-ip", env = "BASE_BOOTNODE_CL_STATIC_IP")]
    pub static_ip: bool,
}

impl ElArgs {
    /// Default combined UDP/TCP listen address for the EL discovery service.
    pub const fn default_addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_EL_BOOTNODE_PORT)
    }
}

impl From<ElArgs> for ElBootnodeConfig {
    fn from(args: ElArgs) -> Self {
        Self {
            addr: args.addr,
            secret_key_path: args.secret_key,
            nat: args.nat,
            enable_v5: !args.no_discv5,
        }
    }
}

impl ClArgs {
    /// Returns the IP that will end up in the local CL ENR.
    pub fn effective_advertise_ip(&self) -> IpAddr {
        self.advertise_ip.unwrap_or(self.listen_ip)
    }

    /// Fails when the effective advertise IP is unspecified (`0.0.0.0` / `::`), since
    /// publishing such an ENR results in undialable peers.
    pub fn validate_advertise_ip(&self) -> eyre::Result<()> {
        let ip = self.effective_advertise_ip();
        if ip.is_unspecified() {
            return Err(eyre::eyre!(
                "CL advertise IP is unspecified ({ip}); pass --cl.advertise-ip with a routable address"
            ));
        }
        Ok(())
    }

    pub fn into_config(self, chain_id: u64) -> ClBootnodeConfig {
        let advertise_ip = self.effective_advertise_ip();
        let advertise_tcp = self.advertise_tcp.unwrap_or(self.listen_port);
        let advertise_udp = self.advertise_udp.unwrap_or(self.listen_port);
        ClBootnodeConfig {
            chain_id,
            listen_ip: self.listen_ip,
            listen_udp_port: self.listen_port,
            advertise_ip,
            advertise_tcp_port: advertise_tcp,
            advertise_udp_port: advertise_udp,
            secret_key_path: self.secret_key,
            bootstore_path: self.bootstore,
            bootnodes: self.bootnodes,
            static_ip: self.static_ip,
        }
    }
}

impl BootnodeArgs {
    /// Runs the bootnode subcommand against the resolved chain.
    pub fn run(self, chain: ResolvedChainConfig) -> eyre::Result<()> {
        if self.no_el && self.no_cl {
            return Err(eyre::eyre!("--no-el and --no-cl cannot both be set"));
        }

        // Fail-fast on the unroutable-advertise-IP misconfiguration before we spin up a runtime;
        // `ClBootnode::run` does the same check, but catching it here gives a cleaner error.
        if !self.no_cl {
            self.cl.validate_advertise_ip()?;
        }

        let chain_id = chain.l2_chain_id;
        let mut bootnode = Bootnode::new();
        if !self.no_el {
            bootnode = bootnode.with_el(ElBootnode::new(self.el.into()));
        }
        if !self.no_cl {
            bootnode = bootnode.with_cl(ClBootnode::new(self.cl.into_config(chain_id)));
        }

        info!(
            target: "bootnode",
            chain = %chain.name,
            chain_id = %chain_id,
            "starting bootnode"
        );

        // Use both SIGINT and SIGTERM via `install_signal_handler` (rather than
        // `run_until_ctrl_c`, which only handles SIGINT) so the bootnode shuts down gracefully
        // under systemd / kubernetes.
        let runtime = RuntimeManager::default()
            .tokio_runtime()
            .map_err(|e| eyre::eyre!("failed to build tokio runtime: {e}"))?;
        runtime.block_on(async move {
            let cancel = CancellationToken::new();
            let _signal_task = RuntimeManager::install_signal_handler(cancel.clone());
            bootnode.run(cancel).await.map_err(eyre::Report::new)
        })
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser, Debug)]
    struct Wrapper {
        #[command(flatten)]
        bootnode: BootnodeArgs,
    }

    #[test]
    fn parses_defaults() {
        let cli = Wrapper::parse_from(["test"]);
        assert!(!cli.bootnode.no_el);
        assert!(!cli.bootnode.no_cl);
        assert_eq!(cli.bootnode.el.addr.port(), DEFAULT_EL_BOOTNODE_PORT);
        assert_eq!(cli.bootnode.cl.listen_port, DEFAULT_CL_BOOTNODE_PORT);
        assert!(!cli.bootnode.el.no_discv5);
        assert!(!cli.bootnode.cl.static_ip);
    }

    #[test]
    fn parses_disable_flags() {
        let cli = Wrapper::parse_from(["test", "--no-el"]);
        assert!(cli.bootnode.no_el);
        assert!(!cli.bootnode.no_cl);

        let cli = Wrapper::parse_from(["test", "--no-cl"]);
        assert!(cli.bootnode.no_cl);
    }

    #[test]
    fn parses_repeated_bootnode_flags() {
        let cli =
            Wrapper::parse_from(["test", "--cl.bootnode", "enr:foo", "--cl.bootnode", "enr:bar"]);
        assert_eq!(cli.bootnode.cl.bootnodes, vec!["enr:foo".to_owned(), "enr:bar".to_owned()]);
    }

    #[test]
    fn cl_advertise_defaults_to_listen() {
        let args = ClArgs {
            listen_ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            listen_port: 9999,
            advertise_ip: None,
            advertise_tcp: None,
            advertise_udp: None,
            secret_key: None,
            bootstore: None,
            bootnodes: Vec::new(),
            static_ip: false,
        };
        let config = args.into_config(8453);
        assert_eq!(config.advertise_ip, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        assert_eq!(config.advertise_tcp_port, 9999);
        assert_eq!(config.advertise_udp_port, 9999);
        assert_eq!(config.chain_id, 8453);
    }

    #[test]
    fn cl_validate_advertise_ip_rejects_unspecified() {
        let args = ClArgs {
            listen_ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            listen_port: DEFAULT_CL_BOOTNODE_PORT,
            advertise_ip: None,
            advertise_tcp: None,
            advertise_udp: None,
            secret_key: None,
            bootstore: None,
            bootnodes: Vec::new(),
            static_ip: false,
        };
        let err = args.validate_advertise_ip().expect_err("should reject 0.0.0.0");
        assert!(err.to_string().contains("unspecified"));
    }

    #[test]
    fn cl_validate_advertise_ip_accepts_routable() {
        let args = ClArgs {
            listen_ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            listen_port: DEFAULT_CL_BOOTNODE_PORT,
            advertise_ip: Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))),
            advertise_tcp: None,
            advertise_udp: None,
            secret_key: None,
            bootstore: None,
            bootnodes: Vec::new(),
            static_ip: false,
        };
        args.validate_advertise_ip().expect("203.0.113.7 should validate");
    }
}
